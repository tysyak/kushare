//! Enlace persistente en el escritorio: conecta `kushare_link::LinkManager`
//! con los transportes (TCP + mDNS, RFCOMM), con las notificaciones del
//! escritorio (captura vía `kushare_tray::monitor`, visualización vía
//! `kushare_tray::Notifier`) y con los controles (D-Bus `org.kushare.Daemon`
//! en [`dbus`], CLI en [`cli`], menú del tray).

pub mod cli;
pub mod clipboard;
pub mod dbus;
pub mod features;
pub mod settings;

use kushare_discovery::mdns::{
    LINK_SERVICE_TYPE, LinkPeerSighting, MdnsAdvertiser, MdnsScanner, link_peer_from_event,
};
use kushare_i18n::{t, tf};
use kushare_link::{
    ConnectIntent, DialFuture, Dialer, LinkConfig, LinkEvent, LinkManager, LinkStream, LocalDevice,
    PeerRecord, RemoteNotification, Transport,
};
use kushare_transfer::bt::{BtService, ProfileSpec};
use kushare_tray::monitor::Captured;
use kushare_tray::notify::INLINE_REPLY_ACTION;
use kushare_tray::{MirrorNotification, Notifier, NotifierEvent};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};

/// Puerto TCP fijo por defecto del enlace: fijo para que la regla del
/// firewall no cambie entre arranques (`firewall-cmd --add-port=47816/tcp`).
pub const DEFAULT_LINK_PORT: u16 = 47816;

const TCP_DIAL_TIMEOUT: Duration = Duration::from_secs(4);

/// Prefijo de las acciones de respuesta cuando el servidor de
/// notificaciones no tiene `inline-reply` (quickshell, mako, dunst…): el
/// botón abre un diálogo (zenity/kdialog) para escribir la respuesta.
const REPLY_DIALOG_PREFIX: &str = "kushare-reply:";
const PAIR_ACCEPT: &str = "kushare-pair-accept";
const PAIR_REJECT: &str = "kushare-pair-reject";
const CALL_ACTION_PREFIX: &str = "kushare-call:";
const RECEIVE_ACCEPT: &str = "kushare-receive-accept";
const RECEIVE_REJECT: &str = "kushare-receive-reject";
/// Sin respuesta en este tiempo, el envío por Wi-Fi se rechaza.
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(60);

/// Alguien en la red quiere enviar archivos: preguntar al usuario.
struct ReceiveRequest {
    device_name: String,
    files: Vec<(String, u64)>,
    answer: tokio::sync::oneshot::Sender<bool>,
}

/// Lo que el resto del binario (tray, D-Bus) necesita del enlace.
#[derive(Clone)]
pub struct LinkHandle {
    pub manager: LinkManager,
    /// Peers vistos por mDNS ahora mismo (emparejados o no).
    discovered: Arc<Mutex<BTreeMap<String, LinkPeerSighting>>>,
    /// Se incrementa con cualquier cambio de peers/descubrimiento, para que
    /// el tray reconstruya su menú.
    changes: watch::Sender<u64>,
    /// Eventos del enlace re-emitidos (para las señales D-Bus).
    pub events: broadcast::Sender<LinkEvent>,
    bt: Option<BtService>,
    /// Perfil RFCOMM de transferencias de archivos (otro UUID que el del
    /// enlace), registrado en la misma sesión BlueZ: lo usan la recepción
    /// por Bluetooth del daemon y `kushare send-bt` vía D-Bus.
    pub transfer_bt: Option<BtService>,
    /// Puerto TCP real del enlace (puede no ser el fijo si estaba ocupado).
    pub link_port: Arc<Mutex<u16>>,
    /// Escáner mDNS; solo busca durante [`LinkHandle::scan`].
    scanner: Arc<MdnsScanner>,
    scanning: Arc<std::sync::atomic::AtomicBool>,
    /// Anuncio mDNS del enlace (siempre activo mientras viva el daemon).
    advertiser: Arc<Mutex<Option<MdnsAdvertiser>>>,
    /// Batería, multimedia, audio, timbre, llamadas y fotos.
    pub features: Arc<features::Features>,
    pub updates: Arc<crate::update::Updates>,
    receive_requests: mpsc::UnboundedSender<ReceiveRequest>,
}

