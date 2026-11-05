//! Tests de extremo a extremo del enlace entre dos `LinkManager` reales,
//! conectados por streams en memoria a través de un proxy que permite
//! cortar la conexión (EOF) o congelarla (sin EOF, como un peer colgado).

use kushare_link::{
    ConnectIntent, Direction, LinkConfig, LinkEvent, LinkManager, LocalDevice, NotificationAction,
    NotificationData, Transport,
};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc::UnboundedReceiver;

const WAIT: Duration = Duration::from_secs(5);

fn fast_config() -> LinkConfig {
    LinkConfig {
        handshake_timeout: Duration::from_secs(3),
        pairing_timeout: Duration::from_secs(3),
        ping_interval: Duration::from_millis(100),
        idle_timeout: Duration::from_millis(500),
        bt_ping_interval: Duration::from_millis(100),
        bt_idle_timeout: Duration::from_millis(500),
        write_timeout: Duration::from_secs(2),
        reconnect_min: Duration::from_millis(50),
        reconnect_max: Duration::from_millis(200),
    }
}

struct Node {
    mgr: LinkManager,
    events: UnboundedReceiver<LinkEvent>,
}

fn node(dir: &Path, name: &str) -> Node {
    let (mgr, events) = LinkManager::open(
        &dir.join(name).join("config"),
        &dir.join(name).join("cache"),
        LocalDevice {
            name: name.to_string(),
            kind: "test".into(),
            bt_address: None,
            capabilities: vec!["notifications".into()],
        },
        fast_config(),
    )
    .unwrap();
    Node { mgr, events }
}

