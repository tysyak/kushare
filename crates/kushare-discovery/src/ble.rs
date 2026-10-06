//! Descubrimiento por BLE (Bluetooth Low Energy) vía BlueZ (`bluer`).
//!
//! ## Qué hace BLE aquí (y qué NO hace)
//!
//! En el protocolo Quick Share real, BLE se usa **únicamente** como baliza
//! de bajo consumo para detectar "hay un dispositivo cerca": nunca
//! transporta los bytes del archivo (el ancho de banda de BLE es
//! insuficiente para eso). Los bytes siempre viajan por TCP una vez que
//! ambos lados se conectan (ver `kushare-transfer`); BLE tampoco expone una
//! dirección IP (una dirección Bluetooth `AA:BB:CC:DD:EE:FF` no es una
//! `SocketAddr`), así que un "avistamiento" BLE por sí solo no basta para
//! iniciar una transferencia — sirve como señal de proximidad que se
//! correlaciona con el descubrimiento mDNS/WiFi LAN (ver `crate::mdns`) por
//! su `endpoint_id` compartido.
//!
//! ## Formato del advertisement
//!
//! El `service_data` bajo [`NEARBY_SERVICE_UUID`] es una convención propia
//! de kushare (marcador de versión `0x00` + 4 bytes ASCII del
//! `endpoint_id`), pensada para que dos instancias de kushare se
//! descubran entre sí por Bluetooth de verdad.
//!
//! ## Hallazgo real (captura del 2026-09-22, ver `examples/ble_dump.rs`)
//!
//! Escaneando BLE sin filtrar mientras un Android real (Android 16, GMS)
//! tenía abierta la pantalla "Recibir" de Quick Share con visibilidad
//! "temporalmente visible para todos", apareció un dispositivo anunciando
//! bajo el UUID **`0000fef3-...`** (no `0xFE2C`) un `service_data` de 127
//! bytes que contiene, en claro, el nombre visible configurado en el
//! teléfono:
//!
//! ```text
//! ... 49 09 15 74 65 6c c3 a9 66 6f 6e 6f 20 64 65 20
//! 43 72 69 73 74 69 61 6e ae 5e 46 c3 c3 5e 00 00 ...
//! ```
//!
//! `74 65 6c c3 a9 ...` decodifica en UTF-8 a `"teléfono de Cristian"`
//! (exactamente el nombre mostrado en pantalla), precedido por `09 15`:
//! un byte de tipo de subcampo (`0x09`, coincide con el AD type estándar
//! de BLE "Complete Local Name") y un byte de longitud (`0x15` = 21,
//! justo el tamaño en bytes UTF-8 de esa cadena). Justo después aparecen
//! `ae 5e 46 c3 c3 5e`, que coincide byte a byte con la dirección BT
//! clásica de ese mismo teléfono (visible como dispositivo ya emparejado
//! en `bluetoothctl`). Esto confirma que:
//!
//! - El UUID de servicio real que usa Quick Share es `0xFEF3`, no
//!   `0xFE2C` (que sí es un UUID Bluetooth SIG real — "Nearby" de
//!   Google/Fast Pair — pero no el que se vio en esta captura).
//! - El payload no es un formato simple y fijo: mezcla al menos un
//!   identificador binario, una subestructura tipo-longitud-valor con el
//!   nombre del dispositivo, y una dirección Bluetooth clásica.
//!
//! [`extract_google_device_name`] implementa la heurística mínima para
//! extraer ese nombre (busca el patrón `[0x09][len][UTF-8 de `len`
//! bytes]` en cualquier posición del payload) — no es un parser completo
//! del formato de Google, que sigue sin documentarse en detalle más allá
//! de esta única captura.
//!
//! TODO(android-validation): en pruebas posteriores, el mismo teléfono
//! alternó entre al menos dos variantes de payload bajo el mismo UUID: una
//! corta (~92 bytes, sin el campo de nombre) y la larga de 127 bytes
//! documentada arriba. `extract_google_device_name` solo encuentra el
//! nombre cuando le toca ver la variante larga durante la ventana de
//! escaneo — es esperable que `AndroidQuickShare { name: None, .. }` sea
//! el resultado más común en la práctica, no un fallo del parser.
//!
//! **Se intentó, y falló, hacer el camino inverso**: `examples/
//! android_mimicry_advertise.rs` anuncia bajo `GOOGLE_NEARBY_SERVICE_UUID`
//! imitando la forma general de esta captura (mismo campo de nombre en la
//! misma posición, resto en ceros). Probado en vivo contra el Android real
//! del 2026-09-22: la pantalla "Enviar" de Quick Share nunca mostró
//! "kushare-pc" tras ~30s de escaneo activo. Conclusión: el prefijo/sufijo
//! no son relleno — Android los valida (probablemente una prueba
//! criptográfica atada a una cuenta de Google real) y descarta en
//! silencio cualquier advertisement que no la traiga. Sin entender ese
//! bloque, kushare no puede hacerse pasar por un peer válido ante un
//! Android real; sí puede seguir *detectando* su presencia (este módulo)
//! y, en el mejor caso, su nombre visible — nunca iniciar una conexión con
//! él por BLE (ver también la nota de más arriba sobre por qué BLE nunca
//! resuelve una IP). Sigue pendiente: más capturas (distintos
//! modelos/versiones de Android) y documentar el resto de la estructura
//! (los bytes antes del nombre y los bloques finales `84 15 0X ...` que se
//! repiten con un patrón regular en varias capturas y podrían ser
//! coordenadas de ubicación u otros metadatos), por si algún día alcanzan
//! para una imitación válida.

