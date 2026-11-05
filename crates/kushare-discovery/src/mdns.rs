//! Descubrimiento por mDNS: anuncia este dispositivo y escucha anuncios de
//! otros dispositivos Quick Share/Nearby Share en la red local.
//!
//! El tipo de servicio `_FC9F5ED42C8A._tcp.local.` está documentado
//! públicamente por implementaciones de referencia (`grishka/NearDrop`,
//! `Martichou/rquickshare`) como el usado por Nearby Connections para Quick
//! Share; no es un identificador oficial de la IANA, es específico del
//! protocolo de Google.
//!
//! TODO(android-validation): el contenido exacto del TXT record `n` (según
//! NearDrop, una codificación TLV en base64 del `EndpointInfo`: PCP, device
//! type e icono, y nombre del dispositivo) se aproxima aquí de forma
//! razonable pero no ha sido verificado byte a byte contra lo que emite un
//! Android real.
//!
//! ## Resultado real: Android nunca intenta conectarse (2026-09-22)
//!
//! Con este anuncio mDNS publicado y confirmado con `avahi-browse` (visible
//! en la interfaz WiFi real, no solo loopback), se dejó la pantalla
//! "Enviar" de Quick Share de un Android real escaneando activamente
//! durante 25 segundos. `kushare_transfer::Receiver::run` registra
//! (`tracing::info!`) cada conexión TCP entrante — no llegó ninguna. Es un
//! resultado más contundente que el de BLE (`crate::ble`): ahí al menos el
//! UUID de servicio coincidía y Android llegaba a evaluar el payload; aquí
//! Android ni siquiera intenta abrir el socket, pese a que el registro
//! mDNS está bien formado.
//!
//! La explicación más coherente con ambos hallazgos: BLE es la puerta de
//! entrada obligatoria del descubrimiento (la señal de bajo consumo que
//! avisa "hay un dispositivo Quick Share cerca"); mDNS/WiFi LAN solo se
//! usa **después**, para promover una conexión ya confiada por BLE a un
//! canal de más ancho de banda — no es un descubrimiento independiente. Al
//! fallar la validación BLE (ver `examples/android_mimicry_advertise.rs`),
//! Android nunca llega a intentar mDNS con kushare, sin importar qué tan
//! correcto sea el TXT record. Confirmar/refutar esto con más pruebas
//! sigue pendiente; si se llega a "romper" el payload BLE, revisar de
//! nuevo si el `n` de aquí basta o también necesita ajustarse.

use kushare_core::DiscoveredDevice;
pub use mdns_sd::ServiceEvent;
use mdns_sd::{Receiver, ServiceDaemon, ServiceInfo};

/// Tipo de servicio mDNS usado por Quick Share/Nearby Connections.
pub const SERVICE_TYPE: &str = "_FC9F5ED42C8A._tcp.local.";

/// Tipo de servicio mDNS del enlace persistente propio de kushare (ver
/// `kushare-link`). Es independiente de Quick Share a propósito: solo lo
/// entienden otros kushare (escritorio o Android), y por él viaja contenido
/// sensible (notificaciones) solo hacia peers emparejados.
pub const LINK_SERVICE_TYPE: &str = "_kushare-link._tcp.local.";

/// Anuncia este dispositivo por mDNS mientras el valor esté vivo; al
/// destruirse (`Drop` del `ServiceDaemon` subyacente) deja de anunciarse.
pub struct MdnsAdvertiser {
    daemon: ServiceDaemon,
    fullname: Option<String>,
}

impl MdnsAdvertiser {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            daemon: ServiceDaemon::new()?,
            fullname: None,
        })
    }

    /// Empieza a anunciar `device_name` en el puerto `port`. `endpoint_id`
    /// es el identificador de 4 caracteres del protocolo Nearby Connections
    /// (ver `kushare_core::Device::endpoint_id`).
    pub fn start_advertising(
        &mut self,
        device_name: &str,
        endpoint_id: &str,
        port: u16,
    ) -> anyhow::Result<()> {
        let properties = [("n", encode_endpoint_name(device_name))];
        self.start_advertising_service(SERVICE_TYPE, endpoint_id, device_name, port, &properties)
    }

    /// Anuncia el servicio del enlace persistente (`LINK_SERVICE_TYPE`):
    /// la instancia es el `device_id` (hex, siempre válido como etiqueta
    /// DNS) y los TXT `id`/`n` llevan el id y el nombre legible.
    pub fn start_advertising_link(
        &mut self,
        device_id: &str,
        device_name: &str,
        port: u16,
    ) -> anyhow::Result<()> {
        let properties = [
            ("id", device_id.to_string()),
            ("n", device_name.to_string()),
        ];
        self.start_advertising_service(LINK_SERVICE_TYPE, device_id, device_id, port, &properties)
    }

    /// Anuncio genérico: `service_type` con la instancia `instance_name`,
    /// en `port`, con las propiedades TXT dadas. Un mismo
    /// `MdnsAdvertiser` anuncia un único servicio a la vez.
    pub fn start_advertising_service(
        &mut self,
        service_type: &str,
        instance_name: &str,
        host_label: &str,
        port: u16,
        properties: &[(&str, String)],
    ) -> anyhow::Result<()> {
        self.stop_advertising()?;
        let host_name = format!("{}.local.", dns_label(host_label));

        let service_info = ServiceInfo::new(
            service_type,
            instance_name,
            &host_name,
            "",
            port,
            properties,
        )?
        .enable_addr_auto();

        self.fullname = Some(service_info.get_fullname().to_string());
        self.daemon.register(service_info)?;
        Ok(())
    }

    pub fn stop_advertising(&mut self) -> anyhow::Result<()> {
        if let Some(fullname) = self.fullname.take() {
            self.daemon.unregister(&fullname)?;
        }
        Ok(())
    }
}

