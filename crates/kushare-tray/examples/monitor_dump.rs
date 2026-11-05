//! Vuelca las notificaciones de escritorio que captura `kushare_tray::monitor`.
//! Uso: `cargo run -p kushare-tray --example monitor_dump` y en otra
//! terminal `notify-send -i firefox "Hola" "<b>desde</b> el PC"`.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut rx = kushare_tray::monitor::start(Default::default()).await?;
    eprintln!("monitorizando…");
    while let Some(c) = rx.recv().await {
        match c {
            kushare_tray::monitor::Captured::Posted(mut n) => {
                let icon = n.icon_png.take().map(|p| p.len());
                println!("POSTED {n:?} icon_png_bytes={icon:?}");
            }
            other => println!("{other:?}"),
        }
    }
    Ok(())
}
