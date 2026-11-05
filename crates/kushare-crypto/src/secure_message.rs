//! Envoltura por-mensaje del canal ya establecido tras el handshake UKEY2:
//! cada `OfflineFrame` (ver `kushare_proto::offline_wire_formats`) se
//! empaqueta primero como `securegcm.DeviceToDeviceMessage` (con número de
//! secuencia creciente), se cifra con AES-256-CBC y se firma con
//! HMAC-SHA256 siguiendo el formato `securemessage.SecureMessage`.
//!
//! Ver [`crate::d2d`] para el origen de las 4 claves usadas aquí
//! (`encrypt_key`/`decrypt_key`/`encrypt_hmac_key`/`decrypt_hmac_key`).

use crate::d2d::D2DKeys;
use crate::error::{CryptoError, Result};
use hmac::{Hmac, KeyInit, Mac};
use kushare_proto::offline_wire_formats::OfflineFrame;
use kushare_proto::securegcm::DeviceToDeviceMessage;
use kushare_proto::securemessage::{EncScheme, Header, HeaderAndBody, SecureMessage, SigScheme};
use libaes::Cipher;
use prost::Message;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const AES_IV_LEN: usize = 16;

fn random_iv() -> Result<[u8; AES_IV_LEN]> {
    let mut iv = [0u8; AES_IV_LEN];
    getrandom::fill(&mut iv)
        .map_err(|_| CryptoError::InvalidState("no se pudo generar el IV de AES"))?;
    Ok(iv)
}

/// Canal seguro, ya derivado del handshake UKEY2, capaz de cifrar/descifrar
/// `OfflineFrame`s en ambas direcciones. Mantiene contadores de secuencia
/// independientes para envío y recepción, requeridos por
/// `DeviceToDeviceMessage.sequence_number`.
pub struct SecureChannel {
    keys: D2DKeys,
    send_seq: i32,
    recv_seq: i32,
    strict_sequence: bool,
}

impl SecureChannel {
    pub fn new(keys: D2DKeys) -> Self {
        Self {
            keys,
            send_seq: 0,
            recv_seq: 0,
            strict_sequence: false,
        }
    }

    /// Exige que cada mensaje recibido traiga exactamente el siguiente
    /// `sequence_number` esperado; si no, [`Self::decrypt_bytes`] falla con
    /// [`CryptoError::SequenceMismatch`]. Así un mensaje repetido,
    /// reordenado o descartado por un intermediario se detecta en vez de
    /// aceptarse en silencio (el HMAC por sí solo no lo impide: un
    /// `SecureMessage` capturado sigue siendo válido si se reenvía).
    ///
    /// Desactivado por defecto porque el camino compatible con Quick Share
    /// aún no confirma la semántica exacta que espera Android (ver el
    /// TODO(android-validation) en `decrypt_bytes`); el enlace persistente
    /// de kushare (`kushare-link`), que solo habla con otro kushare, sí lo
    /// activa.
    pub fn with_strict_sequence(mut self) -> Self {
        self.strict_sequence = true;
        self
    }

    /// Versión in situ de [`Self::with_strict_sequence`], para cuando el
    /// canal ya vive dentro de otra estructura.
    pub fn set_strict_sequence(&mut self, strict: bool) {
        self.strict_sequence = strict;
    }

    /// Cifra un `OfflineFrame` y devuelve los bytes de `SecureMessage`
    /// listos para enviarse por el framing TCP (ver `kushare_transfer`).
    pub fn encrypt_frame(&mut self, frame: &OfflineFrame) -> Result<Vec<u8>> {
        self.encrypt_bytes(&frame.encode_to_vec())
    }

    /// Descifra y verifica un `SecureMessage` recibido, devolviendo el
    /// `OfflineFrame` contenido.
    pub fn decrypt_frame(&mut self, raw: &[u8]) -> Result<OfflineFrame> {
        let frame_bytes = self.decrypt_bytes(raw)?;
        Ok(OfflineFrame::decode(frame_bytes.as_slice())?)
    }

    /// Igual que [`Self::encrypt_frame`] pero para un payload arbitrario
    /// (p. ej. un `LinkFrame` de `kushare-link` ya codificado), no
    /// necesariamente un `OfflineFrame`.
    pub fn encrypt_bytes(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.send_seq = self
            .send_seq
            .checked_add(1)
            .ok_or(CryptoError::InvalidState("contador de secuencia agotado"))?;

        let d2d_message = DeviceToDeviceMessage {
            message: Some(plaintext.to_vec()),
            sequence_number: Some(self.send_seq),
        };

        let iv = random_iv()?;
        let cipher = Cipher::new_256(&self.keys.encrypt_key);
        let ciphertext = cipher.cbc_encrypt(&iv, &d2d_message.encode_to_vec());

        let header = Header {
            signature_scheme: SigScheme::HmacSha256 as i32,
            encryption_scheme: EncScheme::Aes256Cbc as i32,
            verification_key_id: None,
            decryption_key_id: None,
            iv: Some(iv.to_vec()),
            public_metadata: None,
            associated_data_length: Some(0),
        };
        let header_and_body = HeaderAndBody {
            header,
            body: ciphertext,
        }
        .encode_to_vec();

        let mut mac = HmacSha256::new_from_slice(&self.keys.encrypt_hmac_key)
            .expect("HMAC-SHA256 acepta claves de cualquier longitud");
        mac.update(&header_and_body);
        let signature = mac.finalize().into_bytes().to_vec();

        Ok(SecureMessage {
            header_and_body,
            signature,
        }
        .encode_to_vec())
    }

