//! Handshake UKEY2 (`securegcm.Ukey2Message` con tipos `CLIENT_INIT` /
//! `SERVER_INIT` / `CLIENT_FINISH`), implementado a mano sobre RustCrypto
//! (`p256` para ECDH P-256, `sha2` para el commitment SHA-512) siguiendo el
//! flujo público del protocolo UKEY2 de Google, tal como lo documentan
//! `google/ukey2` y las implementaciones de referencia `Martichou/rquickshare`
//! y `grishka/NearDrop` (ver /NOTICE).
//!
//! Flujo (cifrado único soportado: `P256_SHA512`):
//!
//! 1. **Initiator**: genera su par de claves P-256 efímero, construye por
//!    adelantado el mensaje `ClientFinish` que enviará más tarde, calcula
//!    `commitment = SHA512(ClientFinish serializado)`, y envía `ClientInit`
//!    con ese commitment.
//! 2. **Responder**: recibe `ClientInit`, guarda el `commitment` recibido,
//!    genera su propio par de claves P-256 efímero y responde `ServerInit`
//!    con su clave pública.
//! 3. **Initiator**: recibe `ServerInit`, y envía el `ClientFinish` que ya
//!    había preparado en el paso 1 (debe ser exactamente el mismo, para que
//!    su hash siga coincidiendo con el commitment ya enviado).
//! 4. **Responder**: recibe `ClientFinish`, verifica
//!    `SHA512(bytes recibidos) == commitment`, y extrae la clave pública del
//!    cliente.
//! 5. Ambos lados calculan el secreto ECDH con su clave privada efímera y la
//!    clave pública del otro, y lo pasan a [`crate::d2d`] para derivar las
//!    claves D2D finales.
//!
//! TODO(android-validation): el alcance exacto de los bytes cubiertos por el
//! commitment (el `Ukey2Message` completo que envuelve `ClientFinish`, no
//! solo el `Ukey2ClientFinished` interno) y el valor de `next_protocol`
//! siguen sin confirmarse contra una captura real de un dispositivo Android.

use crate::d2d::{D2DKeys, derive_d2d_keys, derive_ukey2_secrets};
use crate::error::{CryptoError, Result};
use kushare_proto::securegcm::{
    Ukey2ClientFinished, Ukey2ClientInit, Ukey2HandshakeCipher, Ukey2Message, Ukey2ServerInit,
    ukey2_client_init::CipherCommitment, ukey2_message::Type as Ukey2MessageType,
};
use kushare_proto::securemessage::{EcP256PublicKey, GenericPublicKey, PublicKeyType};
use p256::PublicKey;
use p256::ecdh::EphemeralSecret;
use p256::elliptic_curve::Generate;
use p256::elliptic_curve::sec1::ToSec1Point;
use prost::Message;
use sha2::{Digest, Sha512};

/// String de "next protocol" anunciada por el cliente en `ClientInit`.
///
/// TODO(android-validation): valor documentado públicamente por
/// implementaciones de referencia para el protocolo Nearby Connections /
/// Quick Share; confirmar contra un `ClientInit` real capturado con
/// tcpdump/Wireshark.
const NEXT_PROTOCOL: &str = "AES_256_CBC-HMAC_SHA256";

const RANDOM_LEN: usize = 32;
const UKEY2_VERSION: i32 = 1;

/// Resultado final de un handshake UKEY2 completado con éxito.
pub struct HandshakeOutput {
    /// Claves D2D (envío/recepción) ya orientadas a este lado de la conexión.
    pub keys: D2DKeys,
    /// "Authentication string" de UKEY2: útil para verificación visual
    /// opcional entre dispositivos; Quick Share en modo v1 (visible para
    /// todos) no lo requiere, pero se expone por si se necesita más adelante.
    pub auth_string: [u8; 32],
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf)
        .map_err(|_| CryptoError::InvalidState("no se pudo generar aleatoriedad del sistema"))?;
    Ok(buf)
}

