//! Enlace persistente entre dos kushare (escritorio ⇄ Android): emparejado
//! con PIN, autenticación con claves Ed25519 a largo plazo, reconexión
//! automática y sincronización de notificaciones y de su configuración.
//!
//! Es un protocolo propio (ver `kushare-proto/proto/kushare_link.proto`),
//! separado de Quick Share: viaja por `_kushare-link._tcp.local.` / el
//! UUID RFCOMM `KUSHARE_LINK_RFCOMM_UUID`, sobre el mismo canal UKEY2 +
//! SecureMessage que las transferencias, con números de secuencia
//! estrictos.
//!
//! No depende de nada específico de Linux de escritorio: también compila
//! para Android. Cada plataforma solo aporta los transportes ([`Dialer`],
//! [`LinkManager::accept`]) y la captura/visualización de notificaciones
//! (vía [`LinkEvent`] y [`LinkManager::post_local_notification`]).

pub mod identity;
pub mod manager;
pub mod pairing;
pub mod store;

pub use identity::DeviceIdentity;
/// Mensajes protobuf del enlace (p. ej. [`pb::Feature`]).
pub use kushare_proto::link as pb;
pub use manager::{
    ConnectIntent, DialFuture, Dialer, LinkConfig, LinkEvent, LinkManager, LinkStream, LocalDevice,
    NotificationAction, NotificationData, PROTOCOL_VERSION, PeerInfo, RemoteNotification,
    Transport,
};
pub use store::{Direction, PeerRecord};
