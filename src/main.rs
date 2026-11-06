mod doctor;
mod link;

use clap::{Parser, Subcommand};
use kushare_core::{Settings, TransferEvent, random_endpoint_id};
use kushare_discovery::ble::{BleAdvertiser, BleScanner};
use kushare_discovery::mdns::{MdnsAdvertiser, MdnsScanner, discovered_device_from_event};
use kushare_link::Direction;
use kushare_transfer::{Receiver, Sender};
use kushare_tray::{DeviceEntry, Notifier, Tray, TrayCommand, tray::tooltip_for_event};
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

    /// Puerto TCP del enlace persistente con otros dispositivos kushare
    /// (notificaciones). Fijo por defecto para poder abrirlo en el firewall.
    #[arg(long, default_value_t = link::DEFAULT_LINK_PORT)]
    link_port: u16,
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
    /// Envía un archivo por Bluetooth RFCOMM a un dispositivo ya vinculado
    /// (`bluetoothctl pair <MAC>`), para cuando no hay conectividad IP entre
    /// ambos. Si el daemon está corriendo, el envío lo hace él (BlueZ solo
    /// admite un registro del perfil por equipo); si no, este proceso.
    SendBt {
        /// Archivo a enviar.
        file: PathBuf,
        /// Dirección Bluetooth del receptor, p. ej. AA:BB:CC:DD:EE:FF.
        address: String,
    },
    /// Diagnostica problemas típicos (puerto, firewall, mDNS, Bluetooth,
    /// notificaciones) y dice cómo arreglarlos.
    Doctor,
    /// Control del enlace persistente del daemon en marcha.
    #[command(flatten)]
    Link(link::cli::LinkCommand),
}

/// Color primario del esquema de kushare (Material 3, tema claro).
const BRAND_RGB: [u8; 3] = [0x9d, 0x06, 0xd8];

/// Icono de bandeja: el mismo dibujo que el icono de la app Android y que
/// `packaging/common/kushare.svg` (círculo de color con el grafo de
/// "compartir"), rasterizado aquí con antialiasing para no depender de un
/// motor de SVG.
fn app_icon() -> anyhow::Result<Icon> {
    const SIZE: u32 = 64;
    Ok(Icon::from_rgba(app_icon_rgba(SIZE), SIZE, SIZE)?)
}

fn app_icon_rgba(size: u32) -> Vec<u8> {
    // Coordenadas en el viewBox 64×64 del SVG.
    let nodes = [(20.0f32, 32.0f32), (44.0, 18.0), (44.0, 46.0)];
    let seg_dist = |px: f32, py: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32)| {
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
    };
    // Cobertura (0..1) a partir de una distancia con signo (negativa = dentro).
    let cover = |d: f32, px_size: f32| (0.5 - d / px_size).clamp(0.0, 1.0);
    let scale = 64.0 / size as f32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = ((x as f32 + 0.5) * scale, (y as f32 + 0.5) * scale);
            let bg = cover(
                ((px - 32.0).powi(2) + (py - 32.0).powi(2)).sqrt() - 30.0,
                scale,
            );
            let glyph = nodes
                .iter()
                .map(|(cx, cy)| ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - 6.0)
                .chain([
                    seg_dist(px, py, nodes[0], nodes[1]) - 1.5,
                    seg_dist(px, py, nodes[0], nodes[2]) - 1.5,
                ])
                .fold(f32::MAX, f32::min);
            let g = cover(glyph, scale);
            let mix = |c: u8| (f32::from(c) * (1.0 - g) + 255.0 * g).round() as u8;
            rgba.extend_from_slice(&[
                mix(BRAND_RGB[0]),
                mix(BRAND_RGB[1]),
                mix(BRAND_RGB[2]),
                (bg * 255.0).round() as u8,
            ]);
        }
    }
    rgba
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
        Some(Command::SendBt { file, address }) => {
            let endpoint_id = random_endpoint_id();
            runtime.block_on(run_send_bt(settings, endpoint_id, file, address))
        }
        Some(Command::Doctor) => runtime.block_on(doctor::run_doctor()),
        Some(Command::Link(command)) => runtime.block_on(link::cli::run(command)),
        None => runtime.block_on(run(settings, cli.link_port)),
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
    let session = Sender::connect(addr).await?;
    send_with_progress(session, settings, endpoint_id, file).await
}