fn encode_public_key(public_key: &PublicKey) -> Vec<u8> {
    let point = public_key.to_sec1_point(false); // sin comprimir: expone x e y
    let x = point
        .x()
        .expect("punto sin comprimir siempre tiene x")
        .to_vec();
    let y = point
        .y()
        .expect("punto sin comprimir siempre tiene y")
        .to_vec();

    GenericPublicKey {
        r#type: PublicKeyType::EcP256 as i32,
        ec_p256_public_key: Some(EcP256PublicKey { x, y }),
        rsa2048_public_key: None,
        dh2048_public_key: None,
    }
    .encode_to_vec()
}

fn decode_public_key(bytes: &[u8]) -> Result<PublicKey> {
    let generic = GenericPublicKey::decode(bytes)?;
    let ec = generic
        .ec_p256_public_key
        .ok_or(CryptoError::MalformedPublicKey)?;

    let mut sec1 = Vec::with_capacity(1 + ec.x.len() + ec.y.len());
    sec1.push(0x04); // prefijo de punto sin comprimir
    sec1.extend_from_slice(&ec.x);
    sec1.extend_from_slice(&ec.y);

    PublicKey::from_sec1_bytes(&sec1).map_err(|_| CryptoError::MalformedPublicKey)
}

fn build_client_finish_message(public_key: &PublicKey) -> Vec<u8> {
    let client_finished = Ukey2ClientFinished {
        public_key: Some(encode_public_key(public_key)),
    };
    Ukey2Message {
        message_type: Some(Ukey2MessageType::ClientFinish as i32),
        message_data: Some(client_finished.encode_to_vec()),
    }
    .encode_to_vec()
}

fn decode_ukey2_message(raw: &[u8], expected: Ukey2MessageType) -> Result<Vec<u8>> {
    let msg = Ukey2Message::decode(raw)?;
    if msg.message_type != Some(expected as i32) {
        return Err(CryptoError::UnexpectedMessageType {
            expected: expected.as_str_name(),
            got: msg.message_type,
        });
    }
    msg.message_data
        .ok_or(CryptoError::MissingField("Ukey2Message.message_data"))
}

// ---------------------------------------------------------------------
// Initiator (rol "cliente" del handshake UKEY2; en RX/TX puede ser tanto
// quien inicia el envío como quien acepta la conexión TCP entrante, según
// quién habla primero a nivel de Nearby Connections).
// ---------------------------------------------------------------------

enum InitiatorState {
    Fresh,
    SentClientInit { client_finish_msg: Vec<u8> },
    ReceivedServerInit { peer_public_key: PublicKey },
}

pub struct Ukey2Initiator {
    secret: EphemeralSecret,
    public_key: PublicKey,
    state: InitiatorState,
}

impl Ukey2Initiator {
    pub fn new() -> Self {
        let secret = EphemeralSecret::generate();
        let public_key = secret.public_key();
        Self {
            secret,
            public_key,
            state: InitiatorState::Fresh,
        }
    }

    /// Construye el `ClientInit` a enviar. Solo puede llamarse una vez, al
    /// inicio del handshake.
    pub fn build_client_init(&mut self) -> Result<Vec<u8>> {
        if !matches!(self.state, InitiatorState::Fresh) {
            return Err(CryptoError::InvalidState(
                "build_client_init ya fue llamado",
            ));
        }

        let client_finish_msg = build_client_finish_message(&self.public_key);
        let commitment = Sha512::digest(&client_finish_msg).to_vec();

        let client_init = Ukey2ClientInit {
            version: Some(UKEY2_VERSION),
            random: Some(random_bytes::<RANDOM_LEN>()?.to_vec()),
            cipher_commitments: vec![CipherCommitment {
                handshake_cipher: Some(Ukey2HandshakeCipher::P256Sha512 as i32),
                commitment: Some(commitment),
            }],
            next_protocol: Some(NEXT_PROTOCOL.to_string()),
        };

        let outer = Ukey2Message {
            message_type: Some(Ukey2MessageType::ClientInit as i32),
            message_data: Some(client_init.encode_to_vec()),
        };

        self.state = InitiatorState::SentClientInit { client_finish_msg };
        Ok(outer.encode_to_vec())
    }