    /// Igual que [`Self::decrypt_frame`] pero devuelve el payload en bruto,
    /// sin decodificarlo como `OfflineFrame`.
    pub fn decrypt_bytes(&mut self, raw: &[u8]) -> Result<Vec<u8>> {
        let secure_message = SecureMessage::decode(raw)?;

        let mut mac = HmacSha256::new_from_slice(&self.keys.decrypt_hmac_key)
            .expect("HMAC-SHA256 acepta claves de cualquier longitud");
        mac.update(&secure_message.header_and_body);
        mac.verify_slice(&secure_message.signature)
            .map_err(|_| CryptoError::SignatureMismatch)?;

        let header_and_body = HeaderAndBody::decode(secure_message.header_and_body.as_slice())?;
        let iv = header_and_body
            .header
            .iv
            .ok_or(CryptoError::MissingField("Header.iv"))?;

        let cipher = Cipher::new_256(&self.keys.decrypt_key);
        let plaintext = cipher.cbc_decrypt(&iv, &header_and_body.body);

        let d2d_message = DeviceToDeviceMessage::decode(plaintext.as_slice())?;
        let expected = self
            .recv_seq
            .checked_add(1)
            .ok_or(CryptoError::InvalidState("contador de secuencia agotado"))?;
        // TODO(android-validation): confirmar si Quick Share exige que
        // sequence_number sea estrictamente igual al contador local o solo
        // creciente; mientras tanto solo se exige en modo estricto.
        if self.strict_sequence && d2d_message.sequence_number != Some(expected) {
            return Err(CryptoError::SequenceMismatch {
                expected,
                got: d2d_message.sequence_number,
            });
        }
        self.recv_seq = expected;

        d2d_message
            .message
            .ok_or(CryptoError::MissingField("DeviceToDeviceMessage.message"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::d2d::{derive_d2d_keys, derive_ukey2_secrets};
    use kushare_proto::offline_wire_formats::{
        ConnectionRequestFrame, V1Frame, connection_request_frame::Medium, offline_frame::Version,
        v1_frame::FrameType,
    };

    fn paired_channels() -> (SecureChannel, SecureChannel) {
        let shared_secret = b"test only shared secret 32bytes";
        let secrets = derive_ukey2_secrets(shared_secret);
        let initiator_keys = derive_d2d_keys(&secrets.next_protocol_secret, true);
        let responder_keys = derive_d2d_keys(&secrets.next_protocol_secret, false);
        (
            SecureChannel::new(initiator_keys),
            SecureChannel::new(responder_keys),
        )
    }

    #[test]
    fn encrypt_then_decrypt_roundtrips() {
        let (mut sender, mut receiver) = paired_channels();

        let frame = OfflineFrame {
            version: Some(Version::V1 as i32),
            v1: Some(V1Frame {
                r#type: Some(FrameType::ConnectionRequest as i32),
                connection_request: Some(ConnectionRequestFrame {
                    endpoint_id: Some("XYZ1".to_string()),
                    endpoint_name: Some("kushare".to_string()),
                    mediums: vec![Medium::WifiLan as i32],
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        let encrypted = sender.encrypt_frame(&frame).expect("encrypt");
        let decrypted = receiver.decrypt_frame(&encrypted).expect("decrypt");
        assert_eq!(decrypted, frame);
    }

    #[test]
    fn tampered_ciphertext_fails_hmac_verification() {
        let (mut sender, mut receiver) = paired_channels();

        let frame = OfflineFrame {
            version: Some(Version::V1 as i32),
            v1: Some(V1Frame {
                r#type: Some(FrameType::KeepAlive as i32),
                ..Default::default()
            }),
        };

        let mut encrypted = sender.encrypt_frame(&frame).expect("encrypt");
        let last = encrypted.len() - 1;
        encrypted[last] ^= 0xFF;

        let err = receiver
            .decrypt_frame(&encrypted)
            .expect_err("un mensaje alterado debe fallar la verificación HMAC");
        assert!(matches!(err, CryptoError::SignatureMismatch));
    }

    #[test]
    fn raw_bytes_roundtrip() {
        let (mut sender, mut receiver) = paired_channels();
        let payload = b"payload arbitrario, no un OfflineFrame".to_vec();
        let encrypted = sender.encrypt_bytes(&payload).expect("encrypt");
        assert_eq!(
            receiver.decrypt_bytes(&encrypted).expect("decrypt"),
            payload
        );
    }

    #[test]
    fn strict_sequence_rejects_replayed_message() {
        let (sender, receiver) = paired_channels();
        let (mut sender, mut receiver) = (
            sender.with_strict_sequence(),
            receiver.with_strict_sequence(),
        );

        let first = sender.encrypt_bytes(b"uno").unwrap();
        let second = sender.encrypt_bytes(b"dos").unwrap();
        assert_eq!(receiver.decrypt_bytes(&first).unwrap(), b"uno");

        // Reenviar el primero (replay) debe fallar aunque su HMAC sea válido.
        let err = receiver.decrypt_bytes(&first).expect_err("replay");
        assert!(matches!(
            err,
            CryptoError::SequenceMismatch {
                expected: 2,
                got: Some(1)
            }
        ));

        // Un fallo no avanza el contador: el mensaje correcto sigue entrando.
        assert_eq!(receiver.decrypt_bytes(&second).unwrap(), b"dos");
    }

    #[test]
    fn lenient_mode_keeps_accepting_out_of_order_messages() {
        let (mut sender, mut receiver) = paired_channels();
        let first = sender.encrypt_bytes(b"uno").unwrap();
        let second = sender.encrypt_bytes(b"dos").unwrap();
        assert_eq!(receiver.decrypt_bytes(&second).unwrap(), b"dos");
        assert_eq!(receiver.decrypt_bytes(&first).unwrap(), b"uno");
    }
}
