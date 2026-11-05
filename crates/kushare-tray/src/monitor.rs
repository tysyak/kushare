//! Captura de las notificaciones de escritorio de OTRAS apps, para
//! reenviarlas al teléfono.
//!
//! No hay una API estándar para "leer" notificaciones ajenas en Linux, así
//! que se observa el bus de sesión con `org.freedesktop.DBus.Monitoring.
//! BecomeMonitor` (el mismo mecanismo que `dbus-monitor`; no requiere
//! privilegios para el bus del propio usuario). Se miran tres cosas:
//!
//! - las llamadas `Notify` a `org.freedesktop.Notifications` (contenido);
//! - sus respuestas (`method_return`), que traen el id asignado por el
//!   servidor: se casan con la llamada por (emisor, serial);
//! - la señal `NotificationClosed` del servidor (para el descartar
//!   sincronizado).
//!
//! Funciona con cualquier servidor (KDE, GNOME, mako, dunst, swaync,
//! quickshell…). La conexión de monitor es de solo lectura: tras
//! `BecomeMonitor` el bus rechaza cualquier envío por ella, por eso es una
//! conexión aparte de la del `Notifier`.

use futures_util::StreamExt;
use kushare_link::NotificationData;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;
use zbus::MatchRule;
use zbus::message::Type;
use zbus::zvariant::{OwnedValue, Structure, Value};

use crate::icon;

/// Lo que el monitor observa, ya interpretado.
#[derive(Debug, Clone)]
pub enum Captured {
    /// Una app publicó (o actualizó) la notificación `key`.
    Posted(NotificationData),
    /// La notificación `key` se cerró. `reason` según la especificación:
    /// 1 caducó (en muchos servidores sigue en el historial), 2 la descartó
    /// el usuario, 3 la cerró la app, 4 indefinido.
    Closed { key: String, reason: u32 },
}

const NOTIFY_IFACE: &str = "org.freedesktop.Notifications";

type NotifyArgs = (
    String,
    u32,
    String,
    String,
    String,
    Vec<String>,
    HashMap<String, OwnedValue>,
    i32,
);

/// Notify aún sin respuesta: (emisor, serial) → datos, a falta del id.
type PendingNotify = HashMap<(String, NonZeroU32), NotificationData>;

/// Empieza a monitorizar. `ignore_senders`: nombres únicos D-Bus cuyas
/// notificaciones no se capturan (las del propio kushare: espejos del
/// teléfono, PIN…; si no, volverían al teléfono en bucle).
pub async fn start(
    ignore_senders: HashSet<String>,
) -> anyhow::Result<tokio::sync::mpsc::Receiver<Captured>> {
    let connection = zbus::Connection::session().await?;
    let rules = [
        MatchRule::builder()
            .msg_type(Type::MethodCall)
            .interface(NOTIFY_IFACE)?
            .member("Notify")?
            .build(),
        MatchRule::builder()
            .msg_type(Type::MethodReturn)
            .sender(NOTIFY_IFACE)?
            .build(),
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(NOTIFY_IFACE)?
            .member("NotificationClosed")?
            .build(),
    ];
    zbus::fdo::MonitoringProxy::new(&connection)
        .await?
        .become_monitor(&rules, 0)
        .await?;

    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let mut stream = zbus::MessageStream::from(&connection);
    tokio::spawn(async move {
        let _connection = connection;
        let mut pending: PendingNotify = HashMap::new();
        // Ids (del servidor) de notificaciones capturadas y aún abiertas.
        let mut tracked: HashSet<u32> = HashSet::new();

        while let Some(msg) = stream.next().await {
            let Ok(msg) = msg else { continue };
            let header = msg.header();
            match header.message_type() {
                Type::MethodCall => {
                    let Some(sender) = header.sender().map(|s| s.to_string()) else {
                        continue;
                    };
                    if ignore_senders.contains(&sender) {
                        continue;
                    }
                    let Ok(args) = msg.body().deserialize::<NotifyArgs>() else {
                        continue;
                    };
                    let serial = header.primary().serial_num();
                    if pending.len() > 256 {
                        pending.clear(); // respuestas perdidas; no crecer sin límite
                    }
                    pending.insert((sender, serial), notification_from_args(args));
                }
                Type::MethodReturn => {
                    let (Some(dest), Some(reply_serial)) =
                        (header.destination(), header.reply_serial())
                    else {
                        continue;
                    };
                    let Some(mut data) = pending.remove(&(dest.to_string(), reply_serial)) else {
                        continue;
                    };
                    let Ok(id) = msg.body().deserialize::<u32>() else {
                        continue;
                    };
                    data.key = id.to_string();
                    tracked.insert(id);
                    if tx.send(Captured::Posted(data)).await.is_err() {
                        break;
                    }
                }
                Type::Signal => {
                    let Ok((id, reason)) = msg.body().deserialize::<(u32, u32)>() else {
                        continue;
                    };
                    if !tracked.remove(&id) {
                        continue; // no la capturamos (p. ej. un espejo propio)
                    }
                    let event = Captured::Closed {
                        key: id.to_string(),
                        reason,
                    };
                    if tx.send(event).await.is_err() {
                        break;
                    }
                }
                Type::Error => {}
            }
        }
        tracing::warn!("el monitor de notificaciones D-Bus terminó");
    });
    Ok(rx)
}

