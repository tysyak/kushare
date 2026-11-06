//! API D-Bus del daemon (`org.kushare.Daemon` en el bus de sesión), al
//! estilo de `kdeconnectd`: la CLI (`kushare devices`, `kushare pair`…) y
//! cualquier otra UI la usan para controlar el enlace del daemon en marcha.
//! Poseer el nombre también impide arrancar dos daemons a la vez.

use kushare_link::{Direction, LinkEvent};
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface};

use super::LinkHandle;

pub use kushare_dbus::{AppsReply, BUS_NAME, DaemonProxy, OBJECT_PATH, PeerRow};
use kushare_dbus::{PeerStateReply, PhotoRow};
use kushare_link::pb;

pub fn parse_direction(direction: &str) -> fdo::Result<Direction> {
    match direction {
        "out" | "saliente" | "enviar" => Ok(Direction::Outgoing),
        "in" | "entrante" | "recibir" => Ok(Direction::Incoming),
        other => Err(fdo::Error::InvalidArgs(format!(
            "dirección {other:?} inválida: usa \"in\" (recibir) u \"out\" (enviar)"
        ))),
    }
}

pub fn parse_media_action(action: &str) -> fdo::Result<pb::MediaAction> {
    Ok(match action {
        "play-pause" | "toggle" => pb::MediaAction::MediaPlayPause,
        "play" => pb::MediaAction::MediaPlay,
        "pause" => pb::MediaAction::MediaPause,
        "next" => pb::MediaAction::MediaNext,
        "previous" | "prev" => pb::MediaAction::MediaPrevious,
        "stop" => pb::MediaAction::MediaStop,
        other => {
            return Err(fdo::Error::InvalidArgs(format!(
                "acción {other:?} inválida: play-pause, play, pause, next, previous o stop"
            )));
        }
    })
}

fn peer_state_reply(state: &super::features::PeerState) -> PeerStateReply {
    let battery = state
        .battery
        .as_ref()
        .filter(|b| b.present)
        .map_or((false, 0, false), |b| (true, b.percent, b.charging));
    let ringer = match state.ringer {
        None => "",
        Some(pb::Ringer::Normal) => "normal",
        Some(pb::Ringer::Vibrate) => "vibrate",
        Some(pb::Ringer::Silent) => "silent",
    };
    let call = state.call.as_ref().map_or_else(
        || ("idle".to_string(), String::new(), String::new(), false),
        |c| {
            let phase = match c.phase() {
                pb::CallPhase::CallIdle => "idle",
                pb::CallPhase::CallRinging => "ringing",
                pb::CallPhase::CallActive => "active",
                pb::CallPhase::CallDialing => "dialing",
            };
            (phase.to_string(), c.number.clone(), c.name.clone(), c.muted)
        },
    );
    let players = state
        .players
        .iter()
        .map(|p| {
            let st = match p.state() {
                pb::PlaybackState::PlaybackPlaying => "playing",
                pb::PlaybackState::PlaybackPaused => "paused",
                pb::PlaybackState::PlaybackStopped => "stopped",
            };
            (
                p.id.clone(),
                p.name.clone(),
                p.title.clone(),
                p.artist.clone(),
                p.album.clone(),
                st.to_string(),
                p.can_play,
                p.can_pause,
                p.can_next,
                p.can_previous,
            )
        })
        .collect();
    let (selectable, outputs) = state.outputs.as_ref().map_or((false, Vec::new()), |o| {
        (
            o.selectable,
            o.outputs
                .iter()
                .map(|o| (o.id.clone(), o.name.clone(), o.active))
                .collect(),
        )
    });
    (
        battery,
        ringer.to_string(),
        call,
        players,
        selectable,
        outputs,
    )
}

fn failed(err: anyhow::Error) -> fdo::Error {
    fdo::Error::Failed(format!("{err:#}"))
}

struct DaemonApi {
    link: LinkHandle,
}

#[interface(name = "org.kushare.Daemon")]
impl DaemonApi {
    #[zbus(property)]
    fn device_id(&self) -> String {
        self.link.manager.device_id().to_string()
    }

    #[zbus(property)]
    fn device_name(&self) -> String {
        self.link.manager.device_name().to_string()
    }

    /// Puerto TCP en el que escucha el enlace.
    #[zbus(property)]
    fn link_port(&self) -> u16 {
        *self.link.link_port.lock().expect("mutex envenenado")
    }

    fn peers(&self) -> Vec<PeerRow> {
        self.link
            .manager
            .peers()
            .into_iter()
            .map(|p| {
                (
                    p.device_id,
                    p.device_name,
                    p.device_kind,
                    p.connected,
                    p.transport.unwrap_or_default(),
                    p.outgoing_enabled,
                    p.incoming_enabled,
                    p.bt_address.unwrap_or_default(),
                )
            })
            .collect()
    }

