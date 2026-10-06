use clap::{Parser, Subcommand};
use kushare_core::{Settings, TransferEvent, random_endpoint_id};
use kushare_discovery::ble::{BleAdvertiser, BleScanner};
use kushare_discovery::mdns::{MdnsAdvertiser, MdnsScanner, discovered_device_from_event};
use kushare_transfer::{Receiver, Sender};
use kushare_tray::{Notifier, Tray, TrayCommand, tray::tooltip_for_event};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tray_icon::Icon;

/// Cliente nativo de Quick Share (Nearby Share) para Linux.
#[derive(Parser, Debug)]
#[command(name = "kushare", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Nombre de este dispositivo tal como lo verán otros. Por defecto se
    /// deriva del hostname.
    #[arg(long, global = true)]
    device_name: Option<String>,

    /// Puerto TCP en el que escuchar transferencias entrantes. Por defecto
    /// el sistema operativo asigna uno libre. Solo aplica al modo por
    /// defecto (tray + recepción), no a `send`.
    #[arg(long, default_value_t = 0)]
    port: u16,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Envía un archivo directamente a host:puerto, sin tray ni
    /// descubrimiento mDNS. Útil para probar TX/RX entre dos instancias en
    /// la misma máquina o red sin depender de la interfaz gráfica.
    Send {
        /// Archivo a enviar.
        file: PathBuf,
        /// Dirección del receptor, p. ej. 127.0.0.1:45678 (el puerto real
        /// aparece en el log de la instancia receptora al arrancar).
        addr: SocketAddr,
    },
}

/// Icono de bandeja mínimo (un cuadrado sólido). Placeholder hasta que el
/// proyecto tenga arte propio; `packaging/common/kushare.svg` es el icono
/// usado por el `.desktop` y los paquetes del sistema, que sí es un diseño
/// real, aunque simple.
fn placeholder_icon() -> anyhow::Result<Icon> {
    const SIZE: u32 = 32;
    // Azul sólido (0x2E, 0x6F, 0xE3) totalmente opaco.
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[0x2E, 0x6F, 0xE3, 0xFF]);
    }
    Ok(Icon::from_rgba(rgba, SIZE, SIZE)?)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let mut settings = Settings::default();
    if let Some(name) = cli.device_name {
        settings.device_name = name;
    }
    settings.listen_port = cli.port;

    // El icono de bandeja necesita crearse en el hilo principal en algunas
    // plataformas; con el backend `ksni` de `tray-icon` (ver
    // `kushare_tray::tray`) esto ya no exige un runtime GTK, así que basta
    // con un runtime tokio normal de varios hilos.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match cli.command {
        Some(Command::Send { file, addr }) => {
            let endpoint_id = random_endpoint_id();
            runtime.block_on(run_send(settings, endpoint_id, file, addr))
        }
        None => runtime.block_on(run(settings)),
    }
}

/// Envío directo sin tray ni mDNS: conecta, transfiere el archivo e imprime
/// el progreso por stdout. Pensado para probar TX/RX de punta a punta desde
/// la terminal, en paralelo con una instancia normal (`kushare`) corriendo
/// como receptora.
async fn run_send(
    settings: Settings,
    endpoint_id: String,
    file: PathBuf,
    addr: SocketAddr,
) -> anyhow::Result<()> {
    println!("Conectando a {addr}...");
    let mut session = Sender::connect(addr).await?;

    let (events_tx, mut events_rx) = mpsc::channel::<TransferEvent>(16);
    let file_for_task = file.clone();
    let device_name = settings.device_name.clone();
    let send_task = tokio::spawn(async move {
        Sender::send_file(
            &mut session,
            &endpoint_id,
            &device_name,
            &file_for_task,
            events_tx,
        )
        .await
    });

    while let Some(event) = events_rx.recv().await {
        match event {
            TransferEvent::Progress {
                bytes_transferred,
                total_bytes,
                ..
            } => {
                let pct = if total_bytes > 0 {
                    (bytes_transferred as f64 / total_bytes as f64) * 100.0
                } else {
                    0.0
                };
                println!("  {bytes_transferred}/{total_bytes} bytes ({pct:.0}%)");
            }
            TransferEvent::Failed { reason, .. } => {
                anyhow::bail!("transferencia fallida: {reason}");
            }
            _ => {}
        }
    }

    send_task.await??;
    println!("Enviado: {}", file.display());
    Ok(())
}

