//! Portapapeles compartido con los teléfonos, en ambos sentidos, con
//! historial opcional (desactivado por defecto).
//!
//! - **Wayland** (Hyprland, KDE, Sway…): protocolo *data-control* vía
//!   `wl-clipboard-rs`, que deja leer y escribir el portapapeles sin tener
//!   una ventana con foco. GNOME no lo implementa: ahí se usa X11
//!   (XWayland), que Mutter mantiene sincronizado.
//! - **X11**: `arboard`.
//!
//! No hay aviso de "el portapapeles cambió" sin ventana, así que se mira
//! una vez por segundo (solo los tipos de texto; lo demás ni se lee).
//! Lo que un gestor de contraseñas marca como secreto
//! (`x-kde-passwordManagerHint`) no se comparte ni se guarda.

use kushare_link::LinkManager;
use kushare_link::pb::{self, feature::Kind as F};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::settings::SettingsStore;

const POLL: Duration = Duration::from_secs(1);
/// Textos más grandes no se comparten (por Bluetooth tardarían mucho).
const MAX_TEXT: usize = 256 * 1024;
const HISTORY_MAX: usize = 100;
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    pub timestamp_ms: i64,
    /// "" = copiado en este PC; si no, el nombre del dispositivo.
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Wayland,
    X11,
    None,
}

#[derive(Default)]
struct State {
    /// Último texto visto o puesto (para no reenviar ecos).
    last: Option<String>,
    history: Vec<Entry>,
}

pub struct Clipboard {
    manager: LinkManager,
    settings: Arc<SettingsStore>,
    history_path: PathBuf,
    backend: Mutex<Backend>,
    state: Mutex<State>,
    /// Se incrementa al cambiar el historial (señal D-Bus `ClipboardChanged`).
    changes: tokio::sync::watch::Sender<u64>,
}

/// Resultado de leer el portapapeles local.
enum Clip {
    Text(String),
    /// Hay algo, pero no es texto o es secreto: no compartir.
    Skip,
    /// Vacío o no disponible ahora.
    Nothing,
}

impl Clipboard {
    pub fn start(manager: LinkManager, settings: Arc<SettingsStore>, data_dir: &Path) -> Arc<Self> {
        let history_path = data_dir.join("clipboard-history.json");
        let history = if settings.get().clipboard_history {
            std::fs::read(&history_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default()
        } else {
            // Si el historial está desactivado no debe quedar nada guardado.
            let _ = std::fs::remove_file(&history_path);
            Vec::new()
        };
        let backend = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            Backend::Wayland
        } else if std::env::var_os("DISPLAY").is_some() {
            Backend::X11
        } else {
            Backend::None
        };
        let clipboard = Arc::new(Self {
            manager,
            settings,
            history_path,
            backend: Mutex::new(backend),
            state: Mutex::new(State {
                last: None,
                history,
            }),
            changes: tokio::sync::watch::channel(0).0,
        });
        let poller = clipboard.clone();
        std::thread::Builder::new()
            .name("kushare-clipboard".into())
            .spawn(move || poller.poll_loop())
            .expect("no se pudo crear el hilo del portapapeles");
        clipboard
    }

    fn backend(&self) -> Backend {
        *self.backend.lock().expect("mutex envenenado")
    }

    fn poll_loop(&self) {
        // Lo que ya estaba copiado al arrancar no se manda.
        if let Clip::Text(text) = self.read() {
            self.state.lock().expect("mutex envenenado").last = Some(text);
        }
        loop {
            std::thread::sleep(POLL);
            let settings = self.settings.get();
            if !settings.clipboard_sync && !settings.clipboard_history {
                continue;
            }
            match self.read() {
                Clip::Text(text) => self.on_local(text),
                Clip::Skip | Clip::Nothing => {}
            }
        }
    }