    /// Procesa el `ServerInit` recibido y devuelve los bytes de
    /// `ClientFinish` a enviar a continuación.
    pub fn handle_server_init(&mut self, raw: &[u8]) -> Result<Vec<u8>> {
        let client_finish_msg = match std::mem::replace(&mut self.state, InitiatorState::Fresh) {
            InitiatorState::SentClientInit { client_finish_msg } => client_finish_msg,
            other => {
                self.state = other;
                return Err(CryptoError::InvalidState(
                    "handle_server_init llamado antes de build_client_init o dos veces",
                ));
            }
        };

        let data = decode_ukey2_message(raw, Ukey2MessageType::ServerInit)?;
        let server_init = Ukey2ServerInit::decode(data.as_slice())?;

        if server_init.handshake_cipher != Some(Ukey2HandshakeCipher::P256Sha512 as i32) {
            return Err(CryptoError::NoSupportedCipher);
        }

        let peer_public_key_bytes = server_init
            .public_key
            .ok_or(CryptoError::MissingField("Ukey2ServerInit.public_key"))?;
        let peer_public_key = decode_public_key(&peer_public_key_bytes)?;

        self.state = InitiatorState::ReceivedServerInit { peer_public_key };
        Ok(client_finish_msg)
    }

    /// Consume el handshake y deriva las claves D2D finales. Solo debe
    /// llamarse después de [`Ukey2Initiator::handle_server_init`] y de haber
    /// enviado el `ClientFinish` devuelto por ese método.
    pub fn finish(self) -> Result<HandshakeOutput> {
        let peer_public_key = match self.state {
            InitiatorState::ReceivedServerInit { peer_public_key } => peer_public_key,
            _ => {
                return Err(CryptoError::InvalidState(
                    "finish llamado antes de completar el handshake",
                ));
            }
        };

        let shared = self.secret.diffie_hellman(&peer_public_key);
        let secrets = derive_ukey2_secrets(shared.raw_secret_bytes().as_slice());
        let keys = derive_d2d_keys(&secrets.next_protocol_secret, true);

        Ok(HandshakeOutput {
            keys,
            auth_string: secrets.auth_string,
        })
    }
}

impl Default for Ukey2Initiator {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------
// Responder (rol "servidor" del handshake UKEY2).
// ---------------------------------------------------------------------

enum ResponderState {
    Fresh,
    AwaitingClientFinish { expected_commitment: Vec<u8> },
    ReceivedClientFinish { peer_public_key: PublicKey },
}

pub struct Ukey2Responder {
    secret: EphemeralSecret,
    public_key: PublicKey,
    state: ResponderState,
}

impl Ukey2Responder {
    pub fn new() -> Self {
        let secret = EphemeralSecret::generate();
        let public_key = secret.public_key();
        Self {
            secret,
            public_key,
            state: ResponderState::Fresh,
        }
    }

    /// Procesa el `ClientInit` recibido y devuelve los bytes de
    /// `ServerInit` a enviar a continuación.
    pub fn handle_client_init(&mut self, raw: &[u8]) -> Result<Vec<u8>> {
        if !matches!(self.state, ResponderState::Fresh) {
            return Err(CryptoError::InvalidState(
                "handle_client_init ya fue llamado",
            ));
        }

        let data = decode_ukey2_message(raw, Ukey2MessageType::ClientInit)?;
        let client_init = Ukey2ClientInit::decode(data.as_slice())?;

        let commitment = client_init
            .cipher_commitments
            .into_iter()
            .find(|c| c.handshake_cipher == Some(Ukey2HandshakeCipher::P256Sha512 as i32))
            .and_then(|c| c.commitment)
            .ok_or(CryptoError::NoSupportedCipher)?;

        let server_init = Ukey2ServerInit {
            version: Some(UKEY2_VERSION),
            random: Some(random_bytes::<RANDOM_LEN>()?.to_vec()),
            handshake_cipher: Some(Ukey2HandshakeCipher::P256Sha512 as i32),
            public_key: Some(encode_public_key(&self.public_key)),
        };

        let outer = Ukey2Message {
            message_type: Some(Ukey2MessageType::ServerInit as i32),
            message_data: Some(server_init.encode_to_vec()),
        };

        self.state = ResponderState::AwaitingClientFinish {
            expected_commitment: commitment,
        };
        Ok(outer.encode_to_vec())
    }