use bluer::Uuid;
use bluer::adv::Advertisement;
use futures_util::StreamExt;
use std::collections::{BTreeMap, BTreeSet};
use tokio::sync::mpsc;

/// UUID de servicio propio de kushare para el advertisement
/// kushare-a-kushare (ver documentación del módulo). No es el UUID que usa
/// el Quick Share real de Google (ver [`GOOGLE_NEARBY_SERVICE_UUID`]), pero
/// tampoco colisiona con nada conocido.
pub const NEARBY_SERVICE_UUID: Uuid = Uuid::from_u128(0x0000fe2c_0000_1000_8000_00805f9b34fb);

/// UUID de servicio real bajo el que un Android con Quick Share/Nearby
/// Share anuncia su presencia por BLE, confirmado por captura real (ver
/// documentación del módulo).
pub const GOOGLE_NEARBY_SERVICE_UUID: Uuid =
    Uuid::from_u128(0x0000fef3_0000_1000_8000_00805f9b34fb);

/// Byte de marcador al inicio del `service_data`: distingue esta
/// convención propia de kushare de un futuro formato compatible con
/// Android real, sin romper a los escuchas que no lo reconozcan.
const KUSHARE_BLE_MARKER: u8 = 0x00;

/// Un dispositivo visto por BLE. Ninguna de las dos variantes trae una
/// dirección de red (ver la nota del módulo sobre por qué BLE no puede
/// resolver una `SocketAddr` por sí solo): sirven como señal de
/// proximidad, a correlacionar con el descubrimiento mDNS/WiFi LAN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BleSighting {
    /// Otra instancia de kushare, identificada por su `endpoint_id`.
    Kushare {
        endpoint_id: String,
        rssi: Option<i16>,
    },
    /// Un dispositivo Android real con Quick Share/Nearby Share visible
    /// (detectado por [`GOOGLE_NEARBY_SERVICE_UUID`]). `name` solo se
    /// rellena si la heurística de [`extract_google_device_name`]
    /// reconoce el patrón del nombre en el payload.
    AndroidQuickShare {
        name: Option<String>,
        rssi: Option<i16>,
    },
}

fn encode_service_data(endpoint_id: &str) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        endpoint_id.len() == 4 && endpoint_id.is_ascii(),
        "endpoint_id debe ser exactamente 4 caracteres ASCII, recibido: {endpoint_id:?}"
    );
    let mut payload = Vec::with_capacity(5);
    payload.push(KUSHARE_BLE_MARKER);
    payload.extend_from_slice(endpoint_id.as_bytes());
    Ok(payload)
}

fn decode_service_data(payload: &[u8]) -> Option<String> {
    if payload.len() != 5 || payload[0] != KUSHARE_BLE_MARKER {
        return None;
    }
    std::str::from_utf8(&payload[1..]).ok().map(str::to_string)
}

/// Heurística para extraer el nombre visible de un advertisement real de
/// Google Nearby/Quick Share (ver documentación del módulo): busca en
/// cualquier posición del payload el patrón `[0x09][len][len bytes UTF-8
/// válidos]`, igual que el AD type BLE estándar "Complete Local Name".
fn extract_google_device_name(payload: &[u8]) -> Option<String> {
    const NAME_FIELD_TYPE: u8 = 0x09;
    for i in 0..payload.len().saturating_sub(1) {
        if payload[i] != NAME_FIELD_TYPE {
            continue;
        }
        let len = payload[i + 1] as usize;
        let start = i + 2;
        let Some(end) = start.checked_add(len) else {
            continue;
        };
        if end > payload.len() || len == 0 {
            continue;
        }
        if let Ok(name) = std::str::from_utf8(&payload[start..end]) {
            return Some(name.to_string());
        }
    }
    None
}

pub struct BleAdvertiser {
    adapter: bluer::Adapter,
}

impl BleAdvertiser {
    pub async fn new() -> anyhow::Result<Self> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;
        Ok(Self { adapter })
    }

    /// Empieza a anunciar este dispositivo por BLE con su `endpoint_id`
    /// (el mismo usado en mDNS y en `ConnectionRequestFrame`). Devuelve un
    /// handle que, al descartarse (`drop`), retira el advertisement.
    pub async fn start_advertising(
        &self,
        endpoint_id: &str,
    ) -> anyhow::Result<bluer::adv::AdvertisementHandle> {
        let mut service_data = BTreeMap::new();
        service_data.insert(NEARBY_SERVICE_UUID, encode_service_data(endpoint_id)?);

        let advertisement = Advertisement {
            service_uuids: BTreeSet::from([NEARBY_SERVICE_UUID]),
            service_data,
            discoverable: Some(true),
            ..Default::default()
        };

        Ok(self.adapter.advertise(advertisement).await?)
    }
}