    fn read(&self) -> Clip {
        match self.backend() {
            Backend::Wayland => match read_wayland() {
                Ok(r) => r,
                Err(WaylandError::Unsupported) => {
                    // Sin data-control (GNOME): probar por XWayland.
                    let next = if std::env::var_os("DISPLAY").is_some() {
                        Backend::X11
                    } else {
                        Backend::None
                    };
                    tracing::info!(
                        ?next,
                        "el compositor no tiene data-control para el portapapeles"
                    );
                    *self.backend.lock().expect("mutex envenenado") = next;
                    Clip::Nothing
                }
                Err(WaylandError::Other) => Clip::Nothing,
            },
            Backend::X11 => match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                Ok(text) if text.len() <= MAX_TEXT => Clip::Text(text),
                Ok(_) => Clip::Skip,
                Err(_) => Clip::Nothing,
            },
            Backend::None => Clip::Nothing,
        }
    }

    /// Algo nuevo copiado en el PC.
    fn on_local(&self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let settings = self.settings.get();
        {
            let mut state = self.state.lock().expect("mutex envenenado");
            if state.last.as_deref() == Some(text.as_str()) {
                return;
            }
            state.last = Some(text.clone());
        }
        if settings.clipboard_history {
            self.remember(&text, "");
        }
        if settings.clipboard_sync {
            tracing::debug!(len = text.len(), "portapapeles del PC → teléfonos");
            self.manager.broadcast_feature(pb::Feature {
                kind: Some(F::Clipboard(pb::ClipboardContent {
                    text,
                    timestamp_ms: now_ms(),
                })),
            });
        }
    }

    /// Texto copiado en un teléfono.
    pub fn on_remote(&self, peer_id: &str, text: String) {
        let settings = self.settings.get();
        if !settings.clipboard_sync || text.is_empty() || text.len() > MAX_TEXT {
            return;
        }
        {
            let mut state = self.state.lock().expect("mutex envenenado");
            if state.last.as_deref() == Some(text.as_str()) {
                return;
            }
            state.last = Some(text.clone());
        }
        tracing::debug!(len = text.len(), "portapapeles del teléfono → PC");
        if let Err(err) = self.write_text(&text) {
            tracing::info!(%err, "no se pudo poner el portapapeles");
        }
        if settings.clipboard_history {
            let name = self
                .manager
                .peers()
                .into_iter()
                .find(|p| p.device_id == peer_id)
                .map(|p| p.device_name)
                .unwrap_or_else(|| peer_id.to_string());
            self.remember(&text, &name);
        }
    }

    fn write_text(&self, text: &str) -> anyhow::Result<()> {
        match self.backend() {
            Backend::Wayland => {
                use wl_clipboard_rs::copy::{MimeType, Options, Source};
                Options::new().copy(
                    Source::Bytes(text.as_bytes().to_vec().into_boxed_slice()),
                    MimeType::Text,
                )?;
            }
            Backend::X11 => arboard::Clipboard::new()?.set_text(text.to_string())?,
            Backend::None => anyhow::bail!("no hay portapapeles (ni Wayland ni X11)"),
        }
        Ok(())
    }

    /// Copia `text` en el PC y lo manda a los teléfonos (p. ej. desde el
    /// historial de la GUI).
    pub fn copy_text(&self, text: &str) -> anyhow::Result<()> {
        self.state.lock().expect("mutex envenenado").last = Some(text.to_string());
        self.write_text(text)?;
        if self.settings.get().clipboard_sync {
            self.manager.broadcast_feature(pb::Feature {
                kind: Some(F::Clipboard(pb::ClipboardContent {
                    text: text.to_string(),
                    timestamp_ms: now_ms(),
                })),
            });
        }
        Ok(())
    }

    /// Copia un archivo (p. ej. una foto del teléfono): su contenido con
    /// su tipo MIME y además como `text/uri-list`, para pegarlo como
    /// imagen en un editor o como archivo en un gestor de archivos.
    pub fn copy_file(&self, path: &Path) -> anyhow::Result<()> {
        if self.backend() != Backend::Wayland {
            anyhow::bail!("copiar imágenes solo funciona en Wayland por ahora");
        }
        use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};
        let data = std::fs::read(path)?;
        let mime = match path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref()
        {
            Some("png") => "image/png",
            Some("webp") => "image/webp",
            Some("gif") => "image/gif",
            Some("heic" | "heif") => "image/heif",
            _ => "image/jpeg",
        };
        let uri = format!("file://{}\r\n", path.display());
        Options::new().copy_multi(vec![
            MimeSource {
                source: Source::Bytes(data.into_boxed_slice()),
                mime_type: MimeType::Specific(mime.into()),
            },
            MimeSource {
                source: Source::Bytes(uri.into_bytes().into_boxed_slice()),
                mime_type: MimeType::Specific("text/uri-list".into()),
            },
        ])?;
        Ok(())
    }

    fn remember(&self, text: &str, source: &str) {
        let mut state = self.state.lock().expect("mutex envenenado");
        state.history.retain(|e| e.text != text);
        state.history.insert(
            0,
            Entry {
                text: text.to_string(),
                timestamp_ms: now_ms(),
                source: source.to_string(),
            },
        );
        state.history.truncate(HISTORY_MAX);
        self.save(&state.history);
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn save(&self, history: &[Entry]) {
        self.changes.send_modify(|n| *n += 1);
        let result = serde_json::to_vec(history)
            .map_err(anyhow::Error::from)
            .and_then(|json| {
                super::features::write_private(&self.history_path, &json).map_err(Into::into)
            });
        if let Err(err) = result {
            tracing::warn!(?err, "no se pudo guardar el historial del portapapeles");
        }
    }

    pub fn history(&self) -> Vec<Entry> {
        self.state.lock().expect("mutex envenenado").history.clone()
    }

    /// Borra una entrada (`Some(i)`) o todo el historial (`None`).
    pub fn forget(&self, index: Option<usize>) {
        let mut state = self.state.lock().expect("mutex envenenado");
        match index {
            Some(i) if i < state.history.len() => {
                state.history.remove(i);
            }
            Some(_) => return,
            None => state.history.clear(),
        }
        if state.history.is_empty() {
            let _ = std::fs::remove_file(&self.history_path);
            self.changes.send_modify(|n| *n += 1);
        } else {
            self.save(&state.history);
        }
    }

    /// Se apagó el historial: borrarlo de memoria y de disco.
    pub fn history_disabled(&self) {
        self.forget(None);
    }
}

