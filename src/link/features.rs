//! Funciones extra del enlace en el escritorio (al estilo KDE Connect):
//!
//! - **Batería**: la propia se lee de `/sys/class/power_supply` (solo si es
//!   un portátil) y se manda a los peers; la del teléfono se muestra.
//! - **Multimedia**: los reproductores MPRIS del PC se publican y se
//!   controlan desde el teléfono; los del teléfono se controlan desde aquí.
//! - **Salida de audio**: las salidas de PipeWire/PulseAudio (vía `pactl`)
//!   se publican y el teléfono puede elegir la predeterminada.
//! - **Timbre**, **llamadas** y **fotos**: solo del teléfono hacia el PC.
//!
//! El enlace solo transporta los mensajes (`pb::Feature`); aquí se guarda
//! el último estado de cada peer y se responden las peticiones.

use kushare_link::LinkManager;
use kushare_link::pb::{self, feature::Kind as F};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const BATTERY_POLL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
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

pub struct Features {
    manager: LinkManager,
    peers: Mutex<HashMap<String, PeerState>>,
    local: Mutex<LocalState>,
    next_request: AtomicU64,
    lists: Mutex<HashMap<u64, oneshot::Sender<pb::PhotoList>>>,
    downloads: Mutex<HashMap<u64, Download>>,
    thumb_dir: PathBuf,
    /// Órdenes hacia las tareas locales (MPRIS, pactl).
    commands: mpsc::UnboundedSender<LocalCommand>,
}

enum LocalCommand {
    Media(pb::MediaCommand),
    SetOutput(String),
}

impl Features {
    /// Crea el estado y arranca las tareas que vigilan batería, MPRIS y
    /// salidas de audio.
    pub fn start(manager: LinkManager, cache_dir: &Path) -> Arc<Self> {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let features = Arc::new(Self {
            manager,
            peers: Mutex::default(),
            local: Mutex::default(),
            next_request: AtomicU64::new(1),
            lists: Mutex::default(),
            downloads: Mutex::default(),
            thumb_dir: cache_dir.join("thumbs"),
            commands,
        });
        tokio::spawn(battery_task(features.clone()));
        tokio::spawn(local_task(features.clone(), command_rx));
        features
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
    }

    pub fn forget(&self, peer_id: &str) {
        self.peers.lock().expect("mutex envenenado").remove(peer_id);
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
            F::PhotoList(list) => {
                drop(peers);
                if let Some(tx) = self
                    .lists
                    .lock()
                    .expect("mutex envenenado")
                    .remove(&list.request_id)
                {
                    let _ = tx.send(list);
                }
                return None;
            }
            F::PhotoData(data) => {
                drop(peers);
                self.on_photo_data(data);
                return None;
            }
            // Lo anuncia la señal D-Bus `PhotosChanged` (ver dbus.rs).
            F::PhotosChanged(_) => return None,
            // Solo tienen sentido hacia un teléfono.
            F::SetRingerMode(_)
            | F::CallCommand(_)
            | F::PhotoListRequest(_)
            | F::PhotoRequest(_) => return None,
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

    /// Pide una página de la galería del peer; guarda las miniaturas en la
    /// caché y devuelve (total, fotos con la ruta de su miniatura).
    pub async fn photos(
        &self,
        peer_id: &str,
        offset: u32,
        limit: u32,
    ) -> anyhow::Result<(u32, Vec<(pb::PhotoInfo, PathBuf)>)> {
        let request_id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.lists
            .lock()
            .expect("mutex envenenado")
            .insert(request_id, tx);
        let sent = self.send(
            peer_id,
            F::PhotoListRequest(pb::PhotoListRequest {
                request_id,
                offset,
                limit: limit.clamp(1, 60),
            }),
        );
        if let Err(err) = sent {
            self.lists
                .lock()
                .expect("mutex envenenado")
                .remove(&request_id);
            return Err(err);
        }
        let list = match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(list)) => list,
            _ => {
                self.lists
                    .lock()
                    .expect("mutex envenenado")
                    .remove(&request_id);
                anyhow::bail!("el teléfono no respondió (¿falta el permiso de fotos?)");
            }
        };
        let dir = self.thumb_dir.join(sanitize(peer_id));
        std::fs::create_dir_all(&dir)?;
        let mut out = Vec::with_capacity(list.photos.len());
        for mut photo in list.photos {
            let path = dir.join(format!("{}.jpg", sanitize(&photo.id)));
            if !photo.thumbnail.is_empty() {
                std::fs::write(&path, &photo.thumbnail)?;
            }
            photo.thumbnail.clear();
            out.push((photo, path));
        }
        Ok((list.total, out))
    }

    /// Descarga la foto original `id` a la carpeta de descargas.
    pub async fn fetch_photo(&self, peer_id: &str, id: &str) -> anyhow::Result<PathBuf> {
        let request_id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let dir = directories::UserDirs::new()
            .and_then(|d| d.download_dir().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&dir)?;
        let part = dir.join(format!(".kushare-{request_id}.part"));
        let file = std::fs::File::create(&part)?;
        self.downloads.lock().expect("mutex envenenado").insert(
            request_id,
            Download {
                file,
                part: part.clone(),
                target: dir,
                done: tx,
            },
        );
        if let Err(err) = self.send(
            peer_id,
            F::PhotoRequest(pb::PhotoRequest {
                request_id,
                id: id.to_string(),
            }),
        ) {
            self.abort_download(request_id);
            return Err(err);
        }
        // Una foto grande por Bluetooth puede tardar: 5 min de margen.
        match tokio::time::timeout(Duration::from_secs(300), rx).await {
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
                let target = unique_path(&d.target, &sanitize_file_name(&data.name, &data.id));
                std::fs::rename(&d.part, &target)
                    .map(|_| target)
                    .map_err(Into::into)
            }
        };
        let _ = d.done.send(result);
    }
}

fn replace_if_changed<T: PartialEq + Clone>(slot: &mut Option<T>, value: &T) -> bool {
    if slot.as_ref() == Some(value) {
        return false;
    }
    *slot = Some(value.clone());
    true
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
            usable = true;
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
        } else if let Some(port) = line.strip_prefix("[Out] ")
            && line.contains("not available")
        {
            if let Some((port, _)) = port.split_once(':') {
                unavailable_ports.push(port.to_string());
            }
        } else if let Some(active) = line.strip_prefix("Active Port: ") {
            let active = active.trim_start_matches("[Out] ");
            if unavailable_ports.iter().any(|p| p == active) {
                usable = false;
            }
        }
    }
    flush(&mut outputs, usable);
    outputs
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