    /// Procesa el `ClientFinish` recibido, verificando que corresponda al
    /// commitment enviado en `ClientInit`.
    pub fn handle_client_finish(&mut self, raw: &[u8]) -> Result<()> {
        let expected_commitment = match std::mem::replace(&mut self.state, ResponderState::Fresh) {
            ResponderState::AwaitingClientFinish {
                expected_commitment,
            } => expected_commitment,
            other => {
                self.state = other;
                return Err(CryptoError::InvalidState(
                    "handle_client_finish llamado antes de handle_client_init o dos veces",
                ));
            }
        };

        let actual_commitment = Sha512::digest(raw).to_vec();
        if actual_commitment != expected_commitment {
            return Err(CryptoError::CommitmentMismatch);
        }

        let data = decode_ukey2_message(raw, Ukey2MessageType::ClientFinish)?;
        let client_finished = Ukey2ClientFinished::decode(data.as_slice())?;
        let peer_public_key_bytes = client_finished
            .public_key
            .ok_or(CryptoError::MissingField("Ukey2ClientFinished.public_key"))?;
        let peer_public_key = decode_public_key(&peer_public_key_bytes)?;

        self.state = ResponderState::ReceivedClientFinish { peer_public_key };
        Ok(())
    }

    /// Consume el handshake y deriva las claves D2D finales. Solo debe
    /// llamarse después de [`Ukey2Responder::handle_client_finish`].
    pub fn finish(self) -> Result<HandshakeOutput> {
        let peer_public_key = match self.state {
            ResponderState::ReceivedClientFinish { peer_public_key } => peer_public_key,
            _ => {
                return Err(CryptoError::InvalidState(
                    "finish llamado antes de completar el handshake",
                ));
            }
        };

        let shared = self.secret.diffie_hellman(&peer_public_key);
        let secrets = derive_ukey2_secrets(shared.raw_secret_bytes().as_slice());
        let keys = derive_d2d_keys(&secrets.next_protocol_secret, false);

        Ok(HandshakeOutput {
            keys,
            auth_string: secrets.auth_string,
        })
    }
}

impl Default for Ukey2Responder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_handshake_in_memory_derives_matching_keys() {
        let mut initiator = Ukey2Initiator::new();
        let mut responder = Ukey2Responder::new();

        let client_init = initiator.build_client_init().expect("build_client_init");
        let server_init = responder
            .handle_client_init(&client_init)
            .expect("handle_client_init");
        let client_finish = initiator
            .handle_server_init(&server_init)
            .expect("handle_server_init");
        responder
            .handle_client_finish(&client_finish)
            .expect("handle_client_finish");

        let initiator_out = initiator.finish().expect("initiator finish");
        let responder_out = responder.finish().expect("responder finish");

        assert_eq!(initiator_out.auth_string, responder_out.auth_string);
        assert_eq!(
            initiator_out.keys.encrypt_key,
            responder_out.keys.decrypt_key
        );
        assert_eq!(
            initiator_out.keys.decrypt_key,
            responder_out.keys.encrypt_key
        );
    }

    #[test]
    fn tampered_client_finish_is_rejected() {
        let mut initiator = Ukey2Initiator::new();
        let mut responder = Ukey2Responder::new();

        let client_init = initiator.build_client_init().expect("build_client_init");
        let server_init = responder
            .handle_client_init(&client_init)
            .expect("handle_client_init");
        let mut client_finish = initiator
            .handle_server_init(&server_init)
            .expect("handle_server_init");

        // Alterar un byte del ClientFinish debe romper la verificación del
        // commitment en el responder.
        let last = client_finish.len() - 1;
        client_finish[last] ^= 0xFF;

        let err = responder
            .handle_client_finish(&client_finish)
            .expect_err("un ClientFinish alterado debe ser rechazado");
        assert!(matches!(err, CryptoError::CommitmentMismatch));
    }
}
