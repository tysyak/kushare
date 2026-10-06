//! Icono de bandeja del sistema.
//!
//! Usa el backend `ksni` de `tray-icon` (StatusNotifierItem sobre D-Bus,
//! implementado en Rust puro sobre `zbus`) en vez del backend por defecto
//! basado en GTK/`libappindicator`. Esto evita una dependencia de
//! compilación en C adicional (coherente con la regla de CLAUDE.md de
//! "evitar en lo posible dependencias que requieran compilación en C") y,
//! como `ksni` corre su propio worker de D-Bus sobre `tokio` internamente,
//! no hace falta un hilo aparte con un loop de eventos GTK bloqueante: el
//! icono puede crearse y vivir directamente en el runtime tokio principal.
//! Los eventos de menú llegan por un callback (no async) que aquí se
//! reenvía a un `tokio::sync::mpsc::Sender<TrayCommand>` con `try_send`.

use kushare_core::TransferEvent;
use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tokio::sync::mpsc;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const MENU_ID_SEND_FILE: &str = "send_file";
const MENU_ID_TOGGLE_VISIBLE: &str = "toggle_visible";
const MENU_ID_QUIT: &str = "quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    SendFile,
    ToggleVisible,
    Quit,
}

/// Icono de bandeja ya registrado; se mantiene vivo mientras este valor no
/// se destruya (al hacerlo, `tray-icon` retira el icono).
pub struct Tray {
    _icon: TrayIcon,
}

impl Tray {
    /// Crea el icono de bandeja con su menú contextual ("Enviar archivo" /
    /// "Hacer visible" / "Salir", tal como pide CLAUDE.md) y empieza a
    /// reenviar los clics del menú por `cmd_tx`.
    pub fn new(icon: Icon, cmd_tx: mpsc::Sender<TrayCommand>) -> anyhow::Result<Self> {
        let menu = Menu::new();
        menu.append(&MenuItem::with_id(
            MENU_ID_SEND_FILE,
            "Enviar archivo",
            true,
            None,
        ))?;
        menu.append(&MenuItem::with_id(
            MENU_ID_TOGGLE_VISIBLE,
            "Hacer visible",
            true,
            None,
        ))?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&MenuItem::with_id(MENU_ID_QUIT, "Salir", true, None))?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip("kushare")
            .build()?;

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let Some(command) = command_for(&event.id) else {
                return;
            };
            // `try_send`: el canal de comandos del tray es pequeño y no
            // debería llenarse en uso normal; si lo hace, se descarta el
            // clic en vez de bloquear este callback síncrono de D-Bus.
            let _ = cmd_tx.try_send(command);
        }));

        Ok(Self { _icon: tray_icon })
    }

    /// Refleja un [`TransferEvent`] en el tooltip del icono (progreso de
    /// transferencia, etc.). La notificación de escritorio propiamente
    /// dicha la maneja [`crate::notify::Notifier`].
    pub fn set_tooltip(&self, text: &str) -> anyhow::Result<()> {
        Ok(self._icon.set_tooltip(Some(text))?)
    }
}

fn command_for(id: &MenuId) -> Option<TrayCommand> {
    match id.0.as_str() {
        MENU_ID_SEND_FILE => Some(TrayCommand::SendFile),
        MENU_ID_TOGGLE_VISIBLE => Some(TrayCommand::ToggleVisible),
        MENU_ID_QUIT => Some(TrayCommand::Quit),
        _ => None,
    }
}

/// Texto de tooltip sugerido para un [`TransferEvent`], usado por el
/// binario principal para mantener el icono informativo sin necesitar su
/// propia lógica de formato.
pub fn tooltip_for_event(event: &TransferEvent) -> String {
    match event {
        TransferEvent::IncomingRequest {
            device_name,
            file_name,
            ..
        } => format!("kushare — recibiendo \"{file_name}\" de {device_name}"),
        TransferEvent::Progress {
            file_name,
            bytes_transferred,
            total_bytes,
        } => {
            let pct = if *total_bytes > 0 {
                (*bytes_transferred as f64 / *total_bytes as f64) * 100.0
            } else {
                0.0
            };
            format!("kushare — {file_name}: {pct:.0}%")
        }
        TransferEvent::Completed { file_name, .. } => {
            format!("kushare — \"{file_name}\" recibido")
        }
        TransferEvent::Failed { file_name, reason } => {
            format!("kushare — error con \"{file_name}\": {reason}")
        }
    }
}