impl LinkHandle {
    pub fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn bump(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Envía `path` por Bluetooth RFCOMM (perfil de transferencias) a
    /// `address`, en segundo plano; el resultado se avisa con una
    /// notificación de escritorio.
    pub fn send_file_bt(
        &self,
        path: std::path::PathBuf,
        address: bluer::Address,
    ) -> anyhow::Result<()> {
        let bt = self
            .transfer_bt
            .clone()
            .ok_or_else(|| anyhow::anyhow!("el daemon no tiene Bluetooth disponible"))?;
        if !path.is_file() {
            anyhow::bail!("{} no es un archivo", path.display());
        }
        let device_name = self.manager.device_name().to_string();
        tokio::spawn(async move {
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let result = async {
                let mut session = bt.connect(address).await?;
                // El progreso no se muestra (envío en segundo plano).
                let (events, _) = tokio::sync::mpsc::channel(64);
                kushare_transfer::Sender::send_file(
                    &mut session,
                    &kushare_core::random_endpoint_id(),
                    &device_name,
                    &path,
                    events,
                )
                .await
            }
            .await;
            let body = match &result {
                Ok(()) => tf(
                    "daemon.bt_sent",
                    &[("file", &file_name), ("address", &address)],
                ),
                Err(err) => tf(
                    "daemon.bt_send_failed",
                    &[
                        ("file", &file_name),
                        ("address", &address),
                        ("error", &format!("{err:#}")),
                    ],
                ),
            };
            if let Err(err) = &result {
                tracing::warn!(?err, %address, "envío por Bluetooth fallido");
            }
            if let Ok(notifier) = Notifier::connect().await {
                let _ = notifier.notify("kushare", &body).await;
            }
        });
        Ok(())
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Busca dispositivos kushare en la red durante `duration`. No hay
    /// escaneo continuo: solo cuando el usuario lo pide (ahorra tráfico
    /// multicast y batería en portátiles). Los peers ya emparejados no lo
    /// necesitan para reconectar: su dirección se aprende en cada conexión.
    pub fn scan(&self, duration: Duration) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        if self.scanning.swap(true, Ordering::SeqCst) {
            return Ok(()); // ya hay un escaneo en curso
        }
        let events = match self.scanner.browse_service(LINK_SERVICE_TYPE) {
            Ok(events) => events,
            Err(err) => {
                self.scanning.store(false, Ordering::SeqCst);
                return Err(err);
            }
        };
        self.discovered.lock().expect("mutex envenenado").clear();
        self.bump();
        let handle = self.clone();
        tokio::spawn(async move {
            let my_id = handle.manager.device_id().to_string();
            let deadline = tokio::time::sleep(duration);
            tokio::pin!(deadline);
            loop {
                let event = tokio::select! {
                    _ = &mut deadline => break,
                    event = events.recv_async() => match event {
                        Ok(event) => event,
                        Err(_) => break,
                    },
                };
                handle.on_mdns_event(&my_id, &event);
            }
            let _ = handle.scanner.stop_browse(LINK_SERVICE_TYPE);
            handle.scanning.store(false, Ordering::SeqCst);
            handle.bump();
            tracing::info!("escaneo de dispositivos terminado");
        });
        tracing::info!(?duration, "buscando dispositivos kushare en la red");
        Ok(())
    }

    fn on_mdns_event(&self, my_id: &str, event: &kushare_discovery::mdns::ServiceEvent) {
        if let kushare_discovery::mdns::ServiceEvent::ServiceRemoved(_, fullname) = event {
            let id = fullname.split('.').next().unwrap_or_default().to_string();
            if self
                .discovered
                .lock()
                .expect("mutex envenenado")
                .remove(&id)
                .is_some()
            {
                self.bump();
            }
            return;
        }
        let Some(sighting) = link_peer_from_event(event) else {
            return;
        };
        if sighting.device_id == my_id {
            return;
        }
        tracing::debug!(?sighting, "peer del enlace visto por mDNS");
        self.manager
            .note_sighting(&sighting.device_id, sighting.addr);
        let changed = self
            .discovered
            .lock()
            .expect("mutex envenenado")
            .insert(sighting.device_id.clone(), sighting.clone())
            .as_ref()
            != Some(&sighting);
        if changed {
            self.bump();
        }
    }

    /// Peers descubiertos por mDNS que aún no están emparejados.
    pub fn discovered_unpaired(&self) -> Vec<LinkPeerSighting> {
        let paired: HashSet<String> = self
            .manager
            .peers()
            .into_iter()
            .map(|p| p.device_id)
            .collect();
        self.discovered
            .lock()
            .expect("mutex envenenado")
            .values()
            .filter(|s| !paired.contains(&s.device_id))
            .cloned()
            .collect()
    }

    /// Resuelve una referencia a un peer (id completo, prefijo del id o
    /// nombre, sin distinguir mayúsculas) entre emparejados y descubiertos.
    pub fn resolve(&self, reference: &str) -> anyhow::Result<String> {
        let reference = reference.trim();
        let mut candidates: Vec<(String, String)> = self
            .manager
            .peers()
            .into_iter()
            .map(|p| (p.device_id, p.device_name))
            .collect();
        candidates.extend(
            self.discovered
                .lock()
                .expect("mutex envenenado")
                .values()
                .map(|s| (s.device_id.clone(), s.device_name.clone())),
        );
        candidates.sort();
        candidates.dedup_by(|a, b| a.0 == b.0);
        if let Some((id, _)) = candidates.iter().find(|(id, _)| id == reference) {
            return Ok(id.clone());
        }
        let exact: Vec<&(String, String)> = candidates
            .iter()
            .filter(|(id, name)| id.starts_with(reference) || name.eq_ignore_ascii_case(reference))
            .collect();
        // Si no hay coincidencia exacta, vale una parte del nombre ("armor").
        let lower = reference.to_lowercase();
        let matches = if exact.is_empty() && !lower.is_empty() {
            candidates
                .iter()
                .filter(|(_, name)| name.to_lowercase().contains(&lower))
                .collect()
        } else {
            exact
        };
        match matches.as_slice() {
            [(id, _)] => Ok(id.clone()),
            [] => anyhow::bail!("ningún dispositivo coincide con {reference:?}"),
            _ => anyhow::bail!("{reference:?} es ambiguo: usa más caracteres del id"),
        }
    }

    /// Empieza a emparejar con un peer descubierto por mDNS o, si
    /// `reference` es una dirección Bluetooth (`AA:BB:CC:DD:EE:FF`, ya
    /// vinculada con `bluetoothctl pair`), por RFCOMM. Vuelve en cuanto la
    /// conexión arranca; el PIN llega como `PairingRequested`.
    pub fn pair(&self, reference: &str) -> anyhow::Result<()> {
        if let Ok(address) = reference.trim().parse::<bluer::Address>() {
            return self.pair_bluetooth(address);
        }
        let id = self.resolve(reference)?;
        let sighting = self
            .discovered
            .lock()
            .expect("mutex envenenado")
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{id} no está visible en la red ahora mismo"))?;
        let manager = self.manager.clone();
        tokio::spawn(async move {
            let result = async {
                let stream =
                    tokio::time::timeout(TCP_DIAL_TIMEOUT, TcpStream::connect(sighting.addr))
                        .await
                        .map_err(|_| {
                            anyhow::anyhow!("tiempo agotado conectando a {}", sighting.addr)
                        })??;
                manager
                    .connect(stream, Transport::Tcp(sighting.addr), ConnectIntent::Pair)
                    .await
            }
            .await;
            if let Err(err) = result {
                tracing::warn!(?err, peer = %sighting.device_name, "emparejado fallido");
            }
        });
        Ok(())
    }
}

impl LinkHandle {
    fn pair_bluetooth(&self, address: bluer::Address) -> anyhow::Result<()> {
        let bt = self
            .bt
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Bluetooth no está disponible en este equipo"))?;
        let manager = self.manager.clone();
        tokio::spawn(async move {
            let result = async {
                let stream = bt.connect_raw(address).await?;
                manager
                    .connect(
                        stream,
                        Transport::Bluetooth(address.to_string()),
                        ConnectIntent::Pair,
                    )
                    .await
            }
            .await;
            if let Err(err) = result {
                tracing::warn!(?err, %address, "emparejado por Bluetooth fallido");
            }
        });
        Ok(())
    }
}

/// Dialer de escritorio: TCP a la última dirección vista por mDNS y, si no
/// hay o falla, RFCOMM a la dirección Bluetooth conocida del peer.
struct DesktopDialer {
    bt: Option<BtService>,
}

impl Dialer for DesktopDialer {
    fn dial(&self, peer: PeerRecord) -> DialFuture {
        let bt = self.bt.clone();
        Box::pin(async move {
            let mut last_err = anyhow::anyhow!("{} no tiene dirección conocida", peer.device_name);
            if let Some(addr) = peer
                .last_tcp_addr
                .as_deref()
                .and_then(|a| a.parse::<SocketAddr>().ok())
            {
                match tokio::time::timeout(TCP_DIAL_TIMEOUT, TcpStream::connect(addr)).await {
                    Ok(Ok(stream)) => {
                        return Ok((
                            Box::new(stream) as Box<dyn LinkStream>,
                            Transport::Tcp(addr),
                        ));
                    }
                    Ok(Err(err)) => last_err = err.into(),
                    Err(_) => last_err = anyhow::anyhow!("tiempo agotado conectando a {addr}"),
                }
            }
            if let (Some(bt), Some(bt_addr)) = (bt, peer.bt_address.as_deref()) {
                let address: bluer::Address = bt_addr.parse()?;
                let stream = bt.connect_raw(address).await?;
                return Ok((
                    Box::new(stream) as Box<dyn LinkStream>,
                    Transport::Bluetooth(bt_addr.to_string()),
                ));
            }
            Err(last_err)
        })
    }
}

pub struct LinkOptions {
    pub device_name: String,
    pub port: u16,
    /// Nombres únicos D-Bus de otras conexiones de kushare cuyas
    /// notificaciones no deben reenviarse (p. ej. el `Notifier` de
    /// transferencias del binario principal).
    pub ignore_senders: HashSet<String>,
}

/// Arranca todo el enlace en segundo plano y devuelve su handle.
pub async fn start(opts: LinkOptions) -> anyhow::Result<LinkHandle> {
    let dirs = directories::ProjectDirs::from("", "", "kushare")
        .ok_or_else(|| anyhow::anyhow!("no se pudo determinar el directorio HOME"))?;

    // Bluetooth es opcional: sin adaptador, el enlace funciona solo por Wi-Fi.
    let (bt, transfer_bt, bt_address) = match start_bluetooth().await {
        Ok((bt, transfer, addr)) => (Some(bt), transfer, Some(addr)),
        Err(err) => {
            tracing::warn!(?err, "enlace por Bluetooth no disponible");
            (None, None, None)
        }
    };

    let (manager, mut link_events) = LinkManager::open(
        dirs.config_dir(),
        dirs.cache_dir(),
        LocalDevice {
            name: opts.device_name.clone(),
            kind: "desktop".into(),
            bt_address: bt_address.clone(),
            capabilities: vec!["notifications".into(), "notification-dismiss".into()],
        },
        LinkConfig::default(),
    )?;
    tracing::info!(device_id = manager.device_id(), "enlace persistente listo");

    let (events, _) = broadcast::channel(64);
    let (receive_requests, mut receive_rx) = mpsc::unbounded_channel();
    let handle = LinkHandle {
        manager: manager.clone(),
        discovered: Arc::default(),
        changes: watch::channel(0).0,
        events,
        bt: bt.clone(),
        transfer_bt,
        scanner: Arc::new(MdnsScanner::new()?),
        scanning: Arc::default(),
        link_port: Arc::default(),
        advertiser: Arc::default(),
        features: features::Features::start(
            manager.clone(),
            dirs.cache_dir(),
            dirs.data_dir(),
            Arc::new(settings::SettingsStore::open(dirs.config_dir())),
        ),
        updates: Arc::new(crate::update::Updates::open(dirs.cache_dir())),
        receive_requests,
    };

    // TCP
    // Si el puerto fijo está ocupado (otro programa, otra instancia), no se
    // falla: se usa uno libre y se avisa. El peer lo aprende igualmente en
    // el `Hello`, pero el firewall tendría que abrir ese otro puerto
    // (`kushare doctor` lo indica).
    let listener = match TcpListener::bind(("0.0.0.0", opts.port)).await {
        Ok(listener) => listener,
        Err(err) if opts.port != 0 => {
            tracing::warn!(port = opts.port, %err, "puerto del enlace ocupado; se usa otro libre");
            TcpListener::bind(("0.0.0.0", 0)).await?
        }
        Err(err) => return Err(err.into()),
    };
    let port = listener.local_addr()?.port();
    manager.set_tcp_port(port);
    *handle.link_port.lock().expect("mutex envenenado") = port;
    tracing::info!(port, "enlace escuchando por TCP");
    {
        let manager = manager.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, addr)) = listener.accept().await else {
                    continue;
                };
                let manager = manager.clone();
                tokio::spawn(async move {
                    if let Err(err) = manager.accept(stream, Transport::Tcp(addr)).await {
                        tracing::info!(%addr, ?err, "conexión de enlace entrante rechazada");
                    }
                });
            }
        });
    }

    // Bluetooth (aceptación)
    if let Some(bt) = &bt {
        let bt = bt.clone();
        let manager = manager.clone();
        tokio::spawn(async move {
            let result = bt
                .run_acceptor(move |stream, device| {
                    let manager = manager.clone();
                    async move {
                        if let Err(err) = manager
                            .accept(stream, Transport::Bluetooth(device.to_string()))
                            .await
                        {
                            tracing::info!(%device, ?err, "conexión de enlace RFCOMM rechazada");
                        }
                    }
                })
                .await;
            tracing::warn!(?result, "el aceptador RFCOMM del enlace terminó");
        });
    }

    // mDNS: anunciarse siempre (pasivo: solo responde a quien pregunta);
    // buscar solo bajo demanda (ver `LinkHandle::scan`). El anunciante debe
    // vivir lo que el proceso.
    let mut advertiser = MdnsAdvertiser::new()?;
    advertiser.start_advertising_link(manager.device_id(), &opts.device_name, port)?;
    *handle.advertiser.lock().expect("mutex envenenado") = Some(advertiser);

    // Reconexión
    {
        let manager = manager.clone();
        tokio::spawn(async move { manager.run_reconnect(DesktopDialer { bt }).await });
    }

    // Notificaciones
    let notifier = Notifier::connect().await?;
    let mut ignore = opts.ignore_senders;
    ignore.extend(notifier.unique_name());
    let mut captured = kushare_tray::monitor::start(ignore).await?;
    let mut notifier_events = notifier.events().await?;
    let inline_reply = notifier
        .capabilities()
        .await
        .iter()
        .any(|c| c == "inline-reply");
    tracing::info!(inline_reply, "servidor de notificaciones listo");

    // Avisar (una vez por versión) de que hay una versión nueva.
    {
        let updates = handle.updates.clone();
        let settings = handle.features.settings.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(90)).await;
            loop {
                if settings.get().update_check
                    && let Ok(release) = updates.check(false).await
                    && crate::update::is_newer(&release.version, crate::update::CURRENT)
                    && updates.should_notify(&release.version)
                    && let Ok(notifier) = Notifier::connect().await
                {
                    let _ = notifier
                        .notify(
                            &tf("daemon.update_title", &[("version", &release.version)]),
                            t("daemon.update_body"),
                        )
                        .await;
                }
                tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
            }
        });
    }

    let loop_handle = handle.clone();
    tokio::spawn(async move {
        let mut ui = DesktopUi {
            handle: loop_handle,
            notifier,
            inline_reply,
            mirrors: HashMap::new(),
            by_id: HashMap::new(),
            pin_prompts: HashMap::new(),
            calls: HashMap::new(),
            receive_prompts: HashMap::new(),
        };
        loop {
            tokio::select! {
                Some(event) = link_events.recv() => ui.on_link_event(event).await,
                Some(c) = captured.recv() => ui.on_captured(c),
                Some(e) = notifier_events.recv() => ui.on_notifier_event(e).await,
                Some(r) = receive_rx.recv() => ui.ask_receive(r).await,
                else => break,
            }
        }
        tracing::warn!("el bucle de notificaciones del enlace terminó");
    });

    Ok(handle)
}

