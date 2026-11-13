//! [`LinkManager`]: núcleo del enlace persistente, independiente de la
//! plataforma. Recibe streams ya conectados de cualquier transporte (TCP,
//! RFCOMM, un `BluetoothSocket` de Android puenteado…), conduce el
//! handshake, mantiene vivo el enlace y enruta notificaciones y cambios de
//! configuración. Hacia la plataforma habla con [`LinkEvent`]s y con los
//! métodos públicos de [`LinkManager`].
//!
//! ## Handshake del enlace (tras UKEY2, con secuencia estricta)
//!
//! ```text
//! ambos:        Hello  →  ← Hello          (identidad + clave pública)
//! ambos:        AuthProof → ← AuthProof    (firma de auth_string, ver pairing.rs)
//! iniciador:    Ready | PairRequest | Unpair
//! respondedor:  Ready | Unpair             (si recibió Ready)
//!               — o — flujo de PIN         (si recibió PairRequest)
//! emparejado:   PairResponse → ← PairResponse   (cada lado tras confirmar el usuario)
//! enlazado:     DirectionConfig/AppFilter (estado completo), luego tráfico
//!               normal + Ping/Pong cada `ping_interval`
//! ```

use kushare_proto::link::{self as pb, link_frame::Kind};
use kushare_transfer::TransferSession;
use prost::Message;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::identity::{DeviceIdentity, encode_hex};
use crate::pairing::{Role, make_auth_proof, pin_for, verify_auth_proof};
use crate::store::{Direction, PeerRecord, PeerStore, Versioned, note_app};

/// Versión del protocolo del enlace (`Hello.protocol_version`).
pub const PROTOCOL_VERSION: u32 = 1;

/// Cualquier transporte de bytes ordenado y fiable.
pub trait LinkStream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> LinkStream for T {}

/// Por dónde llegó una conexión. Solo informativo, salvo `Bluetooth`, cuya
/// dirección se recuerda para reconectar por RFCOMM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Tcp(SocketAddr),
    Bluetooth(String),
    Other(String),
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transport::Tcp(addr) => write!(f, "wifi ({addr})"),
            Transport::Bluetooth(addr) => write!(f, "bluetooth ({addr})"),
            Transport::Other(name) => f.write_str(name),
        }
    }
}

/// Para qué abre el iniciador la conexión.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectIntent {
    /// Emparejar (o volver a emparejar) con PIN.
    Pair,
    /// Reconectar con un peer ya emparejado, sin intervención del usuario.
    Reconnect,
    /// Enlace adicional de datos (modo híbrido) con un peer emparejado:
    /// lleva solo el tráfico pesado y se cierra solo tras `bulk_idle`.
    Bulk,
}

#[derive(Debug, Clone)]
pub struct LinkConfig {
    pub handshake_timeout: Duration,
    pub pairing_timeout: Duration,
    /// Keepalive por TCP: los routers y NAT cierran conexiones inactivas,
    /// y una caída de Wi-Fi no siempre se nota en el socket.
    pub ping_interval: Duration,
    pub idle_timeout: Duration,
    /// Keepalive por Bluetooth, mucho más espaciado para ahorrar energía:
    /// si el otro dispositivo se aleja o apaga el Bluetooth, la propia pila
    /// lo detecta (supervision timeout) y el socket falla enseguida; el ping
    /// solo hace falta para detectar un peer colgado. Debe ser menor que el
    /// `bt_idle_timeout` del OTRO lado.
    pub bt_ping_interval: Duration,
    pub bt_idle_timeout: Duration,
    pub write_timeout: Duration,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    /// El enlace de datos se cierra tras este tiempo sin tráfico (para que
    /// la radio Wi-Fi vuelva a ahorro de energía).
    pub bulk_idle: Duration,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(15),
            pairing_timeout: Duration::from_secs(60),
            ping_interval: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(90),
            bt_ping_interval: Duration::from_secs(240),
            bt_idle_timeout: Duration::from_secs(600),
            write_timeout: Duration::from_secs(15),
            reconnect_min: Duration::from_secs(5),
            reconnect_max: Duration::from_secs(120),
            bulk_idle: Duration::from_secs(30),
        }
    }
}

impl LinkConfig {
    /// Configuración para móviles: reintentos de reconexión mucho más
    /// espaciados (cada intento despierta la CPU y la radio).
    pub fn mobile() -> Self {
        Self {
            reconnect_min: Duration::from_secs(30),
            reconnect_max: Duration::from_secs(15 * 60),
            ..Self::default()
        }
    }

    fn keepalive(&self, transport: &Transport) -> (Duration, Duration) {
        match transport {
            Transport::Bluetooth(_) => (self.bt_ping_interval, self.bt_idle_timeout),
            _ => (self.ping_interval, self.idle_timeout),
        }
    }
}

/// Cómo se presenta este dispositivo en el `Hello`.
#[derive(Debug, Clone)]
pub struct LocalDevice {
    pub name: String,
    /// "desktop" | "android".
    pub kind: String,
    pub bt_address: Option<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationAction {
    pub id: String,
    pub label: String,
    pub is_reply: bool,
}

/// Una notificación de ESTE dispositivo, para reenviar a los peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationData {
    pub key: String,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub text: String,
    pub timestamp_ms: i64,
    /// PNG pequeño (~64 px). Se envía a cada peer solo una vez por sesión.
    pub icon_png: Option<Vec<u8>>,
    pub actions: Vec<NotificationAction>,
}

/// Una notificación de un peer, lista para mostrarse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteNotification {
    pub key: String,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub text: String,
    pub timestamp_ms: i64,
    /// Icono ya guardado en la caché local, si el peer mandó uno.
    pub icon_path: Option<PathBuf>,
    pub actions: Vec<NotificationAction>,
}

/// Estado de un peer emparejado, para la UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub device_id: String,
    pub device_name: String,
    pub device_kind: String,
    pub connected: bool,
    pub transport: Option<String>,
    /// Transporte del enlace de datos abierto ahora (modo híbrido), si hay.
    pub bulk_transport: Option<String>,
    /// Dirección Bluetooth conocida del peer (para reconectar por RFCOMM).
    pub bt_address: Option<String>,
    pub outgoing_enabled: bool,
    pub incoming_enabled: bool,
    pub outgoing_blocked: Vec<String>,
    pub incoming_blocked: Vec<String>,
    /// Apps del peer vistas alguna vez: (app_id, nombre).
    pub peer_apps: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkEvent {
    /// Hay que mostrar el PIN y pedir confirmación; responder con
    /// [`LinkManager::respond_pairing`].
    PairingRequested {
        device_id: String,
        device_name: String,
        pin: String,
        initiated_locally: bool,
    },
    PairingFinished {
        device_id: String,
        device_name: String,
        accepted: bool,
    },
    PeerConnected {
        device_id: String,
        device_name: String,
        transport: String,
    },
    PeerDisconnected {
        device_id: String,
    },
    /// El peer fue olvidado (por este lado o porque el otro lo pidió).
    PeerUnpaired {
        device_id: String,
    },
    /// Cambió la configuración de un peer (localmente o por sincronización).
    ConfigChanged {
        device_id: String,
    },
    /// Mostrar (o actualizar, si la key ya existe) una notificación del peer.
    NotificationPosted {
        device_id: String,
        notification: RemoteNotification,
    },
    /// La notificación del peer desapareció: cerrar su espejo local.
    NotificationRemoved {
        device_id: String,
        key: String,
    },
    /// El usuario descartó en el peer el espejo de una notificación NUESTRA:
    /// cancelar la original.
    DismissLocal {
        device_id: String,
        key: String,
    },
    /// Ejecutar la acción `action_id` de nuestra notificación `key`.
    ActionRequested {
        device_id: String,
        key: String,
        action_id: String,
    },
    /// Responder con `text` a nuestra notificación `key`.
    ReplyRequested {
        device_id: String,
        key: String,
        action_id: String,
        text: String,
    },
    /// Mensaje de una función extra (batería, multimedia, fotos…). El
    /// enlace no lo interpreta: lo hace la plataforma.
    Feature {
        device_id: String,
        feature: pb::Feature,
    },
}

/// Abre conexiones salientes hacia peers emparejados (lo implementa cada
/// plataforma: TCP a la última dirección vista por mDNS, RFCOMM…).
pub trait Dialer: Send + Sync + 'static {
    fn dial(&self, peer: PeerRecord) -> DialFuture;
}

