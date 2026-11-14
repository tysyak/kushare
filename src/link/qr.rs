//! Códigos QR (y de barras) leídos con el escáner del teléfono: historial
//! en este PC (`~/.local/share/kushare/qr.json`, solo para tu usuario) y
//! qué es cada uno (web, Wi-Fi, teléfono…) para ofrecer lo que tenga
//! sentido. Nunca se abre nada solo: abrir una web es siempre un clic.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::sync::watch;

/// Cuántos se guardan como mucho (los más recientes).
const MAX: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrEntry {
    pub text: String,
    /// Formato ZXing ("QR_CODE", "EAN_13"…).
    pub format: String,
    pub date_ms: i64,
    /// Teléfono que lo leyó.
    #[serde(default)]
    pub device: String,
}

/// Qué es el contenido de un código.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrKind {
    /// Web (http/https): se puede abrir en el navegador.
    Url,
    /// Red Wi-Fi: (nombre, contraseña, seguridad).
    Wifi {
        ssid: String,
        password: String,
        security: String,
    },
    Phone(String),
    Email(String),
    /// Contacto (vCard / MECARD).
    Contact,
    Text,
}

impl QrKind {
    pub fn of(text: &str) -> Self {
        let lower = text.trim_start().to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return QrKind::Url;
        }
        if lower.starts_with("wifi:") {
            return parse_wifi(text.trim()).unwrap_or(QrKind::Text);
        }
        if let Some(n) = lower.strip_prefix("tel:") {
            return QrKind::Phone(n.trim().to_string());
        }
        if lower.starts_with("mailto:") || lower.starts_with("matmsg:") {
            let addr = text
                .trim()
                .split_once(':')
                .map(|(_, rest)| {
                    rest.trim_start_matches("TO:")
                        .split([';', '?'])
                        .next()
                        .unwrap_or("")
                })
                .unwrap_or("");
            return QrKind::Email(addr.to_string());
        }
        if lower.starts_with("begin:vcard") || lower.starts_with("mecard:") {
            return QrKind::Contact;
        }
        QrKind::Text
    }

    /// Nombre corto para la D-Bus y la CLI.
    pub fn name(&self) -> &'static str {
        match self {
            QrKind::Url => "url",
            QrKind::Wifi { .. } => "wifi",
            QrKind::Phone(_) => "phone",
            QrKind::Email(_) => "email",
            QrKind::Contact => "contact",
            QrKind::Text => "text",
        }
    }
}

/// `WIFI:T:WPA;S:MiRed;P:clave;;` (con `\` para escapar `;` `,` `:` `\`).
fn parse_wifi(text: &str) -> Option<QrKind> {
    let body = text.get(5..)?;
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ';' => fields.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    let (mut ssid, mut password, mut security) = (String::new(), String::new(), String::new());
    for f in fields {
        if let Some((k, v)) = f.split_once(':') {
            match k.to_ascii_uppercase().as_str() {
                "S" => ssid = v.to_string(),
                "P" => password = v.to_string(),
                "T" => security = v.to_string(),
                _ => {}
            }
        }
    }
    (!ssid.is_empty()).then_some(QrKind::Wifi {
        ssid,
        password,
        security,
    })
}

pub struct QrStore {
    path: PathBuf,
    entries: Mutex<Vec<QrEntry>>,
    changed: watch::Sender<u64>,
}