impl LinkHandle {
    /// Confirmación para el receptor TCP de archivos (Quick Share): con
    /// `receive-confirm` activado, pregunta con una notificación; sin
    /// respuesta en 60 s, rechaza.
    pub fn receive_consent(&self) -> kushare_transfer::Consent {
        let requests = self.receive_requests.clone();
        let settings = self.features.settings.clone();
        Arc::new(move |device_name, files| {
            let requests = requests.clone();
            let ask = settings.get().receive_confirm;
            Box::pin(async move {
                if !ask {
                    return true;
                }
                let (answer, rx) = tokio::sync::oneshot::channel();
                if requests
                    .send(ReceiveRequest {
                        device_name,
                        files,
                        answer,
                    })
                    .is_err()
                {
                    return false;
                }
                matches!(
                    tokio::time::timeout(RECEIVE_TIMEOUT, rx).await,
                    Ok(Ok(true))
                )
            })
        })
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// Registra los dos perfiles RFCOMM del daemon en una misma sesión BlueZ:
/// el del enlace (con el agente) y el de transferencias (sin agente: BlueZ
/// admite uno por conexión D-Bus). Si el de transferencias falla (p. ej.
/// otro proceso `kushare send-bt` lo tiene registrado), el enlace sigue.
async fn start_bluetooth() -> anyhow::Result<(BtService, Option<BtService>, String)> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let address = adapter.address().await?.to_string();
    let link_spec = ProfileSpec {
        with_agent: true,
        ..ProfileSpec::link()
    };
    let bt = BtService::register_profile(session.clone(), link_spec).await?;
    tracing::info!(%address, "enlace escuchando por RFCOMM");
    let transfer_spec = ProfileSpec {
        with_agent: false,
        ..ProfileSpec::transfer()
    };
    let transfer = match BtService::register_profile(session, transfer_spec).await {
        Ok(transfer) => Some(transfer),
        Err(err) => {
            tracing::warn!(
                ?err,
                "no se pudo registrar el perfil RFCOMM de transferencias"
            );
            None
        }
    };
    Ok((bt, transfer, address))
}

/// Espejo local de una notificación del peer.
struct Mirror {
    peer_id: String,
    key: String,
    /// Acción de respuesta del original (para `Replied` / el diálogo).
    reply_action: Option<String>,
    title: String,
    text: String,
}

/// Puente entre los eventos del enlace y el servidor de notificaciones.
struct DesktopUi {
    handle: LinkHandle,
    notifier: Notifier<'static>,
    inline_reply: bool,
    /// (peer, key) → id local del espejo.
    mirrors: HashMap<(String, String), u32>,
    by_id: HashMap<u32, Mirror>,
    /// id de la notificación del PIN → device_id.
    pin_prompts: HashMap<u32, String>,
    /// device_id → id de la notificación de su llamada en curso.
    calls: HashMap<String, u32>,
    /// id de la notificación "¿aceptar archivos?" → respuesta.
    receive_prompts: HashMap<u32, tokio::sync::oneshot::Sender<bool>>,
}

impl DesktopUi {
    async fn ask_receive(&mut self, r: ReceiveRequest) {
        self.receive_prompts.retain(|_, tx| !tx.is_closed());
        let total: u64 = r.files.iter().map(|f| f.1).sum();
        let names = r
            .files
            .iter()
            .map(|f| f.0.as_str())
            .take(3)
            .collect::<Vec<_>>()
            .join(", ");
        let more = if r.files.len() > 3 {
            format!(" (+{})", r.files.len() - 3)
        } else {
            String::new()
        };
        let shown = self
            .notifier
            .notify_full(&MirrorNotification {
                app_name: "kushare".into(),
                summary: tf("daemon.receive_title", &[("name", &r.device_name)]),
                body: tf(
                    "daemon.receive_body",
                    &[
                        ("files", &format!("{names}{more}")),
                        ("size", &format_size(total)),
                    ],
                ),
                actions: vec![
                    (RECEIVE_ACCEPT.into(), t("daemon.accept").into()),
                    (RECEIVE_REJECT.into(), t("daemon.reject").into()),
                ],
                persistent: true,
                ..Default::default()
            })
            .await;
        match shown {
            Ok(id) => {
                self.receive_prompts.insert(id, r.answer);
            }
            Err(err) => {
                tracing::warn!(?err, "no se pudo preguntar si aceptar el envío: se rechaza");
                let _ = r.answer.send(false);
            }
        }
    }

