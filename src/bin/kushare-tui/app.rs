//! Estado de la TUI: lo que se sabe del daemon, la pestaña abierta, las
//! listas y su selección. Las llamadas a D-Bus van en tareas aparte y
//! vuelven como [`Ev`] por el canal, para no bloquear el dibujo.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures_util::StreamExt;
use kushare_dbus::{
    CallLogRow, ClipRow, DaemonProxy, PeerRow, PeerStateReply, PhoneNotificationRow, SmsRow,
    SmsThreadRow,
};
use kushare_i18n::{t, tf};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    Phone,
    Messages,
    Calls,
    Notifications,
    Clipboard,
    Devices,
    Send,
}

impl Tab {
    pub const ALL: [Tab; 8] = [
        Tab::Home,
        Tab::Phone,
        Tab::Messages,
        Tab::Calls,
        Tab::Notifications,
        Tab::Clipboard,
        Tab::Devices,
        Tab::Send,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Home => t("gui.nav.home"),
            Tab::Phone => t("gui.nav.phone"),
            Tab::Messages => t("gui.nav.messages"),
            Tab::Calls => t("gui.nav.calls"),
            Tab::Notifications => t("gui.nav.notifications"),
            Tab::Clipboard => t("gui.nav.clipboard"),
            Tab::Devices => t("gui.nav.devices"),
            Tab::Send => t("gui.nav.send"),
        }
    }

    fn from_name(name: &str) -> Option<Tab> {
        Some(match name {
            "home" => Tab::Home,
            "phone" => Tab::Phone,
            "messages" => Tab::Messages,
            "calls" => Tab::Calls,
            "notifications" => Tab::Notifications,
            "clipboard" => Tab::Clipboard,
            "devices" => Tab::Devices,
            "send" => Tab::Send,
            _ => return None,
        })
    }
}

/// Qué se está escribiendo en la línea de entrada.
#[derive(Debug, Clone, PartialEq)]
pub enum InputKind {
    /// Respuesta a la conversación abierta.
    Sms,
    /// Número a marcar.
    Dial,
    /// Respuesta rápida a una notificación: (key, acción).
    Reply(String, String),
    /// Ruta del archivo a mandar.
    SendPath,
}

#[derive(Debug, Clone)]
pub struct Input {
    pub kind: InputKind,
    pub text: String,
}

/// Lo que llega a la TUI desde las tareas de fondo.
pub enum Ev {
    Connected(DaemonProxy<'static>),
    Disconnected,
    /// Cambió algo del daemon: volver a leer el estado.
    Changed,
    NotificationsChanged,
    SmsChanged(String),
    ClipboardChanged,
    Snapshot {
        name: String,
        peers: Vec<PeerRow>,
        states: HashMap<String, PeerStateReply>,
    },
    Notifications(Result<Vec<PhoneNotificationRow>, String>),
    Threads(Result<Vec<SmsThreadRow>, String>),
    Messages(String, Result<Vec<SmsRow>, String>),
    Calls(Result<Vec<CallLogRow>, String>),
    Clipboard(Result<Vec<ClipRow>, String>),
    /// Resultado de una orden: texto para el aviso de abajo.
    Done(Result<String, String>),
}

pub struct App {
    tx: mpsc::UnboundedSender<Ev>,
    pub daemon: Option<DaemonProxy<'static>>,
    pub connected: bool,
    pub device_name: String,
    pub peers: Vec<PeerRow>,
    pub states: HashMap<String, PeerStateReply>,
    pub tab: Tab,
    pub notifs: Vec<PhoneNotificationRow>,
    pub threads: Vec<SmsThreadRow>,
    /// Conversación abierta: (id, mensajes, más reciente primero).
    pub conversation: Option<(String, Vec<SmsRow>)>,
    pub calls: Vec<CallLogRow>,
    pub clipboard: Vec<ClipRow>,
    /// Selección de cada lista (por pestaña).
    pub selected: HashMap<&'static str, usize>,
    pub input: Option<Input>,
    pub send_path: Option<PathBuf>,
    pub toast: Option<(String, Instant)>,
    pub loading: bool,
    pub quit: bool,
}

/// Cuánto se ve un aviso abajo.
const TOAST: Duration = Duration::from_secs(5);

impl App {
    pub fn new(
        tx: mpsc::UnboundedSender<Ev>,
        send_path: Option<PathBuf>,
        screen: Option<&str>,
    ) -> Self {
        let tab = if send_path.is_some() {
            Tab::Send
        } else {
            screen.and_then(Tab::from_name).unwrap_or(Tab::Home)
        };
        Self {
            tx,
            daemon: None,
            connected: false,
            device_name: String::new(),
            peers: Vec::new(),
            states: HashMap::new(),
            tab,
            notifs: Vec::new(),
            threads: Vec::new(),
            conversation: None,
            calls: Vec::new(),
            clipboard: Vec::new(),
            selected: HashMap::new(),
            input: None,
            send_path,
            toast: None,
            loading: false,
            quit: false,
        }
    }

