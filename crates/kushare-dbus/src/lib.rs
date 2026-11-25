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
/// puede_siguiente, puede_anterior, [`PlayerProgress`]).
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
    PlayerProgress,
);

/// Carátula y progreso de un reproductor: (ruta de la carátula o "",
/// posición en ms, duración en ms (0 = no se sabe), ms Unix en que se midió
/// la posición).
pub type PlayerProgress = (String, i64, i64, i64);

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
    bool,
);

/// Foto de la galería: (id, nombre, fecha en ms, ancho, alto, tamaño en
/// bytes, ruta local de la miniatura, ruta local de la original — vacía si
/// aún no está descargada).
pub type PhotoRow = (String, String, i64, u32, u32, u64, String, String);

/// Conversación de SMS: (id, número(s), nombre, último texto, fecha en ms,
/// sin leer, nº de mensajes, el último lo mandé yo).
pub type SmsThreadRow = (String, String, String, String, i64, bool, u32, bool);

/// SMS: (id, número, texto, fecha en ms, saliente, leído, falló al enviarse).
pub type SmsRow = (String, String, String, i64, bool, bool, bool);

/// Llamada del registro: (número, nombre, fecha en ms, duración en s, tipo
/// "incoming"|"outgoing"|"missed"|"rejected"|"blocked"|"voicemail", ruta
/// local de la foto del contacto o "").
pub type CallLogRow = (String, String, i64, u32, String, String);

/// Cámara del teléfono: (id, etiqueta legible, "back"|"front"|"external").
pub type CameraRow = (String, String, String);

/// Estado de la webcam: (activa, teléfono, cámara, dispositivo `/dev/videoN`,
/// fotogramas por segundo, error).
pub type WebcamRow = (bool, String, String, String, u32, String);
/// Micrófono: (activo, teléfono, códec, nivel 0–1000, error, es el predeterminado).
pub type MicRow = (bool, String, String, u32, String, bool);

/// Punto de acceso «Compartir internet»: (activo, arrancando, nombre de la
/// red, clave, «banda · canal», clientes conectados, último error, antena
/// alternativa — «canal 157 · señal 45» si el canal actual no sirve y hay
/// otra antena de la misma red que sí; vacío si no).
pub type HotspotRow = (bool, bool, String, String, String, u32, String, String);

/// Internet por cable USB: (activo, arrancando, bytes hacia el teléfono,
/// bytes desde el teléfono, último error).
pub type UsbNetRow = (bool, bool, u64, u64, String);

/// Código QR leído: (texto, formato, fecha en ms, tipo
/// "url"|"wifi"|"phone"|"email"|"contact"|"text", teléfono).
pub type QrRow = (String, String, i64, String, String);

/// Contacto: (nombre, números, favorito, ruta local de la foto o "" — solo
/// las ya guardadas en el PC).
pub type ContactRow = (String, Vec<String>, bool, String);

/// Notificación activa del teléfono: (key, app_id, app, título, texto,
/// fecha en ms, ruta del icono o "", acciones (id, etiqueta, es_respuesta)).
pub type PhoneNotificationRow = (
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    Vec<(String, String, bool)>,
);

/// Entrada del historial del portapapeles: (texto, fecha en ms, origen —
/// "" = este PC, si no el nombre del dispositivo).
pub type ClipRow = (String, i64, String);

