//! Funciones extra del enlace en el escritorio (al estilo KDE Connect):
//!
//! - **Batería**: la propia se lee de `/sys/class/power_supply` (solo si es
//!   un portátil) y se manda a los peers; la del teléfono se muestra.
//! - **Multimedia**: los reproductores MPRIS del PC se publican y se
//!   controlan desde el teléfono; los del teléfono se controlan desde aquí.
//! - **Salida de audio**: las salidas de PipeWire/PulseAudio (vía `pactl`)
//!   se publican y el teléfono puede elegir la predeterminada; la GUI elige
//!   también la entrada (micrófono) del PC.
//! - **Timbre**, **llamadas**, **fotos**, **SMS**, **registro de llamadas**,
//!   **contactos**, **notificaciones activas** y **fondo de pantalla**: solo
//!   del teléfono hacia el PC (peticiones con `request_id`).
//! - **Portapapeles**: en ambos sentidos (ver [`super::clipboard`]).
//!
//! El enlace solo transporta los mensajes (`pb::Feature`); aquí se guarda
//! el último estado de cada peer y se responden las peticiones.
//!
//! **Fotos por Bluetooth**: las miniaturas se guardan en caché y el
//! teléfono no las vuelve a mandar (`have_thumbnails`); además las fotos
//! más recientes se precargan en segundo plano, a baja prioridad, para que
//! abrirlas sea instantáneo.

use kushare_link::LinkManager;
use kushare_link::pb::{self, feature::Kind as F};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

use super::clipboard::Clipboard;
use super::settings::SettingsStore;

mod hardware;
mod local_audio;
mod phone_audio;
mod phone_data;
mod photos;