impl Node {
    /// Siguiente evento que cumpla `pred` (descarta los demás).
    async fn wait_for(&mut self, what: &str, pred: impl Fn(&LinkEvent) -> bool) -> LinkEvent {
        tokio::time::timeout(WAIT, async {
            loop {
                let event = self.events.recv().await.expect("canal de eventos cerrado");
                if pred(&event) {
                    return event;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no llegó el evento esperado: {what}"))
    }

    async fn next_notification(&mut self) -> kushare_link::RemoteNotification {
        match self
            .wait_for("NotificationPosted", |e| {
                matches!(e, LinkEvent::NotificationPosted { .. })
            })
            .await
        {
            LinkEvent::NotificationPosted { notification, .. } => notification,
            _ => unreachable!(),
        }
    }
}

/// Proxy entre los dos extremos.
#[derive(Clone)]
struct Proxy {
    frozen: Arc<AtomicBool>,
    tasks: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl Proxy {
    fn new() -> (Self, DuplexStream, DuplexStream) {
        let (a, proxy_a) = tokio::io::duplex(1 << 20);
        let (proxy_b, b) = tokio::io::duplex(1 << 20);
        let proxy = Self {
            frozen: Arc::default(),
            tasks: Arc::default(),
        };
        let (ra, wa) = tokio::io::split(proxy_a);
        let (rb, wb) = tokio::io::split(proxy_b);
        let t1 = tokio::spawn(pump(ra, wb, proxy.frozen.clone()));
        let t2 = tokio::spawn(pump(rb, wa, proxy.frozen.clone()));
        proxy
            .tasks
            .lock()
            .unwrap()
            .extend([t1.abort_handle(), t2.abort_handle()]);
        (proxy, a, b)
    }

    /// Deja de reenviar datos sin cerrar nada.
    fn freeze(&self) {
        self.frozen.store(true, Ordering::SeqCst);
    }

    /// Cierra ambos lados (los extremos ven EOF).
    fn cut(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

async fn pump(
    mut r: tokio::io::ReadHalf<DuplexStream>,
    mut w: tokio::io::WriteHalf<DuplexStream>,
    frozen: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let Ok(n) = r.read(&mut buf).await else {
            return;
        };
        if n == 0 {
            return;
        }
        while frozen.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if w.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

/// Espera a que `cond` se cumpla (p. ej. que un cambio de configuración
/// hecho en el otro lado haya llegado). No sirve esperar a `ConfigChanged`:
/// también se emite por los cambios locales.
async fn eventually(what: &str, cond: impl Fn() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no se cumplió: {what}"));
}

fn test_transport() -> Transport {
    Transport::Other("test".into())
}

/// `a` conecta a `b` (b acepta) con `intent`, sin intervenir en el PIN.
async fn link(
    a: &Node,
    b: &Node,
    intent: ConnectIntent,
) -> (Proxy, anyhow::Result<String>, anyhow::Result<String>) {
    let (proxy, sa, sb) = Proxy::new();
    let (ra, rb) = tokio::join!(
        a.mgr.connect(sa, test_transport(), intent),
        b.mgr.accept(sb, test_transport()),
    );
    (proxy, ra, rb)
}

/// Empareja `a` con `b` respondiendo `accept_a`/`accept_b` al PIN.
/// Devuelve los PIN que vio cada lado y los resultados de conexión.
async fn pair(
    a: &mut Node,
    b: &mut Node,
    accept_a: bool,
    accept_b: bool,
) -> (
    Proxy,
    String,
    String,
    anyhow::Result<String>,
    anyhow::Result<String>,
) {
    let (proxy, sa, sb) = Proxy::new();
    let (mgr_a, mgr_b) = (a.mgr.clone(), b.mgr.clone());
    let task_a = tokio::spawn(async move {
        mgr_a
            .connect(sa, test_transport(), ConnectIntent::Pair)
            .await
    });
    let task_b = tokio::spawn(async move { mgr_b.accept(sb, test_transport()).await });

    let mut pins = Vec::new();
    for (n, accept) in [(&mut *a, accept_a), (&mut *b, accept_b)] {
        let LinkEvent::PairingRequested { device_id, pin, .. } = n
            .wait_for("PairingRequested", |e| {
                matches!(e, LinkEvent::PairingRequested { .. })
            })
            .await
        else {
            unreachable!()
        };
        assert!(n.mgr.respond_pairing(&device_id, accept));
        pins.push(pin);
    }
    let (ra, rb) = (task_a.await.unwrap(), task_b.await.unwrap());
    (proxy, pins.remove(0), pins.remove(0), ra, rb)
}

async fn paired(dir: &Path) -> (Node, Node, Proxy) {
    let mut a = node(dir, "a");
    let mut b = node(dir, "b");
    let (proxy, _, _, ra, rb) = pair(&mut a, &mut b, true, true).await;
    ra.unwrap();
    rb.unwrap();
    for n in [&mut a, &mut b] {
        n.wait_for("PeerConnected", |e| {
            matches!(e, LinkEvent::PeerConnected { .. })
        })
        .await;
    }
    (a, b, proxy)
}

fn notification(key: &str, app: &str) -> NotificationData {
    NotificationData {
        key: key.into(),
        app_id: app.into(),
        app_name: app.to_uppercase(),
        title: format!("título {key}"),
        text: format!("texto {key}"),
        timestamp_ms: 1_700_000_000_000,
        icon_png: Some(format!("png de {app}").into_bytes()),
        actions: vec![
            NotificationAction {
                id: "read".into(),
                label: "Marcar como leído".into(),
                is_reply: false,
            },
            NotificationAction {
                id: "reply".into(),
                label: "Responder".into(),
                is_reply: true,
            },
        ],
    }
}

#[tokio::test]
async fn pairing_with_matching_pin_links_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = node(dir.path(), "a");
    let mut b = node(dir.path(), "b");

    let (_proxy, pin_a, pin_b, ra, rb) = pair(&mut a, &mut b, true, true).await;
    assert_eq!(pin_a, pin_b, "ambos lados deben mostrar el mismo PIN");
    assert_eq!(ra.unwrap(), b.mgr.device_id());
    assert_eq!(rb.unwrap(), a.mgr.device_id());

    let peers_a = a.mgr.peers();
    assert_eq!(peers_a.len(), 1);
    assert_eq!(peers_a[0].device_name, "b");
    assert!(peers_a[0].connected);
    assert!(peers_a[0].outgoing_enabled && peers_a[0].incoming_enabled);
    assert!(b.mgr.is_connected(a.mgr.device_id()));
}

#[tokio::test]
async fn pairing_rejected_on_one_side_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = node(dir.path(), "a");
    let mut b = node(dir.path(), "b");

    let (_proxy, _, _, ra, rb) = pair(&mut a, &mut b, true, false).await;
    assert!(ra.is_err() && rb.is_err());
    for n in [&mut a, &mut b] {
        let finished = n
            .wait_for("PairingFinished", |e| {
                matches!(e, LinkEvent::PairingFinished { .. })
            })
            .await;
        assert!(matches!(
            finished,
            LinkEvent::PairingFinished {
                accepted: false,
                ..
            }
        ));
        assert!(n.mgr.peers().is_empty());
    }
}

#[tokio::test]
async fn reconnect_after_restart_needs_no_pin() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, proxy) = paired(dir.path()).await;
    let (id_a, id_b) = (a.mgr.device_id().to_string(), b.mgr.device_id().to_string());
    proxy.cut();
    drop((a, b));

    // "Reinicio": gestores nuevos leyendo la misma identidad y peers.toml.
    let mut a = node(dir.path(), "a");
    let mut b = node(dir.path(), "b");
    assert_eq!(a.mgr.device_id(), id_a);
    let (_proxy, ra, rb) = link(&a, &b, ConnectIntent::Reconnect).await;
    assert_eq!(ra.unwrap(), id_b);
    assert_eq!(rb.unwrap(), id_a);

    let first = a
        .wait_for("PeerConnected", |e| {
            matches!(
                e,
                LinkEvent::PeerConnected { .. } | LinkEvent::PairingRequested { .. }
            )
        })
        .await;
    assert!(
        matches!(first, LinkEvent::PeerConnected { .. }),
        "no debe pedir PIN: {first:?}"
    );
    b.wait_for("PeerConnected", |e| {
        matches!(e, LinkEvent::PeerConnected { .. })
    })
    .await;
}

#[tokio::test]
async fn unknown_device_cannot_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let (_a, b, _proxy) = paired(dir.path()).await;
    let stranger = node(dir.path(), "intruso");

    let (_proxy, r_stranger, r_b) = link(&stranger, &b, ConnectIntent::Reconnect).await;
    assert!(r_stranger.is_err());
    assert!(r_b.is_err());
    assert_eq!(
        b.mgr.peers().len(),
        1,
        "B no debe aprender nada del intruso"
    );
}

#[tokio::test]
async fn notifications_respect_direction_and_app_filter_set_from_either_side() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _proxy) = paired(dir.path()).await;
    let id_a = a.mgr.device_id().to_string();
    let id_b = b.mgr.device_id().to_string();

    // Llega con su icono, guardado en caché.
    a.mgr.post_local_notification(notification("1", "chat"));
    let got = b.next_notification().await;
    assert_eq!(got.key, "1");
    assert_eq!(got.title, "título 1");
    assert_eq!(got.actions.len(), 2);
    let icon = got.icon_path.expect("icono en caché");
    assert_eq!(std::fs::read(icon).unwrap(), b"png de chat");

    // El icono no se reenvía, pero el receptor lo sigue resolviendo.
    a.mgr.post_local_notification(notification("2", "chat"));
    assert!(b.next_notification().await.icon_path.is_some());

    // B (receptor) bloquea la app "chat"; A (emisor) se entera.
    b.mgr
        .set_app_blocked(&id_a, Direction::Incoming, "chat", true)
        .unwrap();
    eventually("A ve el bloqueo hecho en B", || {
        a.mgr.peers()[0].outgoing_blocked == ["chat"]
    })
    .await;
    a.mgr.post_local_notification(notification("3", "chat"));
    a.mgr.post_local_notification(notification("4", "mail"));
    assert_eq!(
        b.next_notification().await.key,
        "4",
        "la de 'chat' debe filtrarse"
    );
    // B ya conoce ambas apps de A para su UI de filtro.
    let apps: Vec<String> = b.mgr.peers()[0]
        .peer_apps
        .iter()
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(apps, ["chat", "mail"]);

    // A (emisor) desactiva desde su lado la dirección A→B; B lo ve.
    a.mgr
        .set_direction(&id_b, Direction::Outgoing, false)
        .unwrap();
    eventually("B ve A→B apagada", || !b.mgr.peers()[0].incoming_enabled).await;

    // B la reactiva desde el otro lado; A lo ve y vuelve a enviar.
    b.mgr
        .set_direction(&id_a, Direction::Incoming, true)
        .unwrap();
    eventually("A ve A→B encendida de nuevo", || {
        a.mgr.peers()[0].outgoing_enabled
    })
    .await;
    a.mgr.post_local_notification(notification("5", "mail"));
    assert_eq!(b.next_notification().await.key, "5");

    // La otra dirección es independiente: B→A sigue activa.
    b.mgr.post_local_notification(notification("b1", "sms"));
    assert_eq!(a.next_notification().await.key, "b1");
}

#[tokio::test]
async fn dismissals_actions_and_replies_reach_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _proxy) = paired(dir.path()).await;
    let id_a = a.mgr.device_id().to_string();

    a.mgr.post_local_notification(notification("k", "chat"));
    b.next_notification().await;

    b.mgr.invoke_remote_action(&id_a, "k", "read");
    assert_eq!(
        a.wait_for("ActionRequested", |e| matches!(
            e,
            LinkEvent::ActionRequested { .. }
        ))
        .await,
        LinkEvent::ActionRequested {
            device_id: b.mgr.device_id().into(),
            key: "k".into(),
            action_id: "read".into()
        }
    );

    b.mgr.reply_remote(&id_a, "k", "reply", "¡hola!");
    let LinkEvent::ReplyRequested { text, .. } = a
        .wait_for("ReplyRequested", |e| {
            matches!(e, LinkEvent::ReplyRequested { .. })
        })
        .await
    else {
        unreachable!()
    };
    assert_eq!(text, "¡hola!");

    // Descartar el espejo en B pide a A cancelar la original…
    b.mgr.dismiss_remote_notification(&id_a, "k");
    let dismiss = a
        .wait_for("DismissLocal", |e| {
            matches!(e, LinkEvent::DismissLocal { .. })
        })
        .await;
    assert!(matches!(dismiss, LinkEvent::DismissLocal { key, .. } if key == "k"));

    // …y cuando la original desaparece en A, B cierra su espejo.
    a.mgr.remove_local_notification("k");
    let removed = b
        .wait_for("NotificationRemoved", |e| {
            matches!(e, LinkEvent::NotificationRemoved { .. })
        })
        .await;
    assert!(matches!(removed, LinkEvent::NotificationRemoved { key, .. } if key == "k"));
}