    /// El teléfono conectado (el primero).
    pub fn phone(&self) -> Option<&PeerRow> {
        self.peers.iter().find(|p| p.3 && p.2 == "android")
    }

    pub fn phone_state(&self) -> Option<&PeerStateReply> {
        self.phone().and_then(|p| self.states.get(&p.0))
    }

    pub fn sel(&self, list: &'static str) -> usize {
        self.selected.get(list).copied().unwrap_or(0)
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    // --- Tareas -------------------------------------------------------------

    /// Ejecuta `job` con el daemon en otra tarea; su resultado vuelve como evento.
    fn spawn<F, Fut>(&self, job: F)
    where
        F: FnOnce(DaemonProxy<'static>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Ev> + Send,
    {
        if let Some(d) = self.daemon.clone() {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let _ = tx.send(job(d).await);
            });
        }
    }

    /// Una orden al teléfono: avisa con `ok` si sale bien.
    fn command<F, Fut>(&self, ok: String, job: F)
    where
        F: FnOnce(DaemonProxy<'static>, String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = zbus::Result<()>> + Send,
    {
        let Some(peer) = self.phone().map(|p| p.0.clone()) else {
            return;
        };
        self.spawn(
            move |d| async move { Ev::Done(job(d, peer).await.map(|_| ok).map_err(err_text)) },
        );
    }

    fn refresh(&self) {
        self.spawn(|d| async move {
            let name = d.device_name().await.unwrap_or_default();
            let peers = d.peers().await.unwrap_or_default();
            let mut states = HashMap::new();
            for p in peers.iter().filter(|p| p.3) {
                if let Ok(st) = d.peer_state(&p.0).await {
                    states.insert(p.0.clone(), st);
                }
            }
            Ev::Snapshot {
                name,
                peers,
                states,
            }
        });
    }

    fn load_notifications(&self) {
        let Some(peer) = self.phone().map(|p| p.0.clone()) else {
            return;
        };
        self.spawn(move |d| async move {
            Ev::Notifications(d.phone_notifications(&peer).await.map_err(err_text))
        });
    }

    /// Carga lo que necesita la pestaña abierta.
    fn load_tab(&mut self) {
        let Some(peer) = self.phone().map(|p| p.0.clone()) else {
            return;
        };
        self.loading = true;
        match self.tab {
            Tab::Messages => self.spawn(move |d| async move {
                Ev::Threads(
                    d.sms_threads(&peer, 0, 60)
                        .await
                        .map(|(_, list)| list)
                        .map_err(err_text),
                )
            }),
            Tab::Calls => self.spawn(move |d| async move {
                Ev::Calls(d.call_log(&peer, 100).await.map_err(err_text))
            }),
            Tab::Clipboard => self.spawn(|d| async move {
                Ev::Clipboard(d.clipboard_history().await.map_err(err_text))
            }),
            Tab::Notifications => self.load_notifications(),
            _ => self.loading = false,
        }
    }

    fn open_thread(&mut self, thread: String) {
        let Some(peer) = self.phone().map(|p| p.0.clone()) else {
            return;
        };
        self.conversation = Some((thread.clone(), Vec::new()));
        self.spawn(move |d| async move {
            let r = d
                .sms_messages(&peer, &thread, 50, 0)
                .await
                .map_err(err_text);
            Ev::Messages(thread, r)
        });
    }

    // --- Eventos ------------------------------------------------------------

    pub fn on_tick(&mut self) {
        if self
            .toast
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > TOAST)
        {
            self.toast = None;
        }
    }