pub use local_audio::*;
pub use phone_audio::*;
use photos::*;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const BATTERY_POLL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Las listas largas (contactos, SMS) por Bluetooth tardan más.
const SLOW_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Fotos de contacto: cuántas se piden a la vez y cuánto se espera (son un
/// adorno: si tardan, se muestran las iniciales).
const CONTACT_PHOTOS_MAX: usize = 60;
/// Webcam: fotogramas por segundo y calidad JPEG que se piden al teléfono.
const WEBCAM_FPS: u32 = 25;
const WEBCAM_QUALITY: u32 = 75;
const CONTACT_PHOTOS_TIMEOUT: Duration = Duration::from_secs(10);
/// Una foto grande por Bluetooth puede tardar: 5 min de margen (10 de fondo).
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const BACKGROUND_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Máximo de fotos por petición que acepta el teléfono.
const PHOTO_PAGE_MAX: u32 = 60;
/// Cuántas de las fotos más recientes se precargan en segundo plano.
const PREFETCH_COUNT: u32 = 6;
/// Fotos más grandes no se precargan (por Bluetooth tardarían minutos).
const PREFETCH_MAX_BYTES: u64 = 12 * 1024 * 1024;
/// Trozos pequeños en la precarga, para intercalarse con lo demás.
const PREFETCH_CHUNK: u32 = 64 * 1024;
/// Tamaño máximo de la caché de fotos originales.
const PHOTO_CACHE_MAX_BYTES: u64 = 400 * 1024 * 1024;
/// Espera tras conectar antes de precargar (que se asiente el enlace).
const PREFETCH_DELAY_ON_CONNECT: Duration = Duration::from_secs(20);
/// Los contactos cambian poco: se reutilizan unos minutos.
const CONTACTS_TTL: Duration = Duration::from_secs(600);
/// Tiempo para conectar el enlace de datos por Wi-Fi (si no, Bluetooth).
const BULK_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Umbral para avisar de batería baja del teléfono.
pub const LOW_BATTERY: u32 = 15;

/// Último estado conocido de un peer.
#[derive(Debug, Clone, Default)]
pub struct PeerState {
    pub battery: Option<pb::BatteryStatus>,
    pub ringer: Option<pb::Ringer>,
    pub call: Option<pb::CallState>,
    /// Reproductores, sin los bytes de la carátula (van a `art`).
    pub players: Vec<pb::MediaPlayer>,
    /// Cuándo llegaron los reproductores (ms Unix de este PC): la posición
    /// se extrapola desde aquí.
    pub players_at_ms: i64,
    /// Carátula guardada de cada reproductor (por id).
    pub art: HashMap<String, PathBuf>,
    pub outputs: Option<pb::AudioOutputs>,
    /// El teléfono está sonando para encontrarlo.
    pub finding: bool,
}

/// Estado local que se manda a cada peer al conectar.
#[derive(Default)]
struct LocalState {
    battery: Option<pb::BatteryStatus>,
    players: Option<pb::MediaPlayers>,
    outputs: Option<pb::AudioOutputs>,
}

struct Download {
    file: std::fs::File,
    part: PathBuf,
    target: PathBuf,
    done: oneshot::Sender<anyhow::Result<PathBuf>>,
}

/// Lo que cambió en el estado de un peer (para avisos del escritorio).
pub enum RemoteChange {
    /// Nada relevante para el usuario más allá de refrescar la UI.
    State,
    Call(pb::CallState),
    LowBattery(u32),
}

/// Una foto de la galería del teléfono, con sus archivos locales.
#[derive(Debug, Clone)]
pub struct PhotoEntry {
    /// Sin `thumbnail` (está en `thumb`).
    pub info: pb::PhotoInfo,
    pub thumb: PathBuf,
    /// La original, si ya está descargada (o precargada).
    pub original: Option<PathBuf>,
}

/// Última lista de fotos vista, para enseñar la galería al instante
/// (incluso sin conexión).
#[derive(Serialize, Deserialize, Default)]
struct PhotoIndex {
    total: u32,
    photos: Vec<IndexedPhoto>,
}

#[derive(Serialize, Deserialize)]
struct IndexedPhoto {
    id: String,
    name: String,
    date_ms: i64,
    width: u32,
    height: u32,
    size: u64,
}

/// Fondo de pantalla de un peer: imagen (si se pudo leer) y colores ARGB.
#[derive(Debug, Clone, Default)]
pub struct WallpaperInfo {
    pub path: Option<PathBuf>,
    pub colors: Vec<u32>,
}

/// Fotos nuevas en la galería de un peer (para el aviso del escritorio).
#[derive(Debug, Clone)]
pub struct NewPhotos {
    pub peer: String,
    /// La más reciente primero.
    pub photos: Vec<PhotoEntry>,
}

pub struct Features {
    manager: LinkManager,
    peers: Mutex<HashMap<String, PeerState>>,
    local: Mutex<LocalState>,
    next_request: AtomicU64,
    /// Peticiones de una sola respuesta, por `request_id`.
    pending: Mutex<HashMap<u64, oneshot::Sender<F>>>,
    downloads: Mutex<HashMap<u64, Download>>,
    /// Fotos descargándose ahora (peer, id), para no pedirlas dos veces.
    inflight: Mutex<HashSet<(String, String)>>,
    contacts: Mutex<HashMap<String, (Instant, Vec<pb::Contact>)>>,
    /// Fotos de contacto que el teléfono no devolvió: (peer, clave). No se
    /// vuelven a pedir hasta reiniciar.
    photo_missing: Mutex<HashSet<(String, String)>>,
    /// Dónde escucha cada teléfono en la red local (modo híbrido).
    lan: Mutex<HashMap<String, Vec<std::net::SocketAddr>>>,
    /// ¿Se alcanza al peer por Wi-Fi? (resultado de la última prueba).
    lan_ready: Mutex<HashMap<String, bool>>,
    /// Peers cuyo enlace por Wi-Fi hay que (re)comprobar.
    lan_probe: mpsc::UnboundedSender<String>,
    /// Evita abrir dos enlaces de datos a la vez con el mismo peer.
    bulk_opening: tokio::sync::Mutex<()>,
    wallpapers: Mutex<HashMap<String, WallpaperInfo>>,
    cache_dir: PathBuf,
    /// Órdenes hacia las tareas locales (MPRIS, pactl).
    commands: mpsc::UnboundedSender<LocalCommand>,
    prefetch: mpsc::UnboundedSender<String>,
    /// Fecha de la foto más reciente ya vista de cada peer (para avisar
    /// solo de las nuevas).
    newest_seen: Mutex<HashMap<String, i64>>,
    new_photos: mpsc::UnboundedSender<NewPhotos>,
    new_photos_rx: Mutex<Option<mpsc::UnboundedReceiver<NewPhotos>>>,
    pub settings: Arc<SettingsStore>,
    pub clipboard: Arc<Clipboard>,
    /// Historial de códigos QR leídos con el teléfono.
    pub qr: Arc<super::qr::QrStore>,
    /// Códigos nuevos, para avisar en el escritorio.
    qr_new: mpsc::UnboundedSender<super::qr::QrEntry>,
    qr_new_rx: Mutex<Option<mpsc::UnboundedReceiver<super::qr::QrEntry>>>,
    /// El teléfono como webcam (una a la vez).
    webcam: Mutex<Option<WebcamSession>>,
    /// El teléfono como micrófono (uno a la vez).
    mic: Mutex<Option<MicSession>>,
}

/// Micrófono en marcha: de qué teléfono, en qué códec y la fuente virtual.
pub struct MicSession {
    pub peer: String,
    pub sink: super::mic::MicSink,
    pub codec: super::mic::Codec,
    pub decoder: super::mic::Decoder,
    pub active: bool,
    pub error: String,
    pub make_default: bool,
}

/// Webcam en marcha: de qué teléfono y cámara, y a qué cámara virtual.
pub struct WebcamSession {
    pub peer: String,
    pub camera: String,
    pub sink: super::webcam::Sink,
    /// El teléfono confirmó que la cámara está abierta.
    pub active: bool,
    pub error: String,
    /// Para calcular los fotogramas por segundo.
    pub last_count: (Instant, u64),
}

enum LocalCommand {
    Media(pb::MediaCommand),
    SetOutput(String),
}

impl Features {
    /// Crea el estado y arranca las tareas que vigilan batería, MPRIS,
    /// salidas de audio, portapapeles y la precarga de fotos.
    pub fn start(
        manager: LinkManager,
        cache_dir: &Path,
        data_dir: &Path,
        settings: Arc<SettingsStore>,
    ) -> Arc<Self> {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (prefetch, prefetch_rx) = mpsc::unbounded_channel();
        let (new_photos, new_photos_rx) = mpsc::unbounded_channel();
        let (lan_probe, lan_probe_rx) = mpsc::unbounded_channel();
        let clipboard = Clipboard::start(manager.clone(), settings.clone(), data_dir);
        let (qr_new, qr_new_rx) = mpsc::unbounded_channel();
        let features = Arc::new(Self {
            manager,
            peers: Mutex::default(),
            local: Mutex::default(),
            next_request: AtomicU64::new(1),
            pending: Mutex::default(),
            downloads: Mutex::default(),
            inflight: Mutex::default(),
            contacts: Mutex::default(),
            photo_missing: Mutex::default(),
            lan: Mutex::default(),
            lan_ready: Mutex::default(),
            lan_probe,
            bulk_opening: tokio::sync::Mutex::new(()),
            wallpapers: Mutex::default(),
            cache_dir: cache_dir.to_path_buf(),
            commands,
            prefetch,
            newest_seen: Mutex::default(),
            new_photos,
            new_photos_rx: Mutex::new(Some(new_photos_rx)),
            settings,
            clipboard,
            qr: Arc::new(super::qr::QrStore::open(data_dir)),
            qr_new,
            qr_new_rx: Mutex::new(Some(qr_new_rx)),
            webcam: Mutex::default(),
            mic: Mutex::default(),
        });
        // Fuente de micrófono que quedara de una ejecución anterior, y
        // límites de la caché al arrancar.
        std::thread::spawn(super::mic::cleanup_stale);
        {
            let features = features.clone();
            std::thread::spawn(move || features.prune_photo_cache());
        }
        tokio::spawn(battery_task(features.clone()));
        tokio::spawn(local_task(features.clone(), command_rx));
        tokio::spawn(prefetch_task(features.clone(), prefetch_rx));
        tokio::spawn(lan_probe_task(features.clone(), lan_probe_rx));
        tokio::spawn(local_network_task(features.clone()));
        features
    }

    /// Receptor de los avisos de fotos nuevas (solo se puede tomar una vez).
    /// Códigos QR nuevos (para el aviso del escritorio). Se toma una vez.
    pub fn take_new_qr(&self) -> Option<mpsc::UnboundedReceiver<super::qr::QrEntry>> {
        self.qr_new_rx.lock().expect("mutex envenenado").take()
    }

    pub fn take_new_photos(&self) -> Option<mpsc::UnboundedReceiver<NewPhotos>> {
        self.new_photos_rx.lock().expect("mutex envenenado").take()
    }

    pub fn peer_state(&self, peer_id: &str) -> PeerState {
        self.peers
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Un peer acaba de conectar: mandarle el estado local completo.
    pub fn on_connected(&self, peer_id: &str) {
        {
            let local = self.local.lock().expect("mutex envenenado");
            let msgs = [
                local.battery.map(F::Battery),
                local.players.clone().map(F::MediaPlayers),
                local.outputs.clone().map(F::AudioOutputs),
            ];
            for kind in msgs.into_iter().flatten() {
                self.manager
                    .send_feature(peer_id, pb::Feature { kind: Some(kind) });
            }
        }
        // Precargar las fotos recientes cuando el enlace se asiente.
        let prefetch = self.prefetch.clone();
        let peer = peer_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(PREFETCH_DELAY_ON_CONNECT).await;
            let _ = prefetch.send(peer);
        });
    }

    pub fn on_disconnected(&self, peer_id: &str) {
        // La llamada y los reproductores dejan de ser controlables; la
        // batería se conserva como "último valor conocido".
        if let Some(state) = self
            .peers
            .lock()
            .expect("mutex envenenado")
            .get_mut(peer_id)
        {
            state.call = None;
            state.players.clear();
            state.outputs = None;
        }
        self.lan_ready
            .lock()
            .expect("mutex envenenado")
            .remove(peer_id);
    }

    pub fn forget(&self, peer_id: &str) {
        self.peers.lock().expect("mutex envenenado").remove(peer_id);
        self.contacts
            .lock()
            .expect("mutex envenenado")
            .remove(peer_id);
        self.wallpapers
            .lock()
            .expect("mutex envenenado")
            .remove(peer_id);
        // Sus miniaturas, fotos y fondo de pantalla ya no hacen falta.
        let peer = sanitize(peer_id);
        for dir in [
            self.cache_dir.join("thumbs").join(&peer),
            self.cache_dir.join("photos").join(&peer),
        ] {
            let _ = std::fs::remove_dir_all(dir);
        }
        let _ = std::fs::remove_file(self.wallpaper_path(peer_id));
    }

    fn publish_local(&self, kind: F) {
        {
            let mut local = self.local.lock().expect("mutex envenenado");
            let slot_changed = match &kind {
                F::Battery(b) => replace_if_changed(&mut local.battery, b),
                F::MediaPlayers(p) => replace_if_changed(&mut local.players, p),
                F::AudioOutputs(o) => replace_if_changed(&mut local.outputs, o),
                _ => true,
            };
            if !slot_changed {
                return;
            }
        }
        self.manager
            .broadcast_feature(pb::Feature { kind: Some(kind) });
    }

    /// Procesa un mensaje del peer. Devuelve qué cambió, si algo.
    pub fn on_remote(&self, peer_id: &str, feature: pb::Feature) -> Option<RemoteChange> {
        let kind = feature.kind?;
        // Respuestas a peticiones (una sola respuesta por `request_id`).
        let response_id = match &kind {
            F::PhotoList(r) => Some(r.request_id),
            F::SmsThreads(r) => Some(r.request_id),
            F::SmsMessages(r) => Some(r.request_id),
            F::CallLog(r) => Some(r.request_id),
            F::Contacts(r) => Some(r.request_id),
            F::ContactPhotos(r) => Some(r.request_id),
            F::Cameras(r) => Some(r.request_id),
            F::ActiveNotifications(r) => Some(r.request_id),
            F::Wallpaper(r) => Some(r.request_id),
            F::CommandResult(r) => Some(r.request_id),
            _ => None,
        };
        if let Some(id) = response_id {
            if let Some(tx) = self.pending.lock().expect("mutex envenenado").remove(&id) {
                let _ = tx.send(kind);
            }
            return None;
        }
        let mut peers = self.peers.lock().expect("mutex envenenado");
        let state = peers.entry(peer_id.to_string()).or_default();
        let change = match kind {
            F::Battery(b) => {
                let was_low = state
                    .battery
                    .as_ref()
                    .is_some_and(|old| old.percent <= LOW_BATTERY && !old.charging);
                let low = b.present && b.percent <= LOW_BATTERY && !b.charging;
                let percent = b.percent;
                state.battery = Some(b);
                if low && !was_low {
                    RemoteChange::LowBattery(percent)
                } else {
                    RemoteChange::State
                }
            }
            F::MediaPlayers(p) => {
                let art_dir = self.cache_dir.join("art");
                state.art.clear();
                state.players = p
                    .players
                    .into_iter()
                    .map(|mut player| {
                        let art = std::mem::take(&mut player.art);
                        if let Some(path) = save_art(&art_dir, &art) {
                            state.art.insert(player.id.clone(), path);
                        }
                        player
                    })
                    .collect();
                state.players_at_ms = now_unix_ms();
                RemoteChange::State
            }
            F::FindPhone(f) => {
                state.finding = f.ring;
                RemoteChange::State
            }
            F::WebcamFrame(frame) => {
                drop(peers);
                self.on_webcam_frame(peer_id, frame);
                return None;
            }
            F::WebcamState(ws) => {
                drop(peers);
                let mut webcam = self.webcam.lock().expect("mutex envenenado");
                if let Some(session) = webcam.as_mut().filter(|s| s.peer == peer_id) {
                    session.active = ws.active;
                    session.error = ws.error.clone();
                    if !ws.active {
                        tracing::info!(error = %ws.error, "el teléfono paró la webcam");
                        if ws.error.is_empty() {
                            *webcam = None;
                        }
                    }
                }
                return Some(RemoteChange::State);
            }
            F::MicAudio(audio) => {
                drop(peers);
                let mut mic = self.mic.lock().expect("mutex envenenado");
                match mic.as_mut().filter(|s| s.peer == peer_id) {
                    Some(session) => {
                        let pcm = session.decoder.decode(session.codec, &audio.data);
                        session.sink.push(pcm);
                    }
                    None => {
                        // Audio sin haberlo pedido (o tras parar): que pare.
                        drop(mic);
                        let _ = self.send(peer_id, F::MicStop(pb::MicStop {}));
                    }
                }
                return None;
            }
            F::MicState(ms) => {
                drop(peers);
                let mut mic = self.mic.lock().expect("mutex envenenado");
                if let Some(session) = mic.as_mut().filter(|s| s.peer == peer_id) {
                    session.active = ms.active;
                    session.error = ms.error.clone();
                    if ms.rate > 0 {
                        session.codec = if ms.codec == pb::MicCodec::MicImaAdpcm as i32 {
                            super::mic::Codec::Adpcm { rate: ms.rate }
                        } else {
                            super::mic::Codec::Pcm16 { rate: ms.rate }
                        };
                    }
                    if !ms.active {
                        tracing::info!(error = %ms.error, "el teléfono paró el micrófono");
                        if ms.error.is_empty() {
                            *mic = None;
                        }
                    }
                }
                return Some(RemoteChange::State);
            }
            F::QrResult(r) => {
                drop(peers);
                let entry = super::qr::QrEntry {
                    text: r.text,
                    format: r.format,
                    date_ms: if r.date_ms > 0 {
                        r.date_ms
                    } else {
                        now_unix_ms()
                    },
                    device: peer_id.to_string(),
                };
                if self.qr.add(entry.clone()) {
                    let _ = self.qr_new.send(entry);
                }
                return None;
            }
            F::AudioOutputs(o) => {
                state.outputs = Some(o);
                RemoteChange::State
            }
            F::RingerMode(r) => {
                state.ringer = Some(r.mode());
                RemoteChange::State
            }
            F::CallState(c) => {
                state.call = (c.phase() != pb::CallPhase::CallIdle).then(|| c.clone());
                RemoteChange::Call(c)
            }
            F::MediaCommand(cmd) => {
                drop(peers);
                let _ = self.commands.send(LocalCommand::Media(cmd));
                return None;
            }
            F::SetAudioOutput(set) => {
                drop(peers);
                let _ = self.commands.send(LocalCommand::SetOutput(set.id));
                return None;
            }
            F::PhotoData(data) => {
                drop(peers);
                self.on_photo_data(data);
                return None;
            }
            F::Clipboard(c) => {
                drop(peers);
                self.clipboard.on_remote(peer_id, c.text);
                return None;
            }
            F::LanAddress(lan) => {
                drop(peers);
                let port = u16::try_from(lan.port).unwrap_or(0);
                let addrs: Vec<std::net::SocketAddr> = lan
                    .addrs
                    .iter()
                    .filter_map(|a| a.parse::<std::net::IpAddr>().ok())
                    .filter(|ip| port != 0 && is_lan(ip))
                    .map(|ip| std::net::SocketAddr::new(ip, port))
                    .collect();
                tracing::debug!(%peer_id, ?addrs, "dirección de red local del peer");
                self.lan
                    .lock()
                    .expect("mutex envenenado")
                    .insert(peer_id.to_string(), addrs);
                if lan.addrs.is_empty() {
                    // Salió de la Wi-Fi (o está en "Solo Bluetooth").
                    self.manager.close_bulk(peer_id);
                    self.set_lan_ready(peer_id, false);
                } else {
                    // Cambió su red: comprobar si nos alcanzamos por Wi-Fi.
                    let _ = self.lan_probe.send(peer_id.to_string());
                }
                return None;
            }
            // La GUI se entera por la señal D-Bus `PhotosChanged` (ver
            // dbus.rs); aquí solo se precargan las nuevas.
            F::PhotosChanged(_) => {
                drop(peers);
                let _ = self.prefetch.send(peer_id.to_string());
                return None;
            }
            // Señal D-Bus `SmsChanged` (ver dbus.rs).
            F::SmsChanged(_) => return None,
            // Solo tienen sentido hacia un teléfono (o ya se trataron arriba).
            F::SetRingerMode(_)
            | F::CallCommand(_)
            | F::PhotoListRequest(_)
            | F::PhotoRequest(_)
            | F::PhotoList(_)
            | F::SmsThreadsRequest(_)
            | F::SmsThreads(_)
            | F::SmsMessagesRequest(_)
            | F::SmsMessages(_)
            | F::SendSms(_)
            | F::CallLogRequest(_)
            | F::CallLog(_)
            | F::ContactsRequest(_)
            | F::Contacts(_)
            | F::ContactPhotosRequest(_)
            | F::ContactPhotos(_)
            | F::CamerasRequest(_)
            | F::Cameras(_)
            | F::WebcamStart(_)
            | F::WebcamStop(_)
            | F::WebcamAck(_)
            | F::QrScan(_)
            | F::MicStart(_)
            | F::MicStop(_)
            | F::Dtmf(_)
            | F::Dial(_)
            | F::ActiveNotificationsRequest(_)
            | F::ActiveNotifications(_)
            | F::WallpaperRequest(_)
            | F::Wallpaper(_)
            | F::PhotoDelete(_)
            | F::CommandResult(_)
            | F::LanStatus(_) => return None,
        };
        drop(peers);
        Some(change)
    }

    // -----------------------------------------------------------------
    // Órdenes hacia el peer
    // -----------------------------------------------------------------

    fn send(&self, peer_id: &str, kind: F) -> anyhow::Result<()> {
        if !self.manager.is_connected(peer_id) {
            anyhow::bail!("el dispositivo no está conectado");
        }
        self.manager
            .send_feature(peer_id, pb::Feature { kind: Some(kind) });
        Ok(())
    }

    /// Modo híbrido: abre (si hace falta y se puede) el enlace de datos por
    /// Wi-Fi con `peer_id` antes de pedirle algo pesado. Si no hay Wi-Fi, o
    /// falla, no pasa nada: todo sigue por el enlace de control (Bluetooth).
    pub async fn ensure_bulk(&self, peer_id: &str) {
        let ok = self.try_bulk(peer_id).await;
        self.set_lan_ready(peer_id, ok);
    }

    /// Antes de una petición pesada: abre el enlace de datos, salvo que la
    /// última prueba ya dijo que el Wi-Fi no llega (redes distintas o con
    /// aislamiento). Así no se esperan 2 s por cada petición: la prueba se
    /// repite sola al cambiar la red del teléfono o la del PC.
    async fn bulk_for_request(&self, peer_id: &str) {
        let known_unreachable = self
            .lan_ready
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            == Some(&false);
        if !known_unreachable {
            self.ensure_bulk(peer_id).await;
        }
    }

    /// Abre el enlace de datos si se puede. `true` si queda abierto.
    async fn try_bulk(&self, peer_id: &str) -> bool {
        if !self.settings.get().hybrid {
            return false;
        }
        let mut addrs = self
            .lan
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            .cloned()
            .unwrap_or_default();
        // Solo para pruebas: abrir el enlace de datos por otra dirección
        // (p. ej. `adb forward tcp:47817 tcp:47816` y `127.0.0.1:47817`).
        if let Ok(addr) = std::env::var("KUSHARE_TEST_BULK_ADDR")
            && let Ok(addr) = addr.parse()
        {
            addrs = vec![addr];
        }
        if addrs.is_empty() {
            return false;
        }
        if self.manager.has_bulk(peer_id) {
            return true;
        }
        let _guard = self.bulk_opening.lock().await;
        if self.manager.has_bulk(peer_id) {
            return true;
        }
        for addr in addrs {
            let stream = match tokio::time::timeout(
                BULK_CONNECT_TIMEOUT,
                tokio::net::TcpStream::connect(addr),
            )
            .await
            {
                Ok(Ok(stream)) => stream,
                _ => {
                    tracing::debug!(%addr, "enlace de datos: no se pudo conectar");
                    continue;
                }
            };
            let _ = stream.set_nodelay(true);
            let connect = self.manager.connect(
                stream,
                kushare_link::Transport::Tcp(addr),
                kushare_link::ConnectIntent::Bulk,
            );
            match tokio::time::timeout(BULK_CONNECT_TIMEOUT * 3, connect).await {
                Ok(Ok(_)) => return true,
                Ok(Err(err)) => tracing::debug!(%addr, ?err, "enlace de datos rechazado"),
                Err(_) => tracing::debug!(%addr, "enlace de datos: handshake lento"),
            }
        }
        false
    }

    /// Guarda si `peer_id` se alcanza por Wi-Fi y, si cambió, se lo dice
    /// (para que el teléfono también muestre "Bluetooth + Wi-Fi").
    fn set_lan_ready(&self, peer_id: &str, ready: bool) {
        let changed = self
            .lan_ready
            .lock()
            .expect("mutex envenenado")
            .insert(peer_id.to_string(), ready)
            != Some(ready);
        if changed {
            tracing::info!(%peer_id, ready, "datos por Wi-Fi");
            self.manager.send_feature(
                peer_id,
                pb::Feature {
                    kind: Some(F::LanStatus(pb::LanStatus { ready })),
                },
            );
        }
    }

    /// ¿Se alcanzó a `peer_id` por Wi-Fi en la última prueba?
    pub fn lan_ready(&self, peer_id: &str) -> bool {
        self.lan_ready
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            .copied()
            .unwrap_or(false)
    }

    fn new_request_id(&self) -> u64 {
        self.next_request.fetch_add(1, Ordering::Relaxed)
    }

    /// Manda la petición `make(request_id)` y espera su única respuesta.
    async fn request(
        &self,
        peer_id: &str,
        make: impl FnOnce(u64) -> F,
        timeout: Duration,
    ) -> anyhow::Result<F> {
        let request_id = self.new_request_id();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("mutex envenenado")
            .insert(request_id, tx);
        let forget = || {
            self.pending
                .lock()
                .expect("mutex envenenado")
                .remove(&request_id);
        };
        if let Err(err) = self.send(peer_id, make(request_id)) {
            forget();
            return Err(err);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(kind)) => Ok(kind),
            _ => {
                forget();
                anyhow::bail!("el teléfono no respondió a tiempo")
            }
        }
    }

    /// Orden con `CommandResult`: devuelve su `status` ("done", "confirm").
    async fn command(&self, peer_id: &str, make: impl FnOnce(u64) -> F) -> anyhow::Result<String> {
        match self.request(peer_id, make, REQUEST_TIMEOUT).await? {
            F::CommandResult(r) if r.ok => Ok(if r.status.is_empty() {
                "done".into()
            } else {
                r.status
            }),
            F::CommandResult(r) => anyhow::bail!("{}", phone_error(&r.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    pub fn media_command(
        &self,
        peer_id: &str,
        player_id: &str,
        action: pb::MediaAction,
    ) -> anyhow::Result<()> {
        self.send(
            peer_id,
            F::MediaCommand(pb::MediaCommand {
                player_id: player_id.to_string(),
                action: action.into(),
            }),
        )
    }

    pub fn set_audio_output(&self, peer_id: &str, id: &str) -> anyhow::Result<()> {
        self.send(
            peer_id,
            F::SetAudioOutput(pb::SetAudioOutput { id: id.to_string() }),
        )
    }

    pub fn set_ringer(&self, peer_id: &str, mode: pb::Ringer) -> anyhow::Result<()> {
        self.send(
            peer_id,
            F::SetRingerMode(pb::SetRingerMode { mode: mode.into() }),
        )
    }

    pub fn call_command(&self, peer_id: &str, action: pb::CallAction) -> anyhow::Result<()> {
        self.send(
            peer_id,
            F::CallCommand(pb::CallCommand {
                action: action.into(),
            }),
        )
    }
}

/// Prueba el enlace de datos por Wi-Fi con los peers que lo pidan (nueva
/// `LanAddress`, cambio de red del PC, modo híbrido activado). La conexión
/// de prueba es el propio enlace de datos: se cierra sola a los 30 s.
async fn lan_probe_task(features: Arc<Features>, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(first) = rx.recv().await {
        // La red tarda un momento en asentarse tras un cambio.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let mut peers = HashSet::from([first]);
        while let Ok(p) = rx.try_recv() {
            peers.insert(p);
        }
        for peer in peers {
            if features.manager.is_connected(&peer) {
                features.ensure_bulk(&peer).await;
            }
        }
    }
}

/// Vigila las IP de este PC (sin tráfico de red): si cambia de red, vuelve
/// a comprobar si llega a los teléfonos por Wi-Fi.
async fn local_network_task(features: Arc<Features>) {
    let snapshot = || {
        let mut ips: Vec<std::net::IpAddr> = if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter(|i| !i.is_loopback())
            .map(|i| i.ip())
            .collect();
        ips.sort();
        ips
    };
    let mut last = snapshot();
    let mut last_hybrid = features.settings.get().hybrid;
    loop {
        tokio::time::sleep(Duration::from_secs(20)).await;
        let now = snapshot();
        let hybrid = features.settings.get().hybrid;
        if now != last || hybrid != last_hybrid {
            last = now;
            last_hybrid = hybrid;
            let peers: Vec<String> = features
                .lan
                .lock()
                .expect("mutex envenenado")
                .keys()
                .cloned()
                .collect();
            for peer in peers {
                let _ = features.lan_probe.send(peer);
            }
        }
    }
}

/// Solo direcciones de la red local (no Internet): el enlace de datos no
/// debe salir de casa aunque alguien mande otra cosa.
fn is_lan(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Nombre original de una foto en caché (`<id>__<nombre>` → `<nombre>`).
pub fn original_name(cached: &Path) -> String {
    let name = cached
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    name.split_once("__")
        .map(|(_, n)| n.to_string())
        .unwrap_or(name)
}

/// Mensaje legible para los errores que manda el teléfono.
fn phone_error(code: &str) -> String {
    match code {
        "permission" => {
            "falta el permiso en el teléfono (kushare → Ajustes → Funciones del teléfono)".into()
        }
        "unsupported" => "el teléfono no lo admite".into(),
        "no_call" => "no hay ninguna llamada en curso en el teléfono".into(),
        "no_incall" => "Android aún no dejó a kushare acceder a la llamada (actualiza la app \
                        del teléfono a la 0.8 y vuelve a llamar)"
            .into(),
        other => format!("el teléfono respondió: {other}"),
    }
}

/// Número marcable: las letras pasan a su tecla (1-800-FLORES → 1-800-356737).
pub fn dialable(number: &str) -> String {
    number
        .trim()
        .chars()
        .filter_map(|c| match c.to_ascii_uppercase() {
            d @ ('0'..='9' | '+' | '*' | '#') => Some(d),
            'A'..='C' => Some('2'),
            'D'..='F' => Some('3'),
            'G'..='I' => Some('4'),
            'J'..='L' => Some('5'),
            'M'..='O' => Some('6'),
            'P'..='S' => Some('7'),
            'T'..='V' => Some('8'),
            'W'..='Z' => Some('9'),
            _ => None,
        })
        .collect()
}

/// Crea `dir` (y sus padres) solo legible por el usuario.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// Escribe `data` en `path` de forma atómica y solo legible por el usuario.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(data)?;
    drop(f);
    std::fs::rename(&tmp, path)
}

fn replace_if_changed<T: PartialEq + Clone>(slot: &mut Option<T>, value: &T) -> bool {
    if slot.as_ref() == Some(value) {
        return false;
    }
    *slot = Some(value.clone());
    true
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Guarda una carátula por su hash (la GUI la carga por ruta y la cachea:
/// una ruta nueva por imagen nueva) y borra las que lleven días sin usarse.
fn save_art(dir: &Path, image: &[u8]) -> Option<PathBuf> {
    if image.is_empty() {
        return None;
    }
    let hash = kushare_link::icon_hash(image);
    let path = dir.join(format!("{}.{}", &hash[..32], image_extension(image)));
    if !path.exists() {
        if let Err(err) = write_private(&path, image) {
            tracing::debug!(%err, "no se pudo guardar una carátula");
            return None;
        }
        // Limpieza: las de hace más de 3 días.
        if let Ok(entries) = std::fs::read_dir(dir) {
            let old = std::time::SystemTime::now() - Duration::from_secs(3 * 24 * 3600);
            for e in entries.flatten() {
                if e.metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t < old)
                {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    Some(path)
}

/// Extensión de una imagen según sus primeros bytes (Android da JPEG o PNG).
fn image_extension(data: &[u8]) -> &'static str {
    if data.starts_with(b"\x89PNG") {
        "png"
    } else {
        "jpg"
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn sanitize_file_name(name: &str, fallback: &str) -> String {
    let name = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty() && !n.starts_with('.'))
        .unwrap_or_else(|| format!("foto-{}.jpg", sanitize(fallback)));
    name.replace(['/', '\0'], "_")
}

/// `dir/name`, o `dir/name (2).ext`… si ya existe.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("rango infinito")
}

// ---------------------------------------------------------------------------
// Batería
// ---------------------------------------------------------------------------

/// Batería del sistema (no la de ratones/teclados: `scope=Device`).
/// `present = false` si no hay (sobremesa).
pub fn read_battery() -> pb::BatteryStatus {
    read_battery_in(Path::new("/sys/class/power_supply"))
}

fn read_battery_in(root: &Path) -> pb::BatteryStatus {
    let read = |dir: &Path, f: &str| {
        std::fs::read_to_string(dir.join(f))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let mut batteries = Vec::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let dir = entry.path();
        if read(&dir, "type") != "Battery" || read(&dir, "scope") == "Device" {
            continue;
        }
        let Ok(percent) = read(&dir, "capacity").parse::<u32>() else {
            continue;
        };
        let status = read(&dir, "status");
        batteries.push((
            percent.min(100),
            matches!(status.as_str(), "Charging" | "Full"),
        ));
    }
    if batteries.is_empty() {
        return pb::BatteryStatus::default();
    }
    let percent = batteries.iter().map(|b| b.0).sum::<u32>() / batteries.len() as u32;
    pb::BatteryStatus {
        present: true,
        percent,
        charging: batteries.iter().any(|b| b.1),
    }
}

async fn battery_task(features: Arc<Features>) {
    let mut ticker = tokio::time::interval(BATTERY_POLL);
    loop {
        ticker.tick().await;
        let battery = read_battery();
        if !battery.present {
            // Sobremesa: no se manda nada y no hace falta seguir mirando.
            return;
        }
        features.publish_local(F::Battery(battery));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorta_la_cache_por_lo_mas_antiguo() {
        let dir = std::env::temp_dir().join(format!("kushare-prune-{}", std::process::id()));
        let peer = dir.join("peer");
        std::fs::create_dir_all(&peer).unwrap();
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            let path = peer.join(name);
            std::fs::write(&path, vec![0u8; 1000]).unwrap();
            let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + i as u64);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_accessed(t).set_modified(t))
                .unwrap();
        }
        prune_dir(&dir, 2000);
        assert!(!peer.join("a").exists(), "la más antigua se borra");
        assert!(peer.join("b").exists() && peer.join("c").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stream_de_la_llamada() {
        let listing = "Source Output #12\n\tSource: 3\n\tProperties:\n\t\tnode.name = \"firefox\"\n\nSource Output #942\n\tSource: 95\n\tProperties:\n\t\tnode.name = \"bluez_output.AE_5E_46_C3_C3_5E.1\"\n";
        assert_eq!(
            call_stream(listing, "AE_5E_46_C3_C3_5E"),
            Some(("942".into(), "95".into()))
        );
        assert_eq!(call_stream(listing, "00_11_22_33_44_55"), None);
    }

    #[test]
    fn contact_photos_keep_their_real_format() {
        assert_eq!(image_extension(b"\x89PNG\r\n\x1a\n...."), "png");
        assert_eq!(image_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpg");
    }

    #[test]
    fn sinks_are_parsed_with_description_and_default() {
        let listing = "Sink #52\n\tState: RUNNING\n\tName: alsa_output.pci.analog\n\tDescription: Audio interno\n\nSink #60\n\tName: bluez_output.AA\n\tDescription: Auriculares\n";
        let outs = parse_sinks(listing, "bluez_output.AA");
        assert_eq!(outs.len(), 2);
        assert_eq!(outs[0].name, "Audio interno");
        assert!(!outs[0].active);
        assert!(outs[1].active);
        let hdmi = "Sink #1\n\tName: hdmi\n\tDescription: HDMI\n\tPorts:\n\t\t[Out] HDMI1: HDMI 1 (type: HDMI, priority: 500, not available)\n\tActive Port: [Out] HDMI1\nSink #2\n\tName: spk\n\tDescription: Altavoz\n\tActive Port: [Out] Speaker\n";
        let outs = parse_sinks(hdmi, "spk");
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].id, "spk");
    }

    #[test]
    fn sources_skip_monitors() {
        let listing = "Source #1\n\tName: alsa_output.analog.monitor\n\tDescription: Monitor of Audio\n\tMonitor of Sink: alsa_output.analog\nSource #2\n\tName: alsa_input.analog\n\tDescription: Micrófono\n\tMonitor of Sink: n/a\n";
        let ins = parse_devices(listing, "alsa_input.analog", true);
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].name, "Micrófono");
        assert!(ins[0].active);
    }

    #[test]
    fn letters_become_dial_keys() {
        assert_eq!(dialable("1-800-FLORES"), "1800356737");
        assert_eq!(dialable(" +52 (55) 1234 "), "+52551234");
    }

    #[test]
    fn battery_ignores_peripherals_and_desktops() {
        let dir = tempfile::tempdir().unwrap();
        let mouse = dir.path().join("hidpp_battery_0");
        std::fs::create_dir(&mouse).unwrap();
        for (f, v) in [("type", "Battery"), ("scope", "Device"), ("capacity", "5")] {
            std::fs::write(mouse.join(f), v).unwrap();
        }
        assert!(!read_battery_in(dir.path()).present);
        let bat = dir.path().join("BAT0");
        std::fs::create_dir(&bat).unwrap();
        for (f, v) in [
            ("type", "Battery"),
            ("capacity", "81"),
            ("status", "Charging"),
        ] {
            std::fs::write(bat.join(f), v).unwrap();
        }
        let b = read_battery_in(dir.path());
        assert!(b.present && b.charging);
        assert_eq!(b.percent, 81);
    }

    #[test]
    fn photo_names_cannot_escape_the_download_dir() {
        assert_eq!(sanitize_file_name("../../etc/passwd", "1"), "passwd");
        assert_eq!(sanitize_file_name("", "7"), "foto-7.jpg");
        assert_eq!(sanitize_file_name(".hidden", "7"), "foto-7.jpg");
    }
}