#[tokio::test]
async fn unpair_propagates_to_the_other_side() {
    let dir = tempfile::tempdir().unwrap();
    let (a, mut b, _proxy) = paired(dir.path()).await;

    a.mgr.unpair(b.mgr.device_id()).unwrap();
    b.wait_for("PeerUnpaired", |e| {
        matches!(e, LinkEvent::PeerUnpaired { .. })
    })
    .await;
    assert!(a.mgr.peers().is_empty());
    assert!(b.mgr.peers().is_empty());
}

#[tokio::test]
async fn frozen_peer_is_dropped_by_keepalive() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, proxy) = paired(dir.path()).await;

    proxy.freeze();
    for n in [&mut a, &mut b] {
        n.wait_for("PeerDisconnected", |e| {
            matches!(e, LinkEvent::PeerDisconnected { .. })
        })
        .await;
        assert!(!n.mgr.peers()[0].connected);
    }
}

#[tokio::test]
async fn duplicate_links_resolve_to_a_single_working_one() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _proxy1) = paired(dir.path()).await;

    // B marca también hacia A mientras el primer enlace sigue vivo.
    let (_proxy2, _, _) = link(&b, &a, ConnectIntent::Reconnect).await;

    assert!(a.mgr.is_connected(b.mgr.device_id()));
    assert!(b.mgr.is_connected(a.mgr.device_id()));
    // Ambos lados deben haberse quedado con EL MISMO enlace: si no, esto
    // se perdería por el camino.
    a.mgr.post_local_notification(notification("d1", "chat"));
    assert_eq!(b.next_notification().await.key, "d1");
    b.mgr.post_local_notification(notification("d2", "chat"));
    assert_eq!(a.next_notification().await.key, "d2");
}