fn notification_from_args(args: NotifyArgs) -> NotificationData {
    let (app_name, _replaces_id, app_icon, summary, body, _actions, hints, _timeout) = args;
    let desktop_entry = hint_str(&hints, "desktop-entry");
    let icon_png = icon_from_hints(&hints)
        .or_else(|| hint_str(&hints, "image-path").and_then(|p| icon::png_from_icon_name(&p)))
        .or_else(|| icon::png_from_icon_name(&app_icon))
        .or_else(|| desktop_entry.as_deref().and_then(icon::png_from_icon_name));
    let app_id = desktop_entry.unwrap_or_else(|| app_name.clone());
    NotificationData {
        key: String::new(), // se completa con el id de la respuesta
        app_name: if app_name.is_empty() {
            app_id.clone()
        } else {
            app_name
        },
        app_id,
        title: summary,
        text: strip_markup(&body),
        timestamp_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default(),
        icon_png,
        // Las acciones de una notificación ajena no se pueden disparar desde
        // otro proceso (el servidor emite ActionInvoked solo hacia su
        // dueña), así que no se ofrecen en el teléfono.
        actions: Vec::new(),
    }
}

fn hint_str(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match hints.get(key).map(|v| &**v) {
        Some(Value::Str(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

fn icon_from_hints(hints: &HashMap<String, OwnedValue>) -> Option<Vec<u8>> {
    for key in ["image-data", "image_data", "icon_data"] {
        let Some(Value::Structure(s)) = hints.get(key).map(|v| &**v) else {
            continue;
        };
        if let Some(png) = png_from_image_data(s) {
            return Some(png);
        }
    }
    None
}

fn png_from_image_data(s: &Structure<'_>) -> Option<Vec<u8>> {
    let f = s.fields();
    if f.len() != 7 {
        return None;
    }
    let int = |v: &Value<'_>| match v {
        Value::I32(n) => Some(*n),
        _ => None,
    };
    let Value::Bool(has_alpha) = f[3] else {
        return None;
    };
    let data: Vec<u8> = match &f[6] {
        Value::Array(a) => a
            .iter()
            .filter_map(|v| if let Value::U8(b) = v { Some(*b) } else { None })
            .collect(),
        _ => return None,
    };
    let img = icon::rgba_from_image_data(
        int(&f[0])?,
        int(&f[1])?,
        int(&f[2])?,
        has_alpha,
        int(&f[4])?,
        int(&f[5])?,
        &data,
    )?;
    icon::encode_png(&icon::downscale(img, icon::MAX_ICON_SIZE))
}

/// El cuerpo puede traer el subconjunto de HTML de la especificación
/// (`<b>`, `<i>`, `<a href>`, entidades). Se deja en texto plano.
pub fn strip_markup(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_stripped() {
        assert_eq!(
            strip_markup("<b>Hola</b> &amp; <a href=\"x\">adiós</a> &lt;3"),
            "Hola & adiós <3"
        );
    }
}
