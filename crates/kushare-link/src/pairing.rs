//! PIN de emparejado y prueba de identidad atada a la sesión.
//!
//! - **Emparejado** (primera vez): UKEY2 por sí solo no impide un
//!   intermediario (MITM) — negocia claves con quien sea que esté al otro
//!   lado. Por eso ambos lados muestran un PIN derivado del `auth_string`
//!   de *su* sesión UKEY2: si hay un MITM, cada lado tiene una sesión
//!   distinta con él y los PIN no coinciden. El usuario confirma en los dos
//!   dispositivos que ve el mismo número.
//! - **Reconexión**: no hay PIN. Cada lado firma el `auth_string` de esta
//!   sesión con su clave Ed25519 a largo plazo ([`auth_proof_message`]) y
//!   el otro verifica la firma contra la clave guardada al emparejar. Un
//!   MITM no puede reenviar la firma de un lado al otro porque sus dos
//!   sesiones tienen `auth_string` distintos, ni fabricarla sin la clave.

use sha2::{Digest, Sha256};

use crate::identity::{DeviceIdentity, verify};

/// Dominio de separación de las firmas: evita que una firma hecha para
/// otro propósito con la misma clave sirva aquí.
const AUTH_PROOF_DOMAIN: &[u8] = b"kushare-link-v1";

/// Rol en el handshake; entra en lo firmado para que la firma del
/// iniciador no pueda hacerse pasar por la del respondedor (reflejo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Initiator,
    Responder,
}

impl Role {
    pub fn peer(self) -> Self {
        match self {
            Role::Initiator => Role::Responder,
            Role::Responder => Role::Initiator,
        }
    }

    fn tag(self) -> u8 {
        match self {
            Role::Initiator => 0,
            Role::Responder => 1,
        }
    }
}

/// PIN de 6 dígitos que ambos lados muestran al emparejar.
pub fn pin_for(auth_string: &[u8]) -> String {
    let digest = Sha256::new()
        .chain_update(b"kushare-pin")
        .chain_update(auth_string)
        .finalize();
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{n:06}")
}

pub fn auth_proof_message(role: Role, auth_string: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(AUTH_PROOF_DOMAIN.len() + 1 + auth_string.len());
    msg.extend_from_slice(AUTH_PROOF_DOMAIN);
    msg.push(role.tag());
    msg.extend_from_slice(auth_string);
    msg
}

pub fn make_auth_proof(identity: &DeviceIdentity, role: Role, auth_string: &[u8]) -> [u8; 64] {
    identity.sign(&auth_proof_message(role, auth_string))
}

/// Verifica la prueba que mandó el peer (que tenía el rol `peer_role`).
pub fn verify_auth_proof(
    peer_public_key: &[u8],
    peer_role: Role,
    auth_string: &[u8],
    signature: &[u8],
) -> bool {
    verify(
        peer_public_key,
        &auth_proof_message(peer_role, auth_string),
        signature,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_is_six_digits_and_deterministic() {
        let pin = pin_for(&[7u8; 32]);
        assert_eq!(pin.len(), 6);
        assert!(pin.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(pin, pin_for(&[7u8; 32]));
        assert_ne!(pin, pin_for(&[8u8; 32]));
    }

    #[test]
    fn auth_proof_is_bound_to_role_session_and_key() {
        let id = DeviceIdentity::generate().unwrap();
        let other = DeviceIdentity::generate().unwrap();
        let session = [1u8; 32];
        let proof = make_auth_proof(&id, Role::Initiator, &session);

        assert!(verify_auth_proof(
            &id.public_key(),
            Role::Initiator,
            &session,
            &proof
        ));
        // Otro rol (ataque de reflejo), otra sesión (replay) u otra clave: no.
        assert!(!verify_auth_proof(
            &id.public_key(),
            Role::Responder,
            &session,
            &proof
        ));
        assert!(!verify_auth_proof(
            &id.public_key(),
            Role::Initiator,
            &[2u8; 32],
            &proof
        ));
        assert!(!verify_auth_proof(
            &other.public_key(),
            Role::Initiator,
            &session,
            &proof
        ));
    }
}
