//! Transporte de transferencia sobre RFCOMM Bluetooth clásico, como
//! alternativa a TCP/mDNS cuando la red IP entre los dos dispositivos no es
//! fiable (aislamiento de clientes en WiFi, hotspot que bloquea multicast,
//! firewalls que caen en una zona restrictiva, ...). Usa el mismo
//! handshake UKEY2 y el mismo framing que el transporte TCP (ver
//! [`crate::TransferSession`]); lo único que cambia es el `AsyncRead +
//! AsyncWrite` subyacente.
//!
//! A diferencia de `kushare_discovery::ble` (que usa BLE solo para
//! *anunciar/descubrir* por proximidad), este módulo usa Bluetooth clásico
//! (RFCOMM) para mover los bytes de la transferencia en sí, registrando un
//! [`bluer::rfcomm::Profile`] con un UUID propio. BlueZ se encarga del
//! registro y la consulta SDP, así que el mismo UUID sirve tanto para
//! kushare-a-kushare como para el `BluetoothSocket` nativo de Android
//! (`createRfcommSocketToServiceRecord` / `listenUsingRfcommWithServiceRecord`,
//! que hacen la búsqueda SDP del lado Android).
//!
//! BlueZ solo permite **un** registro de `ProfileManager1` por UUID en todo
//! el sistema a la vez (registrar un segundo da
//! `org.bluez.Error.AlreadyExists: UUID already registered`),
//! independientemente del `Role` (cliente/servidor) — a diferencia de TCP,
//! donde cada proceso puede abrir su propio puerto efímero sin pisarse.
//! Por eso [`BtService`] registra el perfil **una sola vez** por proceso y
//! lo comparte entre el bucle de aceptación y cualquier conexión saliente:
//! ambas direcciones llegan por el mismo `NewConnection` de BlueZ (nosotros
//! aceptando a un peer, o BlueZ entregándonos el descriptor tras nuestro
//! propio `device.connect()`), así que basta con serializarlas con un
//! mutex. Dos procesos *distintos* de kushare en la misma máquina no
//! pueden tener ambos el registro activo al mismo tiempo: si el daemon de
//! fondo ya tiene la recepción Bluetooth activa, `kushare send-bt` en otro
//! proceso fallará con ese mismo error hasta que el primero se cierre.

use bluer::agent::{Agent, AgentHandle};
use bluer::rfcomm::{Profile, ProfileHandle, Stream};
use bluer::{Address, Session, Uuid};
use futures_util::StreamExt;
use kushare_core::TransferEvent;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::rx::handle_connection;
use crate::session::TransferSession;

/// UUID propio de kushare para su servicio RFCOMM de transferencia de
/// archivos. No es un UUID estándar de ningún perfil Bluetooth existente
/// (SPP, HFP, ...); se generó una vez al azar para no chocar con otros
/// servicios registrados en el mismo adaptador.
pub const KUSHARE_RFCOMM_UUID: Uuid = Uuid::from_u128(0xb4ac1500_6c92_4e64_8f1e_7e1a0cd4f6a1);

/// Canal RFCOMM fijo para el perfil de kushare (ver el comentario en
/// [`BtService::register`] sobre por qué hace falta fijarlo a mano).
pub const KUSHARE_RFCOMM_CHANNEL: u16 = 19;

/// UUID RFCOMM del enlace persistente de kushare (`kushare-link`),
/// distinto del de transferencias: así BlueZ los trata como perfiles
/// independientes (cada uno con su propio registro) y el Android puede
/// escuchar ambos con dos `BluetoothServerSocket`. Debe coincidir con
/// `KUSHARE_LINK_RFCOMM_UUID` en kushare-android.
pub const KUSHARE_LINK_RFCOMM_UUID: Uuid = Uuid::from_u128(0x6f3a2d10_8c4e_4b7a_9e21_5d0c7b3e9a42);

/// Canal RFCOMM fijo del enlace persistente.
pub const KUSHARE_LINK_RFCOMM_CHANNEL: u16 = 20;