#[tokio::test]
async fn config_changed_while_offline_converges_on_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, proxy) = paired(dir.path()).await;
    let (id_a, id_b) = (a.mgr.device_id().to_string(), b.mgr.device_id().to_string());
    proxy.cut();
    drop((a, b));

    let a = node(dir.path(), "a");
    let b = node(dir.path(), "b");
    // Sin enlace: A apaga A→B una vez; B la apaga y la vuelve a encender
    // (versión mayor) → al reconectar debe ganar "encendida" en ambos.
    a.mgr
        .set_direction(&id_b, Direction::Outgoing, false)
        .unwrap();
    b.mgr
        .set_direction(&id_a, Direction::Incoming, false)
        .unwrap();
    b.mgr
        .set_direction(&id_a, Direction::Incoming, true)
        .unwrap();
    // Y B bloquea una app de A sin que A lo sepa todavía.
    b.mgr
        .set_app_blocked(&id_a, Direction::Incoming, "juego", true)
        .unwrap();

    let (_proxy, ra, rb) = link(&a, &b, ConnectIntent::Reconnect).await;
    ra.unwrap();
    rb.unwrap();

    eventually("la configuración converge", || {
        let pa = &a.mgr.peers()[0];
        let pb = &b.mgr.peers()[0];
        pa.outgoing_enabled && pb.incoming_enabled && pa.outgoing_blocked == ["juego"]
    })
    .await;
}

