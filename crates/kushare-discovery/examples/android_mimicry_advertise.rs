//! Experimento: anuncia por BLE bajo el UUID real de Google Nearby/Quick
//! Share (`0xFEF3`) con un payload que imita la FORMA observada en una
//! captura real (ver `crate::ble` para la documentación completa de esa
//! captura), pero con la mayoría del contenido en cero porque no
//! entendemos su significado.
//!
//! Estructura observada (127 bytes totales):
//!
//! ```text
//! [ 33 bytes: prefijo sin descifrar ]
//! [ 0x09 ][ 0x15 ][ 21 bytes UTF-8: nombre del dispositivo ]
//! [ 71 bytes: sufijo sin descifrar ]
//! ```
//!
//! Este binario reproduce esa forma (33 bytes de ceros, el campo de
//! nombre real con el nombre que le pases, y 71 bytes de ceros).
//!
//! ## Resultado del experimento (2026-09-22, Android 16 real vía ADB)
//!
//! **Negativo.** Con este advertisement corriendo activamente, la pantalla
//! "Enviar" de Quick Share del Android real estuvo ~30 segundos en
//! "Buscando dispositivos cercanos..." sin que "kushare-pc" (ni ningún
//! dispositivo) apareciera. Confirma la sospecha: el prefijo/sufijo que no
//! entendemos no son relleno decorativo, Android los valida (muy
//! probablemente una prueba criptográfica —certificado, hash de sesión,
//! clave pública— atada a una cuenta de Google real) y descarta cualquier
//! advertisement que no la traiga, en vez de mostrarlo como "dispositivo
//! desconocido". Reproducir el formato completo requeriría entender ese
//! bloque, no solo la posición del campo de nombre.
//!
//! Uso: `cargo run -p kushare-discovery --example android_mimicry_advertise -- "nombre visible"`

use bluer::Uuid;
use bluer::adv::Advertisement;
use std::collections::{BTreeMap, BTreeSet};

const GOOGLE_NEARBY_SERVICE_UUID: Uuid = Uuid::from_u128(0x0000fef3_0000_1000_8000_00805f9b34fb);
const NAME_FIELD_TYPE: u8 = 0x09;
const PREFIX_LEN: usize = 33;
const SUFFIX_LEN: usize = 71;

fn build_payload(name: &str) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let mut payload = Vec::with_capacity(PREFIX_LEN + 2 + name_bytes.len() + SUFFIX_LEN);
    payload.extend(std::iter::repeat_n(0u8, PREFIX_LEN));
    payload.push(NAME_FIELD_TYPE);
    payload.push(name_bytes.len() as u8);
    payload.extend_from_slice(name_bytes);
    payload.extend(std::iter::repeat_n(0u8, SUFFIX_LEN));
    payload
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "kushare-pc".to_string());

    let payload = build_payload(&name);
    println!(
        "Anunciando {} bytes bajo {} con nombre {:?} (Ctrl+C para detener)",
        payload.len(),
        GOOGLE_NEARBY_SERVICE_UUID,
        name
    );

    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;

    let mut service_data = BTreeMap::new();
    service_data.insert(GOOGLE_NEARBY_SERVICE_UUID, payload);

    let advertisement = Advertisement {
        service_uuids: BTreeSet::from([GOOGLE_NEARBY_SERVICE_UUID]),
        service_data,
        discoverable: Some(true),
        local_name: Some(name),
        ..Default::default()
    };

    let _handle = adapter.advertise(advertisement).await?;
    tokio::signal::ctrl_c().await?;
    Ok(())
}