    pub fn on_event(&mut self, ev: Ev) {
        match ev {
            Ev::Connected(d) => {
                self.daemon = Some(d);
                self.connected = true;
                self.refresh();
            }
            Ev::Disconnected => {
                self.connected = false;
                self.daemon = None;
                self.peers.clear();
                self.states.clear();
            }
            Ev::Changed => self.refresh(),
            Ev::NotificationsChanged => self.load_notifications(),
            Ev::SmsChanged(thread) => {
                if self.tab == Tab::Messages {
                    self.load_tab();
                    if let Some((open, _)) = &self.conversation
                        && (thread.is_empty() || *open == thread)
                    {
                        let open = open.clone();
                        self.open_thread(open);
                    }
                }
            }
            Ev::ClipboardChanged => {
                if self.tab == Tab::Clipboard {
                    self.load_tab();
                }
            }
            Ev::Snapshot {
                name,
                peers,
                states,
            } => {
                let had_phone = self.phone().is_some();
                self.device_name = name;
                self.peers = peers;
                self.states = states;
                // Al aparecer el teléfono: sus notificaciones (panel) y la pestaña.
                if !had_phone && self.phone().is_some() {
                    self.load_notifications();
                    self.load_tab();
                }
            }
            Ev::Notifications(r) => {
                self.loading = false;
                match r {
                    Ok(mut list) => {
                        list.sort_by_key(|n| std::cmp::Reverse(n.5));
                        self.notifs = list;
                    }
                    Err(e) => self.toast(e),
                }
            }
            Ev::Threads(r) => {
                self.loading = false;
                match r {
                    Ok(list) => self.threads = list,
                    Err(e) => self.toast(e),
                }
            }
            Ev::Messages(thread, r) => match r {
                Ok(list) => {
                    if self
                        .conversation
                        .as_ref()
                        .is_some_and(|(t, _)| *t == thread)
                    {
                        self.conversation = Some((thread, list));
                    }
                }
                Err(e) => self.toast(e),
            },
            Ev::Calls(r) => {
                self.loading = false;
                match r {
                    Ok(list) => self.calls = list,
                    Err(e) => self.toast(e),
                }
            }
            Ev::Clipboard(r) => {
                self.loading = false;
                match r {
                    Ok(list) => self.clipboard = list,
                    Err(e) => self.toast(e),
                }
            }
            Ev::Done(Ok(text)) => {
                if !text.is_empty() {
                    self.toast(text);
                }
                self.refresh();
            }
            Ev::Done(Err(e)) => self.toast(e),
        }
    }

    // --- Teclado ------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.input.is_some() {
            self.on_input_key(key);
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab | KeyCode::Right if self.conversation.is_none() => self.switch(1),
            KeyCode::BackTab | KeyCode::Left if self.conversation.is_none() => self.switch(-1),
            KeyCode::Char(c @ '1'..='8') => {
                let i = c as usize - '1' as usize;
                self.go(Tab::ALL[i]);
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Char('r') => {
                self.refresh();
                self.load_tab();
            }
            // Globales del teléfono.
            KeyCode::Char('f') => {
                let ring = !self.phone_state().is_some_and(|st| st.6);
                self.command(
                    if ring {
                        t("gui.panel.find_started").to_string()
                    } else {
                        String::new()
                    },
                    move |d, peer| async move { d.find_phone(&peer, ring).await },
                );
            }
            KeyCode::Char(' ') => self.media("play-pause"),
            KeyCode::Char('>') | KeyCode::Char('.') => self.media("next"),
            KeyCode::Char('<') | KeyCode::Char(',') => self.media("previous"),
            KeyCode::Esc => self.conversation = None,
            _ => self.on_tab_key(key),
        }
    }

