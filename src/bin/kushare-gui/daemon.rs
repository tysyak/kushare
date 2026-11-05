//! Comunicación con el daemon (`org.kushare.Daemon`): la GUI no tiene
//! lógica de red propia, todo lo hace el daemon.

use futures_util::{SinkExt, StreamExt};
use iced::Subscription;
use kushare_dbus::DaemonProxy;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub connected: bool,
    pub transport: String,
    pub outgoing: bool,
    pub incoming: bool,
    pub bt_address: String,
}

impl Peer {
    /// "Bluetooth" / "Wi-Fi" según el transporte.
    pub fn via(&self) -> &'static str {
        if self.transport.starts_with("bluetooth") {
            "Bluetooth"
        } else if self.transport.starts_with("wifi") {
            "Wi-Fi"
        } else {
            "?"
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub device_id: String,
    pub link_port: u16,
    pub device_name: String,
    pub peers: Vec<Peer>,
    /// (id, nombre, dirección)
    pub discovered: Vec<(String, String, String)>,
    pub scanning: bool,
}

pub async fn snapshot(daemon: DaemonProxy<'static>) -> Result<Snapshot, String> {
    let e = |err: zbus::Error| err.to_string();
    Ok(Snapshot {
        device_id: daemon.device_id().await.map_err(e)?,
        device_name: daemon.device_name().await.map_err(e)?,
        link_port: daemon.link_port().await.unwrap_or_default(),
        peers: daemon
            .peers()
            .await
            .map_err(e)?
            .into_iter()
            .map(
                |(id, name, kind, connected, transport, outgoing, incoming, bt_address)| Peer {
                    id,
                    name,
                    kind,
                    connected,
                    transport,
                    outgoing,
                    incoming,
                    bt_address,
                },
            )
            .collect(),
        discovered: daemon.discovered().await.map_err(e)?,
        scanning: daemon.scanning().await.map_err(e)?,
    })
}

/// Dispositivos Bluetooth ya vinculados en este equipo (dirección, nombre),
/// candidatos a emparejar el enlace por RFCOMM.
pub async fn bonded_bluetooth() -> Vec<(String, String)> {
    async fn list() -> bluer::Result<Vec<(String, String)>> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        let mut out = Vec::new();
        for address in adapter.device_addresses().await? {
            let device = adapter.device(address)?;
            if device.is_paired().await.unwrap_or(false) {
                let name = device.alias().await.unwrap_or_else(|_| address.to_string());
                out.push((address.to_string(), name));
            }
        }
        out.sort_by_key(|(_, name)| name.to_lowercase());
        Ok(out)
    }
    list().await.unwrap_or_default()
}

#[derive(Debug, Clone)]
pub enum Event {
    Connected(DaemonProxy<'static>),
    Disconnected,
    PeersChanged,
    PairingRequested {
        id: String,
        name: String,
        pin: String,
    },
    PairingFinished {
        id: String,
        name: String,
        accepted: bool,
    },
}

/// Conecta con el daemon (reintentando si no está) y emite sus señales.
pub fn events() -> Subscription<Event> {
    Subscription::run(|| {
        iced::stream::channel(32, async move |mut output| {
            loop {
                let Ok(daemon) = kushare_dbus::connect().await else {
                    let _ = output.send(Event::Disconnected).await;
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                };
                let (Ok(mut requested), Ok(mut finished), Ok(mut changed), Ok(mut owner)) = (
                    daemon.receive_pairing_requested().await,
                    daemon.receive_pairing_finished().await,
                    daemon.receive_peers_changed().await,
                    daemon.inner().receive_owner_changed().await,
                ) else {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                };
                let _ = output.send(Event::Connected(daemon.clone())).await;
                loop {
                    let event = tokio::select! {
                        Some(sig) = requested.next() => match sig.args() {
                            Ok(a) => Event::PairingRequested { id: a.peer_id, name: a.name, pin: a.pin },
                            Err(_) => continue,
                        },
                        Some(sig) = finished.next() => match sig.args() {
                            Ok(a) => Event::PairingFinished { id: a.peer_id, name: a.name, accepted: a.accepted },
                            Err(_) => continue,
                        },
                        Some(_) = changed.next() => Event::PeersChanged,
                        Some(new_owner) = owner.next() => {
                            if new_owner.is_none() {
                                break; // el daemon se cerró
                            }
                            continue;
                        }
                        else => break,
                    };
                    if output.send(event).await.is_err() {
                        return;
                    }
                }
                let _ = output.send(Event::Disconnected).await;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    })
}

/// Ejecuta `kushare doctor` y devuelve su informe.
pub async fn doctor() -> String {
    let out = tokio::process::Command::new(kushare_binary())
        .arg("doctor")
        .output()
        .await;
    match out {
        Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(err) => format!("no se pudo ejecutar `kushare doctor`: {err}"),
    }
}

/// El binario `kushare` junto a este o, si no, el del PATH.
fn kushare_binary() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("kushare")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| "kushare".into())
}

/// Arranca el daemon (`kushare`) en segundo plano: el binario junto a este
/// o, si no, el del PATH.
pub fn start_daemon() -> Result<(), String> {
    std::process::Command::new(kushare_binary())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("no se pudo arrancar el daemon: {e}"))
}