enum WaylandError {
    /// El compositor no implementa data-control.
    Unsupported,
    Other,
}

fn read_wayland() -> Result<Clip, WaylandError> {
    use wl_clipboard_rs::paste::{self, ClipboardType, Error, MimeType, Seat};
    let types = match paste::get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
        Ok(types) => types,
        Err(Error::MissingProtocol { .. }) => return Err(WaylandError::Unsupported),
        Err(Error::ClipboardEmpty | Error::NoSeats | Error::NoMimeType) => {
            return Ok(Clip::Nothing);
        }
        Err(_) => return Err(WaylandError::Other),
    };
    if types.contains(PASSWORD_HINT) {
        return Ok(Clip::Skip);
    }
    // Solo texto de verdad (no `text/uri-list` de archivos copiados, ni HTML).
    let Some(mime) = [
        "text/plain;charset=utf-8",
        "UTF8_STRING",
        "text/plain",
        "STRING",
        "TEXT",
    ]
    .into_iter()
    .find(|m| types.contains(*m)) else {
        return Ok(Clip::Skip);
    };
    let (pipe, _) = match paste::get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        MimeType::Specific(mime),
    ) {
        Ok(r) => r,
        Err(_) => return Err(WaylandError::Other),
    };
    // La app dueña escribe en la tubería; si se cuelga, no bloquear el hilo.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let result = pipe
            .take(MAX_TEXT as u64 + 1)
            .read_to_end(&mut buf)
            .map(|_| buf);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(READ_TIMEOUT) {
        Ok(Ok(buf)) if buf.len() <= MAX_TEXT => {
            Ok(String::from_utf8(buf).map(Clip::Text).unwrap_or(Clip::Skip))
        }
        Ok(Ok(_)) => Ok(Clip::Skip),
        _ => Err(WaylandError::Other),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