/// Convierte un texto arbitrario (p. ej. "kushare @ mi-pc") en una
/// etiqueta DNS válida para el nombre de host del anuncio: solo
/// `[a-z0-9-]`, sin guiones en los extremos, máximo 63 caracteres.
fn dns_label(text: &str) -> String {
    let mut label: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while label.contains("--") {
        label = label.replace("--", "-");
    }
    let label = label.trim_matches('-');
    let label: String = label.chars().take(63).collect();
    let label = label.trim_end_matches('-');
    if label.is_empty() {
        "kushare".to_string()
    } else {
        label.to_string()
    }
}

/// Codifica el nombre de dispositivo tal como se espera en el TXT record
/// `n` (base64 estándar). Ver el TODO(android-validation) del módulo.
fn encode_endpoint_name(device_name: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(device_name.as_bytes())
}

/// Escucha anuncios mDNS de otros dispositivos Quick Share en la red local.
pub struct MdnsScanner {
    daemon: ServiceDaemon,
}

impl MdnsScanner {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            daemon: ServiceDaemon::new()?,
        })
    }

    /// Devuelve un receptor de eventos de descubrimiento crudos de
    /// `mdns-sd`; usar [`discovered_device_from_event`] para convertirlos a
    /// [`DiscoveredDevice`].
    pub fn browse(&self) -> anyhow::Result<Receiver<ServiceEvent>> {
        self.browse_service(SERVICE_TYPE)
    }

    /// Como [`Self::browse`] pero para cualquier tipo de servicio (p. ej.
    /// [`LINK_SERVICE_TYPE`]; ver [`link_peer_from_event`]).
    pub fn browse_service(&self, service_type: &str) -> anyhow::Result<Receiver<ServiceEvent>> {
        Ok(self.daemon.browse(service_type)?)
    }

    /// Deja de buscar `service_type` (el receptor de `browse_service`
    /// recibe `SearchStopped` y después se cierra).
    pub fn stop_browse(&self, service_type: &str) -> anyhow::Result<()> {
        Ok(self.daemon.stop_browse(service_type)?)
    }
}

/// Un peer del enlace persistente visto por mDNS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPeerSighting {
    pub device_id: String,
    pub device_name: String,
    pub addr: std::net::SocketAddr,
}

/// Convierte un `ServiceResolved` de [`LINK_SERVICE_TYPE`] en un
/// [`LinkPeerSighting`]. Prefiere una dirección IPv4 si el anuncio trae
/// varias (las IPv6 link-local necesitan scope id y suelen fallar al
/// conectar).
pub fn link_peer_from_event(event: &ServiceEvent) -> Option<LinkPeerSighting> {
    let ServiceEvent::ServiceResolved(resolved) = event else {
        return None;
    };
    if !resolved.ty_domain.eq_ignore_ascii_case(LINK_SERVICE_TYPE) {
        return None;
    }
    let device_id = resolved
        .txt_properties
        .get_property_val_str("id")?
        .to_string();
    let device_name = resolved
        .txt_properties
        .get_property_val_str("n")
        .unwrap_or(&device_id)
        .to_string();
    let ips: Vec<std::net::IpAddr> = resolved.addresses.iter().map(|a| a.to_ip_addr()).collect();
    let ip = ips
        .iter()
        .find(|ip| ip.is_ipv4())
        .or_else(|| ips.first())
        .copied()?;
    Some(LinkPeerSighting {
        device_id,
        device_name,
        addr: std::net::SocketAddr::new(ip, resolved.port),
    })
}

/// Convierte un `ServiceEvent::ServiceResolved` en un [`DiscoveredDevice`],
/// o `None` si el evento no trae información de dirección suficiente o no
/// es un evento de resolución.
pub fn discovered_device_from_event(event: &ServiceEvent) -> Option<DiscoveredDevice> {
    let ServiceEvent::ServiceResolved(resolved) = event else {
        return None;
    };
    let addr = resolved.addresses.iter().next()?.to_ip_addr();
    let endpoint_id = resolved.fullname.split('.').next().unwrap_or_default();

    Some(DiscoveredDevice {
        name: resolved.host.trim_end_matches('.').to_string(),
        endpoint_id: endpoint_id.to_string(),
        addr: std::net::SocketAddr::new(addr, resolved.port),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_label_sanitizes_device_names() {
        assert_eq!(dns_label("kushare @ mi-pc"), "kushare-mi-pc");
        assert_eq!(dns_label("  Ñoño_PC  "), "o-o-pc");
        assert_eq!(dns_label("@@@"), "kushare");
        assert!(dns_label(&"a".repeat(100)).len() <= 63);
    }
}
