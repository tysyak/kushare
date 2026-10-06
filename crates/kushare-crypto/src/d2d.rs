//! Derivación de claves posterior al handshake UKEY2, siguiendo el esquema
//! documentado públicamente para UKEY2 / D2D (Device-to-Device) de Google y
//! confirmado por implementaciones de referencia como `grishka/NearDrop`
//! (ver PROTOCOL.md) y `Martichou/rquickshare`. Todo HKDF usa SHA-256.
//!
//! Cadena de derivación completa:
//!
//! 1. `dh_secret` = ECDH(mi_clave_privada_efímera, clave_pública_del_peer)
//! 2. `auth_string` = HKDF(dh_secret, salt=SHA256("UKEY2 v1 auth"), info="UKEY2 v1 auth"),
//!    `next_protocol_secret` = HKDF(dh_secret, salt=SHA256("UKEY2 v1 next"), info="UKEY2 v1 next")
//! 3. `client_key` = HKDF(next_protocol_secret, salt=SHA256("D2D"), info="client"),
//!    `server_key` = HKDF(next_protocol_secret, salt=SHA256("D2D"), info="server")
//! 4. Por cada dirección: `enc_key` = HKDF(d2d_key, salt=SHA256("SecureMessage"), info="ENC:2"),
//!    `hmac_key` = HKDF(d2d_key, salt=SHA256("SecureMessage"), info="SIG:1")
//!
//! TODO(android-validation): estas cadenas de sal/info están documentadas
//! públicamente y coinciden con lo que reportan las implementaciones de
//! referencia citadas arriba, pero no han sido verificadas byte a byte
//! contra un handshake real capturado de un dispositivo Android con
//! `tcpdump`/Wireshark. Si el auth_string o las claves D2D no coinciden con
//! un peer real, este es el primer lugar a revisar.

use hkdf::Hkdf;
use sha2::{Digest, Sha256};

const KEY_LEN: usize = 32;

fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; KEY_LEN] {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = [0u8; KEY_LEN];
    hk.expand(info, &mut out)
        .expect("KEY_LEN is a valid HKDF-SHA256 output length");
    out
}

fn salt(label: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    hasher.finalize().into()
}

/// Deriva `auth_string` (verificación visual opcional) y
/// `next_protocol_secret` (entrada de la derivación D2D) a partir del
/// secreto compartido ECDH crudo del handshake UKEY2.
pub struct Ukey2Secrets {
    pub auth_string: [u8; KEY_LEN],
    pub next_protocol_secret: [u8; KEY_LEN],
}

pub fn derive_ukey2_secrets(dh_secret: &[u8]) -> Ukey2Secrets {
    Ukey2Secrets {
        auth_string: hkdf_sha256(dh_secret, &salt("UKEY2 v1 auth"), b"UKEY2 v1 auth"),
        next_protocol_secret: hkdf_sha256(dh_secret, &salt("UKEY2 v1 next"), b"UKEY2 v1 next"),
    }
}

/// Claves D2D direccionales (una por cada extremo de la conexión).
pub struct D2DKeys {
    /// Clave AES-256 para cifrar lo que este proceso envía.
    pub encrypt_key: [u8; KEY_LEN],
    /// Clave AES-256 para descifrar lo que este proceso recibe.
    pub decrypt_key: [u8; KEY_LEN],
    /// Clave HMAC-SHA256 para firmar lo que este proceso envía.
    pub encrypt_hmac_key: [u8; KEY_LEN],
    /// Clave HMAC-SHA256 para verificar lo que este proceso recibe.
    pub decrypt_hmac_key: [u8; KEY_LEN],
}

/// `is_client`: true si este proceso fue el iniciador (ClientInit) del
/// handshake UKEY2, false si fue el respondedor (ServerInit).
pub fn derive_d2d_keys(next_protocol_secret: &[u8], is_client: bool) -> D2DKeys {
    let d2d_salt = salt("D2D");
    let client_key = hkdf_sha256(next_protocol_secret, &d2d_salt, b"client");
    let server_key = hkdf_sha256(next_protocol_secret, &d2d_salt, b"server");

    let (send_d2d_key, recv_d2d_key) = if is_client {
        (client_key, server_key)
    } else {
        (server_key, client_key)
    };

    let sm_salt = salt("SecureMessage");
    D2DKeys {
        encrypt_key: hkdf_sha256(&send_d2d_key, &sm_salt, b"ENC:2"),
        encrypt_hmac_key: hkdf_sha256(&send_d2d_key, &sm_salt, b"SIG:1"),
        decrypt_key: hkdf_sha256(&recv_d2d_key, &sm_salt, b"ENC:2"),
        decrypt_hmac_key: hkdf_sha256(&recv_d2d_key, &sm_salt, b"SIG:1"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_and_server_derive_matching_cross_keys() {
        let shared_secret = b"a fixed 32-byte shared secret!!";
        let secrets = derive_ukey2_secrets(shared_secret);

        let client = derive_d2d_keys(&secrets.next_protocol_secret, true);
        let server = derive_d2d_keys(&secrets.next_protocol_secret, false);

        // Lo que el cliente cifra para enviar, el servidor debe poder
        // descifrarlo con su clave de recepción, y viceversa.
        assert_eq!(client.encrypt_key, server.decrypt_key);
        assert_eq!(client.decrypt_key, server.encrypt_key);
        assert_eq!(client.encrypt_hmac_key, server.decrypt_hmac_key);
        assert_eq!(client.decrypt_hmac_key, server.encrypt_hmac_key);
    }

    #[test]
    fn derivation_is_deterministic() {
        let shared_secret = b"another fixed shared secret!!!!";
        let a = derive_ukey2_secrets(shared_secret);
        let b = derive_ukey2_secrets(shared_secret);
        assert_eq!(a.auth_string, b.auth_string);
        assert_eq!(a.next_protocol_secret, b.next_protocol_secret);
    }
}
