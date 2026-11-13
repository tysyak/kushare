//! API D-Bus del daemon (`org.kushare.Daemon` en el bus de sesión), al
//! estilo de `kdeconnectd`: la CLI (`kushare devices`, `kushare pair`…) y
//! cualquier otra UI la usan para controlar el enlace del daemon en marcha.
//! Poseer el nombre también impide arrancar dos daemons a la vez.

use kushare_link::{Direction, LinkEvent};
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface};

use super::LinkHandle;

pub use kushare_dbus::{AppsReply, BUS_NAME, DaemonProxy, OBJECT_PATH, PeerRow};
use kushare_dbus::{
    CallLogRow, ClipRow, ContactRow, OutputRow, PeerStateReply, PhoneNotificationRow, PhotoRow,
    SmsRow, SmsThreadRow, UpdateRow,
};
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

impl DaemonApi {
    /// Resuelve `peer` y comprueba que esté conectado.
    fn connected(&self, peer: &str) -> fdo::Result<String> {
        let id = self.link.resolve(peer).map_err(failed)?;
        if !self.link.manager.is_connected(&id) {
            return Err(fdo::Error::Failed(
                "el dispositivo no está conectado".into(),
            ));
        }
        Ok(id)
    }
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
                // Modo híbrido: "bluetooth (…) + datos wifi (…)" con el
                // enlace de datos abierto, o "… + datos wifi disponible".
                let mut transport = p.transport.unwrap_or_default();
                if let Some(bulk) = &p.bulk_transport {
                    transport = format!("{transport} + datos {bulk}");
                } else if p.connected
                    && self.link.features.settings.get().hybrid
                    && self.link.features.lan_ready(&p.device_id)
                {
                    transport = format!("{transport} + datos wifi listo");
                }
                (
                    p.device_id,
                    p.device_name,
                    p.device_kind,
                    p.connected,
                    transport,
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
        Ok((total, photos.into_iter().map(photo_row).collect()))
    }

    fn cached_photos(&self, peer: &str) -> fdo::Result<(u32, Vec<PhotoRow>)> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let (total, photos) = self.link.features.cached_photos(&id);
        Ok((total, photos.into_iter().map(photo_row).collect()))
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

    async fn save_photo(&self, peer: &str, id: &str, dest: &str) -> fdo::Result<String> {
        let peer_id = self.link.resolve(peer).map_err(failed)?;
        let features = &self.link.features;
        let path = if dest.is_empty() {
            features.save_photo_to_downloads(&peer_id, id).await
        } else {
            let cached = features.fetch_photo(&peer_id, id).await.map_err(failed)?;
            let mut target = std::path::PathBuf::from(dest);
            if target.is_dir() {
                target.push(super::features::original_name(&cached));
            }
            std::fs::copy(&cached, &target)
                .map(|_| target)
                .map_err(Into::into)
        }
        .map_err(failed)?;
        Ok(path.to_string_lossy().to_string())
    }

    async fn delete_photo(&self, peer: &str, id: &str) -> fdo::Result<String> {
        let peer_id = self.link.resolve(peer).map_err(failed)?;
        self.link
            .features
            .delete_photo(&peer_id, id)
            .await
            .map_err(failed)
    }