    fn switch(&mut self, step: isize) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0) as isize;
        let n = Tab::ALL.len() as isize;
        self.go(Tab::ALL[((i + step).rem_euclid(n)) as usize]);
    }

    fn go(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.conversation = None;
            self.load_tab();
        }
    }

    /// Lista de la pestaña abierta: (nombre, largo).
    fn current_list(&self) -> Option<(&'static str, usize)> {
        Some(match self.tab {
            Tab::Messages if self.conversation.is_none() => ("threads", self.threads.len()),
            Tab::Calls => ("calls", self.calls.len()),
            Tab::Notifications => ("notifs", self.notifs.len()),
            Tab::Clipboard => ("clipboard", self.clipboard.len()),
            Tab::Devices => ("devices", self.peers.len()),
            _ => return None,
        })
    }

    fn move_sel(&mut self, step: isize) {
        let Some((list, len)) = self.current_list() else {
            return;
        };
        if len == 0 {
            return;
        }
        let i = (self.sel(list) as isize + step).clamp(0, len as isize - 1) as usize;
        self.selected.insert(list, i);
    }

    fn media(&self, action: &'static str) {
        let player = self
            .phone_state()
            .and_then(|st| {
                st.3.iter()
                    .find(|p| p.5 == "playing")
                    .or_else(|| st.3.first())
            })
            .map(|p| p.0.clone())
            .unwrap_or_default();
        self.command(String::new(), move |d, peer| async move {
            d.media_command(&peer, &player, action).await
        });
    }

    fn on_tab_key(&mut self, key: KeyEvent) {
        let code = key.code;
        match self.tab {
            Tab::Phone => match code {
                KeyCode::Char('n') => self.ringer("normal"),
                KeyCode::Char('v') => self.ringer("vibrate"),
                KeyCode::Char('s') => self.ringer("silent"),
                KeyCode::Char('a') => self.call_cmd("answer"),
                KeyCode::Char('h') => {
                    let ringing = self.phone_state().is_some_and(|st| st.2.0 == "ringing");
                    self.call_cmd(if ringing { "reject" } else { "hangup" });
                }
                KeyCode::Char('m') => {
                    let muted = self.phone_state().is_some_and(|st| st.2.3);
                    self.call_cmd(if muted { "unmute" } else { "mute" });
                }
                _ => {}
            },
            Tab::Messages => match code {
                KeyCode::Enter if self.conversation.is_none() => {
                    if let Some(th) = self.threads.get(self.sel("threads")) {
                        let id = th.0.clone();
                        self.open_thread(id);
                    }
                }
                KeyCode::Char('w') if self.conversation.is_some() => {
                    self.start_input(InputKind::Sms)
                }
                _ => {}
            },
            Tab::Calls => match code {
                KeyCode::Enter => {
                    if let Some(c) = self.calls.get(self.sel("calls")) {
                        self.dial(c.0.clone());
                    }
                }
                KeyCode::Char('d') => self.start_input(InputKind::Dial),
                _ => {}
            },
            Tab::Notifications => match code {
                KeyCode::Char('x') | KeyCode::Delete => {
                    if let Some(n) = self.notifs.get(self.sel("notifs")) {
                        let key = n.0.clone();
                        self.notifs.retain(|m| m.0 != key);
                        self.command(String::new(), move |d, peer| async move {
                            d.dismiss_phone_notification(&peer, &key).await
                        });
                    }
                }
                KeyCode::Char('X') => {
                    let keys: Vec<String> = self.notifs.drain(..).map(|n| n.0).collect();
                    self.command(
                        t("gui.notifs.dismissed_all").into(),
                        move |d, peer| async move {
                            for key in keys {
                                d.dismiss_phone_notification(&peer, &key).await?;
                            }
                            Ok(())
                        },
                    );
                }
                KeyCode::Enter => {
                    // Primera acción que no es responder.
                    if let Some(n) = self.notifs.get(self.sel("notifs"))
                        && let Some(action) = n.7.iter().find(|a| !a.2)
                    {
                        let (key, id) = (n.0.clone(), action.0.clone());
                        self.command(t("gui.notifs.done").into(), move |d, peer| async move {
                            d.phone_notification_action(&peer, &key, &id).await
                        });
                    }
                }
                KeyCode::Char('w') => {
                    if let Some(n) = self.notifs.get(self.sel("notifs"))
                        && let Some(action) = n.7.iter().find(|a| a.2)
                    {
                        let kind = InputKind::Reply(n.0.clone(), action.0.clone());
                        self.start_input(kind);
                    }
                }
                _ => {}
            },
            Tab::Clipboard => {
                if code == KeyCode::Enter
                    && let Some(c) = self.clipboard.get(self.sel("clipboard"))
                {
                    let text = c.0.clone();
                    self.spawn(move |d| async move {
                        Ev::Done(
                            d.clipboard_copy(&text)
                                .await
                                .map(|_| t("tui.copied").to_string())
                                .map_err(err_text),
                        )
                    });
                }
            }
            Tab::Devices => {
                if code == KeyCode::Char('s') {
                    self.spawn(|d| async move {
                        Ev::Done(d.scan(10).await.map(|_| String::new()).map_err(err_text))
                    });
                }
            }
            Tab::Send => match code {
                KeyCode::Char('e') => self.start_input(InputKind::SendPath),
                KeyCode::Enter => self.send_file(),
                _ => {}
            },
            Tab::Home => {}
        }
    }

    fn ringer(&self, mode: &'static str) {
        self.command(String::new(), move |d, peer| async move {
            d.set_ringer(&peer, mode).await
        });
    }

    fn call_cmd(&self, action: &'static str) {
        self.command(String::new(), move |d, peer| async move {
            d.call_command(&peer, action).await
        });
    }

    fn dial(&self, number: String) {
        let msg = tf("gui.calls.calling", &[("number", &number)]);
        self.command(
            msg,
            move |d, peer| async move { d.dial(&peer, &number).await },
        );
    }

    fn send_file(&mut self) {
        let Some(path) = self.send_path.clone() else {
            self.start_input(InputKind::SendPath);
            return;
        };
        let Some(bt) = self.phone().map(|p| p.7.clone()).filter(|a| !a.is_empty()) else {
            self.toast(t("tui.no_phone"));
            return;
        };
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = self.phone().map(|p| p.1.clone()).unwrap_or_default();
        let msg = tf("gui.toast.sending", &[("file", &label), ("name", &name)]);
        self.spawn(move |d| async move {
            Ev::Done(
                d.send_file_bt(&path.to_string_lossy(), &bt)
                    .await
                    .map(|_| msg)
                    .map_err(err_text),
            )
        });
    }

    // --- Línea de entrada ---------------------------------------------------

    fn start_input(&mut self, kind: InputKind) {
        let text = match &kind {
            InputKind::SendPath => self
                .send_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.input = Some(Input { kind, text });
    }

    fn on_input_key(&mut self, key: KeyEvent) {
        let Some(input) = self.input.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                input.text.pop();
            }
            KeyCode::Char(c) => input.text.push(c),
            KeyCode::Enter => {
                let Input { kind, text } = self.input.take().expect("hay entrada");
                let text = text.trim().to_string();
                if text.is_empty() {
                    return;
                }
                self.submit(kind, text);
            }
            _ => {}
        }
    }

    fn submit(&mut self, kind: InputKind, text: String) {
        match kind {
            InputKind::Dial => self.dial(text),
            InputKind::Sms => {
                let Some(address) = self
                    .conversation
                    .as_ref()
                    .and_then(|(id, _)| self.threads.iter().find(|th| th.0 == *id))
                    .map(|th| th.1.clone())
                else {
                    return;
                };
                self.command(t("tui.sent").into(), move |d, peer| async move {
                    d.send_sms(&peer, &address, &text).await
                });
            }
            InputKind::Reply(key, action) => {
                self.command(t("gui.notifs.replied").into(), move |d, peer| async move {
                    d.phone_notification_reply(&peer, &key, &action, &text)
                        .await
                });
            }
            InputKind::SendPath => {
                let path = PathBuf::from(shellexpand_home(&text));
                if path.is_file() {
                    self.send_path = Some(path);
                    self.send_file();
                } else {
                    self.toast(tf("tui.not_a_file", &[("path", &text)]));
                }
            }
        }
    }
}

