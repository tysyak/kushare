//! Contrato D-Bus del daemon de kushare (`org.kushare.Daemon` en el bus de
//! sesión): el proxy cliente y los tipos que intercambia. Lo usan la CLI
//! (`kushare devices`, `pair`…) y la GUI (`kushare-gui`); la implementación
//! del servidor vive en el binario `kushare` (`src/link/dbus.rs`) y debe
//! mantener estas mismas firmas.

pub const BUS_NAME: &str = "org.kushare.Daemon";
pub const OBJECT_PATH: &str = "/org/kushare/Daemon";

/// Un dispositivo emparejado:
/// (id, nombre, tipo, conectado, transporte, saliente, entrante, dirección Bluetooth).
/// `transporte` y `dirección Bluetooth` van vacíos si no aplican.
pub type PeerRow = (String, String, String, bool, String, bool, bool, String);

/// (apps locales, apps del peer, bloqueadas al salir, bloqueadas al entrar);
/// las apps son pares (app_id, nombre).
pub type AppsReply = (
    Vec<(String, String)>,
    Vec<(String, String)>,
    Vec<String>,
    Vec<String>,
);

/// Batería: (conocida, porcentaje, cargando). `conocida = false` si el
/// peer no tiene batería o aún no la ha mandado.
pub type BatteryRow = (bool, u32, bool);

/// Reproductor: (id, nombre, título, artista, álbum, estado
/// "playing"|"paused"|"stopped", puede_reproducir, puede_pausar,
/// puede_siguiente, puede_anterior).
pub type PlayerRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    bool,
    bool,
    bool,
    bool,
);

/// Salida de audio: (id, nombre, activa).
pub type OutputRow = (String, String, bool);

/// Llamada: (fase "idle"|"ringing"|"active"|"dialing", número, nombre, micrófono silenciado).
pub type CallRow = (String, String, String, bool);

/// Estado de las funciones extra de un peer: (batería, timbre
/// ""|"normal"|"vibrate"|"silent", llamada, reproductores, salidas
/// seleccionables, salidas).
pub type PeerStateReply = (
    BatteryRow,
    String,
    CallRow,
    Vec<PlayerRow>,
    bool,
    Vec<OutputRow>,
);

/// Foto de la galería: (id, nombre, fecha en ms, ancho, alto, tamaño en
/// bytes, ruta local de la miniatura).
pub type PhotoRow = (String, String, i64, u32, u32, u64, String);

/// Proxy cliente de `org.kushare.Daemon`.
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
    #[zbus(property)]
    fn scanning(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn link_port(&self) -> zbus::Result<u16>;
    fn peers(&self) -> zbus::Result<Vec<PeerRow>>;
    /// Dispositivos del último escaneo aún no emparejados: (id, nombre, dirección).
    fn discovered(&self) -> zbus::Result<Vec<(String, String, String)>>;
    fn scan(&self, seconds: u32) -> zbus::Result<()>;
    fn send_file_bt(&self, path: &str, address: &str) -> zbus::Result<()>;
    fn quit(&self) -> zbus::Result<()>;
    /// `peer`: id, prefijo, nombre o dirección Bluetooth (emparejado por RFCOMM).
    fn pair(&self, peer: &str) -> zbus::Result<String>;
    fn respond_pairing(&self, peer: &str, accept: bool) -> zbus::Result<bool>;
    fn unpair(&self, peer: &str) -> zbus::Result<()>;
    /// `direction`: "in" (recibir sus notificaciones) u "out" (enviarle las mías).
    fn set_direction(&self, peer: &str, direction: &str, enabled: bool) -> zbus::Result<()>;
    fn set_app_blocked(
        &self,
        peer: &str,
        direction: &str,
        app_id: &str,
        blocked: bool,
    ) -> zbus::Result<()>;
    fn apps(&self, peer: &str) -> zbus::Result<AppsReply>;
    /// Batería, timbre, llamada, reproductores y salidas de audio del peer.
    fn peer_state(&self, peer: &str) -> zbus::Result<PeerStateReply>;
    /// `action`: play-pause|play|pause|next|previous|stop. `player` vacío = todos.
    fn media_command(&self, peer: &str, player: &str, action: &str) -> zbus::Result<()>;
    fn set_audio_output(&self, peer: &str, output: &str) -> zbus::Result<()>;
    /// `mode`: normal|vibrate|silent.
    fn set_ringer(&self, peer: &str, mode: &str) -> zbus::Result<()>;
    /// `action`: answer|reject|hangup|mute|unmute.
    fn call_command(&self, peer: &str, action: &str) -> zbus::Result<()>;
    /// Página de la galería del peer (más reciente primero): (total, fotos).
    fn photos(&self, peer: &str, offset: u32, limit: u32) -> zbus::Result<(u32, Vec<PhotoRow>)>;
    /// Oír en el PC la música y las llamadas del teléfono (Bluetooth A2DP +
    /// HFP). Devuelve los perfiles afectados ("multimedia", "llamadas").
    fn phone_audio(&self, peer: &str, enable: bool) -> zbus::Result<Vec<String>>;
    /// Descarga la foto original a la carpeta de descargas; devuelve la ruta.
    fn fetch_photo(&self, peer: &str, id: &str) -> zbus::Result<String>;
    #[zbus(signal)]
    fn pairing_requested(&self, peer_id: String, name: String, pin: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn pairing_finished(&self, peer_id: String, name: String, accepted: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    fn peers_changed(&self) -> zbus::Result<()>;
    /// Cambió la batería, el timbre, la llamada o la multimedia de un peer.
    #[zbus(signal)]
    fn peer_state_changed(&self, peer_id: String) -> zbus::Result<()>;
}

/// Conecta con el daemon en el bus de sesión. Falla si no está corriendo.
pub async fn connect() -> zbus::Result<DaemonProxy<'static>> {
    let connection = zbus::Connection::session().await?;
    // Sin caché de propiedades: si el daemon no corre, zbus avisaría al
    // intentar poblarla; para los clientes eso es un caso normal.
    let proxy = DaemonProxy::builder(&connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    proxy.device_id().await?;
    Ok(proxy)
}
