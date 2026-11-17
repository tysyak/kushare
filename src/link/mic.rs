//! El teléfono como micrófono del PC: una fuente de audio virtual
//! (`module-pipe-source` de PulseAudio/pipewire-pulse) que lee PCM de un
//! FIFO. El audio llega del teléfono en PCM de 48 kHz (Wi-Fi) o en
//! IMA-ADPCM de 16 kHz (solo Bluetooth) y aquí se pasa a PCM de 48 kHz.

use kushare_core::audio;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;

/// Nombre de la fuente en PipeWire/PulseAudio.
pub const SOURCE_NAME: &str = "kushare_mic";
/// Frecuencia de la fuente virtual.
pub const RATE: u32 = 48_000;
/// Si en el FIFO hay más de esto sin leer, se descarta audio para que el
/// retraso no crezca (el reloj del teléfono no es exactamente el del PC).
const MAX_BACKLOG_BYTES: i32 = (RATE as i32 * 2) * 3 / 10;

/// Códec del audio que llega (el del último `MicState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Pcm16 { rate: u32 },
    Adpcm { rate: u32 },
}

impl Codec {
    pub fn label(self) -> String {
        match self {
            Self::Pcm16 { rate } => format!("PCM {} kHz", rate / 1000),
            Self::Adpcm { rate } => format!("ADPCM {} kHz", rate / 1000),
        }
    }
}

/// Convierte un bloque recibido a PCM mono a [`RATE`].
pub struct Decoder {
    last: i16,
}

impl Decoder {
    pub fn new() -> Self {
        Self { last: 0 }
    }

