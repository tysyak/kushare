//! Bloqueo dinámico (como el de Windows): si el teléfono se aleja, el PC
//! bloquea la sesión. "Alejarse" = se pierde el enlace de control por
//! Bluetooth y no vuelve en un rato (BlueZ no da el RSSI de una conexión
//! clásica; la propia pila corta el enlace a ~10 m tras ~20 s).
//!
//! Desactivado por defecto (`dynamic-lock`). Antes de bloquear avisa con
//! una notificación con *Cancelar*; si el teléfono vuelve, no bloquea.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

/// Tras perder el Bluetooth, esperar esto antes de avisar.
const GRACE: Duration = Duration::from_secs(20);
/// Y esto desde el aviso hasta bloquear.
const WARNING: Duration = Duration::from_secs(10);

/// Lo que la tarea de bloqueo pide a la interfaz (notificaciones).
pub enum ProximityEvent {
    /// Mostrar "se bloqueará en 10 s" con *Cancelar* (marca `cancel`).
    Warn {
        peer: String,
        cancel: Arc<AtomicBool>,
    },
    /// Ya se bloqueó: cerrar el aviso.
    Done { peer: String },
}

#[derive(Default)]
pub struct Proximity {
    /// Teléfonos conectados ahora por Bluetooth.
    bt_peers: HashMap<String, ()>,
    /// Bloqueos en cuenta atrás, por peer.
    pending: HashMap<String, Arc<AtomicBool>>,
}

impl Proximity {
    /// Volvió el enlace: solo un enlace por Bluetooth demuestra que el
    /// teléfono está cerca (por Wi-Fi puede estar en otra habitación).
    pub fn on_connected(&mut self, peer: &str, transport: &str) {
        if transport.starts_with("bluetooth") {
            if let Some(cancel) = self.pending.remove(peer) {
                cancel.store(true, Ordering::Relaxed);
            }
            self.bt_peers.insert(peer.to_string(), ());
        } else {
            self.bt_peers.remove(peer);
        }
    }

    pub fn on_unpaired(&mut self, peer: &str) {
        self.bt_peers.remove(peer);
        if let Some(cancel) = self.pending.remove(peer) {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Se cayó el enlace de `peer`: si iba por Bluetooth y el ajuste está
    /// activado, empieza la cuenta atrás.
    pub fn on_disconnected(
        &mut self,
        peer: &str,
        enabled: bool,
        ui: mpsc::UnboundedSender<ProximityEvent>,
    ) {
        if self.bt_peers.remove(peer).is_none() || !enabled {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(old) = self.pending.insert(peer.to_string(), cancel.clone()) {
            old.store(true, Ordering::Relaxed);
        }
        let peer = peer.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(GRACE).await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let _ = ui.send(ProximityEvent::Warn {
                peer: peer.clone(),
                cancel: cancel.clone(),
            });
            tokio::time::sleep(WARNING).await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            tracing::info!(%peer, "el teléfono se alejó: se bloquea la sesión");
            if let Err(err) = lock_session().await {
                tracing::warn!(?err, "no se pudo bloquear la sesión");
            }
            let _ = ui.send(ProximityEvent::Done { peer });
        });
    }
}

/// Bloquea la sesión: logind (`loginctl lock-session`, lo atienden
/// hypridle, GNOME, KDE…) y, si falla, el salvapantallas por D-Bus.
pub async fn lock_session() -> anyhow::Result<()> {
    let logind = tokio::process::Command::new("loginctl")
        .arg("lock-session")
        .status()
        .await;
    if logind.is_ok_and(|s| s.success()) {
        return Ok(());
    }
    let conn = zbus::Connection::session().await?;
    conn.call_method(
        Some("org.freedesktop.ScreenSaver"),
        "/org/freedesktop/ScreenSaver",
        Some("org.freedesktop.ScreenSaver"),
        "Lock",
        &(),
    )
    .await?;
    Ok(())
}