    async fn sms_threads(
        &self,
        peer: &str,
        offset: u32,
        limit: u32,
    ) -> fdo::Result<(u32, Vec<SmsThreadRow>)> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let t = self
            .link
            .features
            .sms_threads(&id, offset, limit)
            .await
            .map_err(failed)?;
        Ok((
            t.total,
            t.threads
                .into_iter()
                .map(|t| {
                    (
                        t.thread_id,
                        t.address,
                        t.name,
                        t.snippet,
                        t.date_ms,
                        t.unread,
                        t.count,
                        t.outgoing,
                    )
                })
                .collect(),
        ))
    }

    async fn sms_messages(
        &self,
        peer: &str,
        thread_id: &str,
        limit: u32,
        before_ms: i64,
    ) -> fdo::Result<Vec<SmsRow>> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let msgs = self
            .link
            .features
            .sms_messages(&id, thread_id, limit, before_ms)
            .await
            .map_err(failed)?;
        Ok(msgs
            .into_iter()
            .map(|m| {
                (
                    m.id, m.address, m.body, m.date_ms, m.outgoing, m.read, m.failed,
                )
            })
            .collect())
    }

    async fn send_sms(&self, peer: &str, address: &str, body: &str) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        self.link
            .features
            .send_sms(&id, address, body)
            .await
            .map_err(failed)
    }

    async fn call_log(&self, peer: &str, limit: u32) -> fdo::Result<Vec<CallLogRow>> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let entries = self
            .link
            .features
            .call_log(&id, limit)
            .await
            .map_err(failed)?;
        let photos = self
            .link
            .features
            .contact_photos(&id, entries.iter().map(|e| e.photo_key.clone()))
            .await;
        Ok(entries
            .into_iter()
            .map(|e| {
                let kind = match e.r#type() {
                    pb::CallType::Incoming => "incoming",
                    pb::CallType::Outgoing => "outgoing",
                    pb::CallType::Missed => "missed",
                    pb::CallType::Rejected => "rejected",
                    pb::CallType::Blocked => "blocked",
                    pb::CallType::Voicemail => "voicemail",
                };
                let photo = photos
                    .get(&e.photo_key)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (
                    e.number,
                    e.name,
                    e.date_ms,
                    e.duration_s,
                    kind.to_string(),
                    photo,
                )
            })
            .collect())
    }

    async fn contacts(&self, peer: &str, refresh: bool) -> fdo::Result<Vec<ContactRow>> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let features = &self.link.features;
        let list = features.contacts(&id, refresh).await.map_err(failed)?;
        Ok(list
            .into_iter()
            .map(|c| {
                let photo = features
                    .cached_contact_photo(&id, &c.photo_key)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (c.name, c.numbers, c.starred, photo)
            })
            .collect())
    }

    async fn dial(&self, peer: &str, number: &str) -> fdo::Result<()> {
        let id = self.link.resolve(peer).map_err(failed)?;
        self.link.features.dial(&id, number).await.map_err(failed)
    }

    async fn phone_notifications(&self, peer: &str) -> fdo::Result<Vec<PhoneNotificationRow>> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let list = self
            .link
            .features
            .phone_notifications(&id)
            .await
            .map_err(failed)?;
        Ok(list
            .into_iter()
            .map(|(n, icon)| {
                (
                    n.key,
                    n.app_id,
                    n.app_name,
                    n.title,
                    n.text,
                    n.timestamp_ms,
                    icon.map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    n.actions
                        .into_iter()
                        .map(|a| (a.id, a.label, a.is_reply))
                        .collect(),
                )
            })
            .collect())
    }

    fn dismiss_phone_notification(&self, peer: &str, key: &str) -> fdo::Result<()> {
        let id = self.connected(peer)?;
        self.link.manager.dismiss_remote_notification(&id, key);
        Ok(())
    }

    fn phone_notification_action(&self, peer: &str, key: &str, action_id: &str) -> fdo::Result<()> {
        let id = self.connected(peer)?;
        self.link.manager.invoke_remote_action(&id, key, action_id);
        Ok(())
    }

    fn phone_notification_reply(
        &self,
        peer: &str,
        key: &str,
        action_id: &str,
        text: &str,
    ) -> fdo::Result<()> {
        let id = self.connected(peer)?;
        self.link.manager.reply_remote(&id, key, action_id, text);
        Ok(())
    }

    async fn wallpaper(&self, peer: &str, refresh: bool) -> fdo::Result<(String, Vec<u32>)> {
        let id = self.link.resolve(peer).map_err(failed)?;
        let w = self
            .link
            .features
            .wallpaper(&id, refresh)
            .await
            .map_err(failed)?;
        Ok((
            w.path
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            w.colors,
        ))
    }

    async fn local_audio(&self) -> (Vec<OutputRow>, Vec<OutputRow>) {
        let (outputs, inputs) = super::features::local_audio().await;
        let rows = |v: Vec<pb::AudioOutput>| -> Vec<OutputRow> {
            v.into_iter().map(|o| (o.id, o.name, o.active)).collect()
        };
        (rows(outputs), rows(inputs))
    }

    async fn set_local_audio(&self, kind: &str, id: &str) -> fdo::Result<()> {
        match kind {
            "output" => super::features::set_local_output(id).await,
            "input" => super::features::set_local_input(id).await,
            other => {
                return Err(fdo::Error::InvalidArgs(format!(
                    "tipo {other:?} inválido: output o input"
                )));
            }
        }
        .map_err(failed)
    }

    fn clipboard_history(&self) -> Vec<ClipRow> {
        self.link
            .features
            .clipboard
            .history()
            .into_iter()
            .map(|e| (e.text, e.timestamp_ms, e.source))
            .collect()
    }

    fn clipboard_copy(&self, text: &str) -> fdo::Result<()> {
        self.link.features.clipboard.copy_text(text).map_err(failed)
    }

    fn clipboard_copy_file(&self, path: &str) -> fdo::Result<()> {
        self.link
            .features
            .clipboard
            .copy_file(std::path::Path::new(path))
            .map_err(failed)
    }

    fn clipboard_forget(&self, index: i32) {
        self.link
            .features
            .clipboard
            .forget(usize::try_from(index).ok());
    }

    fn settings(&self) -> Vec<(String, bool)> {
        let s = self.link.features.settings.get();
        super::settings::NAMES
            .iter()
            .map(|n| (n.to_string(), s.get(n).unwrap_or_default()))
            .collect()
    }

    fn set_setting(&self, name: &str, enabled: bool) -> fdo::Result<()> {
        self.link
            .features
            .settings
            .set(name, enabled)
            .map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        if name == "clipboard-history" && !enabled {
            self.link.features.clipboard.history_disabled();
        }
        Ok(())
    }

    async fn check_update(&self, force: bool) -> fdo::Result<UpdateRow> {
        let release = self.link.updates.check(force).await.map_err(failed)?;
        let kind = tokio::task::spawn_blocking(crate::update::package_kind)
            .await
            .ok()
            .flatten();
        let (asset_name, asset_url) = kind
            .and_then(|k| crate::update::pick_asset(&release, k))
            .cloned()
            .unwrap_or_default();
        Ok((
            crate::update::CURRENT.to_string(),
            release.version,
            release.page,
            asset_name,
            asset_url,
        ))
    }

    async fn download_update(&self) -> fdo::Result<String> {
        let release = self.link.updates.check(false).await.map_err(failed)?;
        if !crate::update::is_newer(&release.version, crate::update::CURRENT) {
            return Err(fdo::Error::Failed("ya tienes la última versión".into()));
        }
        let path = tokio::task::spawn_blocking(move || {
            let kind = crate::update::package_kind()
                .ok_or_else(|| anyhow::anyhow!("kushare no está instalado desde un paquete"))?;
            let asset = crate::update::pick_asset(&release, kind)
                .ok_or_else(|| anyhow::anyhow!("el release no trae paquete para este sistema"))?
                .clone();
            let dir = directories::UserDirs::new()
                .and_then(|d| d.download_dir().map(std::path::Path::to_path_buf))
                .unwrap_or_else(std::env::temp_dir);
            crate::update::download(&release, &asset, &dir)
        })
        .await
        .map_err(|e| fdo::Error::Failed(e.to_string()))?
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

    #[zbus(signal)]
    async fn sms_changed(
        emitter: &SignalEmitter<'_>,
        peer_id: &str,
        thread_id: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn clipboard_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn phone_notifications_changed(
        emitter: &SignalEmitter<'_>,
        peer_id: &str,
    ) -> zbus::Result<()>;

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
    let clipboard_changes = link.features.clipboard.subscribe();
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
    {
        let iface = iface.clone();
        let mut changes = clipboard_changes;
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let _ = DaemonApi::clipboard_changed(iface.signal_emitter()).await;
            }
        });
    }
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
                LinkEvent::NotificationPosted { device_id, .. }
                | LinkEvent::NotificationRemoved { device_id, .. } => {
                    DaemonApi::phone_notifications_changed(emitter, &device_id).await
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
                        Some(F::SmsChanged(c)) => {
                            DaemonApi::sms_changed(emitter, &device_id, &c.thread_id).await
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

fn photo_row(p: super::features::PhotoEntry) -> PhotoRow {
    (
        p.info.id,
        p.info.name,
        p.info.date_ms,
        p.info.width,
        p.info.height,
        p.info.size,
        p.thumb.to_string_lossy().to_string(),
        p.original
            .map(|o| o.to_string_lossy().to_string())
            .unwrap_or_default(),
    )
}