    fn peer_name(&self, peer_id: &str) -> String {
        self.handle
            .manager
            .peers()
            .into_iter()
            .find(|p| p.device_id == peer_id)
            .map(|p| p.device_name)
            .unwrap_or_else(|| peer_id.to_string())
    }

    async fn on_link_event(&mut self, event: LinkEvent) {
        // El estado de las funciones extra se actualiza ANTES de re-emitir
        // el evento, para que quien lo reciba (D-Bus, tray) ya lo vea.
        let change = match &event {
            LinkEvent::Feature { device_id, feature } => {
                self.handle.features.on_remote(device_id, feature.clone())
            }
            LinkEvent::PeerConnected { device_id, .. } => {
                self.handle.features.on_connected(device_id);
                None
            }
            LinkEvent::PeerDisconnected { device_id } => {
                self.handle.features.on_disconnected(device_id);
                self.show_call(device_id.clone(), None).await;
                None
            }
            LinkEvent::PeerUnpaired { device_id } => {
                self.handle.features.forget(device_id);
                None
            }
            _ => None,
        };
        let _ = self.handle.events.send(event.clone());
        if let LinkEvent::Feature { device_id, .. } = &event {
            let changed = change.is_some();
            match change {
                Some(features::RemoteChange::Call(call)) => {
                    self.show_call(device_id.clone(), Some(call)).await;
                }
                Some(features::RemoteChange::LowBattery(percent)) => {
                    let name = self.peer_name(device_id);
                    let _ = self
                        .notifier
                        .notify(
                            &tf("daemon.low_battery", &[("name", &name)]),
                            &tf("daemon.low_battery_detail", &[("percent", &percent)]),
                        )
                        .await;
                }
                _ => {}
            }
            if changed {
                self.handle.bump();
            }
            return;
        }
        match event {
            LinkEvent::PairingRequested {
                device_id,
                device_name,
                pin,
                ..
            } => {
                let shown = self
                    .notifier
                    .notify_full(&MirrorNotification {
                        app_name: "kushare".into(),
                        summary: tf("daemon.pair_title", &[("name", &device_name)]),
                        body: tf("daemon.pair_body", &[("pin", &pin)]),
                        actions: vec![
                            (PAIR_ACCEPT.into(), t("daemon.accept").into()),
                            (PAIR_REJECT.into(), t("daemon.reject").into()),
                        ],
                        persistent: true,
                        ..Default::default()
                    })
                    .await;
                match shown {
                    Ok(id) => {
                        self.pin_prompts.insert(id, device_id);
                    }
                    Err(err) => {
                        tracing::warn!(?err, %pin, "no se pudo mostrar el PIN; usa `kushare pair`")
                    }
                }
            }
            LinkEvent::PairingFinished {
                device_id,
                device_name,
                accepted,
            } => {
                let prompts: Vec<u32> = self
                    .pin_prompts
                    .iter()
                    .filter(|(_, id)| **id == device_id)
                    .map(|(n, _)| *n)
                    .collect();
                for id in prompts {
                    self.pin_prompts.remove(&id);
                    let _ = self.notifier.close(id).await;
                }
                let body = if accepted {
                    tf("daemon.paired", &[("name", &device_name)])
                } else {
                    tf("daemon.not_paired", &[("name", &device_name)])
                };
                let _ = self.notifier.notify("kushare", &body).await;
                self.handle.bump();
            }
            LinkEvent::PeerConnected { .. }
            | LinkEvent::PeerDisconnected { .. }
            | LinkEvent::PeerUnpaired { .. }
            | LinkEvent::ConfigChanged { .. } => self.handle.bump(),
            LinkEvent::NotificationPosted {
                device_id,
                notification,
            } => self.show_mirror(device_id, notification).await,
            LinkEvent::NotificationRemoved { device_id, key } => {
                if let Some(id) = self.mirrors.remove(&(device_id, key)) {
                    self.by_id.remove(&id);
                    let _ = self.notifier.close(id).await;
                }
            }
            LinkEvent::DismissLocal { key, .. } => {
                // Mejor esfuerzo: cerrar con su id la notificación de otra app.
                if let Ok(id) = key.parse::<u32>()
                    && let Err(err) = self.notifier.close(id).await
                {
                    tracing::debug!(?err, id, "no se pudo cerrar la notificación local");
                }
            }
            LinkEvent::Feature { .. } => {}
            LinkEvent::ActionRequested { .. } | LinkEvent::ReplyRequested { .. } => {
                // Nunca ofrecemos acciones de notificaciones del escritorio
                // (ver kushare_tray::monitor), así que no debería llegar.
                tracing::debug!("acción remota sobre notificación de escritorio: no soportado");
            }
        }
    }