    pub fn decode(&mut self, codec: Codec, data: &[u8]) -> Vec<i16> {
        let (pcm, rate) = match codec {
            Codec::Pcm16 { rate } => (
                data.chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect::<Vec<_>>(),
                rate,
            ),
            Codec::Adpcm { rate } => (audio::adpcm_decode(data), rate),
        };
        let factor = (RATE / rate.max(1)).max(1) as usize;
        if factor == 1 {
            if let Some(&s) = pcm.last() {
                self.last = s;
            }
            pcm
        } else {
            audio::upsample(&pcm, factor, &mut self.last)
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// La fuente virtual: módulo cargado y un hilo que escribe en su FIFO.
pub struct MicSink {
    module: u32,
    tx: mpsc::SyncSender<Vec<i16>>,
    /// Pico (0–1000) del último bloque.
    pub level: Arc<AtomicU32>,
    /// Fuente predeterminada antes de usar el teléfono (para restaurarla).
    previous_default: Option<String>,
}

fn fifo_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("kushare-mic")
}

fn pactl(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("pactl")
        .env("LC_ALL", "C")
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("no se pudo ejecutar pactl: {e}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "pactl {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Descarga fuentes `kushare_mic` que hayan quedado de otra ejecución.
pub fn cleanup_stale() {
    let Ok(list) = pactl(&["list", "modules", "short"]) else {
        return;
    };
    for line in list.lines() {
        if line.contains("module-pipe-source")
            && line.contains(&format!("source_name={SOURCE_NAME}"))
            && let Some(id) = line.split_whitespace().next()
        {
            let _ = pactl(&["unload-module", id]);
            tracing::info!(module = id, "micrófono virtual antiguo descargado");
        }
    }
}

impl MicSink {
    /// Crea la fuente «Micrófono de `<description>`». Con `make_default`,
    /// pasa a ser la predeterminada hasta que se cierre.
    pub fn open(description: &str, make_default: bool) -> anyhow::Result<Self> {
        cleanup_stale();
        let path = fifo_path();
        let _ = std::fs::remove_file(&path);
        let description = description.replace(['"', '\'', '\\'], " ");
        let module: u32 = pactl(&[
            "load-module",
            "module-pipe-source",
            &format!("source_name={SOURCE_NAME}"),
            &format!("file={}", path.display()),
            "format=s16le",
            &format!("rate={RATE}"),
            "channels=1",
            &format!(
                "source_properties=device.description=\"{description}\" device.icon_name=phone"
            ),
        ])?
        .parse()
        .map_err(|_| anyhow::anyhow!("pactl no devolvió el número del módulo"))?;
        // El módulo crea el FIFO y lo abre para leer: abrirlo para escribir
        // no se bloquea. Si algo falla, descargar el módulo.
        let file = match std::fs::OpenOptions::new().write(true).open(&path) {
            Ok(f) => f,
            Err(err) => {
                let _ = pactl(&["unload-module", &module.to_string()]);
                anyhow::bail!("no se pudo abrir el micrófono virtual: {err}");
            }
        };
        let previous_default = if make_default {
            let prev = pactl(&["get-default-source"])
                .ok()
                .filter(|s| s != SOURCE_NAME);
            if let Err(err) = pactl(&["set-default-source", SOURCE_NAME]) {
                tracing::warn!(%err, "no se pudo poner el micrófono como predeterminado");
            }
            prev
        } else {
            None
        };
        tracing::info!(module, "micrófono virtual listo");
        let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(32);
        let level = Arc::new(AtomicU32::new(0));
        let lv = level.clone();
        std::thread::Builder::new()
            .name("kushare-mic".into())
            .spawn(move || writer(file, rx, lv))?;
        Ok(Self {
            module,
            tx,
            level,
            previous_default,
        })
    }

    /// Encola PCM a [`RATE`] (si va atrasado, se descarta).
    pub fn push(&self, pcm: Vec<i16>) {
        let _ = self.tx.try_send(pcm);
    }
}

impl Drop for MicSink {
    fn drop(&mut self) {
        if let Some(prev) = self.previous_default.take() {
            let _ = pactl(&["set-default-source", &prev]);
        }
        let _ = pactl(&["unload-module", &self.module.to_string()]);
        let _ = std::fs::remove_file(fifo_path());
        tracing::info!("micrófono virtual cerrado");
    }
}

fn writer(mut file: std::fs::File, rx: mpsc::Receiver<Vec<i16>>, level: Arc<AtomicU32>) {
    let fd = file.as_raw_fd();
    let mut bytes = Vec::new();
    while let Ok(pcm) = rx.recv() {
        level.store((audio::peak(&pcm) * 1000.0) as u32, Ordering::Relaxed);
        let mut pending: libc::c_int = 0;
        // SAFETY: FIONREAD en un FIFO propio escribe un entero en `pending`.
        let ok = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut pending) } == 0;
        if ok && pending > MAX_BACKLOG_BYTES {
            continue;
        }
        bytes.clear();
        bytes.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
        if let Err(err) = file.write_all(&bytes) {
            tracing::warn!(%err, "no se pudo escribir en el micrófono virtual");
            break;
        }
    }
}

/// Fuente temporal de PCM mono (p. ej. los tonos DTMF hacia una llamada):
/// se descarga al soltarla.
pub struct TempSource {
    module: u32,
    file: std::fs::File,
    path: PathBuf,
}

impl TempSource {
    pub fn open(name: &str, rate: u32) -> anyhow::Result<Self> {
        let path = fifo_path().with_file_name(name);
        let _ = std::fs::remove_file(&path);
        let module: u32 = pactl(&[
            "load-module",
            "module-pipe-source",
            &format!("source_name={name}"),
            &format!("file={}", path.display()),
            "format=s16le",
            &format!("rate={rate}"),
            "channels=1",
            "source_properties=device.description=kushare",
        ])?
        .parse()
        .map_err(|_| anyhow::anyhow!("pactl no devolvió el número del módulo"))?;
        match std::fs::OpenOptions::new().write(true).open(&path) {
            Ok(file) => Ok(Self { module, file, path }),
            Err(err) => {
                let _ = pactl(&["unload-module", &module.to_string()]);
                anyhow::bail!("no se pudo abrir la fuente temporal: {err}")
            }
        }
    }

    pub fn write(&self, pcm: &[i16]) -> anyhow::Result<()> {
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        (&self.file).write_all(&bytes)?;
        Ok(())
    }
}

impl Drop for TempSource {
    fn drop(&mut self) {
        let _ = pactl(&["unload-module", &self.module.to_string()]);
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descodifica_a_48k() {
        let mut d = Decoder::new();
        let pcm: Vec<u8> = [100i16, -100]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        assert_eq!(
            d.decode(Codec::Pcm16 { rate: 48_000 }, &pcm),
            vec![100, -100]
        );
        let mut st = audio::AdpcmState::default();
        let block = audio::adpcm_encode(&mut st, &[0; 320]);
        assert_eq!(d.decode(Codec::Adpcm { rate: 16_000 }, &block).len(), 960);
        assert_eq!(d.decode(Codec::Pcm16 { rate: 16_000 }, &pcm).len(), 6);
    }

    /// Prueba manual: crea la fuente 3 s y escribe un tono.
    /// `cargo test --bin kushare mic_manual -- --ignored`
    #[test]
    #[ignore]
    fn mic_manual() {
        let sink = MicSink::open("prueba de kushare", false).unwrap();
        let tone = audio::dtmf_pcm("5", RATE, 40, 0);
        for _ in 0..75 {
            sink.push(tone.clone());
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        assert!(sink.level.load(Ordering::Relaxed) > 300);
    }
}
