//! Herramienta de diagnóstico: escanea BLE sin filtrar y vuelca nombre +
//! UUIDs + service_data crudos de cada dispositivo visto. Pensada para
//! validar cambios en `kushare-discovery::ble` contra un Android real con
//! Quick Share abierto (pantalla "Recibir" o "Enviar"): `cargo run -p
//! kushare-discovery --example ble_dump`. No forma parte de la API
//! pública del crate.

use futures_util::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;
    println!("Escaneando en {} ...", adapter.name());

    let mut events = adapter.discover_devices_with_changes().await?;
    while let Some(event) = events.next().await {
        let bluer::AdapterEvent::DeviceAdded(address) = event else {
            continue;
        };
        let Ok(device) = adapter.device(address) else {
            continue;
        };
        let name = device.name().await.ok().flatten();
        let uuids = device.uuids().await.ok().flatten().unwrap_or_default();
        let service_data = device
            .service_data()
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let rssi = device.rssi().await.ok().flatten();

        if !service_data.is_empty() || uuids.iter().any(|u| u.to_string().starts_with("0000fe2c")) {
            println!("--- {address} rssi={rssi:?} name={name:?} ---");
            for u in &uuids {
                println!("  uuid: {u}");
            }
            for (uuid, data) in &service_data {
                println!("  service_data[{uuid}] = {data:02x?}  (len={})", data.len());
            }
        }
    }

    Ok(())
}
