//! Subcomandos de la CLI que controlan el enlace del daemon en marcha vía
//! D-Bus (ver [`super::dbus`]). No arrancan nada por sí mismos.

use clap::{Subcommand, ValueEnum};
use futures_util::StreamExt;
use std::io::Write;
use std::time::Duration;

use super::dbus::DaemonProxy;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Way {
    /// Recibir aquí las notificaciones del dispositivo.
    In,
    /// Enviar las notificaciones de aquí al dispositivo.
    Out,
    /// Ambas direcciones.
    Both,
}

impl Way {
    fn directions(self) -> &'static [&'static str] {
        match self {
            Way::In => &["in"],
            Way::Out => &["out"],
            Way::Both => &["in", "out"],
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Toggle {
    On,
    Off,
}

#[derive(Subcommand, Debug)]
pub enum LinkCommand {
    /// Lista los dispositivos emparejados y los encontrados en el último escaneo.
    Devices,
    /// Cierra el daemon en marcha.
    Quit,
    /// Busca dispositivos kushare en la red (no hay escaneo continuo).
    Scan {
        /// Segundos de búsqueda.
        #[arg(default_value_t = 15)]
        seconds: u32,
    },
    /// Empareja con un dispositivo visible en la red (id, prefijo del id o
    /// nombre) o por Bluetooth (dirección MAC de un dispositivo ya vinculado).
    Pair {
        device: String,
        /// Aceptar el PIN sin preguntar (el otro lado debe confirmarlo igual).
        #[arg(long)]
        yes: bool,
    },
    /// Olvida un dispositivo emparejado (en ambos lados).
    Unpair { device: String },
    /// Activa o desactiva las notificaciones con un dispositivo.
    Notif {
        state: Toggle,
        device: String,
        #[arg(value_enum, default_value = "both")]
        way: Way,
    },
    /// Muestra las apps conocidas y cuáles están bloqueadas.
    Apps { device: String },
    /// Bloquea las notificaciones de una app en una dirección.
    Block {
        device: String,
        #[arg(value_enum)]
        way: Way,
        app: String,
    },
    /// Desbloquea las notificaciones de una app en una dirección.
    Unblock {
        device: String,
        #[arg(value_enum)]
        way: Way,
        app: String,
    },
    /// Batería, timbre, llamada, reproductores y salidas de audio de un dispositivo.
    Status { device: String },
    /// Controla los reproductores del dispositivo. Sin reproductor: todos
    /// (`kushare media <disp> pause` = pausar todo).
    Media {
        device: String,
        #[arg(value_enum)]
        action: MediaVerb,
        /// id o parte del nombre del reproductor (ver `kushare status`).
        player: Option<String>,
    },
    /// Lista las salidas de audio del dispositivo o elige una (id o parte del nombre).
    Audio {
        device: String,
        output: Option<String>,
    },
    /// Hace sonar el teléfono para encontrarlo, aunque esté en silencio
    /// (`--stop` lo para).
    Find {
        device: String,
        #[arg(long)]
        stop: bool,
    },
    /// Cambia el modo de timbre del teléfono.
    Ringer {
        device: String,
        #[arg(value_enum)]
        mode: RingerVerb,
    },
    /// Controla la llamada en curso del teléfono.
    Call {
        device: String,
        #[arg(value_enum)]
        action: CallVerb,
    },
    /// Oír en el PC la música y las llamadas del teléfono (Bluetooth A2DP + HFP).
    PhoneAudio { device: String, state: Toggle },
    /// Lista las fotos del teléfono (más recientes primero) o descarga una.
    Photos {
        device: String,
        /// Descargar la foto con este id a la carpeta de descargas.
        #[arg(long)]
        download: Option<String>,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Borrar del teléfono la foto con este id (puede pedir confirmarlo allí).
        #[arg(long)]
        delete: Option<String>,
    },
    /// SMS del teléfono: sin más, las conversaciones; con `--thread`, sus
    /// mensajes; con `--to` y un texto, manda un SMS.
    Sms {
        device: String,
        #[arg(long)]
        thread: Option<String>,
        /// Número al que mandar el SMS.
        #[arg(long)]
        to: Option<String>,
        /// Texto del SMS (con `--to`).
        text: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Registro de llamadas del teléfono.
    Calls {
        device: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Llama desde el teléfono (admite letras: 1-800-FLORES).
    Dial { device: String, number: String },
    /// Busca en los contactos del teléfono.
    Contacts {
        device: String,
        search: Option<String>,
    },
    /// Notificaciones que hay ahora en el teléfono; `--dismiss <key>` descarta una.
    PhoneNotifs {
        device: String,
        #[arg(long)]
        dismiss: Option<String>,
    },
    /// El teléfono como webcam del PC (cámara virtual v4l2loopback).
    Webcam {
        #[command(subcommand)]
        action: WebcamAction,
    },
    /// Escáner de QR del teléfono e historial de códigos leídos.
    Qr {
        #[command(subcommand)]
        action: QrAction,
    },
    /// Historial del portapapeles (`--clear` lo borra; `--copy <n>` copia una entrada).
    Clipboard {
        #[arg(long)]
        clear: bool,
        #[arg(long)]
        copy: Option<usize>,
    },
    /// Ajustes del daemon (sin argumentos los lista): clipboard-sync,
    /// clipboard-history, photo-prefetch, update-check.
    Setting {
        name: Option<String>,
        #[arg(value_enum)]
        state: Option<Toggle>,
    },
    /// Salidas y entradas de audio del PC; `--output`/`--input` elige (id o parte del nombre).
    PcAudio {
        #[arg(long)]
        output: Option<String>,
        #[arg(long)]
        input: Option<String>,
    },
    /// Busca una versión nueva de kushare (`--download` la descarga verificada).
    Update {
        #[arg(long)]
        download: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum WebcamAction {
    /// Lista las cámaras del teléfono (frontal, traseras, infrarroja…).
    Cameras {
        device: String,
    },
    /// Empieza: `--camera` acepta el id o parte de la etiqueta («frontal», «tele»…).
    Start {
        device: String,
        #[arg(long)]
        camera: Option<String>,
    },
    Stop,
    Status,
}

#[derive(Subcommand, Debug)]
pub enum QrAction {
    /// Abre el escáner en el teléfono; lo leído llega al PC con un aviso.
    Scan { device: String },
    /// Códigos leídos (el número es el que usan open/copy/save/forget).
    List,
    /// Abre en el navegador (solo si es una web http/https).
    Open { index: u32 },
    /// Copia al portapapeles del PC.
    Copy { index: u32 },
    /// Guarda el texto en Descargas.
    Save { index: u32 },
    /// Borra uno del historial (`--all`: todos).
    Forget {
        index: Option<i32>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MediaVerb {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RingerVerb {
    Normal,
    Vibrate,
    Silent,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CallVerb {
    Answer,
    Reject,
    Hangup,
    Mute,
    Unmute,
}

fn verb<T: ValueEnum>(v: T) -> String {
    v.to_possible_value()
        .map(|p| p.get_name().to_string())
        .unwrap_or_default()
}

/// Busca un elemento por id exacto o por parte del nombre (sin mayúsculas).
fn pick<'a, T>(
    items: &'a [T],
    query: &str,
    id: impl Fn(&T) -> &str,
    name: impl Fn(&T) -> &str,
) -> anyhow::Result<&'a T> {
    if let Some(item) = items.iter().find(|i| id(i) == query) {
        return Ok(item);
    }
    let q = query.to_lowercase();
    let matches: Vec<&T> = items
        .iter()
        .filter(|i| name(i).to_lowercase().contains(&q) || id(i).to_lowercase().contains(&q))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => anyhow::bail!("ninguno coincide con {query:?}"),
        _ => anyhow::bail!("{query:?} es ambiguo; usa el id"),
    }
}

/// El daemon, si está corriendo (para que otros subcomandos le deleguen).
pub async fn daemon_if_running() -> Option<DaemonProxy<'static>> {
    proxy().await.ok()
}

async fn proxy() -> anyhow::Result<DaemonProxy<'static>> {
    kushare_dbus::connect().await.map_err(|_| {
        anyhow::anyhow!("el daemon de kushare no está corriendo (lánzalo con `kushare`)")
    })
}

pub async fn run(command: LinkCommand) -> anyhow::Result<()> {
    let daemon = proxy().await?;
    match command {
        LinkCommand::Devices => {
            println!(
                "Este equipo: {} ({})",
                daemon.device_name().await?,
                daemon.device_id().await?
            );
            let peers = daemon.peers().await?;
            println!("\nEmparejados:");
            if peers.is_empty() {
                println!("  (ninguno)");
            }
            for (id, name, kind, connected, transport, out, inn, _bt) in peers {
                let state = if connected {
                    format!("conectado por {transport}")
                } else {
                    "desconectado".into()
                };
                println!(
                    "  {name} [{kind}] {id}\n    {state} · enviar: {} · recibir: {}",
                    onoff(out),
                    onoff(inn)
                );
            }
            let discovered = daemon.discovered().await?;
            println!("\nEncontrados en el último escaneo (`kushare scan`):");
            if discovered.is_empty() {
                println!("  (ninguno)");
            }
            for (id, name, addr) in discovered {
                println!(
                    "  {name} {id} ({addr})  →  kushare pair {}",
                    &id[..8.min(id.len())]
                );
            }
        }
        LinkCommand::Quit => {
            daemon.quit().await?;
            println!("Daemon cerrado.");
        }
        LinkCommand::Scan { seconds } => {
            daemon.scan(seconds).await?;
            println!("Buscando durante {seconds} s…");
            let mut seen = std::collections::HashSet::new();
            for _ in 0..seconds {
                tokio::time::sleep(Duration::from_secs(1)).await;
                for (id, name, addr) in daemon.discovered().await? {
                    if seen.insert(id.clone()) {
                        println!(
                            "  {name} {id} ({addr})  →  kushare pair {}",
                            &id[..8.min(id.len())]
                        );
                    }
                }
            }
            if seen.is_empty() {
                println!(
                    "No se encontró ningún dispositivo sin emparejar. Si la red aísla a los clientes,"
                );
                println!(
                    "empareja por Bluetooth: `kushare pair <MAC>` (con el dispositivo ya vinculado)."
                );
            }
        }
        LinkCommand::Pair { device, yes } => pair(&daemon, &device, yes).await?,
        LinkCommand::Unpair { device } => {
            daemon.unpair(&device).await?;
            println!("Desemparejado.");
        }
        LinkCommand::Notif { state, device, way } => {
            for dir in way.directions() {
                daemon
                    .set_direction(&device, dir, matches!(state, Toggle::On))
                    .await?;
            }
            println!("Hecho.");
        }
        LinkCommand::Apps { device } => {
            let (local, remote, out_blocked, in_blocked) = daemon.apps(&device).await?;
            println!("Apps de este equipo (enviar):");
            for (id, name) in local {
                let mark = if out_blocked.contains(&id) {
                    "bloqueada"
                } else {
                    "se envía"
                };
                println!("  {name} ({id}): {mark}");
            }
            println!("\nApps del dispositivo (recibir):");
            for (id, name) in remote {
                let mark = if in_blocked.contains(&id) {
                    "bloqueada"
                } else {
                    "se recibe"
                };
                println!("  {name} ({id}): {mark}");
            }
        }
        LinkCommand::Block { device, way, app } => {
            set_blocked(&daemon, &device, way, &app, true).await?
        }
        LinkCommand::Unblock { device, way, app } => {
            set_blocked(&daemon, &device, way, &app, false).await?
        }
        LinkCommand::Status { device } => {
            let ((has_bat, percent, charging), ringer, call, players, _, outputs, finding) =
                daemon.peer_state(&device).await?;
            if finding {
                println!("Sonando para encontrarlo (`kushare find --stop` lo para).");
            }
            if has_bat {
                let c = if charging { " (cargando)" } else { "" };
                println!("Batería: {percent} %{c}");
            }
            if !ringer.is_empty() {
                let r = match ringer.as_str() {
                    "silent" => "silencio",
                    "vibrate" => "vibración",
                    _ => "sonido",
                };
                println!("Timbre: {r}");
            }
            if call.0 != "idle" {
                let who = if call.2.is_empty() { &call.1 } else { &call.2 };
                println!("Llamada: {} {who}", call.0);
            }
            println!("Reproductores:");
            if players.is_empty() {
                println!("  (ninguno)");
            }
            for p in &players {
                let what = [p.2.as_str(), p.3.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" — ");
                println!("  {} [{}] {what}\n    id: {}", p.1, p.5, p.0);
                // Progreso (extrapolado si está sonando) y carátula.
                let (art, position, duration, at) = &p.10;
                if *duration > 0 {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_millis() as i64);
                    let pos = if p.5 == "playing" {
                        position + (now - at).max(0)
                    } else {
                        *position
                    }
                    .clamp(0, *duration);
                    let fmt = |ms: i64| format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60);
                    println!("    {} / {}", fmt(pos), fmt(*duration));
                }
                if !art.is_empty() {
                    println!("    carátula: {art}");
                }
            }
            println!("Salidas de audio:");
            if outputs.is_empty() {
                println!("  (ninguna)");
            }
            for (id, name, active) in &outputs {
                println!("  {} {name}  ({id})", if *active { "*" } else { " " });
            }
        }
        LinkCommand::Media {
            device,
            action,
            player,
        } => {
            let player_id = match player {
                Some(q) => {
                    let players = daemon.peer_state(&device).await?.3;
                    pick(&players, &q, |p| &p.0, |p| &p.1)?.0.clone()
                }
                None => String::new(),
            };
            daemon
                .media_command(&device, &player_id, &verb(action))
                .await?;
            println!("Hecho.");
        }
        LinkCommand::Audio { device, output } => {
            let (.., selectable, outputs, _) = daemon.peer_state(&device).await?;
            match output {
                None => {
                    for (id, name, active) in &outputs {
                        println!("{} {name}  ({id})", if *active { "*" } else { " " });
                    }
                }
                Some(q) => {
                    if !selectable {
                        anyhow::bail!("ese dispositivo no permite elegir la salida a distancia");
                    }
                    let out = pick(&outputs, &q, |o| &o.0, |o| &o.1)?;
                    daemon.set_audio_output(&device, &out.0).await?;
                    println!("Salida: {}", out.1);
                }
            }
        }
        LinkCommand::Find { device, stop } => {
            daemon.find_phone(&device, !stop).await?;
            println!(
                "{}",
                if stop {
                    "Ya no suena."
                } else {
                    "Sonando: silencíalo en la pantalla del teléfono (o `kushare find --stop`)."
                }
            );
        }
        LinkCommand::Ringer { device, mode } => {
            daemon.set_ringer(&device, &verb(mode)).await?;
            println!("Hecho.");
        }
        LinkCommand::Call { device, action } => {
            daemon.call_command(&device, &verb(action)).await?;
            println!("Hecho.");
        }
        LinkCommand::PhoneAudio { device, state } => {
            let on = matches!(state, Toggle::On);
            let profiles = daemon.phone_audio(&device, on).await?;
            if on {
                println!("Audio del teléfono en el PC: {}", profiles.join(" y "));
            } else {
                println!("Audio del teléfono devuelto al teléfono.");
            }
        }
        LinkCommand::Photos {
            device,
            download,
            offset,
            limit,
            delete,
        } => match (download, delete) {
            (_, Some(id)) => match daemon.delete_photo(&device, &id).await?.as_str() {
                "confirm" => println!("Confírmalo en el teléfono (te llegó un aviso)."),
                _ => println!("Borrada."),
            },
            (Some(id), None) => {
                println!("Descargando…");
                println!("Guardada en {}", daemon.save_photo(&device, &id, "").await?);
            }
            (None, None) => {
                let (total, photos) = daemon.photos(&device, offset, limit).await?;
                println!(
                    "{total} fotos; mostrando {} desde la {offset}:",
                    photos.len()
                );
                for (id, name, _date, w, h, size, _thumb, _orig) in photos {
                    println!("  {id:>8}  {name}  {w}×{h}  {} KB", size / 1024);
                }
                println!("Descargar: kushare photos {device} --download <id>");
            }
        },
        LinkCommand::Sms {
            device,
            thread,
            to,
            text,
            limit,
        } => match (to, thread) {
            (Some(to), _) => {
                let text = text.ok_or_else(|| anyhow::anyhow!("falta el texto del SMS"))?;
                daemon.send_sms(&device, &to, &text).await?;
                println!("Enviado.");
            }
            (None, Some(thread)) => {
                let mut msgs = daemon.sms_messages(&device, &thread, limit, 0).await?;
                msgs.reverse();
                for (_id, _addr, body, date, outgoing, _read, failed) in msgs {
                    let who = if outgoing {
                        "→ enviado"
                    } else {
                        "← recibido"
                    };
                    let mark = if failed { " (falló)" } else { "" };
                    println!("[{}] {who}{mark}: {body}", fmt_date(date));
                }
            }
            (None, None) => {
                let (total, threads) = daemon.sms_threads(&device, 0, limit).await?;
                println!("{total} conversaciones:");
                for (id, address, name, snippet, date, unread, _count, _out) in threads {
                    let who = if name.is_empty() { address } else { name };
                    let dot = if unread { "●" } else { " " };
                    let snippet: String = snippet.chars().take(60).collect();
                    println!("{dot} {id:>6}  {}  {who}: {snippet}", fmt_date(date));
                }
                println!("Leer: kushare sms {device} --thread <id>");
            }
        },
        LinkCommand::Calls { device, limit } => {
            for (number, name, date, duration, kind, _photo) in
                daemon.call_log(&device, limit).await?
            {
                let who = if name.is_empty() {
                    number
                } else {
                    format!("{name} ({number})")
                };
                println!(
                    "{}  {kind:<9} {who}  {}:{:02}",
                    fmt_date(date),
                    duration / 60,
                    duration % 60
                );
            }
        }
        LinkCommand::Dial { device, number } => {
            daemon.dial(&device, &number).await?;
            println!("Llamando desde el teléfono…");
        }
        LinkCommand::Contacts { device, search } => {
            let search = search.map(|s| s.to_lowercase());
            for (name, numbers, starred, _photo) in daemon.contacts(&device, false).await? {
                if search.as_ref().is_some_and(|q| {
                    !name.to_lowercase().contains(q) && !numbers.iter().any(|n| n.contains(q))
                }) {
                    continue;
                }
                let star = if starred { "★" } else { " " };
                println!("{star} {name}: {}", numbers.join(", "));
            }
        }
        LinkCommand::PhoneNotifs { device, dismiss } => match dismiss {
            Some(key) => {
                daemon.dismiss_phone_notification(&device, &key).await?;
                println!("Descartada.");
            }
            None => {
                for (key, _app_id, app, title, text, date, _icon, _actions) in
                    daemon.phone_notifications(&device).await?
                {
                    println!(
                        "[{}] {app} · {title}\n    {text}\n    key: {key}",
                        fmt_date(date)
                    );
                }
            }
        },
        LinkCommand::Webcam { action } => match action {
            WebcamAction::Cameras { device } => {
                for (id, label, _) in daemon.cameras(&device).await? {
                    println!("  {id:>3}  {label}");
                }
            }
            WebcamAction::Start { device, camera } => {
                let camera = match camera {
                    Some(q) => {
                        let cameras = daemon.cameras(&device).await?;
                        pick(&cameras, &q, |c| &c.0, |c| &c.1)?.0.clone()
                    }
                    None => String::new(),
                };
                let path = daemon.webcam_start(&device, &camera).await?;
                println!(
                    "Webcam en {path}: elígela en la app (navegador, OBS, videollamada…). \
                     `kushare webcam stop` la para."
                );
            }
            WebcamAction::Stop => {
                daemon.webcam_stop().await?;
                println!("Webcam parada.");
            }
            WebcamAction::Status => {
                let (active, peer, camera, path, fps, error) = daemon.webcam_status().await?;
                if peer.is_empty() {
                    println!("La webcam no está en marcha.");
                } else {
                    let state = if active { "activa" } else { "arrancando" };
                    let camera = if camera.is_empty() {
                        "por defecto"
                    } else {
                        &camera
                    };
                    println!("Webcam {state} · {path} · cámara {camera} · {fps} fps");
                    if !error.is_empty() {
                        println!("Error del teléfono: {error}");
                    }
                }
            }
        },
        LinkCommand::Qr { action } => match action {
            QrAction::Scan { device } => {
                daemon.qr_scan(&device).await?;
                println!("Escáner abierto en el teléfono: lo que leas llegará aquí con un aviso.");
            }
            QrAction::List => {
                let list = daemon.qr_history().await?;
                if list.is_empty() {
                    println!("Aún no hay códigos (`kushare qr scan <disp>`).");
                }
                for (i, (text, format, date, kind, _)) in list.iter().enumerate() {
                    let short: String = text.chars().take(70).collect();
                    println!(
                        "{i:>3}  [{}] {kind:<7} {}  ({format})",
                        fmt_date(*date),
                        short.replace('\n', " ")
                    );
                }
            }
            QrAction::Open { index } => {
                daemon.qr_open(index).await?;
                println!("Abierto en el navegador.");
            }
            QrAction::Copy { index } => {
                daemon.qr_copy(index).await?;
                println!("Copiado.");
            }
            QrAction::Save { index } => {
                println!("Guardado en {}", daemon.qr_save(index).await?);
            }
            QrAction::Forget { index, all } => {
                let index = if all { -1 } else { index.unwrap_or(-2) };
                if index == -2 {
                    anyhow::bail!("indica el número o --all");
                }
                daemon.qr_forget(index).await?;
                println!("Borrado.");
            }
        },
        LinkCommand::Clipboard { clear, copy } => {
            if clear {
                daemon.clipboard_forget(-1).await?;
                println!("Historial borrado.");
            } else if let Some(i) = copy {
                let history = daemon.clipboard_history().await?;
                let (text, _, _) = history
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("no hay entrada {i}"))?;
                daemon.clipboard_copy(text).await?;
                println!("Copiado.");
            } else {
                let history = daemon.clipboard_history().await?;
                if history.is_empty() {
                    println!(
                        "Historial vacío (actívalo con `kushare setting clipboard-history on`)."
                    );
                }
                for (i, (text, date, source)) in history.iter().enumerate() {
                    let from = if source.is_empty() { "PC" } else { source };
                    let short: String = text.chars().take(70).collect();
                    println!(
                        "{i:>3}  [{}] {from}: {}",
                        fmt_date(*date),
                        short.replace('\n', " ")
                    );
                }
            }
        }
        LinkCommand::Setting { name, state } => match (name, state) {
            (Some(name), Some(state)) => {
                daemon
                    .set_setting(&name, matches!(state, Toggle::On))
                    .await?;
                println!("Hecho.");
            }
            _ => {
                for (name, on) in daemon.settings().await? {
                    println!("  {name:<18} {}", if on { "sí" } else { "no" });
                }
            }
        },
        LinkCommand::PcAudio { output, input } => {
            let (outputs, inputs) = daemon.local_audio().await?;
            let pick = |list: &[(String, String, bool)], q: &str| {
                let q = q.to_lowercase();
                list.iter()
                    .find(|(id, name, _)| *id == q || name.to_lowercase().contains(&q))
                    .map(|(id, _, _)| id.clone())
                    .ok_or_else(|| anyhow::anyhow!("no hay ninguna que coincida con {q:?}"))
            };
            if let Some(q) = output {
                daemon
                    .set_local_audio("output", &pick(&outputs, &q)?)
                    .await?;
                println!("Hecho.");
            } else if let Some(q) = input {
                daemon.set_local_audio("input", &pick(&inputs, &q)?).await?;
                println!("Hecho.");
            } else {
                for (title, list) in [("Salidas", &outputs), ("Entradas", &inputs)] {
                    println!("{title}:");
                    for (_id, name, active) in list {
                        println!("  {} {name}", if *active { "●" } else { " " });
                    }
                }
            }
        }
        LinkCommand::Update { download } => {
            let (current, latest, page, asset, _url) = daemon.check_update(true).await?;
            if !crate::update::is_newer(&latest, &current) {
                println!("Tienes la última versión ({current}).");
            } else if download {
                println!("Descargando kushare {latest}…");
                let path = daemon.download_update().await?;
                println!("Descargado y verificado: {path}");
                if let Some(kind) = crate::update::package_kind()
                    && let Some(cmd) =
                        crate::update::install_command(kind, std::path::Path::new(&path))
                {
                    println!("Instálalo con: sudo {}", cmd[1..].join(" "));
                }
            } else {
                println!("Hay una versión nueva: {latest} (tienes {current}).\n{page}");
                if !asset.is_empty() {
                    println!("Descárgala con: kushare update --download");
                }
            }
        }
    }
    Ok(())
}

/// Fecha local legible ("2026-11-07 14:03") de unos milisegundos UNIX.
fn fmt_date(ms: i64) -> String {
    let secs = ms / 1000;
    // Sin dependencias de zona horaria: `date` hace la conversión local.
    std::process::Command::new("date")
        .args(["-d", &format!("@{secs}"), "+%Y-%m-%d %H:%M"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| secs.to_string())
}

async fn set_blocked(
    daemon: &DaemonProxy<'_>,
    device: &str,
    way: Way,
    app: &str,
    block: bool,
) -> anyhow::Result<()> {
    for dir in way.directions() {
        daemon.set_app_blocked(device, dir, app, block).await?;
    }
    println!("Hecho.");
    Ok(())
}

fn onoff(enabled: bool) -> &'static str {
    if enabled { "sí" } else { "no" }
}

async fn pair(daemon: &DaemonProxy<'_>, device: &str, yes: bool) -> anyhow::Result<()> {
    // Suscribirse ANTES de pedir el emparejado para no perder el PIN.
    let mut requested = daemon.receive_pairing_requested().await?;
    let mut finished = daemon.receive_pairing_finished().await?;
    let target = daemon.pair(device).await?;
    println!("Conectando con {target}… (confirma también en el otro dispositivo)");
    // Por Bluetooth aún no se conoce el device_id: se toma el del primer PIN.
    let mut id = if target.parse::<bluer::Address>().is_ok() {
        None
    } else {
        Some(target)
    };

    let outcome = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            tokio::select! {
                Some(sig) = requested.next() => {
                    let args = sig.args()?;
                    if id.as_ref().is_some_and(|id| *id != args.peer_id) {
                        continue;
                    }
                    let peer = id.get_or_insert_with(|| args.peer_id.clone()).clone();
                    println!("\nPIN: {}   ({})", args.pin, args.name);
                    let accept = yes || ask("¿El otro dispositivo muestra el mismo PIN? [s/N] ").await;
                    if !daemon.respond_pairing(&peer, accept).await? {
                        println!("(ya se respondió desde la notificación)");
                    }
                }
                Some(sig) = finished.next() => {
                    let args = sig.args()?;
                    if id.as_ref().is_none_or(|id| *id == args.peer_id) {
                        return anyhow::Ok(args.accepted);
                    }
                }
                else => anyhow::bail!("se perdió la conexión con el daemon"),
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("tiempo agotado esperando el emparejado"))??;

    if outcome {
        println!("Emparejado.");
        Ok(())
    } else {
        anyhow::bail!("el emparejado no se completó (rechazado o expirado)")
    }
}

async fn ask(question: &str) -> bool {
    print!("{question}");
    let _ = std::io::stdout().flush();
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        matches!(
            line.trim().to_lowercase().as_str(),
            "s" | "si" | "sí" | "y" | "yes"
        )
    })
    .await
    .unwrap_or(false)
}