    /// Muestra (o cierra, con `None`/fase inactiva) la notificación de la
    /// llamada en curso del teléfono, con sus botones.
    async fn show_call(&mut self, peer_id: String, call: Option<kushare_link::pb::CallState>) {
        use kushare_link::pb::CallPhase;
        let phase = call.as_ref().map_or(CallPhase::CallIdle, |c| c.phase());
        let replaces_id = self.calls.get(&peer_id).copied().unwrap_or(0);
        let Some(call) = call.filter(|_| phase != CallPhase::CallIdle) else {
            if let Some(id) = self.calls.remove(&peer_id) {
                let _ = self.notifier.close(id).await;
            }
            return;
        };
        let who = if !call.name.is_empty() {
            call.name.clone()
        } else if !call.number.is_empty() {
            call.number.clone()
        } else {
            t("daemon.unknown_number").into()
        };
        let peer_name = self.peer_name(&peer_id);
        let action = |a: &str, label: &str| (format!("{CALL_ACTION_PREFIX}{a}"), label.to_string());
        let (summary, actions) = match phase {
            CallPhase::CallRinging => (
                tf("daemon.call_incoming", &[("who", &who)]),
                vec![
                    action("answer", t("daemon.answer")),
                    action("reject", t("daemon.reject")),
                ],
            ),
            _ => (
                tf("daemon.call_active", &[("who", &who)]),
                vec![
                    action("hangup", t("daemon.hangup")),
                    if call.muted {
                        action("unmute", t("daemon.unmute"))
                    } else {
                        action("mute", t("daemon.mute"))
                    },
                ],
            ),
        };
        let shown = self
            .notifier
            .notify_full(&MirrorNotification {
                replaces_id,
                app_name: tf("daemon.call_app", &[("name", &peer_name)]),
                summary,
                body: if call.name.is_empty() {
                    String::new()
                } else {
                    call.number.clone()
                },
                actions,
                persistent: true,
                ..Default::default()
            })
            .await;
        match shown {
            Ok(id) => {
                self.calls.insert(peer_id, id);
            }
            Err(err) => tracing::warn!(?err, "no se pudo mostrar la llamada"),
        }
    }