    /// Envía `path` por Bluetooth RFCOMM a `address` (dispositivo ya
    /// vinculado) con el perfil de transferencias del daemon. Vuelve en
    /// cuanto empieza; el resultado se avisa con una notificación.
    fn send_file_bt(&self, path: &str, address: &str) -> fdo::Result<()> {
        let address: bluer::Address = address.parse().map_err(|_| {
            fdo::Error::InvalidArgs(format!("{address:?} no es una dirección Bluetooth"))
        })?;
        self.link
            .send_file_bt(std::path::PathBuf::from(path), address)
            .map_err(failed)
    }

    /// Cierra el daemon (como "Salir" en el tray).
    fn quit(&self) {
        tracing::info!("cierre pedido por D-Bus");
        tokio::spawn(async {
            // Dejar que la respuesta D-Bus salga antes de terminar.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            std::process::exit(0);
        });
    }

    /// Busca dispositivos en la red durante `seconds` (no hay escaneo
    /// continuo). Los resultados se leen con `Discovered`.
    fn scan(&self, seconds: u32) -> fdo::Result<()> {
        let seconds = seconds.clamp(1, 300);
        self.link
            .scan(std::time::Duration::from_secs(u64::from(seconds)))
            .map_err(failed)
    }

    #[zbus(property)]
    fn scanning(&self) -> bool {
        self.link.is_scanning()
    }

    /// Dispositivos visibles por mDNS aún no emparejados: (id, nombre, dirección).
    fn discovered(&self) -> Vec<(String, String, String)> {
        self.link
            .discovered_unpaired()
            .into_iter()
            .map(|s| (s.device_id, s.device_name, s.addr.to_string()))
            .collect()
    }

    /// Devuelve el device_id, o la dirección Bluetooth si se empareja por
    /// RFCOMM (el device_id se conoce al llegar el PIN).
    fn pair(&self, peer: &str) -> fdo::Result<String> {
        if peer.trim().parse::<bluer::Address>().is_ok() {
            self.link.pair(peer).map_err(failed)?;
            return Ok(peer.trim().to_string());
        }
        let id = self.link.resolve(peer).map_err(failed)?;
        self.link.pair(&id).map_err(failed)?;
        Ok(id)
    }

    fn respond_pairing(&self, peer: &str, accept: bool) -> fdo::Result<bool> {
        let id = self.link.resolve(peer).map_err(failed)?;
        Ok(self.link.manager.respond_pairing(&id, accept))
    }