/// Qué perfil RFCOMM registrar ante BlueZ.
#[derive(Debug, Clone)]
pub struct ProfileSpec {
    pub uuid: Uuid,
    pub channel: u16,
    pub name: String,
    /// Registrar además un agente BlueZ que acepte todo (ver el comentario
    /// de `BtService::_agent`). BlueZ admite **un** agente por conexión
    /// D-Bus, así que solo uno de los perfiles de un proceso debe pedirlo.
    pub with_agent: bool,
}

impl ProfileSpec {
    /// Perfil de transferencia de archivos (el de siempre).
    pub fn transfer() -> Self {
        Self {
            uuid: KUSHARE_RFCOMM_UUID,
            channel: KUSHARE_RFCOMM_CHANNEL,
            name: "kushare".to_string(),
            with_agent: true,
        }
    }

    /// Perfil del enlace persistente. Sin agente propio: reusa el del
    /// perfil de transferencia si el mismo proceso lo registró.
    pub fn link() -> Self {
        Self {
            uuid: KUSHARE_LINK_RFCOMM_UUID,
            channel: KUSHARE_LINK_RFCOMM_CHANNEL,
            name: "kushare-link".to_string(),
            with_agent: false,
        }
    }
}

/// Perfil RFCOMM de kushare ya registrado ante BlueZ, compartido entre el
/// bucle de aceptación y las conexiones salientes. Clonar `BtService` es
/// barato (todo va detrás de `Arc`).
///
/// ## Despachador
///
/// BlueZ entrega **por el mismo `ProfileHandle`** tanto las conexiones que
/// abre un peer remoto como la que resulta de nuestro propio
/// `ConnectProfile` (ver [`Self::connect_raw`]). Una única tarea
/// despachadora, lanzada al registrar, es la dueña del handle y reparte
/// cada `ConnectRequest`:
///
/// - si hay una conexión saliente pendiente hacia esa dirección, se la
///   entrega a quien la espera ([`Self::connect_raw`]);
/// - si no, va a la cola de entrantes, que consume [`Self::run_acceptor`] /
///   [`Self::run_receiver`].
///
/// Antes cada bucle de aceptación retenía el mutex del handle mientras
/// esperaba `next()`, así que un `connect()` en el mismo proceso se
/// bloqueaba hasta que entrara alguna conexión ajena. El despachador evita
/// ese interbloqueo.
///
/// Caso límite conocido: si el peer X nos conecta justo mientras nosotros
/// estamos conectando a X, su conexión puede tomarse por la nuestra; ambos
/// lados actuarían como iniciador, el handshake UKEY2 fallaría y el
/// llamador reintentaría.
#[derive(Clone)]
pub struct BtService {
    session: Session,
    pending: Arc<StdMutex<HashMap<Address, oneshot::Sender<Stream>>>>,
    incoming: Arc<Mutex<mpsc::Receiver<(Stream, Address)>>>,
    // Solo necesita mantenerse viva (se desregistra al soltarse); nunca se
    // lee. Un agente con todos los manejadores en `None` (`Agent::default()`)
    // equivale a `NoInputNoOutput` y acepta todo, que es lo que kushare
    // quiere (v1: "visible para todos") — sin uno, BlueZ no tiene a quién
    // preguntarle si llegara a necesitar autorización. `bluer::rfcomm`
    // advierte que hace falta para que el flujo de conexión funcione en
    // general, así que se deja.
    _agent: Option<Arc<AgentHandle>>,
    _dispatcher: Arc<DispatcherGuard>,
    uuid: Uuid,
}

/// Aborta la tarea despachadora (y con ella suelta el `ProfileHandle`, lo
/// que desregistra el perfil) cuando se suelta el último clon de
/// `BtService`.
struct DispatcherGuard(tokio::task::AbortHandle);

impl Drop for DispatcherGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Tamaño de la cola de conexiones entrantes sin atender. Si se llena
/// (nadie está corriendo `run_acceptor`), las nuevas se descartan.
const INCOMING_QUEUE: usize = 8;

/// Cuánto esperar a que BlueZ entregue nuestra propia conexión saliente.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