    async fn show_mirror(&mut self, peer_id: String, n: RemoteNotification) {
        let peer_name = self.peer_name(&peer_id);
        let reply = n.actions.iter().find(|a| a.is_reply).cloned();
        let mut actions: Vec<(String, String)> = n
            .actions
            .iter()
            .filter(|a| !a.is_reply)
            .map(|a| (a.id.clone(), a.label.clone()))
            .collect();
        let mut inline_reply_placeholder = None;
        if let Some(reply) = &reply {
            if self.inline_reply {
                inline_reply_placeholder = Some(tf("daemon.reply_in", &[("app", &n.app_name)]));
            } else {
                actions.push((
                    format!("{REPLY_DIALOG_PREFIX}{}", reply.id),
                    reply.label.clone(),
                ));
            }
        }
        let map_key = (peer_id.clone(), n.key.clone());
        let replaces_id = self.mirrors.get(&map_key).copied().unwrap_or(0);
        let shown = self
            .notifier
            .notify_full(&MirrorNotification {
                replaces_id,
                app_name: format!("{} · {peer_name}", n.app_name),
                summary: n.title.clone(),
                body: n.text.clone(),
                icon_path: n.icon_path.clone(),
                actions,
                inline_reply_placeholder,
                persistent: false,
            })
            .await;
        match shown {
            Ok(id) => {
                if replaces_id != 0 && replaces_id != id {
                    self.by_id.remove(&replaces_id);
                }
                self.mirrors.insert(map_key, id);
                self.by_id.insert(
                    id,
                    Mirror {
                        peer_id,
                        key: n.key,
                        reply_action: reply.map(|r| r.id),
                        title: n.title,
                        text: n.text,
                    },
                );
            }
            Err(err) => tracing::warn!(?err, "no se pudo mostrar la notificación del peer"),
        }
    }