/// `~/archivo` → `$HOME/archivo`.
fn shellexpand_home(path: &str) -> String {
    match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => format!("{home}/{rest}"),
        _ => path.to_string(),
    }
}

pub fn err_text(err: zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(msg), _) => msg,
        other => other.to_string(),
    }
}

/// Conecta con el daemon (reintentando) y reenvía sus señales.
pub async fn daemon_events(tx: mpsc::UnboundedSender<Ev>) {
    loop {
        let Ok(daemon) = kushare_dbus::connect().await else {
            let _ = tx.send(Ev::Disconnected);
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        };
        let (
            Ok(mut peers),
            Ok(mut state),
            Ok(mut sms),
            Ok(mut notifs),
            Ok(mut clip),
            Ok(mut owner),
        ) = (
            daemon.receive_peers_changed().await,
            daemon.receive_peer_state_changed().await,
            daemon.receive_sms_changed().await,
            daemon.receive_phone_notifications_changed().await,
            daemon.receive_clipboard_changed().await,
            daemon.inner().receive_owner_changed().await,
        )
        else {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        };
        // ¿Corre de verdad? (el proxy existe aunque el daemon no esté).
        if daemon.device_id().await.is_err() {
            let _ = tx.send(Ev::Disconnected);
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        if tx.send(Ev::Connected(daemon.clone())).is_err() {
            return;
        }
        // Respaldo por si se pierde alguna señal.
        let mut every = tokio::time::interval(Duration::from_secs(10));
        loop {
            let ev = tokio::select! {
                Some(_) = peers.next() => Ev::Changed,
                Some(_) = state.next() => Ev::Changed,
                _ = every.tick() => Ev::Changed,
                Some(sig) = sms.next() => match sig.args() {
                    Ok(a) => Ev::SmsChanged(a.thread_id),
                    Err(_) => continue,
                },
                Some(_) = notifs.next() => Ev::NotificationsChanged,
                Some(_) = clip.next() => Ev::ClipboardChanged,
                Some(new_owner) = owner.next() => {
                    if new_owner.is_none() {
                        break;
                    }
                    continue;
                }
                else => break,
            };
            if tx.send(ev).is_err() {
                return;
            }
        }
        let _ = tx.send(Ev::Disconnected);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