/// Dialer de prueba: cada intento crea un proxy nuevo y hace que `target`
/// acepte el otro extremo.
struct TestDialer {
    target: LinkManager,
    attempts: Arc<std::sync::atomic::AtomicUsize>,
}

impl kushare_link::Dialer for TestDialer {
    fn dial(&self, _peer: kushare_link::PeerRecord) -> kushare_link::DialFuture {
        let target = self.target.clone();
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let (proxy, ours, theirs) = Proxy::new();
            tokio::spawn(async move {
                let _keep = proxy;
                let _ = target.accept(theirs, test_transport()).await;
                std::future::pending::<()>().await;
            });
            Ok((
                Box::new(ours) as Box<dyn kushare_link::LinkStream>,
                test_transport(),
            ))
        })
    }
}

#[tokio::test]
async fn reconnect_loop_restores_a_dropped_link() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, proxy) = paired(dir.path()).await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dialer = TestDialer {
        target: b.mgr.clone(),
        attempts: attempts.clone(),
    };
    let mgr = a.mgr.clone();
    let reconnect = tokio::spawn(async move { mgr.run_reconnect(dialer).await });

    // Mientras el enlace está vivo, el bucle no marca a nadie.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 0);

    proxy.cut();
    a.wait_for("PeerDisconnected", |e| {
        matches!(e, LinkEvent::PeerDisconnected { .. })
    })
    .await;
    a.wait_for("PeerConnected", |e| {
        matches!(e, LinkEvent::PeerConnected { .. })
    })
    .await;
    eventually("B también reconectado", || {
        b.mgr.is_connected(a.mgr.device_id())
    })
    .await;

    a.mgr.post_local_notification(notification("r", "chat"));
    assert_eq!(b.next_notification().await.key, "r");
    reconnect.abort();
}

#[tokio::test]
async fn echoes_of_own_mirrors_are_not_forwarded() {
    let dir = tempfile::tempdir().unwrap();
    let (a, mut b, _proxy) = paired(dir.path()).await;

    // Otro puente (p. ej. KDE Connect) devuelve a A el espejo que B hizo de
    // una notificación de A: lleva "· a" (el nombre de A) en el título.
    let mut echo = notification("eco", "kdeconnect");
    echo.title = "chat · a".into();
    a.mgr.post_local_notification(echo);
    a.mgr.post_local_notification(notification("real", "chat"));
    assert_eq!(
        b.next_notification().await.key,
        "real",
        "el eco no debe reenviarse"
    );
}

#[tokio::test]
async fn wifi_address_is_learned_from_hello_without_scanning() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, proxy) = paired(dir.path()).await;
    proxy.cut();
    // A escucha el enlace en 47816 y reconecta a B "desde" 10.0.0.5.
    a.mgr.set_tcp_port(47816);
    let (_proxy, sa, sb) = Proxy::new();
    let from_a: std::net::SocketAddr = "10.0.0.5:39999".parse().unwrap();
    let (ra, rb) = tokio::join!(
        a.mgr
            .connect(sa, test_transport(), ConnectIntent::Reconnect),
        b.mgr.accept(sb, Transport::Tcp(from_a)),
    );
    ra.unwrap();
    rb.unwrap();
    let peers_toml = std::fs::read_to_string(dir.path().join("b/config/peers.toml")).unwrap();
    assert!(
        peers_toml.contains("last_tcp_addr = \"10.0.0.5:47816\""),
        "B debe recordar la IP de origen con el puerto anunciado:\n{peers_toml}"
    );
}

#[tokio::test]
async fn disconnect_all_closes_links_but_keeps_pairing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b, _proxy) = paired(dir.path()).await;

    a.mgr.disconnect_all();
    a.wait_for("PeerDisconnected", |e| {
        matches!(e, LinkEvent::PeerDisconnected { .. })
    })
    .await;
    // El otro lado nota el cierre del stream.
    b.wait_for("PeerDisconnected", |e| {
        matches!(e, LinkEvent::PeerDisconnected { .. })
    })
    .await;
    assert!(!a.mgr.is_connected(b.mgr.device_id()));
    assert_eq!(a.mgr.peers().len(), 1, "sigue emparejado");
}