pub type DialFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<(Box<dyn LinkStream>, Transport)>> + Send>>;

/// Ver el módulo. Clonar es barato.
#[derive(Clone)]
pub struct LinkManager {
    inner: Arc<Inner>,
}

struct Inner {
    identity: DeviceIdentity,
    local: LocalDevice,
    config: LinkConfig,
    store: Mutex<PeerStore>,
    icon_dir: PathBuf,
    links: Mutex<HashMap<String, ActiveLink>>,
    /// Enlaces de datos (modo híbrido), aparte de los de control.
    bulk: Mutex<HashMap<String, ActiveLink>>,
    pairings: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    events: mpsc::UnboundedSender<LinkEvent>,
    next_conn_id: AtomicU64,
    reconnect_wake: Notify,
    /// Puerto TCP del enlace en este dispositivo (0 = desconocido), que se
    /// anuncia en el `Hello` (ver `set_tcp_port`).
    tcp_port: std::sync::atomic::AtomicU16,
}

struct ActiveLink {
    conn_id: u64,
    initiator_id: String,
    transport: String,
    outbound: mpsc::Sender<pb::LinkFrame>,
    sent_icons: Arc<Mutex<HashSet<String>>>,
    /// Última actividad (ms UNIX) en cualquier sentido; solo la usa el
    /// enlace de datos para cerrarse cuando no se usa.
    activity: Arc<std::sync::atomic::AtomicU64>,
    /// Soltarlo (al reemplazar o cerrar el enlace) termina su bucle.
    _cancel: oneshot::Sender<()>,
}

/// Tamaño de la cola de salida de cada enlace. Si se llena (el peer no
/// lee), los mensajes se descartan y el keepalive acaba cerrando el enlace.
const OUTBOUND_QUEUE: usize = 256;

/// Aborta tareas auxiliares (lector del stream) al soltarse.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Un enlace ya autenticado, antes de registrarse.
struct Established {
    bulk: bool,
    peer_id: String,
    peer_name: String,
    initiator_id: String,
    writer: Box<dyn FrameWriter>,
    inbound: mpsc::Receiver<pb::LinkFrame>,
    reader_guard: AbortOnDrop,
}

/// Mitad de escritura del canal seguro, con el tipo de stream borrado.
trait FrameWriter: Send {
    fn send<'a>(
        &'a mut self,
        frame: &'a pb::LinkFrame,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;
}

impl<W: AsyncWrite + Unpin + Send> FrameWriter for kushare_transfer::SessionWriter<W> {
    fn send<'a>(
        &'a mut self,
        frame: &'a pb::LinkFrame,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move { self.send_raw(&frame.encode_to_vec()).await })
    }
}

fn frame(kind: Kind) -> pb::LinkFrame {
    pb::LinkFrame { kind: Some(kind) }
}

impl LinkManager {
    /// Abre (o crea) la identidad y los peers en `config_dir`
    /// (`identity.key`, `peers.toml`) y usa `cache_dir/icons` para los
    /// iconos recibidos. Devuelve el gestor y el receptor de eventos (un
    /// único consumidor: la plataforma).
    pub fn open(
        config_dir: &Path,
        cache_dir: &Path,
        local: LocalDevice,
        config: LinkConfig,
    ) -> anyhow::Result<(Self, mpsc::UnboundedReceiver<LinkEvent>)> {
        let identity = DeviceIdentity::load_or_create(&config_dir.join("identity.key"))?;
        let store = PeerStore::load(&config_dir.join("peers.toml"))?;
        Ok(Self::with_parts(
            identity,
            store,
            cache_dir.join("icons"),
            local,
            config,
        ))
    }

    pub fn with_parts(
        identity: DeviceIdentity,
        store: PeerStore,
        icon_dir: PathBuf,
        local: LocalDevice,
        config: LinkConfig,
    ) -> (Self, mpsc::UnboundedReceiver<LinkEvent>) {
        let (events, events_rx) = mpsc::unbounded_channel();
        let manager = Self {
            inner: Arc::new(Inner {
                identity,
                local,
                config,
                store: Mutex::new(store),
                icon_dir,
                links: Mutex::new(HashMap::new()),
                bulk: Mutex::new(HashMap::new()),
                pairings: Mutex::new(HashMap::new()),
                events,
                next_conn_id: AtomicU64::new(1),
                reconnect_wake: Notify::new(),
                tcp_port: std::sync::atomic::AtomicU16::new(0),
            }),
        };
        (manager, events_rx)
    }

    pub fn device_id(&self) -> &str {
        self.inner.identity.device_id()
    }

    pub fn device_name(&self) -> &str {
        &self.inner.local.name
    }

    /// Puerto en el que este dispositivo acepta el enlace por TCP; se manda
    /// en el `Hello` para que el peer pueda reconectar por Wi-Fi sin
    /// escanear la red.
    pub fn set_tcp_port(&self, port: u16) {
        self.inner.tcp_port.store(port, Ordering::Relaxed);
    }

    /// Reintentar ya la reconexión con los peers desconectados (p. ej. la
    /// plataforma detectó que volvió el Wi-Fi o que un peer se conectó por
    /// Bluetooth), sin esperar al backoff.
    pub fn wake_reconnect(&self) {
        self.inner.reconnect_wake.notify_one();
    }

    // -----------------------------------------------------------------
    // Conexiones
    // -----------------------------------------------------------------

    /// Abre el enlace como iniciador sobre un stream ya conectado. Vuelve
    /// cuando el enlace queda establecido (devuelve el `device_id` del
    /// peer) o falla; el enlace sigue vivo en segundo plano.
    pub async fn connect(
        &self,
        stream: impl LinkStream,
        transport: Transport,
        intent: ConnectIntent,
    ) -> anyhow::Result<String> {
        self.run_connection(Box::new(stream), transport, Role::Initiator, intent)
            .await
    }

    /// Atiende una conexión entrante. Igual que [`Self::connect`].
    pub async fn accept(
        &self,
        stream: impl LinkStream,
        transport: Transport,
    ) -> anyhow::Result<String> {
        self.run_connection(
            Box::new(stream),
            transport,
            Role::Responder,
            ConnectIntent::Reconnect,
        )
        .await
    }

    async fn run_connection(
        &self,
        stream: Box<dyn LinkStream>,
        transport: Transport,
        role: Role,
        intent: ConnectIntent,
    ) -> anyhow::Result<String> {
        let established = self.establish(stream, &transport, role, intent).await?;
        let peer_id = established.peer_id.clone();
        self.start_link(established, transport)?;
        Ok(peer_id)
    }

