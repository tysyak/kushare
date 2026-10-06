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
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};

/// Tipo de servicio mDNS usado por Quick Share/Nearby Connections.
pub const SERVICE_TYPE: &str = "_FC9F5ED42C8A._tcp.local.";

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
        let host_name = format!("{device_name}.local.");
        let properties = [("n", encode_endpoint_name(device_name))];

        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            endpoint_id,
            &host_name,
            "",
            port,
            &properties[..],
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
        Ok(self.daemon.browse(SERVICE_TYPE)?)
    }
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
