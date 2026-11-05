//! Identidad a largo plazo de este dispositivo: un par de claves Ed25519
//! generado una vez y guardado en disco. El `device_id` se deriva de la
//! clave pública, así que no se puede reclamar el id de otro dispositivo
//! sin tener su clave privada (ver `crate::pairing::verify_auth_proof`).

use anyhow::Context;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Longitud del `device_id` en caracteres hex (64 bits). Suficiente para
/// que una colisión dirigida (segunda preimagen) sea impracticable; además
/// la clave pública completa se compara siempre contra la guardada.
const DEVICE_ID_HEX_LEN: usize = 16;

pub struct DeviceIdentity {
    signing: SigningKey,
    device_id: String,
}

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Nunca imprimir la clave privada.
        f.debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity {
    pub fn generate() -> anyhow::Result<Self> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|e| anyhow::anyhow!("no se pudo generar la clave de identidad: {e}"))?;
        Ok(Self::from_secret(secret))
    }

    pub fn from_secret(secret: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&secret);
        let device_id = device_id_for(signing.verifying_key().as_bytes());
        Self { signing, device_id }
    }

    /// Carga la identidad de `path` (32 bytes de clave privada en hex), o
    /// la genera y la guarda si el archivo no existe. En Unix el archivo se
    /// crea con permisos 0600.
    pub fn load_or_create(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let bytes = decode_hex(text.trim())
                    .with_context(|| format!("identidad corrupta en {}", path.display()))?;
                let secret: [u8; 32] = bytes.try_into().map_err(|_| {
                    anyhow::anyhow!("identidad en {} no mide 32 bytes", path.display())
                })?;
                Ok(Self::from_secret(secret))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                write_private_file(path, encode_hex(identity.signing.as_bytes()).as_bytes())
                    .with_context(|| {
                        format!("no se pudo guardar la identidad en {}", path.display())
                    })?;
                tracing::info!(device_id = %identity.device_id, "identidad nueva generada");
                Ok(identity)
            }
            Err(err) => Err(err).with_context(|| format!("no se pudo leer {}", path.display())),
        }
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }
}

/// `device_id` que corresponde a una clave pública: los primeros 64 bits
/// de su SHA-256, en hex.
pub fn device_id_for(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    let mut id = encode_hex(&digest);
    id.truncate(DEVICE_ID_HEX_LEN);
    id
}

/// Verifica una firma Ed25519 (en modo estricto: rechaza claves débiles y
/// firmas maleables).
pub fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let Ok(pk) = <[u8; 32]>::try_from(public_key) else {
        return false;
    };
    let Ok(sig) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&pk) else {
        return false;
    };
    let sig = Signature::from_bytes(&sig);
    key.verify_strict(message, &sig).is_ok() && key.verify(message, &sig).is_ok()
}

pub fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

pub fn decode_hex(text: &str) -> anyhow::Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        anyhow::bail!("hex inválido");
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| anyhow::anyhow!("hex inválido"))
        })
        .collect()
}

/// Escribe `contents` en `path` de forma atómica (archivo temporal +
/// rename) y, en Unix, con permisos 0600 desde el primer momento.
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/identity.key");
        let first = DeviceIdentity::load_or_create(&path).unwrap();
        let second = DeviceIdentity::load_or_create(&path).unwrap();
        assert_eq!(first.device_id(), second.device_id());
        assert_eq!(first.public_key(), second.public_key());
        assert_eq!(first.device_id().len(), DEVICE_ID_HEX_LEN);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn signatures_verify_only_with_the_right_key() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        let sig = a.sign(b"mensaje");
        assert!(verify(&a.public_key(), b"mensaje", &sig));
        assert!(!verify(&a.public_key(), b"otro mensaje", &sig));
        assert!(!verify(&b.public_key(), b"mensaje", &sig));
        assert!(!verify(&a.public_key()[..31], b"mensaje", &sig));
    }

    #[test]
    fn hex_roundtrips() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(decode_hex(&encode_hex(&bytes)).unwrap(), bytes);
        assert!(decode_hex("abc").is_err());
        assert!(decode_hex("zz").is_err());
    }
}
