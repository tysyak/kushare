//! TUI de kushare: el "Enlace Móvil" en la terminal, como la GUI. No hace
//! red por su cuenta: controla al daemon por D-Bus (`org.kushare.Daemon`).
//!
//! Pestañas (Inicio, Teléfono, Mensajes, Llamadas, Notificaciones,
//! Portapapeles, Dispositivos, Enviar) y, si la terminal es ancha, el panel
//! del teléfono a la izquierda: estado, lo que suena y sus notificaciones.

mod app;
mod ui;

use app::{App, Ev};
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(name = "kushare-tui", version, about)]
struct Cli {
    /// Archivo a mandar al teléfono: abre la pestaña «Enviar» con él.
    #[arg(long)]
    send: Option<PathBuf>,

    /// Pestaña inicial: home, phone, messages, calls, notifications,
    /// clipboard, devices o send.
    #[arg(long)]
    screen: Option<String>,
}

/// Los logs van a un archivo: escribir en la terminal rompería el dibujo.
fn log_file_path() -> PathBuf {
    std::env::temp_dir().join("kushare-tui.log")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Ok(file) = std::fs::File::create(log_file_path()) {
        tracing_subscriber::fmt()
            .with_writer(file)
            .with_ansi(false)
            .init();
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Ev>();
    tokio::spawn(app::daemon_events(tx.clone()));
    let mut app = App::new(tx, cli.send, cli.screen.as_deref());

    let mut terminal = ratatui::init();
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let result = loop {
        if let Err(err) = terminal.draw(|frame| ui::draw(frame, &app)) {
            break Err(err.into());
        }
        tokio::select! {
            Some(Ok(event)) = keys.next() => {
                if let Event::Key(key) = event
                    && key.kind == KeyEventKind::Press
                {
                    app.on_key(key);
                }
            }
            Some(event) = rx.recv() => app.on_event(event),
            _ = tick.tick() => app.on_tick(),
        }
        if app.quit {
            break Ok(());
        }
    };
    ratatui::restore();
    result
}