/// Actualización: (versión actual, última publicada — "" si no se sabe,
/// página del release, nombre del paquete para este sistema, URL del paquete).
pub type UpdateRow = (String, String, String, String, String);

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
    /// Hace sonar el teléfono para encontrarlo (`ring = false`: lo para).
    fn find_phone(&self, peer: &str, ring: bool) -> zbus::Result<()>;
    /// Cámaras del teléfono.
    fn cameras(&self, peer: &str) -> zbus::Result<Vec<CameraRow>>;
    /// Usa la cámara del teléfono como webcam; devuelve `/dev/videoN` o
    /// «PipeWire: …». `backend`: ""|auto, v4l2 o pipewire.
    fn webcam_start(&self, peer: &str, camera: &str, backend: &str) -> zbus::Result<String>;
    fn webcam_stop(&self) -> zbus::Result<()>;
    fn webcam_status(&self) -> zbus::Result<WebcamRow>;
    /// Usa el micrófono del teléfono como fuente de audio del PC; devuelve
    /// el códec. `make_default`: ponerla como entrada predeterminada.
    fn mic_start(&self, peer: &str, make_default: bool) -> zbus::Result<String>;
    fn mic_stop(&self) -> zbus::Result<()>;
    fn mic_status(&self) -> zbus::Result<MicRow>;
    /// Tonos DTMF en la llamada en curso ("0-9*#ABCD"). Devuelve por dónde
    /// fueron: "phone", "bluetooth" (HFP) o "audio".
    fn dtmf(&self, peer: &str, digits: &str) -> zbus::Result<String>;
    /// Estado del punto de acceso Wi-Fi («Compartir internet»).
    fn hotspot_status(&self) -> zbus::Result<HotspotRow>;
    /// Lo enciende en segundo plano (puede pedir la contraseña de
    /// administrador por polkit): el resultado llega con `hotspot_changed`
    /// y `hotspot_status` (activo o error).
    fn hotspot_start(&self) -> zbus::Result<()>;
    fn hotspot_stop(&self) -> zbus::Result<()>;
    /// Pasa la Wi-Fi del PC a la antena alternativa (mientras la red esté
    /// encendida) y la enciende, en segundo plano como `hotspot_start`.
    fn hotspot_switch_antenna(&self) -> zbus::Result<()>;
    /// Nombre de la red (vacío = el de por defecto) y clave WPA2 (8–63).
    fn hotspot_set_config(&self, ssid: &str, password: &str) -> zbus::Result<()>;
    fn hotspot_random_password(&self) -> zbus::Result<String>;
    /// Texto del QR para conectarse (`WIFI:T:WPA;S:…;P:…;;`).
    fn hotspot_qr(&self) -> zbus::Result<String>;
    /// Manda la red al teléfono para que ofrezca conectarse.
    fn hotspot_send(&self, peer: &str) -> zbus::Result<()>;
    /// Comparte el internet del PC con el teléfono por el cable USB (VPN en
    /// el teléfono + `adb reverse`). En segundo plano: el resultado llega con
    /// `usbnet_changed` y `usbnet_status`.
    fn usbnet_start(&self, peer: &str) -> zbus::Result<()>;
    fn usbnet_stop(&self) -> zbus::Result<()>;
    fn usbnet_status(&self) -> zbus::Result<UsbNetRow>;
    /// Abre el escáner de QR en el teléfono.
    fn qr_scan(&self, peer: &str) -> zbus::Result<()>;
    /// Códigos leídos, más reciente primero.
    fn qr_history(&self) -> zbus::Result<Vec<QrRow>>;
    /// Abre en el navegador el código `index` (solo si es una web).
    fn qr_open(&self, index: u32) -> zbus::Result<()>;
    /// Copia el código `index` al portapapeles del PC.
    fn qr_copy(&self, index: u32) -> zbus::Result<()>;
    /// Guarda el código `index` en Descargas; devuelve la ruta.
    fn qr_save(&self, index: u32) -> zbus::Result<String>;
    /// Borra uno (`index` ≥ 0) o todos (`-1`).
    fn qr_forget(&self, index: i32) -> zbus::Result<()>;
    /// `action`: answer|reject|hangup|mute|unmute.
    fn call_command(&self, peer: &str, action: &str) -> zbus::Result<()>;
    /// Página de la galería del peer (más reciente primero): (total, fotos).
    fn photos(&self, peer: &str, offset: u32, limit: u32) -> zbus::Result<(u32, Vec<PhotoRow>)>;
    /// Oír en el PC la música y las llamadas del teléfono (Bluetooth A2DP +
    /// HFP). Devuelve los perfiles afectados ("multimedia", "llamadas").
    fn phone_audio(&self, peer: &str, enable: bool) -> zbus::Result<Vec<String>>;
    /// Descarga la foto original (a la caché de kushare); devuelve la ruta.
    fn fetch_photo(&self, peer: &str, id: &str) -> zbus::Result<String>;
    /// La última galería vista, sin preguntar al teléfono (también sin conexión).
    fn cached_photos(&self, peer: &str) -> zbus::Result<(u32, Vec<PhotoRow>)>;
    /// Guarda la original en `dest` (archivo o carpeta; "" = Descargas).
    fn save_photo(&self, peer: &str, id: &str, dest: &str) -> zbus::Result<String>;
    /// Borra la foto del teléfono: "done", o "confirm" si hay que confirmarlo allí.
    fn delete_photo(&self, peer: &str, id: &str) -> zbus::Result<String>;
    /// Conversaciones de SMS (más reciente primero): (total, conversaciones).
    fn sms_threads(
        &self,
        peer: &str,
        offset: u32,
        limit: u32,
    ) -> zbus::Result<(u32, Vec<SmsThreadRow>)>;
    /// Mensajes de una conversación, más reciente primero; `before_ms` > 0 pagina.
    fn sms_messages(
        &self,
        peer: &str,
        thread_id: &str,
        limit: u32,
        before_ms: i64,
    ) -> zbus::Result<Vec<SmsRow>>;
    fn send_sms(&self, peer: &str, address: &str, body: &str) -> zbus::Result<()>;
    fn call_log(&self, peer: &str, limit: u32) -> zbus::Result<Vec<CallLogRow>>;
    fn contacts(&self, peer: &str, refresh: bool) -> zbus::Result<Vec<ContactRow>>;
    /// Llama desde el teléfono (las letras se convierten a su tecla).
    fn dial(&self, peer: &str, number: &str) -> zbus::Result<()>;
    /// Notificaciones que hay ahora en el teléfono.
    fn phone_notifications(&self, peer: &str) -> zbus::Result<Vec<PhoneNotificationRow>>;
    fn dismiss_phone_notification(&self, peer: &str, key: &str) -> zbus::Result<()>;
    fn phone_notification_action(&self, peer: &str, key: &str, action_id: &str)
    -> zbus::Result<()>;
    fn phone_notification_reply(
        &self,
        peer: &str,
        key: &str,
        action_id: &str,
        text: &str,
    ) -> zbus::Result<()>;
    /// Fondo de pantalla del teléfono: (ruta de la imagen o "", colores ARGB).
    fn wallpaper(&self, peer: &str, refresh: bool) -> zbus::Result<(String, Vec<u32>)>;
    /// Salidas y entradas (micrófonos) de audio del PC.
    fn local_audio(&self) -> zbus::Result<(Vec<OutputRow>, Vec<OutputRow>)>;
    /// `kind`: "output" | "input".
    fn set_local_audio(&self, kind: &str, id: &str) -> zbus::Result<()>;
    fn clipboard_history(&self) -> zbus::Result<Vec<ClipRow>>;
    /// Copia `text` en el PC (y lo comparte con los teléfonos si está activado).
    fn clipboard_copy(&self, text: &str) -> zbus::Result<()>;
    /// Copia un archivo (imagen) al portapapeles del PC.
    fn clipboard_copy_file(&self, path: &str) -> zbus::Result<()>;
    /// Borra una entrada del historial (`index` ≥ 0) o todo (`-1`).
    fn clipboard_forget(&self, index: i32) -> zbus::Result<()>;
    /// Ajustes del daemon: (nombre, activado).
    fn settings(&self) -> zbus::Result<Vec<(String, bool)>>;
    fn set_setting(&self, name: &str, enabled: bool) -> zbus::Result<()>;
    /// Busca una versión nueva (con `force`, aunque se haya mirado hace poco).
    fn check_update(&self, force: bool) -> zbus::Result<UpdateRow>;
    /// Descarga el paquete de la versión nueva a Descargas (comprobando su
    /// SHA-256); devuelve la ruta.
    fn download_update(&self) -> zbus::Result<String>;
    #[zbus(signal)]
    fn pairing_requested(&self, peer_id: String, name: String, pin: String) -> zbus::Result<()>;
    #[zbus(signal)]
    fn pairing_finished(&self, peer_id: String, name: String, accepted: bool) -> zbus::Result<()>;
    #[zbus(signal)]
    fn peers_changed(&self) -> zbus::Result<()>;
    /// Cambió la batería, el timbre, la llamada o la multimedia de un peer.
    #[zbus(signal)]
    fn peer_state_changed(&self, peer_id: String) -> zbus::Result<()>;
    /// La galería del peer tiene fotos nuevas (pedir las más recientes).
    #[zbus(signal)]
    fn photos_changed(&self, peer_id: String) -> zbus::Result<()>;
    /// Llegó o se mandó un SMS (`thread_id` puede ir vacío).
    #[zbus(signal)]
    fn sms_changed(&self, peer_id: String, thread_id: String) -> zbus::Result<()>;
    /// Llegó o se quitó una notificación del teléfono.
    #[zbus(signal)]
    fn phone_notifications_changed(&self, peer_id: String) -> zbus::Result<()>;
    /// Cambió el historial del portapapeles.
    #[zbus(signal)]
    fn clipboard_changed(&self) -> zbus::Result<()>;
    /// Cambió el historial de códigos QR.
    #[zbus(signal)]
    fn qr_changed(&self) -> zbus::Result<()>;
    /// Cambió el estado o la configuración del punto de acceso.
    #[zbus(signal)]
    fn hotspot_changed(&self) -> zbus::Result<()>;
    /// Cambió el estado del internet por USB.
    #[zbus(signal)]
    fn usbnet_changed(&self) -> zbus::Result<()>;
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
