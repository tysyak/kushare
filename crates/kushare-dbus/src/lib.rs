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
    #[zbus(signal)]
    fn pairing_requested(&self, peer_id: String, name: String, pin: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn pairing_finished(&self, peer_id: String, name: String, accepted: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    fn peers_changed(&self) -> zbus::Result<()>;
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