    async fn establish(
        &self,
        stream: Box<dyn LinkStream>,
        transport: &Transport,
        role: Role,
        intent: ConnectIntent,
    ) -> anyhow::Result<Established> {
        let cfg = &self.inner.config;
        let mut session = tokio::time::timeout(cfg.handshake_timeout, async {
            match role {
                Role::Initiator => TransferSession::handshake_as_initiator(stream).await,
                Role::Responder => TransferSession::handshake_as_responder(stream).await,
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("tiempo agotado en el handshake UKEY2"))??;
        session.set_strict_sequence(true);
        let auth_string = *session.auth_string();
        let (writer, mut reader) = session.into_split();
        let mut writer: Box<dyn FrameWriter> = Box::new(writer);

        // Lector dedicado: así esperar un mensaje es cancelable (select!)
        // sin riesgo de perder un frame leído a medias.
        let (in_tx, mut inbound) = mpsc::channel::<pb::LinkFrame>(64);
        let reader_task = tokio::spawn(async move {
            loop {
                match reader.recv_raw().await {
                    Ok(bytes) => match pb::LinkFrame::decode(bytes.as_slice()) {
                        Ok(frame) => {
                            if in_tx.send(frame).await.is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::warn!(?err, "LinkFrame inválido; se cierra el enlace");
                            break;
                        }
                    },
                    Err(err) => {
                        tracing::debug!(?err, "el stream del enlace se cerró");
                        break;
                    }
                }
            }
        });
        let reader_guard = AbortOnDrop(reader_task.abort_handle());

        let timeout = cfg.handshake_timeout;
        let local_id = self.device_id().to_string();

        // 1. Hello
        let local = &self.inner.local;
        writer
            .send(&frame(Kind::Hello(pb::Hello {
                protocol_version: PROTOCOL_VERSION,
                device_id: local_id.clone(),
                device_name: local.name.clone(),
                public_key: self.inner.identity.public_key().to_vec(),
                device_kind: local.kind.clone(),
                capabilities: local.capabilities.clone(),
                bt_address: local.bt_address.clone().unwrap_or_default(),
                tcp_port: u32::from(self.inner.tcp_port.load(Ordering::Relaxed)),
            })))
            .await?;
        let Kind::Hello(hello) = recv_kind(&mut inbound, timeout, "Hello").await? else {
            anyhow::bail!("se esperaba Hello");
        };
        validate_hello(&hello, &local_id)?;
        let peer_id = hello.device_id.clone();
        let peer_name: String = hello.device_name.chars().take(64).collect();

        // 2. AuthProof
        writer
            .send(&frame(Kind::AuthProof(pb::AuthProof {
                signature: make_auth_proof(&self.inner.identity, role, &auth_string).to_vec(),
            })))
            .await?;
        let Kind::AuthProof(proof) = recv_kind(&mut inbound, timeout, "AuthProof").await? else {
            anyhow::bail!("se esperaba AuthProof");
        };
        if !verify_auth_proof(
            &hello.public_key,
            role.peer(),
            &auth_string,
            &proof.signature,
        ) {
            anyhow::bail!("AuthProof inválido de {peer_id}: el peer no posee la clave que anuncia");
        }

        // 3. ¿Lo conocemos?
        let peer_key_hex = encode_hex(&hello.public_key);
        let known = {
            let store = self.inner.store.lock().expect("mutex envenenado");
            match store.get(&peer_id) {
                Some(rec) if rec.public_key == peer_key_hex => true,
                Some(_) => anyhow::bail!(
                    "{peer_id} se presenta con una clave distinta a la emparejada; se rechaza"
                ),
                None => false,
            }
        };

        let mut bulk = intent == ConnectIntent::Bulk;
        let pair = match role {
            Role::Initiator => match (intent, known) {
                (ConnectIntent::Pair, _) => {
                    writer
                        .send(&frame(Kind::PairRequest(pb::PairRequest {})))
                        .await?;
                    true
                }
                (ConnectIntent::Reconnect, true) => {
                    writer
                        .send(&frame(Kind::Ready(pb::Ready { bulk: false })))
                        .await?;
                    match recv_kind(&mut inbound, timeout, "Ready").await? {
                        Kind::Ready(_) => false,
                        Kind::Unpair(_) => {
                            self.forget_peer(&peer_id);
                            anyhow::bail!("{peer_id} ya no nos tiene emparejados");
                        }
                        other => anyhow::bail!("respuesta inesperada a Ready: {other:?}"),
                    }
                }
                (ConnectIntent::Bulk, true) => {
                    writer
                        .send(&frame(Kind::Ready(pb::Ready { bulk: true })))
                        .await?;
                    match recv_kind(&mut inbound, timeout, "Ready").await? {
                        Kind::Ready(r) if r.bulk => false,
                        Kind::Ready(_) => anyhow::bail!("{peer_id} no admite enlace de datos"),
                        other => anyhow::bail!("respuesta inesperada a Ready: {other:?}"),
                    }
                }
                (ConnectIntent::Reconnect | ConnectIntent::Bulk, false) => {
                    let _ = writer.send(&frame(Kind::Unpair(pb::Unpair {}))).await;
                    anyhow::bail!("{peer_id} no está emparejado");
                }
            },
            Role::Responder => {
                match recv_kind(&mut inbound, cfg.pairing_timeout, "Ready/PairRequest").await? {
                    Kind::Ready(r) if known => {
                        bulk = r.bulk;
                        writer
                            .send(&frame(Kind::Ready(pb::Ready { bulk: r.bulk })))
                            .await?;
                        false
                    }
                    Kind::Ready(_) => {
                        let _ = writer.send(&frame(Kind::Unpair(pb::Unpair {}))).await;
                        anyhow::bail!("{peer_id} intenta reconectar pero no está emparejado");
                    }
                    Kind::PairRequest(_) => true,
                    Kind::Unpair(_) => {
                        self.forget_peer(&peer_id);
                        anyhow::bail!("{peer_id} ya no nos tiene emparejados");
                    }
                    other => anyhow::bail!("mensaje inesperado tras AuthProof: {other:?}"),
                }
            }
        };

        if pair {
            let accepted = self
                .run_pairing(
                    &mut *writer,
                    &mut inbound,
                    &peer_id,
                    &peer_name,
                    &auth_string,
                    role,
                )
                .await;
            let _ = self.inner.events.send(LinkEvent::PairingFinished {
                device_id: peer_id.clone(),
                device_name: peer_name.clone(),
                accepted: matches!(accepted, Ok(true)),
            });
            match accepted {
                Ok(true) => {
                    let mut record = PeerRecord::new(
                        peer_id.clone(),
                        peer_name.clone(),
                        hello.device_kind.clone(),
                        peer_key_hex,
                    );
                    // Re-emparejar con un peer conocido conserva lo útil.
                    if let Some(old) = self
                        .inner
                        .store
                        .lock()
                        .expect("mutex envenenado")
                        .get(&peer_id)
                    {
                        record.last_tcp_addr = old.last_tcp_addr.clone();
                        record.peer_apps = old.peer_apps.clone();
                    }
                    let mut store = self.inner.store.lock().expect("mutex envenenado");
                    store.upsert(record);
                    store.save()?;
                    tracing::info!(%peer_id, %peer_name, "peer emparejado");
                }
                Ok(false) => anyhow::bail!("emparejado con {peer_name} rechazado"),
                Err(err) => return Err(err),
            }
        }

        // 4. Actualizar lo que el peer cuenta de sí mismo.
        {
            let mut store = self.inner.store.lock().expect("mutex envenenado");
            if let Some(rec) = store.get_mut(&peer_id) {
                let mut changed = false;
                if rec.device_name != peer_name {
                    rec.device_name = peer_name.clone();
                    changed = true;
                }
                if !hello.device_kind.is_empty() && rec.device_kind != hello.device_kind {
                    rec.device_kind = hello.device_kind.clone();
                    changed = true;
                }
                let bt = match transport {
                    Transport::Bluetooth(addr) => Some(addr.clone()),
                    _ if !hello.bt_address.is_empty() => Some(hello.bt_address.clone()),
                    _ => None,
                };
                if bt.is_some() && rec.bt_address != bt {
                    rec.bt_address = bt;
                    changed = true;
                }
                // Dirección para reconectar por Wi-Fi: IP de origen de esta
                // conexión + el puerto que el peer dice escuchar.
                if let (Transport::Tcp(remote), Ok(port)) =
                    (transport, u16::try_from(hello.tcp_port))
                    && port != 0
                {
                    let addr = SocketAddr::new(remote.ip(), port).to_string();
                    if rec.last_tcp_addr.as_deref() != Some(addr.as_str()) {
                        rec.last_tcp_addr = Some(addr);
                        changed = true;
                    }
                }
                if changed {
                    store.save()?;
                }
            }
        }

        let initiator_id = match role {
            Role::Initiator => local_id,
            Role::Responder => peer_id.clone(),
        };
        Ok(Established {
            bulk,
            peer_id,
            peer_name,
            initiator_id,
            writer,
            inbound,
            reader_guard,
        })
    }

    async fn run_pairing(
        &self,
        writer: &mut dyn FrameWriter,
        inbound: &mut mpsc::Receiver<pb::LinkFrame>,
        peer_id: &str,
        peer_name: &str,
        auth_string: &[u8],
        role: Role,
    ) -> anyhow::Result<bool> {
        let (decision_tx, mut decision_rx) = oneshot::channel();
        // Una petición anterior pendiente para el mismo peer queda
        // rechazada al soltarse su `Sender`.
        self.inner
            .pairings
            .lock()
            .expect("mutex envenenado")
            .insert(peer_id.to_string(), decision_tx);
        let _ = self.inner.events.send(LinkEvent::PairingRequested {
            device_id: peer_id.to_string(),
            device_name: peer_name.to_string(),
            pin: pin_for(auth_string),
            initiated_locally: role == Role::Initiator,
        });

        let outcome = tokio::time::timeout(self.inner.config.pairing_timeout, async {
            let mut local: Option<bool> = None;
            let mut remote: Option<bool> = None;
            loop {
                match (local, remote) {
                    (Some(l), Some(r)) => return Ok(l && r),
                    (Some(false), _) | (_, Some(false)) => return Ok(false),
                    _ => {}
                }
                tokio::select! {
                    decision = &mut decision_rx, if local.is_none() => {
                        let accepted = decision.unwrap_or(false);
                        local = Some(accepted);
                        writer.send(&frame(Kind::PairResponse(pb::PairResponse { accepted }))).await?;
                    }
                    incoming = inbound.recv(), if remote.is_none() => match incoming {
                        Some(pb::LinkFrame { kind: Some(Kind::PairResponse(resp)) }) => remote = Some(resp.accepted),
                        Some(other) => anyhow::bail!("mensaje inesperado durante el emparejado: {other:?}"),
                        None => anyhow::bail!("{peer_name} cerró la conexión durante el emparejado"),
                    },
                }
            }
        })
        .await;

        self.inner
            .pairings
            .lock()
            .expect("mutex envenenado")
            .remove(peer_id);
        match outcome {
            Ok(result) => result,
            Err(_) => {
                let _ = writer
                    .send(&frame(Kind::PairResponse(pb::PairResponse {
                        accepted: false,
                    })))
                    .await;
                anyhow::bail!("tiempo agotado esperando la confirmación del PIN")
            }
        }
    }

    /// Registra el enlace (resolviendo duplicados) y lanza sus tareas.
    fn start_link(&self, est: Established, transport: Transport) -> anyhow::Result<()> {
        if est.bulk {
            return self.start_bulk_link(est, transport);
        }
        let Established {
            bulk: _,
            peer_id,
            peer_name,
            initiator_id,
            mut writer,
            mut inbound,
            reader_guard,
        } = est;
        let conn_id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let (outbound, mut out_rx) = mpsc::channel::<pb::LinkFrame>(OUTBOUND_QUEUE);
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let transport_label = transport.to_string();

        {
            let mut links = self.inner.links.lock().expect("mutex envenenado");
            if let Some(existing) = links.get(&peer_id) {
                // Dos enlaces con el mismo peer (ambos lados marcaron a la
                // vez, o uno reconectó antes de que el otro notara la caída).
                // Ambos lados aplican la misma regla y conservan el mismo:
                // - mismo iniciador → el más nuevo (el viejo está muerto);
                // - distinto iniciador → el que inició el device_id menor.
                // - Bluetooth frente a Wi-Fi → siempre el de Bluetooth (el de
                //   control debe ir por la radio de bajo consumo; lo pesado
                //   va aparte por Wi-Fi). Ambos lados ven el mismo transporte.
                let (old_bt, new_bt) = (
                    existing.transport.starts_with("bluetooth"),
                    transport_label.starts_with("bluetooth"),
                );
                let keep_new = if old_bt != new_bt {
                    new_bt
                } else {
                    existing.initiator_id == initiator_id
                        || initiator_id == std::cmp::min(self.device_id(), peer_id.as_str())
                };
                if !keep_new {
                    tracing::debug!(%peer_id, "enlace duplicado descartado");
                    anyhow::bail!("ya hay un enlace activo con {peer_id}");
                }
                tracing::debug!(%peer_id, "enlace duplicado: se reemplaza el anterior");
            }
            links.insert(
                peer_id.clone(),
                ActiveLink {
                    conn_id,
                    initiator_id,
                    transport: transport_label.clone(),
                    outbound: outbound.clone(),
                    sent_icons: Arc::default(),
                    activity: Arc::default(),
                    _cancel: cancel_tx,
                },
            );
        }

        tracing::info!(%peer_id, %peer_name, transport = %transport_label, "enlace establecido");
        let _ = self.inner.events.send(LinkEvent::PeerConnected {
            device_id: peer_id.clone(),
            device_name: peer_name,
            transport: transport_label,
        });

        // Estado completo de configuración, para reconciliar (ver store.rs).
        if let Some(rec) = self
            .inner
            .store
            .lock()
            .expect("mutex envenenado")
            .get(&peer_id)
        {
            for dir in [Direction::Outgoing, Direction::Incoming] {
                let _ = outbound.try_send(self.direction_frame(rec, dir));
                let _ = outbound.try_send(self.filter_frame(rec, dir));
            }
        }

        let write_timeout = self.inner.config.write_timeout;
        let mut writer_task = tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                match tokio::time::timeout(write_timeout, writer.send(&frame)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        tracing::debug!(?err, "fallo al escribir en el enlace");
                        break;
                    }
                    Err(_) => {
                        tracing::warn!("escritura en el enlace bloqueada; se cierra");
                        break;
                    }
                }
            }
        });

        let (ping_interval, idle_timeout) = self.inner.config.keepalive(&transport);
        let manager = self.clone();
        tokio::spawn(async move {
            let _reader_guard = reader_guard;
            let mut ticker = tokio::time::interval(ping_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // el primer tick es inmediato
            let mut last_seen = Instant::now();
            let mut nonce: u64 = 0;
            let replaced = loop {
                tokio::select! {
                    _ = &mut cancel_rx => break true,
                    _ = &mut writer_task => break false,
                    frame = inbound.recv() => match frame {
                        None => break false,
                        Some(frame) => {
                            last_seen = Instant::now();
                            if manager.handle_frame(&peer_id, frame, &outbound).is_break() {
                                break false;
                            }
                        }
                    },
                    _ = ticker.tick() => {
                        if last_seen.elapsed() > idle_timeout {
                            tracing::warn!(%peer_id, "enlace inactivo demasiado tiempo; se cierra");
                            break false;
                        }
                        nonce = nonce.wrapping_add(1);
                        let _ = outbound.try_send(frame(Kind::Ping(pb::Ping { nonce })));
                    }
                }
            };

            if !replaced {
                let removed = {
                    let mut links = manager.inner.links.lock().expect("mutex envenenado");
                    match links.get(&peer_id) {
                        Some(link) if link.conn_id == conn_id => links.remove(&peer_id).is_some(),
                        _ => false,
                    }
                };
                if removed {
                    tracing::info!(%peer_id, "enlace cerrado");
                    let _ = manager.inner.events.send(LinkEvent::PeerDisconnected {
                        device_id: peer_id.clone(),
                    });
                    manager.inner.reconnect_wake.notify_one();
                }
            }
            // `outbound` y el `writer_task` (si sigue vivo) terminan solos al
            // soltarse el último `Sender`: así un `Unpair` encolado llega a
            // salir antes de cerrar.
        });
        Ok(())
    }

    /// Registra un enlace de datos (modo híbrido): sin pings, sin eventos
    /// de conexión, y se cierra solo tras `bulk_idle` sin tráfico o si se
    /// cae el enlace de control.
    fn start_bulk_link(&self, est: Established, transport: Transport) -> anyhow::Result<()> {
        let Established {
            peer_id,
            mut writer,
            mut inbound,
            reader_guard,
            initiator_id,
            ..
        } = est;
        if !self.is_connected(&peer_id) {
            anyhow::bail!("no hay enlace de control con {peer_id}: no se abre el de datos");
        }
        let conn_id = self.inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
        let (outbound, mut out_rx) = mpsc::channel::<pb::LinkFrame>(OUTBOUND_QUEUE);
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let activity: Arc<std::sync::atomic::AtomicU64> = Arc::new(now_ms().into());
        let transport_label = transport.to_string();
        // Uno solo por peer: el nuevo sustituye al anterior.
        self.inner.bulk.lock().expect("mutex envenenado").insert(
            peer_id.clone(),
            ActiveLink {
                conn_id,
                initiator_id,
                transport: transport_label.clone(),
                outbound: outbound.clone(),
                sent_icons: Arc::default(),
                activity: activity.clone(),
                _cancel: cancel_tx,
            },
        );
        tracing::info!(%peer_id, transport = %transport_label, "enlace de datos abierto");

        let write_timeout = self.inner.config.write_timeout;
        let writer_activity = activity.clone();
        let mut writer_task = tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                writer_activity.store(now_ms(), Ordering::Relaxed);
                match tokio::time::timeout(write_timeout, writer.send(&frame)).await {
                    Ok(Ok(())) => writer_activity.store(now_ms(), Ordering::Relaxed),
                    _ => break,
                }
            }
        });
        let idle = self.inner.config.bulk_idle;
        let manager = self.clone();
        tokio::spawn(async move {
            let _reader_guard = reader_guard;
            let mut ticker = tokio::time::interval((idle / 2).min(Duration::from_secs(2)));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => break,
                    _ = &mut writer_task => break,
                    frame = inbound.recv() => match frame {
                        None => break,
                        Some(frame) => {
                            activity.store(now_ms(), Ordering::Relaxed);
                            if manager.handle_frame(&peer_id, frame, &outbound).is_break() {
                                break;
                            }
                        }
                    },
                    _ = ticker.tick() => {
                        let quiet = now_ms().saturating_sub(activity.load(Ordering::Relaxed));
                        if quiet > idle.as_millis() as u64 || !manager.is_connected(&peer_id) {
                            break;
                        }
                    }
                }
            }
            let mut bulk = manager.inner.bulk.lock().expect("mutex envenenado");
            if bulk.get(&peer_id).is_some_and(|l| l.conn_id == conn_id) {
                bulk.remove(&peer_id);
                tracing::info!(%peer_id, "enlace de datos cerrado");
            }
        });
        Ok(())
    }

    /// Cola por la que mandar `feature` a `peer_id`: lo pesado por el enlace
    /// de datos si hay uno abierto; lo demás, por el de control.
    fn lane(&self, peer_id: &str, feature: &pb::Feature) -> Option<mpsc::Sender<pb::LinkFrame>> {
        if is_heavy(feature)
            && let Some(link) = self
                .inner
                .bulk
                .lock()
                .expect("mutex envenenado")
                .get(peer_id)
        {
            link.activity.store(now_ms(), Ordering::Relaxed);
            return Some(link.outbound.clone());
        }
        self.inner
            .links
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            .map(|link| link.outbound.clone())
    }

    /// Transporte del enlace de control con `peer_id`, si está conectado.
    pub fn control_transport(&self, peer_id: &str) -> Option<String> {
        self.inner
            .links
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            .map(|l| l.transport.clone())
    }

    /// Cierra el enlace de datos con `peer_id` (p. ej. el peer dejó la Wi-Fi).
    pub fn close_bulk(&self, peer_id: &str) {
        self.inner
            .bulk
            .lock()
            .expect("mutex envenenado")
            .remove(peer_id);
    }

    /// ¿Hay un enlace de datos abierto con `peer_id`?
    pub fn has_bulk(&self, peer_id: &str) -> bool {
        self.inner
            .bulk
            .lock()
            .expect("mutex envenenado")
            .contains_key(peer_id)
    }

    /// Transporte del enlace de datos abierto con `peer_id`, si hay.
    pub fn bulk_transport(&self, peer_id: &str) -> Option<String> {
        self.inner
            .bulk
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            .map(|l| l.transport.clone())
    }

    fn handle_frame(
        &self,
        peer_id: &str,
        frame: pb::LinkFrame,
        outbound: &mpsc::Sender<pb::LinkFrame>,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};
        let Some(kind) = frame.kind else {
            return Continue(()); // variante desconocida (versión más nueva)
        };
        match kind {
            Kind::Ping(ping) => {
                let _ = outbound.try_send(pb::LinkFrame {
                    kind: Some(Kind::Pong(pb::Pong { nonce: ping.nonce })),
                });
            }
            Kind::Pong(_) => {}
            Kind::DirectionConfig(dc) => self.apply_direction(peer_id, dc),
            Kind::AppFilter(af) => self.apply_filter(peer_id, af),
            Kind::Notification(n) => self.receive_notification(peer_id, n),
            Kind::NotificationRemoved(removed) => {
                let event = if removed.owner_is_receiver {
                    if !self.peer_allows(peer_id, |rec| rec.outgoing.value) {
                        return Continue(());
                    }
                    LinkEvent::DismissLocal {
                        device_id: peer_id.to_string(),
                        key: removed.key,
                    }
                } else {
                    LinkEvent::NotificationRemoved {
                        device_id: peer_id.to_string(),
                        key: removed.key,
                    }
                };
                let _ = self.inner.events.send(event);
            }
            Kind::InvokeAction(action) => {
                // Solo sobre notificaciones que de verdad le reenviamos.
                if self.peer_allows(peer_id, |rec| rec.outgoing.value) {
                    let _ = self.inner.events.send(LinkEvent::ActionRequested {
                        device_id: peer_id.to_string(),
                        key: action.key,
                        action_id: action.action_id,
                    });
                }
            }
            Kind::Reply(reply) => {
                if self.peer_allows(peer_id, |rec| rec.outgoing.value) {
                    let _ = self.inner.events.send(LinkEvent::ReplyRequested {
                        device_id: peer_id.to_string(),
                        key: reply.key,
                        action_id: reply.action_id,
                        text: reply.text,
                    });
                }
            }
            Kind::Feature(feature) => {
                let _ = self.inner.events.send(LinkEvent::Feature {
                    device_id: peer_id.to_string(),
                    feature,
                });
            }
            Kind::Unpair(_) => {
                tracing::info!(%peer_id, "el peer pidió desemparejar");
                self.forget_peer(peer_id);
                return Break(());
            }
            other @ (Kind::Hello(_)
            | Kind::AuthProof(_)
            | Kind::PairRequest(_)
            | Kind::PairResponse(_)
            | Kind::Ready(_)) => {
                tracing::warn!(%peer_id, ?other, "mensaje de handshake fuera de lugar; se ignora");
            }
        }
        Continue(())
    }

    fn peer_allows(&self, peer_id: &str, check: impl Fn(&PeerRecord) -> bool) -> bool {
        self.inner
            .store
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            .is_some_and(check)
    }

    // -----------------------------------------------------------------
    // Sincronización de configuración
    // -----------------------------------------------------------------

    /// Sentido de un mensaje `from → to` visto desde este dispositivo.
    fn local_direction(&self, peer_id: &str, from: &str, to: &str) -> Option<Direction> {
        let me = self.device_id();
        if from == me && to == peer_id {
            Some(Direction::Outgoing)
        } else if from == peer_id && to == me {
            Some(Direction::Incoming)
        } else {
            None
        }
    }

    fn ends(&self, peer_id: &str, dir: Direction) -> (String, String) {
        let me = self.device_id().to_string();
        match dir {
            Direction::Outgoing => (me, peer_id.to_string()),
            Direction::Incoming => (peer_id.to_string(), me),
        }
    }

    fn direction_frame(&self, rec: &PeerRecord, dir: Direction) -> pb::LinkFrame {
        let (from, to) = self.ends(&rec.device_id, dir);
        let v = rec.direction(dir);
        frame(Kind::DirectionConfig(pb::DirectionConfig {
            from_device_id: from,
            to_device_id: to,
            enabled: v.value,
            version: v.version,
            updated_by: v.updated_by.clone(),
        }))
    }

    fn filter_frame(&self, rec: &PeerRecord, dir: Direction) -> pb::LinkFrame {
        let (from, to) = self.ends(&rec.device_id, dir);
        let v = rec.blocked(dir);
        frame(Kind::AppFilter(pb::AppFilter {
            from_device_id: from,
            to_device_id: to,
            blocked_app_ids: v.value.iter().cloned().collect(),
            version: v.version,
            updated_by: v.updated_by.clone(),
        }))
    }

    fn apply_direction(&self, peer_id: &str, dc: pb::DirectionConfig) {
        let Some(dir) = self.local_direction(peer_id, &dc.from_device_id, &dc.to_device_id) else {
            tracing::warn!(%peer_id, "DirectionConfig con ids ajenos; se ignora");
            return;
        };
        let incoming = Versioned {
            value: dc.enabled,
            version: dc.version,
            updated_by: dc.updated_by,
        };
        self.apply_versioned(peer_id, |rec| {
            let current = rec.direction_mut(dir);
            if incoming.supersedes(current) {
                *current = incoming;
                true
            } else {
                false
            }
        });
    }

    fn apply_filter(&self, peer_id: &str, af: pb::AppFilter) {
        let Some(dir) = self.local_direction(peer_id, &af.from_device_id, &af.to_device_id) else {
            tracing::warn!(%peer_id, "AppFilter con ids ajenos; se ignora");
            return;
        };
        let incoming = Versioned {
            value: af.blocked_app_ids.into_iter().collect::<BTreeSet<_>>(),
            version: af.version,
            updated_by: af.updated_by,
        };
        self.apply_versioned(peer_id, |rec| {
            let current = rec.blocked_mut(dir);
            if incoming.supersedes(current) {
                *current = incoming;
                true
            } else {
                false
            }
        });
    }

    fn apply_versioned(&self, peer_id: &str, apply: impl FnOnce(&mut PeerRecord) -> bool) {
        let changed = {
            let mut store = self.inner.store.lock().expect("mutex envenenado");
            let Some(rec) = store.get_mut(peer_id) else {
                return;
            };
            let changed = apply(rec);
            if changed && let Err(err) = store.save() {
                tracing::warn!(?err, "no se pudo guardar peers.toml");
            }
            changed
        };
        if changed {
            let _ = self.inner.events.send(LinkEvent::ConfigChanged {
                device_id: peer_id.to_string(),
            });
        }
    }

    /// Activa o desactiva una dirección de notificaciones con `peer_id`.
    /// Se guarda, se sincroniza con el peer si está conectado (y si no, al
    /// reconectar) y se emite `ConfigChanged`.
    pub fn set_direction(
        &self,
        peer_id: &str,
        dir: Direction,
        enabled: bool,
    ) -> anyhow::Result<()> {
        let me = self.device_id().to_string();
        self.local_change(peer_id, |rec| {
            if rec.direction(dir).value == enabled {
                return None;
            }
            rec.direction_mut(dir).bump(enabled, &me);
            Some(self.direction_frame(rec, dir))
        })
    }

    /// Bloquea o desbloquea `app_id` en una dirección con `peer_id`.
    pub fn set_app_blocked(
        &self,
        peer_id: &str,
        dir: Direction,
        app_id: &str,
        blocked: bool,
    ) -> anyhow::Result<()> {
        let me = self.device_id().to_string();
        self.local_change(peer_id, |rec| {
            let mut set = rec.blocked(dir).value.clone();
            let changed = if blocked {
                set.insert(app_id.to_string())
            } else {
                set.remove(app_id)
            };
            if !changed {
                return None;
            }
            rec.blocked_mut(dir).bump(set, &me);
            Some(self.filter_frame(rec, dir))
        })
    }

    fn local_change(
        &self,
        peer_id: &str,
        change: impl FnOnce(&mut PeerRecord) -> Option<pb::LinkFrame>,
    ) -> anyhow::Result<()> {
        let frame = {
            let mut store = self.inner.store.lock().expect("mutex envenenado");
            let rec = store
                .get_mut(peer_id)
                .ok_or_else(|| anyhow::anyhow!("{peer_id} no está emparejado"))?;
            let Some(frame) = change(rec) else {
                return Ok(());
            };
            store.save()?;
            frame
        };
        self.send_to(peer_id, frame);
        let _ = self.inner.events.send(LinkEvent::ConfigChanged {
            device_id: peer_id.to_string(),
        });
        Ok(())
    }

    // -----------------------------------------------------------------
    // Emparejado
    // -----------------------------------------------------------------

    /// Respuesta del usuario a un `PairingRequested`. Devuelve `false` si
    /// no había ninguna petición pendiente para ese peer.
    pub fn respond_pairing(&self, device_id: &str, accept: bool) -> bool {
        let pending = self
            .inner
            .pairings
            .lock()
            .expect("mutex envenenado")
            .remove(device_id);
        match pending {
            Some(tx) => tx.send(accept).is_ok(),
            None => false,
        }
    }

    /// Olvida a `peer_id` en ambos lados (si está conectado se le avisa).
    pub fn unpair(&self, peer_id: &str) -> anyhow::Result<()> {
        self.send_to(peer_id, frame(Kind::Unpair(pb::Unpair {})));
        if !self.forget_peer(peer_id) {
            anyhow::bail!("{peer_id} no está emparejado");
        }
        Ok(())
    }

    /// Borra el peer del almacén, cierra su enlace y emite `PeerUnpaired`.
    fn forget_peer(&self, peer_id: &str) -> bool {
        let removed = {
            let mut store = self.inner.store.lock().expect("mutex envenenado");
            let removed = store.remove(peer_id).is_some();
            if removed && let Err(err) = store.save() {
                tracing::warn!(?err, "no se pudo guardar peers.toml");
            }
            removed
        };
        // Soltar el ActiveLink cancela su bucle; lo encolado aún sale.
        self.inner
            .links
            .lock()
            .expect("mutex envenenado")
            .remove(peer_id);
        self.inner
            .bulk
            .lock()
            .expect("mutex envenenado")
            .remove(peer_id);
        if removed {
            let _ = self.inner.events.send(LinkEvent::PeerUnpaired {
                device_id: peer_id.to_string(),
            });
        }
        removed
    }

    // -----------------------------------------------------------------
    // Notificaciones
    // -----------------------------------------------------------------

    /// Reenvía una notificación de este dispositivo a cada peer conectado
    /// que la acepte (dirección activada y app no bloqueada). Repetir la
    /// misma `key` la actualiza en el peer.
    pub fn post_local_notification(&self, n: NotificationData) {
        if self.is_echo_of_own_mirror(&n) {
            tracing::debug!(app = %n.app_id, "eco de un espejo de kushare; no se reenvía");
            return;
        }
        let icon = n
            .icon_png
            .as_ref()
            .filter(|png| !png.is_empty())
            .map(|png| (encode_hex(&Sha256::digest(png)), png));

        let targets: Vec<String> = {
            let mut store = self.inner.store.lock().expect("mutex envenenado");
            if store.note_local_app(&n.app_id, &n.app_name)
                && let Err(err) = store.save()
            {
                tracing::warn!(?err, "no se pudo guardar peers.toml");
            }
            store
                .peers()
                .filter(|rec| rec.allows_outgoing(&n.app_id))
                .map(|rec| rec.device_id.clone())
                .collect()
        };

        let links = self.inner.links.lock().expect("mutex envenenado");
        for peer_id in targets {
            let Some(link) = links.get(&peer_id) else {
                continue;
            };
            let (icon_hash, icon_png) = match &icon {
                Some((hash, png)) => {
                    let first_time = link
                        .sent_icons
                        .lock()
                        .expect("mutex envenenado")
                        .insert(hash.clone());
                    (
                        hash.clone(),
                        if first_time { png.to_vec() } else { Vec::new() },
                    )
                }
                None => (String::new(), Vec::new()),
            };
            let msg = frame(Kind::Notification(pb::Notification {
                key: n.key.clone(),
                app_id: n.app_id.clone(),
                app_name: n.app_name.clone(),
                title: n.title.clone(),
                text: n.text.clone(),
                timestamp_ms: n.timestamp_ms,
                icon_hash,
                icon_png,
                actions: n
                    .actions
                    .iter()
                    .map(|a| pb::NotificationAction {
                        id: a.id.clone(),
                        label: a.label.clone(),
                        is_reply: a.is_reply,
                    })
                    .collect(),
            }));
            if link.outbound.try_send(msg).is_err() {
                tracing::warn!(%peer_id, "cola del enlace llena; notificación descartada");
            }
        }
    }

    /// ¿Es `n` un espejo que kushare mostró aquí, devuelto por OTRO puente
    /// de notificaciones (p. ej. KDE Connect, que reenvía todo lo del
    /// escritorio al teléfono)? Los espejos de kushare llevan
    /// "<app> · <dispositivo de origen>" en el nombre de app o subtítulo;
    /// si una notificación local menciona " · <este dispositivo>", es el
    /// eco de una notificación nuestra y reenviarla crearía un bucle
    /// infinito entre los dos puentes (visto en pruebas reales).
    fn is_echo_of_own_mirror(&self, n: &NotificationData) -> bool {
        let signature = format!(" · {}", self.inner.local.name);
        [&n.app_name, &n.title, &n.text]
            .iter()
            .any(|field| field.contains(&signature))
    }

    /// Una notificación de este dispositivo desapareció: avisar a los peers
    /// para que cierren su espejo.
    pub fn remove_local_notification(&self, key: &str) {
        let targets: Vec<String> = {
            let store = self.inner.store.lock().expect("mutex envenenado");
            store
                .peers()
                .filter(|rec| rec.outgoing.value)
                .map(|rec| rec.device_id.clone())
                .collect()
        };
        for peer_id in targets {
            self.send_to(
                &peer_id,
                frame(Kind::NotificationRemoved(pb::NotificationRemoved {
                    key: key.to_string(),
                    owner_is_receiver: false,
                })),
            );
        }
    }

    /// El usuario descartó aquí el espejo de la notificación `key` de
    /// `peer_id`: pedirle que cancele la original.
    pub fn dismiss_remote_notification(&self, peer_id: &str, key: &str) {
        self.send_to(
            peer_id,
            frame(Kind::NotificationRemoved(pb::NotificationRemoved {
                key: key.to_string(),
                owner_is_receiver: true,
            })),
        );
    }

    pub fn invoke_remote_action(&self, peer_id: &str, key: &str, action_id: &str) {
        self.send_to(
            peer_id,
            frame(Kind::InvokeAction(pb::InvokeAction {
                key: key.to_string(),
                action_id: action_id.to_string(),
            })),
        );
    }

    pub fn reply_remote(&self, peer_id: &str, key: &str, action_id: &str, text: &str) {
        self.send_to(
            peer_id,
            frame(Kind::Reply(pb::Reply {
                key: key.to_string(),
                action_id: action_id.to_string(),
                text: text.to_string(),
            })),
        );
    }

    /// Manda un mensaje de función extra a `peer_id` (si está conectado).
    pub fn send_feature(&self, peer_id: &str, feature: pb::Feature) {
        if let Some(lane) = self.lane(peer_id, &feature)
            && lane.try_send(frame(Kind::Feature(feature))).is_err()
        {
            tracing::warn!(%peer_id, "cola del enlace llena; mensaje descartado");
        }
    }

    /// Como [`send_feature`](Self::send_feature), pero espera a que haya
    /// sitio en la cola del enlace en vez de descartar el mensaje: para
    /// respuestas grandes troceadas (fotos) en las que perder un trozo
    /// corrompería el resultado.
    pub async fn send_feature_wait(
        &self,
        peer_id: &str,
        feature: pb::Feature,
    ) -> anyhow::Result<()> {
        let outbound = self
            .lane(peer_id, &feature)
            .ok_or_else(|| anyhow::anyhow!("{peer_id} no está conectado"))?;
        outbound
            .send(frame(Kind::Feature(feature)))
            .await
            .map_err(|_| anyhow::anyhow!("el enlace con {peer_id} se cerró"))
    }

    /// Como [`send_feature_wait`](Self::send_feature_wait), pero de baja
    /// prioridad: solo encola cuando la cola del enlace está (casi) vacía,
    /// así una descarga de fondo no retrasa notificaciones ni órdenes.
    pub async fn send_feature_idle(
        &self,
        peer_id: &str,
        feature: pb::Feature,
    ) -> anyhow::Result<()> {
        loop {
            let free = self
                .lane(peer_id, &feature)
                .map(|lane| lane.capacity())
                .ok_or_else(|| anyhow::anyhow!("{peer_id} no está conectado"))?;
            if free + 1 >= OUTBOUND_QUEUE {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        self.send_feature_wait(peer_id, feature).await
    }

    /// Manda un mensaje de función extra a todos los peers conectados.
    pub fn broadcast_feature(&self, feature: pb::Feature) {
        let ids: Vec<String> = self
            .inner
            .links
            .lock()
            .expect("mutex envenenado")
            .keys()
            .cloned()
            .collect();
        for id in ids {
            self.send_feature(&id, feature.clone());
        }
    }

    fn receive_notification(&self, peer_id: &str, n: pb::Notification) {
        {
            let mut store = self.inner.store.lock().expect("mutex envenenado");
            let Some(rec) = store.get_mut(peer_id) else {
                return;
            };
            let new_app = note_app(&mut rec.peer_apps, &n.app_id, &n.app_name);
            let allowed = rec.allows_incoming(&n.app_id);
            if new_app && let Err(err) = store.save() {
                tracing::warn!(?err, "no se pudo guardar peers.toml");
            }
            if !allowed {
                tracing::debug!(%peer_id, app = %n.app_id, "notificación filtrada");
                return;
            }
        }

        let icon_path = self.cache_icon(&n.icon_hash, &n.icon_png);
        let _ = self.inner.events.send(LinkEvent::NotificationPosted {
            device_id: peer_id.to_string(),
            notification: RemoteNotification {
                key: n.key,
                app_id: n.app_id,
                app_name: n.app_name,
                title: n.title,
                text: n.text,
                timestamp_ms: n.timestamp_ms,
                icon_path,
                actions: n
                    .actions
                    .into_iter()
                    .map(|a| NotificationAction {
                        id: a.id,
                        label: a.label,
                        is_reply: a.is_reply,
                    })
                    .collect(),
            },
        });
    }

    /// Guarda (si hace falta) y devuelve la ruta del icono `hash`. El hash
    /// se valida (64 hex, coincide con el PNG) antes de usarlo como nombre
    /// de archivo: un peer no puede escribir fuera de la caché ni
    /// envenenarla con un icono que no corresponde a su hash.
    /// Guarda (si hace falta) y devuelve la ruta del icono `hash` de la
    /// caché compartida de iconos. `png` puede ir vacío si ya estaba.
    pub fn cached_icon(&self, hash: &str, png: &[u8]) -> Option<PathBuf> {
        self.cache_icon(hash, png)
    }

    /// Hashes de los iconos que ya están en la caché (para no pedirlos otra vez).
    pub fn cached_icon_hashes(&self) -> Vec<String> {
        std::fs::read_dir(&self.inner.icon_dir)
            .map(|dir| {
                dir.flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        name.strip_suffix(".png").map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn cache_icon(&self, hash: &str, png: &[u8]) -> Option<PathBuf> {
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let path = self.inner.icon_dir.join(format!("{hash}.png"));
        if path.exists() {
            return Some(path);
        }
        if png.is_empty() || encode_hex(&Sha256::digest(png)) != hash {
            return None;
        }
        let write = std::fs::create_dir_all(&self.inner.icon_dir).and_then(|()| {
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, png)?;
            std::fs::rename(&tmp, &path)
        });
        match write {
            Ok(()) => Some(path),
            Err(err) => {
                tracing::warn!(?err, "no se pudo guardar el icono en caché");
                None
            }
        }
    }

    fn send_to(&self, peer_id: &str, frame: pb::LinkFrame) {
        if let Some(link) = self
            .inner
            .links
            .lock()
            .expect("mutex envenenado")
            .get(peer_id)
            && link.outbound.try_send(frame).is_err()
        {
            tracing::warn!(%peer_id, "cola del enlace llena; mensaje descartado");
        }
    }

    // -----------------------------------------------------------------
    // Consulta, descubrimiento y reconexión
    // -----------------------------------------------------------------

    /// Cierra todos los enlaces activos y rechaza los emparejados en curso
    /// (al "apagar" kushare). No olvida a ningún peer: al volver a abrir el
    /// enlace, la reconexión es automática.
    pub fn disconnect_all(&self) {
        self.inner
            .pairings
            .lock()
            .expect("mutex envenenado")
            .clear();
        self.inner.bulk.lock().expect("mutex envenenado").clear();
        let closed: Vec<String> = self
            .inner
            .links
            .lock()
            .expect("mutex envenenado")
            .drain()
            .map(|(id, _)| id)
            .collect();
        for device_id in closed {
            let _ = self
                .inner
                .events
                .send(LinkEvent::PeerDisconnected { device_id });
        }
    }

    pub fn is_connected(&self, peer_id: &str) -> bool {
        self.inner
            .links
            .lock()
            .expect("mutex envenenado")
            .contains_key(peer_id)
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        let store = self.inner.store.lock().expect("mutex envenenado");
        let links = self.inner.links.lock().expect("mutex envenenado");
        let bulk = self.inner.bulk.lock().expect("mutex envenenado");
        store
            .peers()
            .map(|rec| {
                let link = links.get(&rec.device_id);
                PeerInfo {
                    device_id: rec.device_id.clone(),
                    device_name: rec.device_name.clone(),
                    device_kind: rec.device_kind.clone(),
                    connected: link.is_some(),
                    transport: link.map(|l| l.transport.clone()),
                    bulk_transport: bulk.get(&rec.device_id).map(|l| l.transport.clone()),
                    bt_address: rec.bt_address.clone(),
                    outgoing_enabled: rec.outgoing.value,
                    incoming_enabled: rec.incoming.value,
                    outgoing_blocked: rec.outgoing_blocked.value.iter().cloned().collect(),
                    incoming_blocked: rec.incoming_blocked.value.iter().cloned().collect(),
                    peer_apps: rec
                        .peer_apps
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                }
            })
            .collect()
    }

    /// Apps de este dispositivo vistas alguna vez: (app_id, nombre).
    pub fn local_apps(&self) -> Vec<(String, String)> {
        let store = self.inner.store.lock().expect("mutex envenenado");
        store
            .local_apps()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// `peer_id` se anuncia por mDNS en `addr`: se recuerda y, si no está
    /// conectado, se despierta al bucle de reconexión.
    pub fn note_sighting(&self, peer_id: &str, addr: SocketAddr) {
        let mut store = self.inner.store.lock().expect("mutex envenenado");
        let Some(rec) = store.get_mut(peer_id) else {
            return;
        };
        let addr = addr.to_string();
        if rec.last_tcp_addr.as_deref() != Some(addr.as_str()) {
            rec.last_tcp_addr = Some(addr);
            if let Err(err) = store.save() {
                tracing::warn!(?err, "no se pudo guardar peers.toml");
            }
        }
        drop(store);
        if !self.is_connected(peer_id) {
            self.inner.reconnect_wake.notify_one();
        }
    }

    /// Bucle de reconexión: intenta, con backoff exponencial, enlazar con
    /// cada peer emparejado que no esté conectado. Entre intentos duerme
    /// hasta el siguiente que toque (no se despierta periódicamente: en un
    /// móvil cada despertar cuesta batería), o hasta que algo lo despierte
    /// (`note_sighting`, `wake_reconnect`, la caída de un enlace).
    /// Corre indefinidamente; lanzarlo con `tokio::spawn`.
    pub async fn run_reconnect(&self, dialer: impl Dialer) {
        let cfg = self.inner.config.clone();
        let mut backoff: HashMap<String, (Instant, Duration)> = HashMap::new();
        loop {
            let candidates: Vec<PeerRecord> = {
                let store = self.inner.store.lock().expect("mutex envenenado");
                store
                    .peers()
                    .filter(|rec| !self.is_connected(&rec.device_id))
                    .cloned()
                    .collect()
            };
            backoff.retain(|id, _| candidates.iter().any(|rec| &rec.device_id == id));

            for peer in &candidates {
                if backoff
                    .get(&peer.device_id)
                    .is_some_and(|(next, _)| Instant::now() < *next)
                {
                    continue;
                }
                let peer_id = peer.device_id.clone();
                let result = match dialer.dial(peer.clone()).await {
                    Ok((stream, transport)) => {
                        self.connect(stream, transport, ConnectIntent::Reconnect)
                            .await
                    }
                    Err(err) => Err(err),
                };
                match result {
                    Ok(_) => {
                        backoff.remove(&peer_id);
                    }
                    Err(err) => {
                        let delay = backoff
                            .get(&peer_id)
                            .map(|(_, d)| (*d * 2).min(cfg.reconnect_max))
                            .unwrap_or(cfg.reconnect_min);
                        tracing::debug!(%peer_id, ?err, ?delay, "reconexión fallida");
                        backoff.insert(peer_id, (Instant::now() + delay, delay));
                    }
                }
            }

            // Dormir hasta el próximo reintento pendiente; sin peers
            // desconectados, hasta que algo despierte al bucle.
            let pending: Vec<Instant> = candidates
                .iter()
                .filter(|rec| !self.is_connected(&rec.device_id))
                .filter_map(|rec| backoff.get(&rec.device_id).map(|(next, _)| *next))
                .collect();
            let sleep = match pending.iter().min() {
                Some(next) => next
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(50)),
                None if candidates.is_empty() => Duration::from_secs(24 * 3600),
                None => cfg.reconnect_min,
            };
            tokio::select! {
                _ = tokio::time::sleep(sleep) => {}
                // Un peer apareció, cambió la red o se cayó un enlace:
                // reintentar ya, sin esperar al backoff.
                _ = self.inner.reconnect_wake.notified() => backoff.clear(),
            }
        }
    }
}

async fn recv_kind(
    inbound: &mut mpsc::Receiver<pb::LinkFrame>,
    timeout: Duration,
    expected: &str,
) -> anyhow::Result<Kind> {
    match tokio::time::timeout(timeout, inbound.recv()).await {
        Ok(Some(pb::LinkFrame { kind: Some(kind) })) => Ok(kind),
        Ok(Some(_)) => anyhow::bail!("frame vacío esperando {expected}"),
        Ok(None) => anyhow::bail!("el peer cerró la conexión esperando {expected}"),
        Err(_) => anyhow::bail!("tiempo agotado esperando {expected}"),
    }
}

fn validate_hello(hello: &pb::Hello, local_id: &str) -> anyhow::Result<()> {
    if hello.protocol_version != PROTOCOL_VERSION {
        anyhow::bail!(
            "versión de protocolo incompatible: el peer usa {}, este lado {PROTOCOL_VERSION}",
            hello.protocol_version
        );
    }
    if hello.public_key.len() != 32 {
        anyhow::bail!("clave pública de longitud inválida");
    }
    if hello.device_id != crate::identity::device_id_for(&hello.public_key) {
        anyhow::bail!(
            "device_id {} no corresponde a su clave pública",
            hello.device_id
        );
    }
    if hello.device_id == local_id {
        anyhow::bail!("conexión con uno mismo");
    }
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// ¿Va por el enlace de datos (modo híbrido)? Las respuestas grandes: fotos,
/// listas de SMS/contactos/llamadas/avisos y el fondo de pantalla.
pub fn is_heavy(feature: &pb::Feature) -> bool {
    use pb::feature::Kind as F;
    matches!(
        feature.kind,
        Some(
            F::PhotoList(_)
                | F::PhotoData(_)
                | F::SmsThreads(_)
                | F::SmsMessages(_)
                | F::CallLog(_)
                | F::Contacts(_)
                | F::ActiveNotifications(_)
                | F::Wallpaper(_)
                | F::ContactPhotos(_)
        )
    )
}

/// SHA-256 hex de un icono PNG (la `icon_hash` de [`pb::Notification`]).
pub fn icon_hash(png: &[u8]) -> String {
    encode_hex(&Sha256::digest(png))
}

/// Convierte una notificación local al mensaje del protocolo, sin el PNG
/// del icono si su hash está en `have_icons` (el receptor ya lo tiene).
pub fn notification_to_pb(n: &NotificationData, have_icons: &HashSet<String>) -> pb::Notification {
    let (icon_hash, icon_png) = match n.icon_png.as_deref().filter(|p| !p.is_empty()) {
        Some(png) => {
            let hash = icon_hash(png);
            let png = if have_icons.contains(&hash) {
                Vec::new()
            } else {
                png.to_vec()
            };
            (hash, png)
        }
        None => (String::new(), Vec::new()),
    };
    pb::Notification {
        key: n.key.clone(),
        app_id: n.app_id.clone(),
        app_name: n.app_name.clone(),
        title: n.title.clone(),
        text: n.text.clone(),
        timestamp_ms: n.timestamp_ms,
        icon_hash,
        icon_png,
        actions: n
            .actions
            .iter()
            .map(|a| pb::NotificationAction {
                id: a.id.clone(),
                label: a.label.clone(),
                is_reply: a.is_reply,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello_for(identity: &DeviceIdentity) -> pb::Hello {
        pb::Hello {
            protocol_version: PROTOCOL_VERSION,
            device_id: identity.device_id().to_string(),
            device_name: "x".into(),
            public_key: identity.public_key().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn contact_photos_go_by_the_data_lane() {
        use pb::feature::Kind as F;
        let f = |kind| pb::Feature { kind: Some(kind) };
        assert!(is_heavy(&f(F::ContactPhotos(Default::default()))));
        assert!(!is_heavy(&f(F::ContactPhotosRequest(Default::default()))));
    }

    #[test]
    fn hello_must_match_its_public_key() {
        let a = DeviceIdentity::generate().unwrap();
        let b = DeviceIdentity::generate().unwrap();
        assert!(validate_hello(&hello_for(&a), "otro").is_ok());

        // Reclamar el id de B con la clave de A.
        let mut forged = hello_for(&a);
        forged.device_id = b.device_id().to_string();
        assert!(validate_hello(&forged, "otro").is_err());

        let mut old = hello_for(&a);
        old.protocol_version = 0;
        assert!(validate_hello(&old, "otro").is_err());

        assert!(validate_hello(&hello_for(&a), a.device_id()).is_err());
    }
}
