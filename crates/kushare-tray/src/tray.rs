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
use muda::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tokio::sync::mpsc;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const MENU_ID_SEND_FILE: &str = "send_file";
const MENU_ID_TOGGLE_VISIBLE: &str = "toggle_visible";
const MENU_ID_QUIT: &str = "quit";

const MENU_LINK_IN: &str = "link-in:";
const MENU_LINK_OUT: &str = "link-out:";
const MENU_LINK_UNPAIR: &str = "link-unpair:";
const MENU_LINK_PAIR: &str = "link-pair:";
const MENU_LINK_SCAN: &str = "link-scan";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCommand {
    SendFile,
    ToggleVisible,
    Quit,
    /// Alternar "recibir notificaciones de" este dispositivo.
    ToggleIncoming(String),
    /// Alternar "enviar mis notificaciones a" este dispositivo.
    ToggleOutgoing(String),
    Unpair(String),
    Pair(String),
    /// Buscar dispositivos en la red (bajo demanda).
    Scan,
}

/// Un dispositivo emparejado, tal como se muestra en el menú.
#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub device_id: String,
    pub name: String,
    pub connected: bool,
    pub incoming: bool,
    pub outgoing: bool,
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
        let menu = build_menu(&[], &[], false)?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .with_tooltip("kushare")
            .build()?;

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let Some(command) = command_for(event.id()) else {
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

    /// Reconstruye el menú con la lista actual de dispositivos emparejados
    /// y de los visibles sin emparejar, (id, nombre).
    pub fn set_devices(
        &self,
        paired: &[DeviceEntry],
        discovered: &[(String, String)],
        scanning: bool,
    ) -> anyhow::Result<()> {
        self._icon
            .set_menu(Some(Box::new(build_menu(paired, discovered, scanning)?)));
        Ok(())
    }
}

fn build_menu(
    paired: &[DeviceEntry],
    discovered: &[(String, String)],
    scanning: bool,
) -> anyhow::Result<Menu> {
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

    let devices = Submenu::new("Dispositivos", true);
    let scan_label = if scanning {
        "Buscando…"
    } else {
        "Buscar dispositivos en la red"
    };
    devices.append(&MenuItem::with_id(
        MENU_LINK_SCAN,
        scan_label,
        !scanning,
        None,
    ))?;
    devices.append(&PredefinedMenuItem::separator())?;
    if paired.is_empty() && discovered.is_empty() {
        devices.append(&MenuItem::new("(ninguno)", false, None))?;
    }
    for d in paired {
        let state = if d.connected {
            "conectado"
        } else {
            "desconectado"
        };
        let sub = Submenu::new(format!("{} ({state})", d.name), true);
        sub.append(&CheckMenuItem::with_id(
            format!("{MENU_LINK_IN}{}", d.device_id),
            "Recibir sus notificaciones",
            true,
            d.incoming,
            None,
        ))?;
        sub.append(&CheckMenuItem::with_id(
            format!("{MENU_LINK_OUT}{}", d.device_id),
            "Enviarle mis notificaciones",
            true,
            d.outgoing,
            None,
        ))?;
        sub.append(&PredefinedMenuItem::separator())?;
        sub.append(&MenuItem::with_id(
            format!("{MENU_LINK_UNPAIR}{}", d.device_id),
            "Desemparejar",
            true,
            None,
        ))?;
        devices.append(&sub)?;
    }
    if !discovered.is_empty() {
        if !paired.is_empty() {
            devices.append(&PredefinedMenuItem::separator())?;
        }
        for (id, name) in discovered {
            devices.append(&MenuItem::with_id(
                format!("{MENU_LINK_PAIR}{id}"),
                format!("Emparejar con {name}…"),
                true,
                None,
            ))?;
        }
    }
    menu.append(&devices)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(MENU_ID_QUIT, "Salir", true, None))?;
    Ok(menu)
}

fn command_for(id: &MenuId) -> Option<TrayCommand> {
    let id = id.0.as_str();
    let arg = |prefix: &str| id.strip_prefix(prefix).map(str::to_string);
    match id {
        MENU_ID_SEND_FILE => Some(TrayCommand::SendFile),
        MENU_ID_TOGGLE_VISIBLE => Some(TrayCommand::ToggleVisible),
        MENU_ID_QUIT => Some(TrayCommand::Quit),
        MENU_LINK_SCAN => Some(TrayCommand::Scan),
        _ => arg(MENU_LINK_IN)
            .map(TrayCommand::ToggleIncoming)
            .or_else(|| arg(MENU_LINK_OUT).map(TrayCommand::ToggleOutgoing))
            .or_else(|| arg(MENU_LINK_UNPAIR).map(TrayCommand::Unpair))
            .or_else(|| arg(MENU_LINK_PAIR).map(TrayCommand::Pair)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_roundtrip_to_commands() {
        let id = |s: &str| MenuId(s.to_string());
        assert_eq!(command_for(&id("quit")), Some(TrayCommand::Quit));
        assert_eq!(
            command_for(&id("link-in:abc")),
            Some(TrayCommand::ToggleIncoming("abc".into()))
        );
        assert_eq!(
            command_for(&id("link-pair:abc")),
            Some(TrayCommand::Pair("abc".into()))
        );
        assert_eq!(command_for(&id("otra-cosa")), None);
    }
}
