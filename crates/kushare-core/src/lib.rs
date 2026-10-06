//! Tipos compartidos por el resto del workspace `kushare`: no contienen
//! lógica de red, criptografía ni UI, solo las estructuras de datos que
//! esos módulos intercambian entre sí.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

/// Identidad de este dispositivo tal como se anuncia por mDNS/BLE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    /// Identificador de punto final (endpoint id) de 4 caracteres usado por
    /// el protocolo Nearby Connections.
    pub endpoint_id: String,
}

/// Un dispositivo remoto encontrado por descubrimiento (mDNS o BLE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    pub name: String,
    pub endpoint_id: String,
    pub addr: SocketAddr,
}

/// Estado de visibilidad de este dispositivo frente a otros.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Visibility {
    /// Visible para cualquier dispositivo cercano (sin autenticación de
    /// cuenta de Google). Ver CLAUDE.md: "enrolamiento opcional en v1".
    #[default]
    VisibleToEveryone,
    Hidden,
}

/// Eventos emitidos por los módulos de transferencia (`kushare-transfer`)
/// hacia la UI (`kushare-tray`) para notificaciones y progreso.
#[derive(Debug, Clone)]
pub enum TransferEvent {
    IncomingRequest {
        device_name: String,
        file_name: String,
        file_size: u64,
    },
    Progress {
        file_name: String,
        bytes_transferred: u64,
        total_bytes: u64,
    },
    Completed {
        device_name: String,
        file_name: String,
        saved_to: PathBuf,
    },
    Failed {
        file_name: String,
        reason: String,
    },
}

/// Configuración persistente del usuario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub device_name: String,
    pub visibility: Visibility,
    pub download_dir: PathBuf,
    pub listen_port: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: default_device_name(),
            visibility: Visibility::default(),
            download_dir: default_download_dir(),
            listen_port: 0, // 0 = el sistema operativo asigna un puerto libre
        }
    }
}

fn default_device_name() -> String {
    hostname()
        .map(|h| format!("kushare @ {h}"))
        .unwrap_or_else(|| "kushare".to_string())
}

/// Genera un identificador de 4 caracteres alfanuméricos en mayúsculas para
/// `Device::endpoint_id`, tal como usa el protocolo Nearby Connections.
///
/// No necesita ser criptográficamente aleatorio (no es un secreto: viaja en
/// claro en el `ConnectionRequestFrame`), solo prácticamente distinto entre
/// dispositivos visibles en la misma red local, así que basta con una
/// semilla derivada del reloj y el PID.
pub fn random_endpoint_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let seed = hasher.finish();

    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..4)
        .map(|i| {
            let shift = i * 8;
            let idx = ((seed >> shift) as usize) % ALPHABET.len();
            ALPHABET[idx] as char
        })
        .collect()
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Directorio donde `kushare-transfer` guarda los archivos recibidos:
/// `~/Downloads` si existe, o `/tmp/kushare/` como respaldo (pedido
/// explícitamente por CLAUDE.md).
pub fn default_download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp/kushare"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_download_dir_is_never_empty() {
        assert!(!default_download_dir().as_os_str().is_empty());
    }

    #[test]
    fn random_endpoint_id_has_expected_shape() {
        let id = random_endpoint_id();
        assert_eq!(id.len(), 4);
        assert!(
            id.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        );
    }
}
