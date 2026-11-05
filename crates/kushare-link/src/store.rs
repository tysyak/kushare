//! Peers emparejados y su configuración, persistidos en un archivo TOML.
//!
//! ## Sincronización de configuración (last-writer-wins)
//!
//! Cada dirección de notificaciones (`A→B`, `B→A`) y cada filtro de apps
//! es un registro versionado ([`Versioned`]) que existe en ambos
//! dispositivos. Quien lo cambia sube `version` y firma `updated_by` con su
//! `device_id`; al recibir el del otro lado se queda con el "mayor" según
//! [`Versioned::supersedes`]: más versión gana y, en empate (dos cambios
//! concurrentes con el enlace caído), gana el `device_id` menor. Ambos
//! lados aplican la misma regla, así que tras intercambiar el estado al
//! reconectar convergen siempre al mismo valor.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::identity::write_private_file;

/// Un valor sincronizado entre los dos lados de un enlace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versioned<T> {
    pub value: T,
    pub version: u64,
    /// `device_id` de quien hizo el último cambio; vacío = valor por
    /// defecto, nunca modificado.
    pub updated_by: String,
}

impl<T> Versioned<T> {
    pub fn initial(value: T) -> Self {
        Self {
            value,
            version: 0,
            updated_by: String::new(),
        }
    }

    /// ¿Debe `self` (recién recibido) reemplazar a `current`?
    pub fn supersedes(&self, current: &Self) -> bool {
        match self.version.cmp(&current.version) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            // Empate: gana el device_id menor. Un `updated_by` vacío (valor
            // por defecto) pierde siempre contra un cambio real.
            std::cmp::Ordering::Equal => {
                match (self.updated_by.is_empty(), current.updated_by.is_empty()) {
                    (false, true) => true,
                    (true, _) => false,
                    (false, false) => self.updated_by < current.updated_by,
                }
            }
        }
    }

    /// Cambio local: nueva versión firmada por `local_id`.
    pub fn bump(&mut self, value: T, local_id: &str) {
        self.value = value;
        self.version += 1;
        self.updated_by = local_id.to_string();
    }
}

/// Sentido de las notificaciones respecto a este dispositivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// De este dispositivo hacia el peer.
    Outgoing,
    /// Del peer hacia este dispositivo.
    Incoming,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub device_kind: String,
    /// Clave pública Ed25519 en hex.
    pub public_key: String,
    /// Última dirección `ip:puerto` del servicio de enlace vista por mDNS.
    #[serde(default)]
    pub last_tcp_addr: Option<String>,
    /// Dirección Bluetooth del peer, si se conoce (ver `Hello.bt_address`).
    #[serde(default)]
    pub bt_address: Option<String>,

    /// ¿Se reenvían las notificaciones de este dispositivo al peer?
    pub outgoing: Versioned<bool>,
    /// ¿Se muestran aquí las notificaciones del peer?
    pub incoming: Versioned<bool>,
    /// Apps de este dispositivo que no se reenvían al peer.
    pub outgoing_blocked: Versioned<BTreeSet<String>>,
    /// Apps del peer cuyas notificaciones no se muestran aquí.
    pub incoming_blocked: Versioned<BTreeSet<String>>,
    /// Apps del peer vistas alguna vez (app_id → nombre), para la UI del
    /// filtro de entrada.
    #[serde(default)]
    pub peer_apps: BTreeMap<String, String>,
}

impl PeerRecord {
    /// Peer recién emparejado: ambas direcciones activadas, sin filtros.
    pub fn new(
        device_id: String,
        device_name: String,
        device_kind: String,
        public_key: String,
    ) -> Self {
        Self {
            device_id,
            device_name,
            device_kind,
            public_key,
            last_tcp_addr: None,
            bt_address: None,
            outgoing: Versioned::initial(true),
            incoming: Versioned::initial(true),
            outgoing_blocked: Versioned::initial(BTreeSet::new()),
            incoming_blocked: Versioned::initial(BTreeSet::new()),
            peer_apps: BTreeMap::new(),
        }
    }

    pub fn direction(&self, direction: Direction) -> &Versioned<bool> {
        match direction {
            Direction::Outgoing => &self.outgoing,
            Direction::Incoming => &self.incoming,
        }
    }

    pub fn direction_mut(&mut self, direction: Direction) -> &mut Versioned<bool> {
        match direction {
            Direction::Outgoing => &mut self.outgoing,
            Direction::Incoming => &mut self.incoming,
        }
    }

    pub fn blocked(&self, direction: Direction) -> &Versioned<BTreeSet<String>> {
        match direction {
            Direction::Outgoing => &self.outgoing_blocked,
            Direction::Incoming => &self.incoming_blocked,
        }
    }

    pub fn blocked_mut(&mut self, direction: Direction) -> &mut Versioned<BTreeSet<String>> {
        match direction {
            Direction::Outgoing => &mut self.outgoing_blocked,
            Direction::Incoming => &mut self.incoming_blocked,
        }
    }

    /// ¿Debe reenviarse al peer una notificación local de `app_id`?
    pub fn allows_outgoing(&self, app_id: &str) -> bool {
        self.outgoing.value && !self.outgoing_blocked.value.contains(app_id)
    }

