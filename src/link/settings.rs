//! Ajustes del daemon (`~/.config/kushare/daemon.toml`): portapapeles,
//! precarga de fotos y comprobación de actualizaciones. Se cambian por
//! D-Bus (`SetSetting`) desde la GUI o la CLI.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Compartir el portapapeles con los teléfonos en ambos sentidos.
    pub clipboard_sync: bool,
    /// Guardar lo copiado en un historial (en este PC). Desactivado por
    /// defecto: el portapapeles puede llevar contraseñas.
    pub clipboard_history: bool,
    /// Precargar en segundo plano las fotos más recientes del teléfono.
    pub photo_prefetch: bool,
    /// Buscar versiones nuevas en GitHub una vez al día.
    pub update_check: bool,
    /// Preguntar antes de aceptar archivos por Wi-Fi (Quick Share): a ese
    /// receptor puede conectarse cualquiera de la red.
    pub receive_confirm: bool,
    /// Avisar en el escritorio de las fotos nuevas del teléfono.
    pub photo_notify: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            clipboard_sync: true,
            clipboard_history: false,
            photo_prefetch: true,
            update_check: true,
            receive_confirm: true,
            photo_notify: true,
        }
    }
}

/// Nombres de los ajustes tal como los ven D-Bus y la CLI.
pub const NAMES: &[&str] = &[
    "clipboard-sync",
    "clipboard-history",
    "photo-prefetch",
    "update-check",
    "receive-confirm",
    "photo-notify",
];

impl Settings {
    pub fn get(&self, name: &str) -> Option<bool> {
        Some(match name {
            "clipboard-sync" => self.clipboard_sync,
            "clipboard-history" => self.clipboard_history,
            "photo-prefetch" => self.photo_prefetch,
            "update-check" => self.update_check,
            "receive-confirm" => self.receive_confirm,
            "photo-notify" => self.photo_notify,
            _ => return None,
        })
    }

    fn slot(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "clipboard-sync" => &mut self.clipboard_sync,
            "clipboard-history" => &mut self.clipboard_history,
            "photo-prefetch" => &mut self.photo_prefetch,
            "update-check" => &mut self.update_check,
            "receive-confirm" => &mut self.receive_confirm,
            "photo-notify" => &mut self.photo_notify,
            _ => return None,
        })
    }
}

pub struct SettingsStore {
    path: PathBuf,
    value: Mutex<Settings>,
}

impl SettingsStore {
    pub fn open(config_dir: &Path) -> Self {
        let path = config_dir.join("daemon.toml");
        let value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            value: Mutex::new(value),
        }
    }

    pub fn get(&self) -> Settings {
        self.value.lock().expect("mutex envenenado").clone()
    }

    /// Cambia un ajuste y lo guarda. Error si el nombre no existe.
    pub fn set(&self, name: &str, on: bool) -> anyhow::Result<()> {
        let mut value = self.value.lock().expect("mutex envenenado");
        let slot = value
            .slot(name)
            .ok_or_else(|| anyhow::anyhow!("ajuste {name:?} desconocido: {}", NAMES.join(", ")))?;
        *slot = on;
        let text = toml::to_string_pretty(&*value)?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(dir.path());
        assert!(store.get().clipboard_sync);
        assert!(!store.get().clipboard_history);
        store.set("clipboard-history", true).unwrap();
        assert!(store.set("nope", true).is_err());
        let again = SettingsStore::open(dir.path());
        assert!(again.get().clipboard_history);
        for name in NAMES {
            assert!(again.get().get(name).is_some());
        }
    }
}