impl BtService {
    /// Registra el perfil RFCOMM de transferencias de kushare ante BlueZ.
    /// Solo debe llamarse una vez por proceso (ver el aviso de módulo sobre
    /// el límite de BlueZ de un registro por UUID).
    pub async fn register(session: Session) -> anyhow::Result<Self> {
        Self::register_profile(session, ProfileSpec::transfer()).await
    }

    /// Registra un perfil RFCOMM cualquiera (ver [`ProfileSpec`]). Mismo
    /// límite que [`Self::register`]: una vez por UUID en todo el sistema.
    pub async fn register_profile(session: Session, spec: ProfileSpec) -> anyhow::Result<Self> {
        let agent = if spec.with_agent {
            Some(Arc::new(
                session
                    .register_agent(Agent {
                        request_default: true,
                        ..Default::default()
                    })
                    .await?,
            ))
        } else {
            None
        };
        let mut handle: ProfileHandle = session
            .register_profile(Profile {
                uuid: spec.uuid,
                name: Some(spec.name.clone()),
                // Sin esto, BlueZ registra el perfil y acepta conexiones,
                // pero el registro SDP que genera automáticamente no
                // incluye el canal RFCOMM en su Protocol Descriptor List
                // (confirmado con una captura HCI real: la respuesta SDP
                // solo listaba L2CAP, nunca RFCOMM+canal). Android usa ese
                // campo para `createRfcommSocketToServiceRecord`, así que
                // sin canal explícito su conexión falla justo después de
                // la búsqueda SDP ("read failed ... read ret: -1", sin
                // ningún error visible del lado BlueZ). Fijar el canal a
                // mano hace que sí aparezca en el registro.
                channel: Some(spec.channel),
                require_authentication: Some(false),
                require_authorization: Some(false),
                ..Default::default()
            })
            .await?;

        let pending: Arc<StdMutex<HashMap<Address, oneshot::Sender<Stream>>>> = Arc::default();
        let (incoming_tx, incoming_rx) = mpsc::channel(INCOMING_QUEUE);
        let uuid = spec.uuid;
        let dispatcher = {
            let pending = pending.clone();
            tokio::spawn(async move {
                while let Some(req) = handle.next().await {
                    let device = req.device();
                    let stream = match req.accept() {
                        Ok(stream) => stream,
                        Err(err) => {
                            tracing::warn!(%device, ?err, "no se pudo aceptar la conexión RFCOMM");
                            continue;
                        }
                    };
                    let waiter = pending.lock().expect("mutex envenenado").remove(&device);
                    match waiter {
                        Some(waiter) => {
                            let _ = waiter.send(stream);
                        }
                        None => {
                            tracing::info!(%device, %uuid, "conexión RFCOMM entrante");
                            if incoming_tx.try_send((stream, device)).is_err() {
                                tracing::warn!(%device, "cola RFCOMM entrante llena o sin consumidor; se descarta");
                            }
                        }
                    }
                }
                tracing::warn!(%uuid, "BlueZ cerró el perfil RFCOMM");
            })
        };

        Ok(Self {
            session,
            pending,
            incoming: Arc::new(Mutex::new(incoming_rx)),
            _agent: agent,
            _dispatcher: Arc::new(DispatcherGuard(dispatcher.abort_handle())),
            uuid,
        })
    }

