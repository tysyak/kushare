//! Notificaciones de escritorio nativas vía D-Bus, hablando directamente
//! con `org.freedesktop.Notifications` a través de un proxy `zbus`, tal
//! como pide CLAUDE.md explícitamente (en vez de una crate de más alto
//! nivel como `notify-rust`).

#![allow(clippy::too_many_arguments)] // firma fija de la interfaz D-Bus Notify

use kushare_core::TransferEvent;
use std::collections::HashMap;
use zbus::zvariant::Value;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[zbus(name = "Notify")]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

pub struct Notifier<'a> {
    proxy: NotificationsProxy<'a>,
}

impl Notifier<'static> {
    pub async fn connect() -> anyhow::Result<Self> {
        let connection = zbus::Connection::session().await?;
        let proxy = NotificationsProxy::new(&connection).await?;
        Ok(Self { proxy })
    }
}

impl<'a> Notifier<'a> {
    /// Envía una notificación genérica; devuelve el id asignado por el
    /// servidor de notificaciones (útil para reemplazarla más tarde con
    /// `replaces_id`, p. ej. para actualizar el progreso de una
    /// transferencia en la misma notificación).
    pub async fn notify(&self, summary: &str, body: &str) -> anyhow::Result<u32> {
        let id = self
            .proxy
            .notify(
                "kushare",
                0,
                "kushare",
                summary,
                body,
                &[],
                HashMap::new(),
                5000,
            )
            .await?;
        Ok(id)
    }

    /// Traduce un [`TransferEvent`] a una notificación legible.
    pub async fn notify_transfer_event(&self, event: &TransferEvent) -> anyhow::Result<()> {
        match event {
            TransferEvent::IncomingRequest {
                device_name,
                file_name,
                ..
            } => {
                self.notify(
                    "kushare",
                    &format!("Recibiendo \"{file_name}\" de {device_name}"),
                )
                .await?;
            }
            TransferEvent::Completed {
                device_name,
                file_name,
                ..
            } => {
                self.notify(
                    "kushare",
                    &format!("Archivo recibido de {device_name}: {file_name}"),
                )
                .await?;
            }
            TransferEvent::Failed { file_name, reason } => {
                self.notify(
                    "kushare",
                    &format!("Error al transferir \"{file_name}\": {reason}"),
                )
                .await?;
            }
            TransferEvent::Progress { .. } => {
                // El progreso se refleja en el tooltip del tray (ver
                // `crate::tray::tooltip_for_event`), no en una notificación
                // nueva por cada chunk.
            }
        }
        Ok(())
    }
}