    /// ¿Debe mostrarse aquí una notificación del peer de `app_id`?
    pub fn allows_incoming(&self, app_id: &str) -> bool {
        self.incoming.value && !self.incoming_blocked.value.contains(app_id)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoreData {
    #[serde(default)]
    peers: BTreeMap<String, PeerRecord>,
    /// Apps de ESTE dispositivo vistas alguna vez (app_id → nombre), para
    /// la UI del filtro de salida.
    #[serde(default)]
    local_apps: BTreeMap<String, String>,
}

/// Archivo `peers.toml`. Cada cambio se guarda en el acto (escritura
/// atómica, permisos 0600: contiene las claves públicas de confianza y la
/// lista de apps).
#[derive(Debug)]
pub struct PeerStore {
    path: Option<PathBuf>,
    data: StoreData,
}

impl PeerStore {
    /// Carga `path`, o empieza vacío si no existe.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let data = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                anyhow::anyhow!("{} no es un peers.toml válido: {e}", path.display())
            })?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => StoreData::default(),
            Err(err) => return Err(err.into()),
        };
        Ok(Self {
            path: Some(path.to_path_buf()),
            data,
        })
    }

    /// Almacén solo en memoria (tests).
    pub fn in_memory() -> Self {
        Self {
            path: None,
            data: StoreData::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&self.data)?;
        write_private_file(path, text.as_bytes())?;
        Ok(())
    }

    pub fn get(&self, device_id: &str) -> Option<&PeerRecord> {
        self.data.peers.get(device_id)
    }

    pub fn get_mut(&mut self, device_id: &str) -> Option<&mut PeerRecord> {
        self.data.peers.get_mut(device_id)
    }

    pub fn upsert(&mut self, record: PeerRecord) {
        self.data.peers.insert(record.device_id.clone(), record);
    }

    pub fn remove(&mut self, device_id: &str) -> Option<PeerRecord> {
        self.data.peers.remove(device_id)
    }

    pub fn peers(&self) -> impl Iterator<Item = &PeerRecord> {
        self.data.peers.values()
    }

    pub fn local_apps(&self) -> &BTreeMap<String, String> {
        &self.data.local_apps
    }

    /// Registra una app local; devuelve `true` si era nueva (o cambió de
    /// nombre) y por tanto hay que guardar.
    pub fn note_local_app(&mut self, app_id: &str, app_name: &str) -> bool {
        note_app(&mut self.data.local_apps, app_id, app_name)
    }
}

pub(crate) fn note_app(apps: &mut BTreeMap<String, String>, app_id: &str, app_name: &str) -> bool {
    if app_id.is_empty() {
        return false;
    }
    let name = if app_name.is_empty() {
        app_id
    } else {
        app_name
    };
    if apps.get(app_id).map(String::as_str) == Some(name) {
        return false;
    }
    apps.insert(app_id.to_string(), name.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(value: bool, version: u64, by: &str) -> Versioned<bool> {
        Versioned {
            value,
            version,
            updated_by: by.to_string(),
        }
    }

    #[test]
    fn higher_version_wins() {
        assert!(v(false, 2, "b").supersedes(&v(true, 1, "a")));
        assert!(!v(false, 1, "a").supersedes(&v(true, 2, "b")));
    }

    #[test]
    fn tie_is_broken_by_smaller_device_id_on_both_sides() {
        let from_a = v(false, 3, "aaaa");
        let from_b = v(true, 3, "bbbb");
        // Cada lado recibe el valor del otro: ambos deben acabar con el de "aaaa".
        assert!(from_a.supersedes(&from_b));
        assert!(!from_b.supersedes(&from_a));
        // Recibir el mismo valor que ya se tiene no lo "reemplaza".
        assert!(!from_a.supersedes(&from_a.clone()));
    }

    #[test]
    fn default_never_overrides_a_real_change() {
        let default = Versioned::initial(true);
        let changed = v(false, 0, "zzzz");
        assert!(changed.supersedes(&default));
        assert!(!default.supersedes(&changed));
    }

    #[test]
    fn store_roundtrips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.toml");

        let mut store = PeerStore::load(&path).unwrap();
        let mut peer = PeerRecord::new(
            "abcd".into(),
            "Mi teléfono".into(),
            "android".into(),
            "00ff".into(),
        );
        peer.last_tcp_addr = Some("192.168.1.20:4000".into());
        peer.outgoing.bump(false, "self");
        peer.incoming_blocked
            .bump(["com.whatsapp".to_string()].into(), "abcd");
        peer.peer_apps
            .insert("com.whatsapp".into(), "WhatsApp".into());
        store.upsert(peer.clone());
        assert!(store.note_local_app("firefox", "Firefox"));
        assert!(!store.note_local_app("firefox", "Firefox"));
        store.save().unwrap();

        let loaded = PeerStore::load(&path).unwrap();
        assert_eq!(loaded.get("abcd"), Some(&peer));
        assert_eq!(
            loaded.local_apps().get("firefox").map(String::as_str),
            Some("Firefox")
        );
        assert!(!loaded.get("abcd").unwrap().allows_outgoing("firefox"));
        assert!(!loaded.get("abcd").unwrap().allows_incoming("com.whatsapp"));
        assert!(loaded.get("abcd").unwrap().allows_incoming("org.telegram"));
    }
}
