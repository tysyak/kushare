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
}

/// El daemon, si está corriendo (para que otros subcomandos le deleguen).
pub async fn daemon_if_running() -> Option<DaemonProxy<'static>> {
    proxy().await.ok()
}

async fn proxy() -> anyhow::Result<DaemonProxy<'static>> {
    let connection = zbus::Connection::session().await?;
    // Sin caché de propiedades: si el daemon no corre, zbus avisaría al
    // intentar poblarla; aquí eso es un caso normal, no un error.
    let proxy = DaemonProxy::builder(&connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    // Falla con un mensaje claro si el daemon no está corriendo.
    proxy.device_id().await.map_err(|_| {
        anyhow::anyhow!("el daemon de kushare no está corriendo (lánzalo con `kushare`)")
    })?;
    Ok(proxy)
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
            for (id, name, kind, connected, transport, out, inn) in peers {
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
    }
    Ok(())
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
