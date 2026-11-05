//! API D-Bus del daemon (`org.kushare.Daemon` en el bus de sesión), al
//! estilo de `kdeconnectd`: la CLI (`kushare devices`, `kushare pair`…) y
//! cualquier otra UI la usan para controlar el enlace del daemon en marcha.
//! Poseer el nombre también impide arrancar dos daemons a la vez.

use kushare_link::{Direction, LinkEvent};
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface};

use super::LinkHandle;

pub const BUS_NAME: &str = "org.kushare.Daemon";
pub const OBJECT_PATH: &str = "/org/kushare/Daemon";

/// (id, nombre, tipo, conectado, transporte, saliente, entrante)
pub type PeerRow = (String, String, String, bool, String, bool, bool);
/// (apps locales, apps del peer, bloqueadas al salir, bloqueadas al entrar)
pub type AppsReply = (
    Vec<(String, String)>,
    Vec<(String, String)>,
    Vec<String>,
    Vec<String>,
);

pub fn parse_direction(direction: &str) -> fdo::Result<Direction> {
    match direction {
        "out" | "saliente" | "enviar" => Ok(Direction::Outgoing),
        "in" | "entrante" | "recibir" => Ok(Direction::Incoming),
        other => Err(fdo::Error::InvalidArgs(format!(
            "dirección {other:?} inválida: usa \"in\" (recibir) u \"out\" (enviar)"
        ))),
    }
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
                | LinkEvent::ConfigChanged { .. } => DaemonApi::peers_changed(emitter).await,
                _ => Ok(()),
            };
            if let Err(err) = result {
                tracing::debug!(?err, "no se pudo emitir la señal D-Bus");
            }
        }
    });
    Ok(connection)
}

/// Proxy cliente de la misma interfaz, para la CLI.
#[zbus::proxy(
    interface = "org.kushare.Daemon",
    default_service = "org.kushare.Daemon",
    default_path = "/org/kushare/Daemon"
)]
pub trait Daemon {
    #[zbus(property)]
    fn device_id(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn device_name(&self) -> zbus::Result<String>;
    fn peers(&self) -> zbus::Result<Vec<PeerRow>>;
    fn discovered(&self) -> zbus::Result<Vec<(String, String, String)>>;
    fn scan(&self, seconds: u32) -> zbus::Result<()>;
    fn send_file_bt(&self, path: &str, address: &str) -> zbus::Result<()>;
    fn quit(&self) -> zbus::Result<()>;
    fn pair(&self, peer: &str) -> zbus::Result<String>;
    fn respond_pairing(&self, peer: &str, accept: bool) -> zbus::Result<bool>;
    fn unpair(&self, peer: &str) -> zbus::Result<()>;
    fn set_direction(&self, peer: &str, direction: &str, enabled: bool) -> zbus::Result<()>;
    fn set_app_blocked(
        &self,
        peer: &str,
        direction: &str,
        app_id: &str,
        blocked: bool,
    ) -> zbus::Result<()>;
    fn apps(&self, peer: &str) -> zbus::Result<AppsReply>;
    #[zbus(signal)]
    fn pairing_requested(&self, peer_id: String, name: String, pin: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn pairing_finished(&self, peer_id: String, name: String, accepted: bool) -> zbus::Result<()>;
}
