//! Notificaciones de escritorio nativas vía D-Bus, hablando directamente
//! con `org.freedesktop.Notifications` a través de un proxy `zbus`, tal
//! como pide CLAUDE.md explícitamente (en vez de una crate de más alto
//! nivel como `notify-rust`).

#![allow(clippy::too_many_arguments)] // firma fija de la interfaz D-Bus Notify

use futures_util::StreamExt;
use kushare_core::TransferEvent;
use std::collections::HashMap;
use std::path::PathBuf;
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

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;

    /// Extensión de KDE (capacidad `inline-reply`).
    #[zbus(signal)]
    fn notification_replied(&self, id: u32, text: String) -> zbus::Result<()>;
}

/// Clave de acción que kushare usa para "Responder" (ver
/// [`MirrorNotification::reply_action`]).
pub const INLINE_REPLY_ACTION: &str = "inline-reply";

/// Una notificación a mostrar con todas las opciones que usa kushare.
#[derive(Debug, Clone, Default)]
pub struct MirrorNotification {
    /// Id a reemplazar (0 = nueva).
    pub replaces_id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub icon_path: Option<PathBuf>,
    /// (clave, etiqueta) de cada botón.
    pub actions: Vec<(String, String)>,
    /// Si es `Some`, el servidor tiene `inline-reply` y se pide un campo de
    /// respuesta con ese texto de ejemplo (llega por `Replied`).
    pub inline_reply_placeholder: Option<String>,
    /// Sin caducidad (para el PIN o mensajes que no deben desaparecer).
    pub persistent: bool,
}

/// Señales del servidor de notificaciones sobre notificaciones de kushare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifierEvent {
    ActionInvoked { id: u32, action: String },
    Closed { id: u32, by_user: bool },
    Replied { id: u32, text: String },
}

pub struct Notifier<'a> {
    proxy: NotificationsProxy<'a>,
    connection: zbus::Connection,
}

impl Notifier<'static> {
    pub async fn connect() -> anyhow::Result<Self> {
        let connection = zbus::Connection::session().await?;
        let proxy = NotificationsProxy::new(&connection).await?;
        Ok(Self { proxy, connection })
    }

    /// Escucha las señales del servidor. Recibe las de todas las apps; el
    /// llamador filtra por los ids que creó.
    pub async fn events(&self) -> anyhow::Result<tokio::sync::mpsc::Receiver<NotifierEvent>> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let mut actions = self.proxy.receive_action_invoked().await?;
        let mut closed = self.proxy.receive_notification_closed().await?;
        let mut replied = self.proxy.receive_notification_replied().await?;
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    Some(sig) = actions.next() => match sig.args() {
                        Ok(a) => NotifierEvent::ActionInvoked { id: a.id, action: a.action_key },
                        Err(_) => continue,
                    },
                    Some(sig) = closed.next() => match sig.args() {
                        Ok(a) => NotifierEvent::Closed { id: a.id, by_user: a.reason == 2 },
                        Err(_) => continue,
                    },
                    Some(sig) = replied.next() => match sig.args() {
                        Ok(a) => NotifierEvent::Replied { id: a.id, text: a.text },
                        Err(_) => continue,
                    },
                    else => break,
                };
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }
}

impl<'a> Notifier<'a> {
    /// Nombre único de esta conexión en el bus, para que el monitor
    /// (`crate::monitor`) ignore las notificaciones del propio kushare.
    pub fn unique_name(&self) -> Option<String> {
        self.connection.unique_name().map(|n| n.to_string())
    }

    pub async fn capabilities(&self) -> Vec<String> {
        self.proxy.get_capabilities().await.unwrap_or_default()
    }

    pub async fn notify_full(&self, n: &MirrorNotification) -> anyhow::Result<u32> {
        let mut actions: Vec<&str> = Vec::new();
        for (key, label) in &n.actions {
            actions.push(key);
            actions.push(label);
        }
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        let icon_uri = n
            .icon_path
            .as_ref()
            .map(|p| format!("file://{}", p.display()));
        if let Some(uri) = &icon_uri {
            hints.insert("image-path", Value::from(uri.as_str()));
        }
        if let Some(placeholder) = &n.inline_reply_placeholder {
            actions.push(INLINE_REPLY_ACTION);
            actions.push("Responder");
            hints.insert(
                "x-kde-reply-placeholder-text",
                Value::from(placeholder.as_str()),
            );
        }
        if n.persistent {
            hints.insert("resident", Value::from(true));
        }
        let id = self
            .proxy
            .notify(
                &n.app_name,
                n.replaces_id,
                "kushare",
                &n.summary,
                &n.body,
                &actions,
                hints,
                if n.persistent { 0 } else { -1 },
            )
            .await?;
        Ok(id)
    }

    /// Cierra una notificación (propia o, como mejor esfuerzo, de otra app:
    /// la mayoría de servidores lo permiten con el id).
    pub async fn close(&self, id: u32) -> anyhow::Result<()> {
        Ok(self.proxy.close_notification(id).await?)
    }

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