impl QrStore {
    pub fn open(data_dir: &Path) -> Self {
        let path = data_dir.join("qr.json");
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path,
            entries: Mutex::new(entries),
            changed: watch::channel(0).0,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Más reciente primero.
    pub fn list(&self) -> Vec<QrEntry> {
        self.entries.lock().expect("mutex envenenado").clone()
    }

    pub fn get(&self, index: usize) -> Option<QrEntry> {
        self.entries
            .lock()
            .expect("mutex envenenado")
            .get(index)
            .cloned()
    }

    /// Añade uno; `false` si es el mismo que el último de hace un momento
    /// (el escáner puede leerlo dos veces seguidas).
    pub fn add(&self, entry: QrEntry) -> bool {
        {
            let mut entries = self.entries.lock().expect("mutex envenenado");
            if entries.first().is_some_and(|last| {
                last.text == entry.text && (entry.date_ms - last.date_ms).abs() < 5000
            }) {
                return false;
            }
            entries.insert(0, entry);
            entries.truncate(MAX);
        }
        self.save();
        true
    }

    /// Borra uno (`index` ≥ 0) o todos (`-1`).
    pub fn forget(&self, index: i32) {
        {
            let mut entries = self.entries.lock().expect("mutex envenenado");
            if index < 0 {
                entries.clear();
            } else if (index as usize) < entries.len() {
                entries.remove(index as usize);
            }
        }
        self.save();
    }

    fn save(&self) {
        let text = serde_json::to_string_pretty(&*self.entries.lock().expect("mutex envenenado"))
            .unwrap_or_default();
        if let Err(err) = super::features::write_private(&self.path, text.as_bytes()) {
            tracing::warn!(%err, "no se pudo guardar el historial de QR");
        }
        self.changed.send_modify(|n| *n += 1);
    }
}

/// Guarda el texto de un código en Descargas (`kushare-qr-<fecha>.txt`).
pub fn save_to_downloads(entry: &QrEntry) -> std::io::Result<PathBuf> {
    let dir = directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("kushare-qr-{}.txt", entry.date_ms));
    std::fs::write(&path, &entry.text)?;
    Ok(path)
}

/// Abre una web en el navegador. Solo http/https: un QR no puede abrir
/// otro tipo de enlace (`file:`, esquemas de apps…).
pub fn open_url(text: &str) -> anyhow::Result<()> {
    if QrKind::of(text) != QrKind::Url {
        anyhow::bail!("no es una web (http/https)");
    }
    std::process::Command::new("xdg-open")
        .arg(text.trim())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_detected() {
        assert_eq!(QrKind::of("https://kushare.org/x"), QrKind::Url);
        assert_eq!(QrKind::of("HTTP://A.B"), QrKind::Url);
        assert_eq!(QrKind::of("file:///etc/passwd"), QrKind::Text);
        assert_eq!(QrKind::of("javascript:alert(1)"), QrKind::Text);
        assert_eq!(
            QrKind::of(r"WIFI:T:WPA;S:Mi\;Red;P:cla\:ve;;"),
            QrKind::Wifi {
                ssid: "Mi;Red".into(),
                password: "cla:ve".into(),
                security: "WPA".into()
            }
        );
        assert_eq!(
            QrKind::of("tel:+525512345678"),
            QrKind::Phone("+525512345678".into())
        );
        assert_eq!(
            QrKind::of("mailto:a@b.c?subject=x"),
            QrKind::Email("a@b.c".into())
        );
        assert_eq!(QrKind::of("BEGIN:VCARD\nFN:Ana"), QrKind::Contact);
        assert_eq!(QrKind::of("7501234567890"), QrKind::Text);
    }

    #[test]
    fn store_dedupes_and_forgets() {
        let dir = tempfile::tempdir().unwrap();
        let store = QrStore::open(dir.path());
        let e = |text: &str, date_ms| QrEntry {
            text: text.into(),
            format: "QR_CODE".into(),
            date_ms,
            device: String::new(),
        };
        assert!(store.add(e("hola", 1000)));
        assert!(!store.add(e("hola", 2000)));
        assert!(store.add(e("adiós", 3000)));
        assert_eq!(store.list()[0].text, "adiós");
        let again = QrStore::open(dir.path());
        assert_eq!(again.list().len(), 2);
        again.forget(0);
        assert_eq!(again.list()[0].text, "hola");
        again.forget(-1);
        assert!(again.list().is_empty());
    }
}
