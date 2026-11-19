//! Reproductores (MPRIS) y salidas/entradas de audio (`pactl`) de este PC.

use super::*;

// ---------------------------------------------------------------------------
// MPRIS y salidas de audio
// ---------------------------------------------------------------------------

pub(super) async fn local_task(
    features: Arc<Features>,
    mut commands: mpsc::UnboundedReceiver<LocalCommand>,
) {
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
                LocalCommand::SetVolume(set) => {
                    if let Err(err) = pactl_set_volume(&set).await {
                        tracing::info!(?err, id = %set.id, "no se pudo cambiar el volumen");
                    }
                    let _ = dirty_tx.send(Dirty::Audio);
                }
            },
            else => break,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dirty {
    Media,
    Audio,
}

pub(super) async fn watch_mpris(conn: zbus::Connection, dirty: mpsc::UnboundedSender<Dirty>) {
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

pub(super) async fn mpris_names(conn: &zbus::Connection) -> Vec<String> {
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

pub(super) async fn props(
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

pub(super) fn as_string(v: Option<&zbus::zvariant::OwnedValue>) -> String {
    v.and_then(|v| v.downcast_ref::<String>().ok())
        .unwrap_or_default()
}

pub(super) fn as_bool(v: Option<&zbus::zvariant::OwnedValue>) -> bool {
    v.and_then(|v| v.downcast_ref::<bool>().ok())
        .unwrap_or(false)
}

pub(super) async fn mpris_players(conn: &zbus::Connection) -> Vec<pb::MediaPlayer> {
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
            ..Default::default()
        });
    }
    players
}

pub(super) async fn mpris_command(
    conn: &zbus::Connection,
    cmd: &pb::MediaCommand,
) -> zbus::Result<()> {
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
pub(super) fn pactl() -> tokio::process::Command {
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

pub(super) async fn watch_pactl(dirty: mpsc::UnboundedSender<Dirty>) {
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

pub(super) async fn pactl_output(args: &[&str]) -> Option<String> {
    let out = pactl().args(args).output().await.ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

pub(super) async fn pactl_outputs() -> Option<pb::AudioOutputs> {
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
pub(super) fn parse_sinks(listing: &str, default: &str) -> Vec<pb::AudioOutput> {
    parse_devices(listing, default, false)
}

/// Como [`parse_sinks`] para `pactl list sinks` (`input = false`) o
/// `pactl list sources` (`input = true`, sin los "monitor" de las salidas).
pub(super) fn parse_devices(listing: &str, default: &str, input: bool) -> Vec<pb::AudioOutput> {
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
                ..Default::default()
            });
        } else if let Some(desc) = line.strip_prefix("Description: ")
            && let Some(last) = outputs.last_mut()
        {
            last.name = desc.to_string();
        } else if let Some(mute) = line.strip_prefix("Mute: ")
            && let Some(last) = outputs.last_mut()
        {
            last.muted = mute == "yes";
        } else if let Some(volume) = line.strip_prefix("Volume: ")
            && let Some(last) = outputs.last_mut()
            && let Some(percent) = first_percent(volume)
        {
            // "front-left: 65536 /  100% / 0,00 dB,   front-right: …"
            last.volume = percent;
            last.has_volume = true;
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

/// Primer porcentaje de una línea `Volume:` de `pactl list` (el del primer
/// canal).
fn first_percent(volume: &str) -> Option<u32> {
    let end = volume.find('%')?;
    let start = volume[..end]
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    volume[start..end].parse().ok()
}

/// Volumen máximo que se acepta desde el otro lado (el 150 % de pavucontrol).
const MAX_VOLUME: u32 = 150;

/// Pone el volumen y el silencio de una salida (`id` vacío = la activa).
pub(super) async fn pactl_set_volume(set: &pb::SetAudioVolume) -> anyhow::Result<()> {
    let sink = if set.id.is_empty() {
        "@DEFAULT_SINK@"
    } else {
        set.id.as_str()
    };
    let volume = format!("{}%", set.volume.min(MAX_VOLUME));
    let mute = if set.muted { "1" } else { "0" };
    for args in [
        ["set-sink-volume", sink, volume.as_str()],
        ["set-sink-mute", sink, mute],
    ] {
        let status = pactl().args(args).status().await?;
        anyhow::ensure!(status.success(), "pactl {} falló", args[0]);
    }
    Ok(())
}

/// Salidas y entradas (micrófonos) de audio del PC, para la GUI.
pub async fn local_audio() -> (Vec<pb::AudioOutput>, Vec<pb::AudioOutput>) {
    let outputs = pactl_outputs().await.unwrap_or_default().outputs;
    let inputs = pactl_inputs().await;
    (outputs, inputs)
}

pub(super) async fn pactl_inputs() -> Vec<pb::AudioOutput> {
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

pub(super) async fn pactl_set_output(id: &str) -> anyhow::Result<()> {
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