pub struct BleScanner {
    adapter: bluer::Adapter,
}

impl BleScanner {
    pub async fn new() -> anyhow::Result<Self> {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        adapter.set_powered(true).await?;
        Ok(Self { adapter })
    }

    /// Inicia un escaneo BLE continuo y devuelve un canal de
    /// [`BleSighting`]s: uno por cada dispositivo visto anunciando
    /// [`NEARBY_SERVICE_UUID`] con la convención de payload de kushare
    /// (ver documentación del módulo). El escaneo sigue corriendo en una
    /// tarea de tokio hasta que el `Receiver` devuelto se descarta.
    pub async fn scan(&self) -> anyhow::Result<mpsc::Receiver<BleSighting>> {
        // `discover_devices_with_changes` reemite `DeviceAdded` cada vez
        // que cambian las propiedades de un dispositivo (incluido
        // ServiceData), a diferencia de `discover_devices` que solo lo
        // hace una vez; el service_data de un advertisement casi nunca
        // está listo en el primer evento.
        let mut events = self.adapter.discover_devices_with_changes().await?;
        let adapter = self.adapter.clone();
        let (tx, rx) = mpsc::channel(32);

        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                let bluer::AdapterEvent::DeviceAdded(address) = event else {
                    continue;
                };
                let Ok(device) = adapter.device(address) else {
                    continue;
                };
                let Ok(Some(service_data)) = device.service_data().await else {
                    continue;
                };
                let rssi = device.rssi().await.ok().flatten();

                let sighting = if let Some(payload) = service_data.get(&NEARBY_SERVICE_UUID) {
                    decode_service_data(payload)
                        .map(|endpoint_id| BleSighting::Kushare { endpoint_id, rssi })
                } else {
                    service_data
                        .get(&GOOGLE_NEARBY_SERVICE_UUID)
                        .map(|payload| BleSighting::AndroidQuickShare {
                            name: extract_google_device_name(payload),
                            rssi,
                        })
                };

                let Some(sighting) = sighting else { continue };
                if tx.send(sighting).await.is_err() {
                    break; // el receptor se descartó: dejar de escanear.
                }
            }
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_data_roundtrips() {
        let encoded = encode_service_data("AB12").unwrap();
        assert_eq!(decode_service_data(&encoded), Some("AB12".to_string()));
    }

    #[test]
    fn rejects_wrong_length_endpoint_id() {
        assert!(encode_service_data("ABC").is_err());
        assert!(encode_service_data("ABCDE").is_err());
    }

    #[test]
    fn decode_ignores_foreign_payloads() {
        // Un payload de otra longitud u otro marcador (p. ej. un futuro
        // formato compatible con Android real) no debe confundirse con el
        // de kushare.
        assert_eq!(decode_service_data(&[0x01, b'A', b'B', b'C', b'D']), None);
        assert_eq!(decode_service_data(&[0x00, b'A', b'B', b'C']), None);
    }

    /// Bytes capturados en vivo el 2026-09-22 (ver `examples/ble_dump.rs`)
    /// de un Android real anunciando bajo `GOOGLE_NEARBY_SERVICE_UUID` con
    /// Quick Share en modo "Recibir, visible para todos". Sirve como test
    /// de regresión de la heurística contra un caso real, no inventado.
    #[test]
    fn extracts_name_from_real_captured_android_advertisement() {
        let payload: &[u8] = &[
            0x48, 0xfc, 0x9f, 0x5e, 0x00, 0x00, 0x00, 0x38, 0x23, 0xfc, 0x9f, 0x5e, 0x4f, 0x44,
            0x58, 0x54, 0x27, 0x22, 0xc6, 0xbc, 0x1f, 0x30, 0x78, 0xbf, 0xe7, 0x3b, 0xfb, 0xe0,
            0x7c, 0xd1, 0x77, 0x9b, 0x49, 0x09, 0x15, 0x74, 0x65, 0x6c, 0xc3, 0xa9, 0x66, 0x6f,
            0x6e, 0x6f, 0x20, 0x64, 0x65, 0x20, 0x43, 0x72, 0x69, 0x73, 0x74, 0x69, 0x61, 0x6e,
            0xae, 0x5e, 0x46, 0xc3, 0xc3, 0x5e, 0x00, 0x00,
        ];
        assert_eq!(
            extract_google_device_name(payload),
            Some("teléfono de Cristian".to_string())
        );
    }

    #[test]
    fn extract_google_device_name_returns_none_without_the_pattern() {
        assert_eq!(extract_google_device_name(&[0x01, 0x02, 0x03]), None);
    }
}