async fn run(settings: Settings) -> anyhow::Result<()> {
    let endpoint_id = random_endpoint_id();
    tracing::info!(device_name = %settings.device_name, %endpoint_id, "iniciando kushare");

    let (events_tx, mut events_rx) = mpsc::channel::<TransferEvent>(64);

    let receiver = Receiver::bind(
        settings.listen_port,
        settings.download_dir.clone(),
        events_tx,
    )
    .await?;
    let local_addr = receiver.local_addr()?;
    tracing::info!(%local_addr, "escuchando transferencias entrantes");
    tokio::spawn(receiver.run());

    let mut advertiser = MdnsAdvertiser::new()?;
    advertiser.start_advertising(&settings.device_name, &endpoint_id, local_addr.port())?;

    let scanner = Arc::new(MdnsScanner::new()?);
    let mdns_events = scanner.browse()?;
    let last_discovered = Arc::new(Mutex::new(None));
    {
        let last_discovered = last_discovered.clone();
        tokio::spawn(async move {
            while let Ok(event) = mdns_events.recv_async().await {
                if let Some(device) = discovered_device_from_event(&event) {
                    tracing::debug!(?device, "dispositivo descubierto por mDNS");
                    *last_discovered.lock().await = Some(device);
                }
            }
        });
    }

    // BLE es opcional (adaptador ausente, sin permisos, BlueZ caído...): un
    // fallo aquí no debe tumbar el resto de la app, igual que con D-Bus más
    // abajo. Ver kushare-discovery::ble: solo sirve como señal de
    // proximidad (kushare-a-kushare por ahora), nunca transporta el
    // archivo — eso sigue siendo TCP.
    let _ble_advertisement = match BleAdvertiser::new().await {
        Ok(advertiser) => match advertiser.start_advertising(&endpoint_id).await {
            Ok(handle) => {
                tracing::info!("anunciando por BLE");
                Some(handle)
            }
            Err(err) => {
                tracing::warn!(?err, "no se pudo iniciar el advertisement BLE");
                None
            }
        },
        Err(err) => {
            tracing::warn!(?err, "BLE no disponible (¿adaptador Bluetooth ausente?)");
            None
        }
    };

    match BleScanner::new().await {
        Ok(scanner) => match scanner.scan().await {
            Ok(mut sightings) => {
                tokio::spawn(async move {
                    while let Some(sighting) = sightings.recv().await {
                        match sighting {
                            kushare_discovery::ble::BleSighting::Kushare { endpoint_id, rssi } => {
                                tracing::info!(%endpoint_id, ?rssi, "kushare visto por BLE");
                            }
                            kushare_discovery::ble::BleSighting::AndroidQuickShare {
                                name,
                                rssi,
                            } => {
                                tracing::info!(
                                    ?name,
                                    ?rssi,
                                    "Android con Quick Share visto por BLE"
                                );
                            }
                        }
                    }
                });
            }
            Err(err) => tracing::warn!(?err, "no se pudo iniciar el escaneo BLE"),
        },
        Err(err) => tracing::warn!(?err, "BLE no disponible para escanear"),
    }

    let notifier = Notifier::connect().await.ok();
    if notifier.is_none() {
        tracing::warn!(
            "no se pudo conectar al bus de sesión D-Bus; las notificaciones estarán deshabilitadas"
        );
    }

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<TrayCommand>(16);
    let tray = Tray::new(placeholder_icon()?, cmd_tx)?;

    let mut visible = true;
    let my_device_name = settings.device_name.clone();

    loop {
        tokio::select! {
            Some(event) = events_rx.recv() => {
                let _ = tray.set_tooltip(&tooltip_for_event(&event));
                if let Some(notifier) = &notifier
                    && let Err(err) = notifier.notify_transfer_event(&event).await {
                        tracing::warn!(?err, "no se pudo enviar la notificación de escritorio");
                    }
            }

            Some(command) = cmd_rx.recv() => {
                match command {
                    TrayCommand::SendFile => {
                        let Some(path) = rfd::AsyncFileDialog::new().pick_file().await else {
                            continue;
                        };
                        let Some(device) = last_discovered.lock().await.clone() else {
                            tracing::warn!("no se ha descubierto ningún dispositivo todavía");
                            continue;
                        };

                        let my_device_name = my_device_name.clone();
                        let endpoint_id = endpoint_id.clone();
                        let events_tx = mpsc::channel::<TransferEvent>(16).0; // ver TODO abajo
                        tokio::spawn(async move {
                            let path = path.path().to_path_buf();
                            let result = async {
                                let mut session = Sender::connect(device.addr).await?;
                                Sender::send_file(&mut session, &endpoint_id, &my_device_name, &path, events_tx).await
                            }.await;
                            if let Err(err) = result {
                                tracing::error!(?err, "fallo al enviar el archivo");
                            }
                        });
                        // TODO: conectar los TransferEvent::Progress de este envío
                        // de vuelta al bucle principal (por ahora se descartan; ver
                        // el canal `events_tx` recién creado arriba) para reflejar
                        // el progreso de subida en el tooltip/notificaciones igual
                        // que ya se hace con la recepción.
                    }
                    TrayCommand::ToggleVisible => {
                        visible = !visible;
                        if visible {
                            advertiser.start_advertising(&settings.device_name, &endpoint_id, local_addr.port())?;
                            tracing::info!("dispositivo visible de nuevo");
                        } else {
                            advertiser.stop_advertising()?;
                            tracing::info!("dispositivo oculto");
                        }
                    }
                    TrayCommand::Quit => {
                        advertiser.stop_advertising()?;
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