    fn on_captured(&mut self, captured: Captured) {
        match captured {
            Captured::Posted(n) => {
                if is_notification_bridge(&n.app_id, &n.app_name) {
                    return;
                }
                self.handle.manager.post_local_notification(n);
            }
            // Las caducadas (motivo 1) suelen seguir en el historial del
            // servidor: no se quitan del teléfono.
            Captured::Closed { key, reason } if reason != 1 => {
                self.handle.manager.remove_local_notification(&key);
            }
            Captured::Closed { .. } => {}
        }
    }

    async fn on_notifier_event(&mut self, event: NotifierEvent) {
        match event {
            NotifierEvent::ActionInvoked { id, action } => {
                if action == RECEIVE_ACCEPT || action == RECEIVE_REJECT {
                    if let Some(answer) = self.receive_prompts.remove(&id) {
                        let _ = answer.send(action == RECEIVE_ACCEPT);
                    }
                    return;
                }
                if let Some(device_id) = self.pin_prompts.remove(&id) {
                    let accept = action == PAIR_ACCEPT;
                    if action == PAIR_ACCEPT || action == PAIR_REJECT {
                        self.handle.manager.respond_pairing(&device_id, accept);
                    } else {
                        self.pin_prompts.insert(id, device_id);
                    }
                    return;
                }
                if let Some(call_action) = action.strip_prefix(CALL_ACTION_PREFIX)
                    && let Some(peer) = self
                        .calls
                        .iter()
                        .find(|(_, nid)| **nid == id)
                        .map(|(p, _)| p.clone())
                {
                    use kushare_link::pb::CallAction;
                    let call_action = match call_action {
                        "answer" => CallAction::CallAnswer,
                        "reject" => CallAction::CallReject,
                        "hangup" => CallAction::CallHangup,
                        "mute" => CallAction::CallMute,
                        _ => CallAction::CallUnmute,
                    };
                    if let Err(err) = self.handle.features.call_command(&peer, call_action) {
                        tracing::info!(?err, "orden de llamada fallida");
                    }
                    return;
                }
                let Some(mirror) = self.by_id.get(&id) else {
                    return;
                };
                if action == "default" || action == INLINE_REPLY_ACTION {
                    return; // clic en el cuerpo / el campo de respuesta inline
                }
                if let Some(action_id) = action.strip_prefix(REPLY_DIALOG_PREFIX) {
                    let (manager, peer, key, action_id) = (
                        self.handle.manager.clone(),
                        mirror.peer_id.clone(),
                        mirror.key.clone(),
                        action_id.to_string(),
                    );
                    let (title, text) = (mirror.title.clone(), mirror.text.clone());
                    tokio::spawn(async move {
                        match ask_reply(&title, &text).await {
                            Some(reply) => manager.reply_remote(&peer, &key, &action_id, &reply),
                            None => tracing::debug!("respuesta cancelada"),
                        }
                    });
                } else {
                    self.handle
                        .manager
                        .invoke_remote_action(&mirror.peer_id, &mirror.key, &action);
                }
            }
            NotifierEvent::Replied { id, text } => {
                if let Some(mirror) = self.by_id.get(&id)
                    && let Some(action_id) = &mirror.reply_action
                {
                    self.handle.manager.reply_remote(
                        &mirror.peer_id,
                        &mirror.key,
                        action_id,
                        &text,
                    );
                }
            }
            NotifierEvent::Closed { id, by_user } => {
                self.pin_prompts.remove(&id);
                self.calls.retain(|_, nid| *nid != id);
                if let Some(mirror) = self.by_id.remove(&id) {
                    self.mirrors
                        .remove(&(mirror.peer_id.clone(), mirror.key.clone()));
                    if by_user {
                        self.handle
                            .manager
                            .dismiss_remote_notification(&mirror.peer_id, &mirror.key);
                    }
                }
            }
        }
    }
}

/// Notificaciones que NO se reenvían al teléfono: las del propio kushare y
/// las de otros puentes de notificaciones (KDE Connect muestra aquí las del
/// teléfono; reenviarlas se las devolvería duplicadas, o crearía un bucle
/// entre ambos puentes).
fn is_notification_bridge(app_id: &str, app_name: &str) -> bool {
    const BRIDGES: &[&str] = &[
        "kushare",
        "kde connect",
        "kdeconnect",
        "org.kde.kdeconnect.daemon",
        "gsconnect",
    ];
    let (id, name) = (app_id.to_lowercase(), app_name.to_lowercase());
    BRIDGES
        .iter()
        .any(|b| id == *b || name == *b || id.starts_with("org.kde.kdeconnect"))
}

/// Pide el texto de una respuesta con zenity (o kdialog). `None` si se
/// canceló o no hay ninguno instalado.
async fn ask_reply(title: &str, text: &str) -> Option<String> {
    let prompt = format!("{title}\n{text}");
    let attempts: [(&str, Vec<String>); 2] = [
        (
            "zenity",
            vec![
                "--entry".into(),
                "--title=Responder".into(),
                format!("--text={prompt}"),
            ],
        ),
        (
            "kdialog",
            vec![
                "--title".into(),
                "Responder".into(),
                "--inputbox".into(),
                prompt.clone(),
            ],
        ),
    ];
    for (program, args) in attempts {
        match tokio::process::Command::new(program)
            .args(&args)
            .output()
            .await
        {
            Ok(out) if out.status.success() => {
                let reply = String::from_utf8_lossy(&out.stdout)
                    .trim_end_matches('\n')
                    .to_string();
                return (!reply.is_empty()).then_some(reply);
            }
            Ok(_) => return None, // el usuario canceló
            Err(_) => continue,   // no instalado: probar el siguiente
        }
    }
    tracing::warn!("ni zenity ni kdialog están instalados: no se puede pedir la respuesta");
    None
}
