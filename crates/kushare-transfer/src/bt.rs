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
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

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

/// Perfil RFCOMM de kushare ya registrado ante BlueZ, compartido entre el
/// bucle de aceptación (`run_receiver`) y el envío saliente (`connect`).
/// Clonar `BtService` es barato (`Session` y el `Mutex` interno van detrás
/// de `Arc`); se puede tener una copia en la tarea del receptor y otra en
/// cada envío sin volver a registrar el perfil.
#[derive(Clone)]
pub struct BtService {
    session: Session,
    handle: Arc<Mutex<ProfileHandle>>,
    // Solo necesita mantenerse viva (se desregistra al soltarse); nunca se
    // lee. Un agente con todos los manejadores en `None` (`Agent::default()`)
    // equivale a `NoInputNoOutput` y acepta todo, que es lo que kushare
    // quiere (v1: "visible para todos") — sin uno, BlueZ no tiene a quién
    // preguntarle si llegara a necesitar autorización. No resultó ser la
    // causa del bug de abajo, pero `bluer::rfcomm` advierte que hace falta
    // para que el flujo de conexión funcione en general, así que se deja.
    _agent: Arc<AgentHandle>,
}

impl BtService {
    /// Registra el perfil RFCOMM de kushare ante BlueZ. Solo debe llamarse
    /// una vez por proceso (ver el aviso de módulo sobre el límite de
    /// BlueZ de un registro por UUID).
    pub async fn register(session: Session) -> anyhow::Result<Self> {
        let agent = session
            .register_agent(Agent {
                request_default: true,
                ..Default::default()
            })
            .await?;
        let handle = session
            .register_profile(Profile {
                uuid: KUSHARE_RFCOMM_UUID,
                name: Some("kushare".to_string()),
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
                channel: Some(KUSHARE_RFCOMM_CHANNEL),
                require_authentication: Some(false),
                require_authorization: Some(false),
                ..Default::default()
            })
            .await?;
        Ok(Self {
            session,
            handle: Arc::new(Mutex::new(handle)),
            _agent: Arc::new(agent),
        })
    }

    /// Bucle de aceptación: espera conexiones RFCOMM entrantes (iniciadas
    /// por un peer remoto) y atiende cada una en su propia tarea de tokio,
    /// igual que [`crate::Receiver`] para TCP. Corre indefinidamente; se
    /// pensado para lanzarse con `tokio::spawn`.
    pub async fn run_receiver(
        &self,
        download_dir: PathBuf,
        events: mpsc::Sender<TransferEvent>,
    ) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&download_dir).await?;
        loop {
            let req = {
                let mut handle = self.handle.lock().await;
                handle.next().await
            };
            let Some(req) = req else {
                anyhow::bail!("BlueZ cerró el perfil RFCOMM de kushare");
            };
            let device = req.device();
            tracing::info!(%device, "conexión RFCOMM entrante");
            let stream = match req.accept() {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(%device, ?err, "no se pudo aceptar la conexión RFCOMM");
                    continue;
                }
            };
            let download_dir = download_dir.clone();
            let events = events.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_connection(stream, download_dir, events).await {
                    tracing::warn!(%device, ?err, "conexión de recepción RFCOMM terminó con error");
                }
            });
        }
    }

    /// Conecta por RFCOMM a `device_addr` (debe estar emparejado, o BlueZ
    /// pedirá autorización) y conduce el handshake UKEY2 como iniciador.
    ///
    /// `device.connect_profile(uuid)` (D-Bus `Device1.ConnectProfile`), no
    /// el genérico `device.connect()` (`Device1.Connect`, que solo conecta
    /// los perfiles que BlueZ ya reconoce como auto-conectables —
    /// audio/HID/etc. — y nunca nuestro UUID propio): así BlueZ busca el
    /// registro SDP del servidor remoto para [`KUSHARE_RFCOMM_UUID`]
    /// específicamente y descubre su canal RFCOMM.
    ///
    /// Esa llamada D-Bus **no vuelve hasta que nuestro propio perfil
    /// confirma la conexión** (BlueZ nos entrega el `fd` vía
    /// `NewConnection` y espera a que `ConnectRequest::accept()` le
    /// responda antes de completar `ConnectProfile`). Si aquí se hiciera
    /// `.await` sobre `connect_profile()` antes de leer de `handle`, sería
    /// un interbloqueo perfecto: la llamada nunca volvería porque nosotros
    /// nunca llegaríamos al código que la desbloquea (se confirmó en
    /// pruebas reales: se quedaba colgada indefinidamente). Por eso se
    /// lanza en una tarea aparte y se espera a `handle.next()` en
    /// paralelo — así si hace falta podemos aceptar la `ConnectRequest`
    /// mientras `connect_profile()` sigue en vuelo.
    pub async fn connect(&self, device_addr: Address) -> anyhow::Result<TransferSession<Stream>> {
        let mut handle = self.handle.lock().await;

        let adapter = self.session.default_adapter().await?;
        let device = adapter.device(device_addr)?;
        let connect_task = tokio::spawn(async move { device.connect_profile(&KUSHARE_RFCOMM_UUID).await });

        let req = handle
            .next()
            .await
            .ok_or_else(|| anyhow::anyhow!("BlueZ cerró el perfil RFCOMM sin conectar"))?;
        let stream = req.accept()?;
        drop(handle);

        if let Ok(Err(err)) = connect_task.await {
            tracing::warn!(?err, "ConnectProfile devolvió error, pero la conexión ya se había recibido");
        }

        TransferSession::handshake_as_initiator(stream).await
    }
}
