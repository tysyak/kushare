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
    pub players: Vec<pb::MediaPlayer>,
    pub outputs: Option<pb::AudioOutputs>,
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
        });
        tokio::spawn(battery_task(features.clone()));
        tokio::spawn(local_task(features.clone(), command_rx));
        tokio::spawn(prefetch_task(features.clone(), prefetch_rx));
        tokio::spawn(lan_probe_task(features.clone(), lan_probe_rx));
        tokio::spawn(local_network_task(features.clone()));
        features
    }

    /// Receptor de los avisos de fotos nuevas (solo se puede tomar una vez).
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
                state.players = p.players;
                RemoteChange::State
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
        let addrs = self
            .lan
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            .cloned()
            .unwrap_or_default();
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

    // -----------------------------------------------------------------
    // SMS, llamadas, contactos, notificaciones y fondo de pantalla
    // -----------------------------------------------------------------

    pub async fn sms_threads(
        &self,
        peer_id: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<pb::SmsThreads> {
        self.bulk_for_request(peer_id).await;
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::SmsThreadsRequest(pb::SmsThreadsRequest {
                        request_id,
                        offset,
                        limit: limit.clamp(1, 100),
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        match reply {
            F::SmsThreads(t) if t.error.is_empty() => Ok(t),
            F::SmsThreads(t) => anyhow::bail!("{}", phone_error(&t.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    pub async fn sms_messages(
        &self,
        peer_id: &str,
        thread_id: &str,
        limit: u32,
        before_ms: i64,
    ) -> anyhow::Result<Vec<pb::SmsMessage>> {
        self.bulk_for_request(peer_id).await;
        let thread_id = thread_id.to_string();
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::SmsMessagesRequest(pb::SmsMessagesRequest {
                        request_id,
                        thread_id,
                        limit: limit.clamp(1, 200),
                        before_ms,
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        match reply {
            F::SmsMessages(m) if m.error.is_empty() => Ok(m.messages),
            F::SmsMessages(m) => anyhow::bail!("{}", phone_error(&m.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    pub async fn send_sms(&self, peer_id: &str, address: &str, body: &str) -> anyhow::Result<()> {
        if address.trim().is_empty() || body.trim().is_empty() {
            anyhow::bail!("falta el número o el texto");
        }
        let (address, body) = (address.trim().to_string(), body.to_string());
        self.command(peer_id, |request_id| {
            F::SendSms(pb::SendSms {
                request_id,
                address,
                body,
            })
        })
        .await
        .map(|_| ())
    }

    pub async fn call_log(
        &self,
        peer_id: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<pb::CallLogEntry>> {
        self.bulk_for_request(peer_id).await;
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::CallLogRequest(pb::CallLogRequest {
                        request_id,
                        limit: limit.clamp(1, 500),
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        match reply {
            F::CallLog(l) if l.error.is_empty() => Ok(l.entries),
            F::CallLog(l) => anyhow::bail!("{}", phone_error(&l.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    /// Contactos del teléfono (con número). Se reutilizan unos minutos.
    pub async fn contacts(&self, peer_id: &str, refresh: bool) -> anyhow::Result<Vec<pb::Contact>> {
        if !refresh
            && let Some((at, list)) = self.contacts.lock().expect("mutex envenenado").get(peer_id)
            && at.elapsed() < CONTACTS_TTL
        {
            return Ok(list.clone());
        }
        self.bulk_for_request(peer_id).await;
        let reply = self
            .request(
                peer_id,
                |request_id| F::ContactsRequest(pb::ContactsRequest { request_id }),
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        let list = match reply {
            F::Contacts(c) if c.error.is_empty() => c.contacts,
            F::Contacts(c) => anyhow::bail!("{}", phone_error(&c.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        };
        self.contacts
            .lock()
            .expect("mutex envenenado")
            .insert(peer_id.to_string(), (Instant::now(), list.clone()));
        Ok(list)
    }

    /// Ruta de una foto de contacto. La extensión importa: el visor de
    /// imágenes de la GUI deduce el formato por ella.
    fn contact_photo_path(&self, peer_id: &str, key: &str, ext: &str) -> PathBuf {
        self.cache_dir
            .join("contacts")
            .join(sanitize(peer_id))
            .join(format!("{}.{ext}", sanitize(key)))
    }

    /// Foto de contacto ya guardada en este PC (sin pedir nada al teléfono).
    pub fn cached_contact_photo(&self, peer_id: &str, key: &str) -> Option<PathBuf> {
        if key.is_empty() {
            return None;
        }
        ["jpg", "png"]
            .into_iter()
            .map(|ext| self.contact_photo_path(peer_id, key, ext))
            .find(|path| path.exists())
    }

    /// Fotos de contacto por clave: las de la caché y, en una sola petición
    /// corta, las que faltan. Si el teléfono no responde (o es una versión
    /// sin fotos), simplemente no hay foto.
    pub async fn contact_photos(
        &self,
        peer_id: &str,
        keys: impl IntoIterator<Item = String>,
    ) -> HashMap<String, PathBuf> {
        let mut found = HashMap::new();
        let mut missing: Vec<String> = Vec::new();
        {
            let skip = self.photo_missing.lock().expect("mutex envenenado");
            for key in keys {
                if key.is_empty() || found.contains_key(&key) || missing.contains(&key) {
                    continue;
                }
                match self.cached_contact_photo(peer_id, &key) {
                    Some(path) => {
                        found.insert(key, path);
                    }
                    None if !skip.contains(&(peer_id.to_string(), key.clone())) => {
                        missing.push(key)
                    }
                    None => {}
                }
            }
        }
        missing.truncate(CONTACT_PHOTOS_MAX);
        if missing.is_empty() {
            return found;
        }
        self.bulk_for_request(peer_id).await;
        let asked = missing.clone();
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::ContactPhotosRequest(pb::ContactPhotosRequest {
                        request_id,
                        keys: asked,
                    })
                },
                CONTACT_PHOTOS_TIMEOUT,
            )
            .await;
        let photos = match reply {
            Ok(F::ContactPhotos(p)) if p.error.is_empty() => p.photos,
            Ok(F::ContactPhotos(p)) => {
                tracing::debug!("fotos de contacto: {}", p.error);
                return found;
            }
            _ => return found,
        };
        for photo in photos {
            if !missing.contains(&photo.key) || photo.image.is_empty() {
                continue;
            }
            let ext = image_extension(&photo.image);
            let path = self.contact_photo_path(peer_id, &photo.key, ext);
            match write_private(&path, &photo.image) {
                Ok(()) => {
                    found.insert(photo.key, path);
                }
                Err(err) => tracing::warn!("no se pudo guardar una foto de contacto: {err}"),
            }
        }
        let mut skip = self.photo_missing.lock().expect("mutex envenenado");
        for key in missing {
            if !found.contains_key(&key) {
                skip.insert((peer_id.to_string(), key));
            }
        }
        found
    }

    /// Llama a `number` desde el teléfono.
    pub async fn dial(&self, peer_id: &str, number: &str) -> anyhow::Result<()> {
        let number = dialable(number);
        if number.is_empty() {
            anyhow::bail!("número vacío");
        }
        self.command(peer_id, |request_id| {
            F::Dial(pb::Dial { request_id, number })
        })
        .await
        .map(|_| ())
    }

    /// Notificaciones que hay ahora en el teléfono, con la ruta de su icono.
    pub async fn phone_notifications(
        &self,
        peer_id: &str,
    ) -> anyhow::Result<Vec<(pb::Notification, Option<PathBuf>)>> {
        self.bulk_for_request(peer_id).await;
        let have_icons = self.manager.cached_icon_hashes();
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::ActiveNotificationsRequest(pb::ActiveNotificationsRequest {
                        request_id,
                        have_icons,
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        let list = match reply {
            F::ActiveNotifications(a) if a.error.is_empty() => a.notifications,
            F::ActiveNotifications(a) => anyhow::bail!("{}", phone_error(&a.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        };
        Ok(list
            .into_iter()
            .map(|mut n| {
                let icon = (!n.icon_hash.is_empty())
                    .then(|| self.manager.cached_icon(&n.icon_hash, &n.icon_png))
                    .flatten();
                n.icon_png.clear();
                (n, icon)
            })
            .collect())
    }

    fn wallpaper_path(&self, peer_id: &str) -> PathBuf {
        self.cache_dir
            .join("wallpapers")
            .join(format!("{}.jpg", sanitize(peer_id)))
    }

    /// Fondo de pantalla del teléfono. Sin `refresh`, el de la caché (el
    /// del disco sirve incluso sin conexión).
    pub async fn wallpaper(&self, peer_id: &str, refresh: bool) -> anyhow::Result<WallpaperInfo> {
        if !refresh {
            if let Some(w) = self
                .wallpapers
                .lock()
                .expect("mutex envenenado")
                .get(peer_id)
            {
                return Ok(w.clone());
            }
            let path = self.wallpaper_path(peer_id);
            if path.exists() && !self.manager.is_connected(peer_id) {
                return Ok(WallpaperInfo {
                    path: Some(path),
                    colors: Vec::new(),
                });
            }
        }
        self.bulk_for_request(peer_id).await;
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::WallpaperRequest(pb::WallpaperRequest {
                        request_id,
                        max_size: 1280,
                    })
                },
                SLOW_REQUEST_TIMEOUT,
            )
            .await?;
        let F::Wallpaper(w) = reply else {
            anyhow::bail!("respuesta inesperada del teléfono");
        };
        let path = self.wallpaper_path(peer_id);
        let path = if w.jpeg.is_empty() {
            let _ = std::fs::remove_file(&path);
            None
        } else {
            write_private(&path, &w.jpeg)?;
            Some(path)
        };
        let info = WallpaperInfo {
            path,
            colors: w.colors,
        };
        self.wallpapers
            .lock()
            .expect("mutex envenenado")
            .insert(peer_id.to_string(), info.clone());
        Ok(info)
    }

    // -----------------------------------------------------------------
    // Fotos
    // -----------------------------------------------------------------

    fn thumb_dir(&self, peer_id: &str) -> PathBuf {
        self.cache_dir.join("thumbs").join(sanitize(peer_id))
    }

    fn photo_dir(&self, peer_id: &str) -> PathBuf {
        self.cache_dir.join("photos").join(sanitize(peer_id))
    }

    /// La original de `id` en la caché (`<id>__<nombre>`), si está.
    pub fn original_path(&self, peer_id: &str, id: &str) -> Option<PathBuf> {
        let prefix = format!("{}__", sanitize(id));
        std::fs::read_dir(self.photo_dir(peer_id))
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .map(|e| e.path())
    }

    fn entry(&self, peer_id: &str, info: pb::PhotoInfo) -> PhotoEntry {
        PhotoEntry {
            thumb: self
                .thumb_dir(peer_id)
                .join(format!("{}.jpg", sanitize(&info.id))),
            original: self.original_path(peer_id, &info.id),
            info,
        }
    }

    /// Página de la galería del peer (más reciente primero); guarda las
    /// miniaturas en la caché. Con `offset == 0` además recuerda la lista
    /// (para [`Self::cached_photos`]) y precarga las más recientes.
    pub async fn photos(
        &self,
        peer_id: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<(u32, Vec<PhotoEntry>)> {
        let (total, photos) = self.list_photos(peer_id, offset, limit).await?;
        if offset == 0 {
            self.save_index(peer_id, total, &photos);
            let _ = self.prefetch.send(peer_id.to_string());
        }
        Ok((total, photos))
    }

    async fn list_photos(
        &self,
        peer_id: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<(u32, Vec<PhotoEntry>)> {
        self.bulk_for_request(peer_id).await;
        let dir = self.thumb_dir(peer_id);
        create_private_dir(&dir)?;
        let have = cached_thumbnails(&dir);
        let mut out = Vec::new();
        let mut total = 0;
        let limit = limit.max(1);
        while (out.len() as u32) < limit {
            let page_offset = offset + out.len() as u32;
            let page = (limit - out.len() as u32).min(PHOTO_PAGE_MAX);
            let have_thumbnails = have.clone();
            let reply = self
                .request(
                    peer_id,
                    |request_id| {
                        F::PhotoListRequest(pb::PhotoListRequest {
                            request_id,
                            offset: page_offset,
                            limit: page,
                            have_thumbnails,
                        })
                    },
                    SLOW_REQUEST_TIMEOUT,
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!("el teléfono no respondió (¿falta el permiso de fotos?)")
                })?;
            let F::PhotoList(list) = reply else {
                anyhow::bail!("respuesta inesperada del teléfono");
            };
            total = list.total;
            let got = list.photos.len();
            for mut photo in list.photos {
                if !photo.thumbnail.is_empty() {
                    let path = dir.join(format!("{}.jpg", sanitize(&photo.id)));
                    write_private(&path, &photo.thumbnail)?;
                }
                photo.thumbnail.clear();
                out.push(self.entry(peer_id, photo));
            }
            if got < page as usize {
                break;
            }
        }
        Ok((total, out))
    }

    fn index_path(&self, peer_id: &str) -> PathBuf {
        self.thumb_dir(peer_id).join("index.json")
    }

    fn save_index(&self, peer_id: &str, total: u32, photos: &[PhotoEntry]) {
        let index = PhotoIndex {
            total,
            photos: photos
                .iter()
                .map(|p| IndexedPhoto {
                    id: p.info.id.clone(),
                    name: p.info.name.clone(),
                    date_ms: p.info.date_ms,
                    width: p.info.width,
                    height: p.info.height,
                    size: p.info.size,
                })
                .collect(),
        };
        if let Ok(json) = serde_json::to_vec(&index)
            && let Err(err) = write_private(&self.index_path(peer_id), &json)
        {
            tracing::debug!(?err, "no se pudo guardar el índice de fotos");
        }
    }

    /// La última lista de fotos vista (sin preguntar al teléfono): la
    /// galería se enseña al instante y también sin conexión.
    pub fn cached_photos(&self, peer_id: &str) -> (u32, Vec<PhotoEntry>) {
        let index: PhotoIndex = std::fs::read(self.index_path(peer_id))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let photos = index
            .photos
            .into_iter()
            .map(|p| {
                self.entry(
                    peer_id,
                    pb::PhotoInfo {
                        id: p.id,
                        name: p.name,
                        date_ms: p.date_ms,
                        width: p.width,
                        height: p.height,
                        size: p.size,
                        thumbnail: Vec::new(),
                    },
                )
            })
            .filter(|e| e.thumb.exists())
            .collect();
        (index.total, photos)
    }

    /// La foto original `id` en la caché (la descarga si no está).
    pub async fn fetch_photo(&self, peer_id: &str, id: &str) -> anyhow::Result<PathBuf> {
        self.download(peer_id, id, false).await
    }

    /// Descarga la original a la carpeta de descargas (`kushare photos --download`).
    pub async fn save_photo_to_downloads(
        &self,
        peer_id: &str,
        id: &str,
    ) -> anyhow::Result<PathBuf> {
        let cached = self.fetch_photo(peer_id, id).await?;
        let dir = directories::UserDirs::new()
            .and_then(|d| d.download_dir().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&dir)?;
        let target = unique_path(&dir, &original_name(&cached));
        std::fs::copy(&cached, &target)?;
        Ok(target)
    }

    async fn download(&self, peer_id: &str, id: &str, background: bool) -> anyhow::Result<PathBuf> {
        let key = (peer_id.to_string(), id.to_string());
        // Si ya se está descargando (p. ej. la precarga), esperar a esa.
        let started = Instant::now();
        while self
            .inflight
            .lock()
            .expect("mutex envenenado")
            .contains(&key)
        {
            if started.elapsed() > BACKGROUND_DOWNLOAD_TIMEOUT {
                anyhow::bail!("la descarga de la foto no terminó a tiempo");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if let Some(path) = self.original_path(peer_id, id) {
            return Ok(path);
        }
        self.inflight
            .lock()
            .expect("mutex envenenado")
            .insert(key.clone());
        let result = self.download_now(peer_id, id, background).await;
        self.inflight.lock().expect("mutex envenenado").remove(&key);
        result
    }

    async fn download_now(
        &self,
        peer_id: &str,
        id: &str,
        background: bool,
    ) -> anyhow::Result<PathBuf> {
        self.bulk_for_request(peer_id).await;
        let request_id = self.new_request_id();
        let (tx, rx) = oneshot::channel();
        let dir = self.photo_dir(peer_id);
        create_private_dir(&dir)?;
        let part = dir.join(format!(".kushare-{request_id}.part"));
        let file = std::fs::File::create(&part)?;
        self.downloads.lock().expect("mutex envenenado").insert(
            request_id,
            Download {
                file,
                part: part.clone(),
                // El nombre definitivo se completa con el que mande el teléfono.
                target: dir.join(format!("{}__", sanitize(id))),
                done: tx,
            },
        );
        if let Err(err) = self.send(
            peer_id,
            F::PhotoRequest(pb::PhotoRequest {
                request_id,
                id: id.to_string(),
                chunk_size: if background { PREFETCH_CHUNK } else { 0 },
                background,
            }),
        ) {
            self.abort_download(request_id);
            return Err(err);
        }
        let timeout = if background {
            BACKGROUND_DOWNLOAD_TIMEOUT
        } else {
            DOWNLOAD_TIMEOUT
        };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            _ => {
                self.abort_download(request_id);
                anyhow::bail!("la descarga de la foto no terminó a tiempo")
            }
        }
    }

    fn abort_download(&self, request_id: u64) {
        if let Some(d) = self
            .downloads
            .lock()
            .expect("mutex envenenado")
            .remove(&request_id)
        {
            let _ = std::fs::remove_file(&d.part);
        }
    }

    fn on_photo_data(&self, data: pb::PhotoData) {
        use std::io::{Seek, SeekFrom, Write};
        let mut downloads = self.downloads.lock().expect("mutex envenenado");
        let Some(d) = downloads.get_mut(&data.request_id) else {
            return;
        };
        let written = d
            .file
            .seek(SeekFrom::Start(data.offset))
            .and_then(|_| d.file.write_all(&data.data));
        let failure = if !data.error.is_empty() {
            Some(anyhow::anyhow!(
                "el teléfono no pudo leer la foto: {}",
                data.error
            ))
        } else {
            written.err().map(anyhow::Error::from)
        };
        if failure.is_none() && !data.last {
            return;
        }
        let d = downloads.remove(&data.request_id).expect("existe");
        drop(downloads);
        let result = match failure {
            Some(err) => {
                let _ = std::fs::remove_file(&d.part);
                Err(err)
            }
            None => {
                let mut target = d.target.into_os_string();
                target.push(sanitize_file_name(&data.name, &data.id));
                let target = PathBuf::from(target);
                std::fs::rename(&d.part, &target)
                    .map(|_| target)
                    .map_err(Into::into)
            }
        };
        let _ = d.done.send(result);
    }

    /// Borra la foto del teléfono. "done", o "confirm" si el teléfono pide
    /// confirmarlo allí (Android protege las fotos de otras apps).
    pub async fn delete_photo(&self, peer_id: &str, id: &str) -> anyhow::Result<String> {
        let photo = id.to_string();
        let status = self
            .command(peer_id, |request_id| {
                F::PhotoDelete(pb::PhotoDelete {
                    request_id,
                    id: photo,
                })
            })
            .await?;
        if status == "done" {
            if let Some(path) = self.original_path(peer_id, id) {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::remove_file(
                self.thumb_dir(peer_id)
                    .join(format!("{}.jpg", sanitize(id))),
            );
        }
        Ok(status)
    }

    /// Avisa (una vez) de las fotos nuevas `new` de `peer`.
    fn announce(&self, peer: &str, new: &[PhotoEntry]) {
        let Some(first) = new.first() else {
            return;
        };
        if !self.settings.get().photo_notify {
            return;
        }
        // No repetir el aviso del mismo lote.
        static LAST: Mutex<Option<String>> = Mutex::new(None);
        let key = format!("{peer}/{}", first.info.id);
        {
            let mut last = LAST.lock().expect("mutex envenenado");
            if last.as_deref() == Some(key.as_str()) {
                return;
            }
            *last = Some(key);
        }
        let photos = new
            .iter()
            .map(|p| PhotoEntry {
                original: self.original_path(peer, &p.info.id),
                ..p.clone()
            })
            .collect();
        let _ = self.new_photos.send(NewPhotos {
            peer: peer.to_string(),
            photos,
        });
    }

    /// Deja la caché de originales por debajo de [`PHOTO_CACHE_MAX_BYTES`]
    /// (borra primero las que hace más que no se usan).
    fn prune_photo_cache(&self) {
        let root = self.cache_dir.join("photos");
        let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
        for peer in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            for f in std::fs::read_dir(peer.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Ok(meta) = f.metadata()
                    && meta.is_file()
                {
                    let used = meta.accessed().or_else(|_| meta.modified());
                    files.push((used.unwrap_or(std::time::UNIX_EPOCH), meta.len(), f.path()));
                }
            }
        }
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        files.sort();
        for (_, len, path) in files {
            if total <= PHOTO_CACHE_MAX_BYTES {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }
}

/// Precarga en segundo plano las fotos más recientes de cada peer que lo
/// pida (al conectar, al haber fotos nuevas, al abrir la galería).
async fn prefetch_task(features: Arc<Features>, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(first) = rx.recv().await {
        // Agrupar avisos seguidos (una ráfaga de capturas, etc.).
        tokio::time::sleep(Duration::from_secs(4)).await;
        let mut peers = HashSet::from([first]);
        while let Ok(p) = rx.try_recv() {
            peers.insert(p);
        }
        for peer in peers {
            let settings = features.settings.get();
            if !(settings.photo_prefetch || settings.photo_notify)
                || !features.manager.is_connected(&peer)
            {
                continue;
            }
            let list = match features.list_photos(&peer, 0, PREFETCH_COUNT).await {
                Ok((_, list)) => list,
                Err(err) => {
                    tracing::debug!(?err, "precarga de fotos: no se pudo listar");
                    continue;
                }
            };
            // Nuevas: posteriores a la más reciente ya vista. La primera vez
            // (al arrancar) solo se toma nota, sin avisar.
            let newest = list.iter().map(|p| p.info.date_ms).max().unwrap_or(0);
            let seen = features
                .newest_seen
                .lock()
                .expect("mutex envenenado")
                .insert(peer.clone(), newest);
            let new: Vec<PhotoEntry> = match seen {
                Some(seen) => list
                    .iter()
                    .filter(|p| p.info.date_ms > seen)
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            // Solo por Wi-Fi (`list_photos` ya probó el enlace de datos): por
            // Bluetooth, precargar fotos ocupa el enlace minutos y retrasa todo
            // lo demás (SMS, llamadas, notificaciones). Las que se abren a mano
            // se siguen descargando por Bluetooth.
            let wifi = features.lan_ready(&peer) || features.manager.has_bulk(&peer);
            if settings.photo_prefetch && wifi {
                // Las nuevas primero, para que "Ver" del aviso sea instantáneo.
                for photo in new.iter().chain(list.iter()) {
                    if features.original_path(&peer, &photo.info.id).is_some()
                        || photo.info.size > PREFETCH_MAX_BYTES
                    {
                        continue;
                    }
                    if !features.manager.is_connected(&peer) {
                        break;
                    }
                    match features.download(&peer, &photo.info.id, true).await {
                        Ok(path) => tracing::debug!(?path, "foto precargada"),
                        Err(err) => tracing::debug!(?err, "precarga de foto fallida"),
                    }
                    // Avisar en cuanto esté la primera nueva (no esperar a las demás).
                    if !new.is_empty() && photo.info.id == new[0].info.id {
                        features.announce(&peer, &new);
                    }
                }
                features.prune_photo_cache();
            }
            features.announce(&peer, &new);
        }
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

/// Ids de las miniaturas en caché (las más recientes primero, hasta 1000).
fn cached_thumbnails(dir: &Path) -> Vec<String> {
    let mut thumbs: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".jpg")?.to_string();
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, id))
        })
        .collect();
    thumbs.sort_by_key(|t| std::cmp::Reverse(t.0));
    thumbs.into_iter().take(1000).map(|(_, id)| id).collect()
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

// ---------------------------------------------------------------------------
// MPRIS y salidas de audio
// ---------------------------------------------------------------------------

async fn local_task(features: Arc<Features>, mut commands: mpsc::UnboundedReceiver<LocalCommand>) {
    let conn = match zbus::Connection::session().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(?err, "sin bus de sesión: multimedia desactivada");
            return;
        }
    };
    let (dirty_tx, mut dirty_rx) = mpsc::unbounded_channel::<Dirty>();
    tokio::spawn(watch_mpris(conn.clone(), dirty_tx.clone()));
    tokio::spawn(watch_pactl(dirty_tx.clone()));
    let _ = dirty_tx.send(Dirty::Media);
    let _ = dirty_tx.send(Dirty::Audio);

    loop {
        tokio::select! {
            Some(first) = dirty_rx.recv() => {
                // Agrupar ráfagas de cambios (un salto de pista emite varios).
                let (mut media, mut audio) = (first == Dirty::Media, first == Dirty::Audio);
                tokio::time::sleep(Duration::from_millis(300)).await;
                while let Ok(d) = dirty_rx.try_recv() {
                    media |= d == Dirty::Media;
                    audio |= d == Dirty::Audio;
                }
                if media {
                    let players = mpris_players(&conn).await;
                    tracing::debug!(count = players.len(), "reproductores MPRIS");
                    features.publish_local(F::MediaPlayers(pb::MediaPlayers { players }));
                }
                if audio && let Some(outputs) = pactl_outputs().await {
                    features.publish_local(F::AudioOutputs(outputs));
                }
            }
            Some(cmd) = commands.recv() => match cmd {
                LocalCommand::Media(cmd) => {
                    if let Err(err) = mpris_command(&conn, &cmd).await {
                        tracing::info!(?err, "orden multimedia fallida");
                    }
                    let _ = dirty_tx.send(Dirty::Media);
                }
                LocalCommand::SetOutput(id) => {
                    if let Err(err) = pactl_set_output(&id).await {
                        tracing::info!(?err, %id, "no se pudo cambiar la salida de audio");
                    }
                    let _ = dirty_tx.send(Dirty::Audio);
                }
            },
            else => break,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dirty {
    Media,
    Audio,
}

async fn watch_mpris(conn: zbus::Connection, dirty: mpsc::UnboundedSender<Dirty>) {
    use futures_util::StreamExt;
    let rules = [
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus.Properties")
            .and_then(|b| b.member("PropertiesChanged"))
            .and_then(|b| b.path(MPRIS_PATH))
            .map(|b| b.build()),
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus")
            .and_then(|b| b.member("NameOwnerChanged"))
            .and_then(|b| b.arg0ns("org.mpris.MediaPlayer2"))
            .map(|b| b.build()),
    ];
    let mut streams = Vec::new();
    for rule in rules {
        let stream = match rule {
            Ok(rule) => zbus::MessageStream::for_match_rule(rule, &conn, Some(64)).await,
            Err(err) => Err(err),
        };
        match stream {
            Ok(s) => streams.push(s),
            Err(err) => tracing::warn!(?err, "no se pudo vigilar MPRIS"),
        }
    }
    let mut merged = futures_util::stream::select_all(streams);
    while merged.next().await.is_some() {
        if dirty.send(Dirty::Media).is_err() {
            break;
        }
    }
}

async fn mpris_names(conn: &zbus::Connection) -> Vec<String> {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(conn).await else {
        return Vec::new();
    };
    let mut names: Vec<String> = dbus
        .list_names()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|n| n.to_string())
        // playerctld solo replica al reproductor activo: saldría duplicado.
        .filter(|n| n.starts_with(MPRIS_PREFIX) && !n.ends_with(".playerctld"))
        .collect();
    names.sort();
    names
}

async fn props(
    conn: &zbus::Connection,
    name: &str,
    interface: &'static str,
) -> zbus::Result<HashMap<String, zbus::zvariant::OwnedValue>> {
    let proxy = zbus::fdo::PropertiesProxy::builder(conn)
        .destination(name.to_string())?
        .path(MPRIS_PATH)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    proxy
        .get_all(zbus::names::InterfaceName::from_static_str_unchecked(
            interface,
        ))
        .await
        .map_err(Into::into)
}

fn as_string(v: Option<&zbus::zvariant::OwnedValue>) -> String {
    v.and_then(|v| v.downcast_ref::<String>().ok())
        .unwrap_or_default()
}

fn as_bool(v: Option<&zbus::zvariant::OwnedValue>) -> bool {
    v.and_then(|v| v.downcast_ref::<bool>().ok())
        .unwrap_or(false)
}

async fn mpris_players(conn: &zbus::Connection) -> Vec<pb::MediaPlayer> {
    let mut players = Vec::new();
    for name in mpris_names(conn).await {
        let player = match props(conn, &name, MPRIS_PLAYER).await {
            Ok(player) => player,
            Err(err) => {
                tracing::debug!(%name, %err, "reproductor MPRIS ilegible");
                continue;
            }
        };
        let identity = props(conn, &name, "org.mpris.MediaPlayer2")
            .await
            .ok()
            .map(|p| as_string(p.get("Identity")))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                name.trim_start_matches(MPRIS_PREFIX)
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .to_string()
            });
        let (mut title, mut artist, mut album) = Default::default();
        if let Some(md) = player.get("Metadata")
            && let Ok(md) = HashMap::<String, zbus::zvariant::OwnedValue>::try_from(
                md.try_clone()
                    .unwrap_or_else(|_| zbus::zvariant::OwnedValue::from(0u8)),
            )
        {
            title = as_string(md.get("xesam:title"));
            album = as_string(md.get("xesam:album"));
            artist = md
                .get("xesam:artist")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| Vec::<String>::try_from(v).ok())
                .map(|a| a.join(", "))
                .unwrap_or_default();
        }
        let state = match as_string(player.get("PlaybackStatus")).as_str() {
            "Playing" => pb::PlaybackState::PlaybackPlaying,
            "Paused" => pb::PlaybackState::PlaybackPaused,
            _ => pb::PlaybackState::PlaybackStopped,
        };
        players.push(pb::MediaPlayer {
            id: name.clone(),
            name: identity,
            title,
            artist,
            album,
            state: state.into(),
            can_play: as_bool(player.get("CanPlay")),
            can_pause: as_bool(player.get("CanPause")),
            can_next: as_bool(player.get("CanGoNext")),
            can_previous: as_bool(player.get("CanGoPrevious")),
        });
    }
    players
}

async fn mpris_command(conn: &zbus::Connection, cmd: &pb::MediaCommand) -> zbus::Result<()> {
    let method = match cmd.action() {
        pb::MediaAction::MediaPlayPause => "PlayPause",
        pb::MediaAction::MediaPlay => "Play",
        pb::MediaAction::MediaPause => "Pause",
        pb::MediaAction::MediaNext => "Next",
        pb::MediaAction::MediaPrevious => "Previous",
        pb::MediaAction::MediaStop => "Stop",
    };
    let targets = if cmd.player_id.is_empty() {
        mpris_names(conn).await
    } else if cmd.player_id.starts_with(MPRIS_PREFIX) {
        vec![cmd.player_id.clone()]
    } else {
        return Err(zbus::Error::Failure("reproductor inválido".into()));
    };
    for name in targets {
        conn.call_method(
            Some(name.as_str()),
            MPRIS_PATH,
            Some(MPRIS_PLAYER),
            method,
            &(),
        )
        .await?;
    }
    Ok(())
}

/// `pactl` con salida en inglés (sus etiquetas se traducen).
fn pactl() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("pactl");
    cmd.env("LC_ALL", "C").kill_on_drop(true);
    // Que muera con el daemon: `kushare quit` sale con `process::exit` y
    // `kill_on_drop` no llega a actuar (quedaban `pactl subscribe` huérfanos).
    // SAFETY: prctl es async-signal-safe y no toca memoria del proceso padre.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd
}

async fn watch_pactl(dirty: mpsc::UnboundedSender<Dirty>) {
    use tokio::io::AsyncBufReadExt;
    loop {
        let child = pactl()
            .arg("subscribe")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            tracing::info!("pactl no disponible: no se publican las salidas de audio");
            return;
        };
        let mut lines = tokio::io::BufReader::new(child.stdout.take().expect("piped")).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            // "Event 'change' on sink #52", "Event 'change' on server"…
            if (line.contains(" sink #") || line.contains(" server"))
                && dirty.send(Dirty::Audio).is_err()
            {
                return;
            }
        }
        let _ = child.wait().await;
        // El servidor de audio se reinició: reintentar más tarde.
        tokio::time::sleep(Duration::from_secs(10)).await;
        let _ = dirty.send(Dirty::Audio);
    }
}

async fn pactl_output(args: &[&str]) -> Option<String> {
    let out = pactl().args(args).output().await.ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

async fn pactl_outputs() -> Option<pb::AudioOutputs> {
    let default = pactl_output(&["get-default-sink"])
        .await?
        .trim()
        .to_string();
    let listing = pactl_output(&["list", "sinks"]).await?;
    Some(pb::AudioOutputs {
        outputs: parse_sinks(&listing, &default),
        selectable: true,
    })
}

/// Extrae (Name, Description) de `pactl list sinks` (con `LC_ALL=C`).
/// Omite las salidas cuyo puerto activo no está disponible (p. ej. un HDMI
/// sin nada enchufado): WirePlumber no deja ponerlas por defecto.
fn parse_sinks(listing: &str, default: &str) -> Vec<pb::AudioOutput> {
    parse_devices(listing, default, false)
}

/// Como [`parse_sinks`] para `pactl list sinks` (`input = false`) o
/// `pactl list sources` (`input = true`, sin los "monitor" de las salidas).
fn parse_devices(listing: &str, default: &str, input: bool) -> Vec<pb::AudioOutput> {
    let port_tag = if input { "[In] " } else { "[Out] " };
    let mut outputs: Vec<pb::AudioOutput> = Vec::new();
    let mut unavailable_ports: Vec<String> = Vec::new();
    let mut usable = true;
    let flush = |outputs: &mut Vec<pb::AudioOutput>, usable: bool| {
        if !usable {
            outputs.pop();
        }
    };
    for line in listing.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("Name: ") {
            flush(&mut outputs, usable);
            usable = !(input && name.ends_with(".monitor"));
            unavailable_ports.clear();
            outputs.push(pb::AudioOutput {
                id: name.to_string(),
                name: name.to_string(),
                active: name == default,
            });
        } else if let Some(desc) = line.strip_prefix("Description: ")
            && let Some(last) = outputs.last_mut()
        {
            last.name = desc.to_string();
        } else if let Some(sink) = line.strip_prefix("Monitor of Sink: ") {
            if input && sink != "n/a" {
                usable = false;
            }
        } else if let Some(port) = line.strip_prefix(port_tag)
            && line.contains("not available")
        {
            if let Some((port, _)) = port.split_once(':') {
                unavailable_ports.push(port.to_string());
            }
        } else if let Some(active) = line.strip_prefix("Active Port: ") {
            let active = active.trim_start_matches(port_tag);
            if unavailable_ports.iter().any(|p| p == active) {
                usable = false;
            }
        }
    }
    flush(&mut outputs, usable);
    outputs
}

/// Salidas y entradas (micrófonos) de audio del PC, para la GUI.
pub async fn local_audio() -> (Vec<pb::AudioOutput>, Vec<pb::AudioOutput>) {
    let outputs = pactl_outputs().await.unwrap_or_default().outputs;
    let inputs = pactl_inputs().await;
    (outputs, inputs)
}

async fn pactl_inputs() -> Vec<pb::AudioOutput> {
    let Some(default) = pactl_output(&["get-default-source"]).await else {
        return Vec::new();
    };
    let Some(listing) = pactl_output(&["list", "sources"]).await else {
        return Vec::new();
    };
    parse_devices(&listing, default.trim(), true)
}

/// Elige la entrada de audio (micrófono) predeterminada del PC y mueve a
/// ella lo que ya esté grabando (p. ej. una llamada en curso).
pub async fn set_local_input(id: &str) -> anyhow::Result<()> {
    if !pactl_inputs().await.iter().any(|o| o.id == id) {
        anyhow::bail!("entrada desconocida");
    }
    let status = pactl().args(["set-default-source", id]).status().await?;
    if !status.success() {
        anyhow::bail!("pactl set-default-source falló");
    }
    let now = pactl_output(&["get-default-source"])
        .await
        .unwrap_or_default();
    if now.trim() != id {
        anyhow::bail!("el servidor de audio no aceptó la entrada (¿no está disponible?)");
    }
    if let Some(outputs) = pactl_output(&["list", "short", "source-outputs"]).await {
        for index in outputs.lines().filter_map(|l| l.split_whitespace().next()) {
            let _ = pactl()
                .args(["move-source-output", index, id])
                .status()
                .await;
        }
    }
    Ok(())
}

/// Elige la salida de audio predeterminada del PC (desde la GUI).
pub async fn set_local_output(id: &str) -> anyhow::Result<()> {
    pactl_set_output(id).await
}

async fn pactl_set_output(id: &str) -> anyhow::Result<()> {
    let current = pactl_outputs().await.unwrap_or_default();
    if !current.outputs.iter().any(|o| o.id == id) {
        anyhow::bail!("salida desconocida");
    }
    let status = pactl().args(["set-default-sink", id]).status().await?;
    if !status.success() {
        anyhow::bail!("pactl set-default-sink falló");
    }
    // pactl responde bien aunque el gestor de sesión (WirePlumber) rechace
    // el cambio: comprobarlo.
    let now = pactl_output(&["get-default-sink"])
        .await
        .unwrap_or_default();
    if now.trim() != id {
        anyhow::bail!("el servidor de audio no aceptó la salida (¿no está disponible?)");
    }
    // Mover también lo que ya está sonando.
    if let Some(inputs) = pactl_output(&["list", "short", "sink-inputs"]).await {
        for index in inputs.lines().filter_map(|l| l.split_whitespace().next()) {
            let _ = pactl().args(["move-sink-input", index, id]).status().await;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Audio del teléfono en el PC (Bluetooth A2DP + HFP)
// ---------------------------------------------------------------------------

/// Perfil A2DP "Audio Source" del teléfono: su música suena en el PC.
const A2DP_SOURCE: &str = "0000110a-0000-1000-8000-00805f9b34fb";
/// Perfil HFP "Audio Gateway" del teléfono: el PC hace de manos libres en las llamadas.
const HFP_AG: &str = "0000111f-0000-1000-8000-00805f9b34fb";

/// Conecta (o desconecta) los perfiles de audio del teléfono con el PC,
/// para oír en el PC su música y sus llamadas (y hablar con el micrófono
/// del PC). Requiere que PipeWire/WirePlumber tenga los roles `a2dp_sink`
/// y `hfp_hf` (vienen activados por defecto). Devuelve qué perfiles quedaron.
pub async fn phone_audio(address: &str, enable: bool) -> anyhow::Result<Vec<&'static str>> {
    let address: bluer::Address = address
        .parse()
        .map_err(|_| anyhow::anyhow!("dirección Bluetooth inválida: {address}"))?;
    let session = bluer::Session::new().await?;
    let device = session.default_adapter().await?.device(address)?;
    if enable && !device.is_paired().await.unwrap_or(false) {
        anyhow::bail!("el teléfono no está vinculado por Bluetooth con este PC");
    }
    let mut done = Vec::new();
    let mut last_err = None;
    for (uuid, label) in [(A2DP_SOURCE, "multimedia"), (HFP_AG, "llamadas")] {
        let uuid: bluer::Uuid = uuid.parse().expect("UUID válido");
        let result = if enable {
            device.connect_profile(&uuid).await
        } else {
            device.disconnect_profile(&uuid).await
        };
        match result {
            Ok(()) => done.push(label),
            Err(err) => {
                tracing::info!(%err, label, enable, "perfil de audio del teléfono");
                last_err = Some(err);
            }
        }
    }
    match (done.is_empty(), last_err) {
        (true, Some(err)) if enable => Err(anyhow::anyhow!(
            "no se pudo conectar el audio ({err}); en el teléfono, activa «Audio multimedia» y «Llamadas» para este PC en los ajustes de Bluetooth"
        )),
        _ => Ok(done),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
