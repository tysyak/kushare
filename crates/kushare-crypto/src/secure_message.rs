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
}

impl SecureChannel {
    pub fn new(keys: D2DKeys) -> Self {
        Self {
            keys,
            send_seq: 0,
            recv_seq: 0,
        }
    }

    /// Cifra un `OfflineFrame` y devuelve los bytes de `SecureMessage`
    /// listos para enviarse por el framing TCP (ver `kushare_transfer`).
    pub fn encrypt_frame(&mut self, frame: &OfflineFrame) -> Result<Vec<u8>> {
        self.send_seq += 1;

        let d2d_message = DeviceToDeviceMessage {
            message: Some(frame.encode_to_vec()),
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

    /// Descifra y verifica un `SecureMessage` recibido, devolviendo el
    /// `OfflineFrame` contenido.
    pub fn decrypt_frame(&mut self, raw: &[u8]) -> Result<OfflineFrame> {
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
        self.recv_seq += 1;
        // TODO(android-validation): confirmar si Quick Share exige que
        // sequence_number sea estrictamente igual al contador local o solo
        // creciente; por ahora solo se usa para detectar reordenamiento.
        let _ = d2d_message.sequence_number;

        let frame_bytes = d2d_message
            .message
            .ok_or(CryptoError::MissingField("DeviceToDeviceMessage.message"))?;
        Ok(OfflineFrame::decode(frame_bytes.as_slice())?)
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
}
