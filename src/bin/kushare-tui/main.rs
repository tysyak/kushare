mod app;
mod ui;

use app::{App, AppEvent};
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_util::StreamExt;
use kushare_core::Settings;
use std::path::PathBuf;
use tokio::sync::mpsc;

/// TUI de kushare, similar a la app Quick Share de Android: dos pantallas,
/// "Enviar" y "Recibir".
#[derive(Parser, Debug)]
#[command(name = "kushare-tui", version, about)]
struct Cli {
    /// Archivo a enviar: entra directo a la pantalla "Enviar" con este
    /// archivo ya elegido (kushare-tui --send="archivo.txt").
    #[arg(long)]
    send: Option<PathBuf>,

    /// Entra directo a la pantalla "Recibir".
    #[arg(long)]
    receive: bool,

    /// Nombre de este dispositivo. Por defecto se deriva del hostname.
    #[arg(long)]
    device_name: Option<String>,

    /// Puerto TCP en el que escuchar transferencias entrantes.
    #[arg(long, default_value_t = 0)]
    port: u16,
}

/// Ruta del log de esta sesión. Escribir a stdout/stderr corrompería el
/// dibujado de la TUI (que usa la pantalla alterna del terminal), así que
/// los logs de `tracing` (incluyendo los de `kushare-transfer`/
/// `kushare-discovery`) van a este archivo en su lugar.
fn log_file_path() -> std::path::PathBuf {
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

    let mut settings = Settings::default();
    if let Some(name) = cli.device_name {
        settings.device_name = name;
    }
    settings.listen_port = cli.port;

    let mut terminal = ratatui::init();
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut app = App::new(settings, app_tx);

    if cli.receive {
        app.enter_receive().await;
    } else if let Some(path) = cli.send {
        app.enter_send(Some(path)).await;
    }

    let mut events = EventStream::new();
    let result = run_event_loop(&mut terminal, &mut app, &mut events, &mut app_rx).await;

    ratatui::restore();
    result
}

async fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    events: &mut EventStream,
    app_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        if !app.handle_key(key, terminal).await {
                            return Ok(());
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(err.into()),
                    None => return Ok(()),
                }
            }
            Some(event) = app_rx.recv() => {
                app.handle_app_event(event);
            }
        }
    }
}