    fn unpair(&self, peer: &str) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        self.link.manager.unpair(&id).map_err(failed)
    }

    fn set_direction(&self, peer: &str, direction: &str, enabled: bool) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        self.link
            .manager
            .set_direction(&id, parse_direction(direction)?, enabled)
            .map_err(failed)
    }

    fn set_app_blocked(
        &self,
        peer: &str,
        direction: &str,
        app_id: &str,
        blocked: bool,
    ) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        self.link
            .manager
            .set_app_blocked(&id, parse_direction(direction)?, app_id, blocked)
            .map_err(failed)
    }

    fn apps(&self, peer: &str) -> fdo::Result<AppsReply> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let info = self
            .link
            .manager
            .peers()
            .into_iter()
            .find(|p| p.device_id == id)
            .ok_or_else(|| fdo::Error::Failed(format!("{id} no está emparejado")))?;
        Ok((
            self.link.manager.local_apps(),
            info.peer_apps,
            info.outgoing_blocked,
            info.incoming_blocked,
        ))
    }

    fn peer_state(&self, peer: &str) -> fdo::Result<PeerStateReply> {
        let id = self.link.resolve(peer).map_err(failed)?;
        Ok(peer_state_reply(&self.link.features.peer_state(&id)))
    }

    fn media_command(&self, peer: &str, player: &str, action: &str) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let action = parse_media_action(action)?;
        self.link
            .features
            .media_command(&id, player, action)
            .map_err(failed)
    }

    fn set_audio_output(&self, peer: &str, output: &str) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        self.link
            .features
            .set_audio_output(&id, output)
            .map_err(failed)
    }

    fn set_ringer(&self, peer: &str, mode: &str) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let mode = match mode {
            "normal" | "sonido" => pb::Ringer::Normal,
            "vibrate" | "vibrar" | "vibración" => pb::Ringer::Vibrate,
            "silent" | "silencio" => pb::Ringer::Silent,
            other => {
                return Err(fdo::Error::InvalidArgs(format!(
                    "modo {other:?} inválido: normal, vibrate o silent"
                )));
            }
        };
        self.link.features.set_ringer(&id, mode).map_err(failed)
    }

    fn call_command(&self, peer: &str, action: &str) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let action = match action {
            "answer" | "contestar" => pb::CallAction::CallAnswer,
            "reject" | "rechazar" => pb::CallAction::CallReject,
            "hangup" | "colgar" => pb::CallAction::CallHangup,
            "mute" | "silenciar" => pb::CallAction::CallMute,
            "unmute" => pb::CallAction::CallUnmute,
            other => {
                return Err(fdo::Error::InvalidArgs(format!(
                    "acción {other:?} inválida: answer, reject, hangup, mute o unmute"
                )));
            }
        };
        self.link.features.call_command(&id, action).map_err(failed)
    }

    async fn photos(
        &self,
        peer: &str,
        offset: u32,
        limit: u32,
    ) -> fdo::Result<(u32, Vec<PhotoRow>)> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let (total, photos) = self
            .link
            .features
            .photos(&id, offset, limit)
            .await
            .map_err(failed)?;
        Ok((
            total,
            photos
                .into_iter()
                .map(|(p, thumb)| {
                    (
                        p.id,
                        p.name,
                        p.date_ms,
                        p.width,
                        p.height,
                        p.size,
                        thumb.to_string_lossy().to_string(),
                    )
                })
                .collect(),
        ))
    }

    async fn fetch_photo(&self, peer: &str, id: &str) -> fdo::Result<String> {
        let peer_id = self.link.resolve(peer).map_err(failed)?;
        let path = self
            .link
            .features
            .fetch_photo(&peer_id, id)
            .await
            .map_err(failed)?;
        Ok(path.to_string_lossy().to_string())
    }

    /// Conecta/desconecta el audio Bluetooth del teléfono (A2DP + HFP) con
    /// el PC: su música y sus llamadas se oyen aquí. Devuelve los perfiles
    /// que quedaron conectados/desconectados ("multimedia", "llamadas").
    async fn phone_audio(&self, peer: &str, enable: bool) -> fdo::Result<Vec<String>> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let address = self
            .link
            .manager
            .peers()
            .into_iter()
            .find(|p| p.device_id == id)
            .and_then(|p| p.bt_address)
            .ok_or_else(|| {
                fdo::Error::Failed(
                    "no se conoce la dirección Bluetooth del teléfono (emparéjalo por Bluetooth)"
                        .into(),
                )
            })?;
        super::features::phone_audio(&address, enable)
            .await
            .map(|v| v.into_iter().map(str::to_string).collect())
            .map_err(failed)
    }

    /// La galería del peer tiene fotos nuevas.
    #[zbus(signal)]
    async fn photos_changed(emitter: &SignalEmitter<'_>, peer_id: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn peer_state_changed(emitter: &SignalEmitter<'_>, peer_id: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn pairing_requested(
        emitter: &SignalEmitter<'_>,
        peer_id: &str,
        name: &str,
        pin: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn pairing_finished(
        emitter: &SignalEmitter<'_>,
        peer_id: &str,
        name: &str,
        accepted: bool,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn peers_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// Publica la API en el bus de sesión. Falla si ya hay otro daemon.
pub async fn serve(link: LinkHandle) -> anyhow::Result<zbus::Connection> {
    let mut events = link.events.subscribe();
    let connection = zbus::connection::Builder::session()?
        .serve_at(OBJECT_PATH, DaemonApi { link })?
        .build()
        .await?;
    connection
        .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await
        .map_err(|e| anyhow::anyhow!("¿ya hay otro daemon de kushare corriendo? ({e})"))?;

    let iface = connection
        .object_server()
        .interface::<_, DaemonApi>(OBJECT_PATH)
        .await?;
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let emitter = iface.signal_emitter();
            let result = match event {
                LinkEvent::PairingRequested {
                    device_id,
                    device_name,
                    pin,
                    ..
                } => DaemonApi::pairing_requested(emitter, &device_id, &device_name, &pin).await,
                LinkEvent::PairingFinished {
                    device_id,
                    device_name,
                    accepted,
                } => DaemonApi::pairing_finished(emitter, &device_id, &device_name, accepted).await,
                LinkEvent::PeerConnected { .. }
                | LinkEvent::PeerDisconnected { .. }
                | LinkEvent::PeerUnpaired { .. }
                | LinkEvent::ConfigChanged { .. } => {
                    let peer = match &event {
                        LinkEvent::PeerConnected { device_id, .. }
                        | LinkEvent::PeerDisconnected { device_id } => Some(device_id.clone()),
                        _ => None,
                    };
                    let result = DaemonApi::peers_changed(emitter).await;
                    if let Some(peer) = peer {
                        let _ = DaemonApi::peer_state_changed(emitter, &peer).await;
                    }
                    result
                }
                LinkEvent::Feature { device_id, feature } => {
                    use pb::feature::Kind as F;
                    match feature.kind {
                        Some(
                            F::Battery(_)
                            | F::MediaPlayers(_)
                            | F::AudioOutputs(_)
                            | F::RingerMode(_)
                            | F::CallState(_),
                        ) => DaemonApi::peer_state_changed(emitter, &device_id).await,
                        Some(F::PhotosChanged(_)) => {
                            DaemonApi::photos_changed(emitter, &device_id).await
                        }
                        _ => Ok(()),
                    }
                }
                _ => Ok(()),
            };
            if let Err(err) = result {
                tracing::debug!(?err, "no se pudo emitir la señal D-Bus");
            }
        }
    });
    Ok(connection)
}