    /// Bucle de aceptación genérico: entrega cada stream RFCOMM entrante ya
    /// aceptado (sin handshake) a `handler`, en su propia tarea. Para
    /// protocolos propios sobre RFCOMM (p. ej. el enlace persistente), que
    /// conducen el handshake por su cuenta. Solo debe haber un consumidor
    /// por `BtService`.
    pub async fn run_acceptor<F, Fut>(&self, handler: F) -> anyhow::Result<()>
    where
        F: Fn(Stream, Address) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut incoming = self.incoming.lock().await;
        while let Some((stream, device)) = incoming.recv().await {
            tokio::spawn(handler(stream, device));
        }
        anyhow::bail!("BlueZ cerró el perfil RFCOMM {}", self.uuid)
    }

    /// Bucle de aceptación de transferencias: atiende cada conexión
    /// entrante con [`handle_connection`], igual que [`crate::Receiver`]
    /// para TCP. Corre indefinidamente; pensado para `tokio::spawn`.
    pub async fn run_receiver(
        &self,
        download_dir: PathBuf,
        events: mpsc::Sender<TransferEvent>,
    ) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&download_dir).await?;
        self.run_acceptor(move |stream, device| {
            let download_dir = download_dir.clone();
            let events = events.clone();
            async move {
                if let Err(err) = handle_connection(stream, download_dir, events).await {
                    tracing::warn!(%device, ?err, "conexión de recepción RFCOMM terminó con error");
                }
            }
        })
        .await
    }

    /// Conecta por RFCOMM a `device_addr` (debe estar emparejado, o BlueZ
    /// pedirá autorización) y conduce el handshake UKEY2 como iniciador.
    pub async fn connect(&self, device_addr: Address) -> anyhow::Result<TransferSession<Stream>> {
        let stream = self.connect_raw(device_addr).await?;
        TransferSession::handshake_as_initiator(stream).await
    }

    /// Como [`Self::connect`] pero devuelve el stream RFCOMM sin conducir
    /// ningún handshake (ver [`Self::run_acceptor`]).
    ///
    /// Usa `device.connect_profile(uuid)` (D-Bus `Device1.ConnectProfile`),
    /// no el genérico `device.connect()` (`Device1.Connect`, que solo
    /// conecta los perfiles que BlueZ ya reconoce como auto-conectables —
    /// audio/HID/etc. — y nunca nuestro UUID propio): así BlueZ busca el
    /// registro SDP del servidor remoto para nuestro UUID específicamente y
    /// descubre su canal RFCOMM.
    ///
    /// Esa llamada D-Bus **no vuelve hasta que nuestro propio perfil acepta
    /// la conexión** (BlueZ nos entrega el `fd` vía `NewConnection` y espera
    /// a `ConnectRequest::accept()` antes de completar `ConnectProfile`; se
    /// confirmó en pruebas reales que esperarla primero se cuelga para
    /// siempre). Por eso se lanza en una tarea aparte y aquí se espera, en
    /// paralelo, a que el despachador nos entregue el stream.
    pub async fn connect_raw(&self, device_addr: Address) -> anyhow::Result<Stream> {
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().expect("mutex envenenado");
            if pending.contains_key(&device_addr) {
                anyhow::bail!("ya hay una conexión RFCOMM en curso hacia {device_addr}");
            }
            pending.insert(device_addr, tx);
        }
        let result = self.connect_profile_and_wait(device_addr, rx).await;
        self.pending
            .lock()
            .expect("mutex envenenado")
            .remove(&device_addr);
        result
    }

    async fn connect_profile_and_wait(
        &self,
        device_addr: Address,
        rx: oneshot::Receiver<Stream>,
    ) -> anyhow::Result<Stream> {
        let adapter = self.session.default_adapter().await?;
        let device = adapter.device(device_addr)?;
        let uuid = self.uuid;
        let mut connect_task = tokio::spawn(async move { device.connect_profile(&uuid).await });

        let wait = async {
            tokio::select! {
                stream = rx => stream.map_err(|_| anyhow::anyhow!("el despachador RFCOMM terminó")),
                // Si ConnectProfile falla antes de entregar nada, no tiene
                // sentido seguir esperando al despachador.
                joined = &mut connect_task => match joined {
                    Ok(Err(err)) => Err(anyhow::anyhow!("ConnectProfile hacia {device_addr} falló: {err}")),
                    _ => Err(anyhow::anyhow!("ConnectProfile terminó sin entregar conexión")),
                },
            }
        };
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, wait)
            .await
            .map_err(|_| {
                anyhow::anyhow!("tiempo agotado conectando por RFCOMM a {device_addr}")
            })??;

        // ConnectProfile vuelve después de que el despachador aceptó; su
        // resultado ya no importa (a veces devuelve error aunque la
        // conexión se haya establecido).
        if !connect_task.is_finished() {
            tokio::spawn(async move {
                if let Ok(Err(err)) = connect_task.await {
                    tracing::debug!(?err, "ConnectProfile devolvió error tras conectar");
                }
            });
        }
        Ok(stream)
    }
}