/// `kushare send-bt`: por el daemon si está corriendo, si no directamente.
async fn run_send_bt(
    settings: Settings,
    endpoint_id: String,
    file: PathBuf,
    address: String,
) -> anyhow::Result<()> {
    let address: bluer::Address = address.parse().map_err(|_| {
        anyhow::anyhow!("{address:?} no es una dirección Bluetooth (AA:BB:CC:DD:EE:FF)")
    })?;
    let file =
        std::fs::canonicalize(&file).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;

    if let Some(daemon) = link::cli::daemon_if_running().await {
        daemon
            .send_file_bt(&file.to_string_lossy(), &address.to_string())
            .await?;
        println!(
            "El daemon de kushare está enviando {} a {address}; avisará con una notificación al terminar.",
            file.display()
        );
        return Ok(());
    }

    println!("Conectando por Bluetooth a {address}...");
    let session = bluer::Session::new().await?;
    let bt = kushare_transfer::bt::BtService::register(session).await?;
    let transfer = bt.connect(address).await?;
    send_with_progress(transfer, settings, endpoint_id, file).await
}

/// Envía `file` por una sesión ya establecida (TCP o RFCOMM) imprimiendo el
/// progreso por stdout.
async fn send_with_progress<S>(
    mut session: kushare_transfer::TransferSession<S>,
    settings: Settings,
    endpoint_id: String,
    file: PathBuf,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
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

async fn run(settings: Settings, link_port: u16) -> anyhow::Result<()> {
    let endpoint_id = random_endpoint_id();
    tracing::info!(device_name = %settings.device_name, %endpoint_id, "iniciando kushare");

    let (events_tx, mut events_rx) = mpsc::channel::<TransferEvent>(64);
    let bt_events_tx = events_tx.clone();

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

    // Enlace persistente (notificaciones con otros dispositivos kushare).
    let link = link::start(link::LinkOptions {
        device_name: settings.device_name.clone(),
        port: link_port,
        ignore_senders: notifier.iter().filter_map(|n| n.unique_name()).collect(),
    })
    .await?;
    let _dbus = link::dbus::serve(link.clone()).await?;

    // Recepción de archivos por Bluetooth RFCOMM (además de TCP), con el
    // perfil de transferencias que registró el enlace en la misma sesión BlueZ.
    if let Some(bt) = link.transfer_bt.clone() {
        let download_dir = settings.download_dir.clone();
        tokio::spawn(async move {
            if let Err(err) = bt.run_receiver(download_dir, bt_events_tx).await {
                tracing::warn!(?err, "la recepción de archivos por Bluetooth terminó");
            }
        });
        tracing::info!("escuchando transferencias por Bluetooth");
    }
    let mut link_changes = link.subscribe_changes();

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<TrayCommand>(16);
    let tray = Tray::new(app_icon()?, cmd_tx)?;
    refresh_devices_menu(&tray, &link);

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

            Ok(()) = link_changes.changed() => refresh_devices_menu(&tray, &link),

            Some(command) = cmd_rx.recv() => {
                match command {
                    TrayCommand::ToggleIncoming(ref id) | TrayCommand::ToggleOutgoing(ref id) => {
                        let dir = if matches!(command, TrayCommand::ToggleIncoming(_)) {
                            Direction::Incoming
                        } else {
                            Direction::Outgoing
                        };
                        let id = id.clone();
                        let current = link.manager.peers().into_iter().find(|p| p.device_id == id);
                        if let Some(peer) = current {
                            let enabled = match dir {
                                Direction::Incoming => peer.incoming_enabled,
                                Direction::Outgoing => peer.outgoing_enabled,
                            };
                            if let Err(err) = link.manager.set_direction(&id, dir, !enabled) {
                                tracing::warn!(?err, "no se pudo cambiar la dirección");
                            }
                        }
                    }
                    TrayCommand::Unpair(id) => {
                        if let Err(err) = link.manager.unpair(&id) {
                            tracing::warn!(?err, "no se pudo desemparejar");
                        }
                    }
                    TrayCommand::Media { peer, player, action } => {
                        let result = link::dbus::parse_media_action(&action)
                            .map_err(|e| anyhow::anyhow!("{e}"))
                            .and_then(|a| link.features.media_command(&peer, &player, a));
                        if let Err(err) = result {
                            tracing::warn!(?err, "orden multimedia fallida");
                        }
                    }
                    TrayCommand::Ringer { peer, mode } => {
                        use kushare_link::pb::Ringer;
                        let mode = match mode.as_str() {
                            "silent" => Ringer::Silent,
                            "vibrate" => Ringer::Vibrate,
                            _ => Ringer::Normal,
                        };
                        if let Err(err) = link.features.set_ringer(&peer, mode) {
                            tracing::warn!(?err, "no se pudo cambiar el timbre");
                        }
                    }
                    TrayCommand::Output { peer, output } => {
                        if let Err(err) = link.features.set_audio_output(&peer, &output) {
                            tracing::warn!(?err, "no se pudo cambiar la salida de audio");
                        }
                    }
                    TrayCommand::SendTo(id) => {
                        let peer = link.manager.peers().into_iter().find(|p| p.device_id == id);
                        let Some(address) = peer
                            .as_ref()
                            .and_then(|p| p.bt_address.as_deref())
                            .and_then(|a| a.parse::<bluer::Address>().ok())
                        else {
                            tracing::warn!(%id, "no se conoce la dirección Bluetooth del dispositivo");
                            continue;
                        };
                        let link = link.clone();
                        tokio::spawn(async move {
                            let Some(file) = rfd::AsyncFileDialog::new().pick_file().await else {
                                return;
                            };
                            if let Err(err) = link.send_file_bt(file.path().to_path_buf(), address) {
                                tracing::warn!(?err, "no se pudo enviar el archivo");
                            }
                        });
                    }
                    TrayCommand::Scan => {
                        if let Err(err) = link.scan(std::time::Duration::from_secs(30)) {
                            tracing::warn!(?err, "no se pudo iniciar el escaneo");
                        }
                    }
                    TrayCommand::Pair(id) => {
                        if let Err(err) = link.pair(&id) {
                            tracing::warn!(?err, "no se pudo iniciar el emparejado");
                        }
                    }
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

fn refresh_devices_menu(tray: &Tray, link: &link::LinkHandle) {
    let paired: Vec<DeviceEntry> = link
        .manager
        .peers()
        .into_iter()
        .map(|p| {
            use kushare_link::pb::{PlaybackState, Ringer};
            let state = link.features.peer_state(&p.device_id);
            DeviceEntry {
                battery: state
                    .battery
                    .filter(|b| b.present)
                    .map(|b| (b.percent, b.charging)),
                ringer: state.ringer.map(|r| {
                    match r {
                        Ringer::Normal => "normal",
                        Ringer::Vibrate => "vibrate",
                        Ringer::Silent => "silent",
                    }
                    .to_string()
                }),
                players: state
                    .players
                    .iter()
                    .map(|pl| kushare_tray::PlayerEntry {
                        id: pl.id.clone(),
                        label: if pl.title.is_empty() {
                            pl.name.clone()
                        } else {
                            format!("{} — {}", pl.name, pl.title)
                        },
                        playing: pl.state() == PlaybackState::PlaybackPlaying,
                    })
                    .collect(),
                outputs: state
                    .outputs
                    .filter(|o| o.selectable)
                    .map(|o| {
                        o.outputs
                            .into_iter()
                            .map(|o| (o.id, o.name, o.active))
                            .collect()
                    })
                    .unwrap_or_default(),
                device_id: p.device_id,
                name: p.device_name,
                connected: p.connected,
                incoming: p.incoming_enabled,
                outgoing: p.outgoing_enabled,
                can_send_file: p.bt_address.is_some() && link.transfer_bt.is_some(),
            }
        })
        .collect();
    let discovered: Vec<(String, String)> = link
        .discovered_unpaired()
        .into_iter()
        .map(|s| (s.device_id, s.device_name))
        .collect();
    if let Err(err) = tray.set_devices(&paired, &discovered, link.is_scanning()) {
        tracing::warn!(?err, "no se pudo actualizar el menú de dispositivos");
    }
}
