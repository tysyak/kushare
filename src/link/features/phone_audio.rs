//! Audio del teléfono en el PC (Bluetooth A2DP + HFP) y tonos DTMF por
//! Bluetooth manos libres o por el audio de la llamada.

use super::*;

// ---------------------------------------------------------------------------
// Audio del teléfono en el PC (Bluetooth A2DP + HFP)
// ---------------------------------------------------------------------------

/// Perfil A2DP "Audio Source" del teléfono: su música suena en el PC.
pub(super) const A2DP_SOURCE: &str = "0000110a-0000-1000-8000-00805f9b34fb";
/// Perfil HFP "Audio Gateway" del teléfono: el PC hace de manos libres en las llamadas.
pub(super) const HFP_AG: &str = "0000111f-0000-1000-8000-00805f9b34fb";

/// Conecta (o desconecta) los perfiles de audio del teléfono con el PC,
/// para oír en el PC su música y sus llamadas (y hablar con el micrófono
/// del PC). Requiere que PipeWire/WirePlumber tenga los roles `a2dp_sink`
/// y `hfp_hf` (vienen activados por defecto). Devuelve qué perfiles quedaron.
pub async fn phone_audio(address: &str, enable: bool) -> anyhow::Result<Vec<&'static str>> {
    let address: bluer::Address = address
        .parse()
        .map_err(|_| anyhow::anyhow!("dirección Bluetooth inválida: {address}"))?;
    let session = bluer::Session::new().await?;
    let device = session.default_adapter().await?.device(address)?;
    if enable && !device.is_paired().await.unwrap_or(false) {
        anyhow::bail!("el teléfono no está vinculado por Bluetooth con este PC");
    }
    let mut done = Vec::new();
    let mut last_err = None;
    for (uuid, label) in [(A2DP_SOURCE, "multimedia"), (HFP_AG, "llamadas")] {
        let uuid: bluer::Uuid = uuid.parse().expect("UUID válido");
        let result = if enable {
            device.connect_profile(&uuid).await
        } else {
            device.disconnect_profile(&uuid).await
        };
        match result {
            Ok(()) => done.push(label),
            Err(err) => {
                tracing::info!(%err, label, enable, "perfil de audio del teléfono");
                last_err = Some(err);
            }
        }
    }
    match (done.is_empty(), last_err) {
        (true, Some(err)) if enable => Err(anyhow::anyhow!(
            "no se pudo conectar el audio ({err}); en el teléfono, activa «Audio multimedia» y «Llamadas» para este PC en los ajustes de Bluetooth"
        )),
        _ => Ok(done),
    }
}

/// Pide al teléfono `address` que toque `digits` en su llamada por
/// Bluetooth manos libres (`AT+VTS`), con la API de telefonía de PipeWire
/// (`org.pipewire.Telephony`, PipeWire ≥ 1.4 con el backend HFP nativo).
pub async fn dtmf_hfp(address: &str, digits: &str) -> anyhow::Result<()> {
    const SERVICE: &str = "org.pipewire.Telephony";
    const AG: &str = "org.pipewire.Telephony.AudioGateway1";
    let conn = zbus::Connection::session().await?;
    let manager = zbus::fdo::ObjectManagerProxy::builder(&conn)
        .destination(SERVICE)?
        .path("/org/pipewire/Telephony")?
        .build()
        .await?;
    let objects = manager.get_managed_objects().await?;
    let path = objects
        .into_iter()
        .find(|(_, ifaces)| {
            ifaces
                .iter()
                .find(|(name, _)| name.as_str() == AG)
                .and_then(|(_, props)| props.get("Address"))
                .and_then(|v| <&str>::try_from(&**v).ok())
                .is_some_and(|a| a.eq_ignore_ascii_case(address))
        })
        .map(|(path, _)| path)
        .ok_or_else(|| anyhow::anyhow!("el teléfono no está conectado como manos libres (HFP)"))?;
    let proxy = zbus::Proxy::new(&conn, SERVICE, path, AG).await?;
    proxy
        .call_method("SendTones", &(digits,))
        .await
        .map_err(|e| anyhow::anyhow!("el teléfono no aceptó los tonos: {e}"))?;
    Ok(())
}

/// Toca `digits` como tonos DTMF en la llamada del teléfono `address`
/// cuando su audio pasa por el PC (HFP). PipeWire representa el envío al
/// teléfono como un *stream* `bluez_output.<MAC>` que graba de una fuente
/// (el micrófono del PC): mientras suenan los tonos se le cambia a una
/// fuente temporal con ellos y luego se devuelve a la suya.
pub async fn dtmf_audio(address: &str, digits: &str) -> anyhow::Result<()> {
    let mac = address.replace(':', "_").to_uppercase();
    let out = pactl().args(["list", "source-outputs"]).output().await?;
    let listing = String::from_utf8_lossy(&out.stdout);
    let Some((stream, source)) = call_stream(&listing, &mac) else {
        anyhow::bail!(
            "kushare no puede entrar en la llamada del teléfono (Android 16 solo deja a la app \
             de teléfono) y la llamada no se oye en el PC: activa «Escuchar aquí» en la página \
             Teléfono para mandar los tonos por el audio de la llamada"
        );
    };
    const RATE: u32 = 16_000;
    let pcm = kushare_core::audio::dtmf_pcm(digits, RATE, 160, 90);
    let millis = pcm.len() as u64 * 1000 / RATE as u64;
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let tones = crate::link::mic::TempSource::open("kushare_dtmf", RATE)?;
        let run = |args: &[&str]| {
            std::process::Command::new("pactl")
                .args(args)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !run(&["move-source-output", &stream, "kushare_dtmf"]) {
            anyhow::bail!("no se pudo pasar los tonos a la llamada");
        }
        // Un poco de silencio delante: el cambio de fuente tarda unos ms.
        tones.write(&vec![0; RATE as usize / 10])?;
        tones.write(&pcm)?;
        std::thread::sleep(std::time::Duration::from_millis(millis + 250));
        run(&["move-source-output", &stream, &source]);
        Ok(())
    })
    .await??;
    Ok(())
}

/// El stream que manda audio a la llamada del teléfono `mac` (con «_»):
/// (índice del source-output, índice de su fuente). Recibe
/// `pactl list source-outputs` en inglés.
pub(super) fn call_stream(listing: &str, mac: &str) -> Option<(String, String)> {
    listing.split("Source Output #").skip(1).find_map(|block| {
        let index = block.lines().next()?.trim().to_string();
        let source = block
            .lines()
            .find_map(|l| l.trim().strip_prefix("Source: "))?
            .trim()
            .to_string();
        let is_call = block.lines().any(|l| {
            let l = l.trim();
            l.starts_with("node.name = \"bluez_output.") && l.to_uppercase().contains(mac)
        });
        is_call.then_some((index, source))
    })
}
