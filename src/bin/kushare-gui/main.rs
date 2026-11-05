//! `kushare-gui`: interfaz gráfica de escritorio de kushare (iced), con lo
//! mismo que la app Android: estado del enlace, dispositivos (emparejar con
//! PIN, direcciones de notificaciones, filtros por app), enviar archivos y
//! ajustes de tema (Sistema, Claro, Oscuro, OLED). No hace red por su
//! cuenta: controla al daemon `kushare` por D-Bus.

mod daemon;
mod icons;
mod style;

use daemon::{Event, Peer, Snapshot};
use iced::font::Weight;
use iced::widget::{
    button, center, checkbox, column, container, opaque, row, scrollable, space, stack, text,
    toggler,
};
use iced::{Alignment, Color, Element, Font, Length, Padding, Subscription, Task, Theme};
use icons::{Blob, Icon, shape_icon};
use kushare_dbus::{AppsReply, DaemonProxy};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use style::{Appearance, ButtonKind, Colors, Shape, ThemeMode};

const BOLD: Font = Font {
    weight: Weight::Bold,
    ..Font::DEFAULT
};
const EXTRA_BOLD: Font = Font {
    weight: Weight::ExtraBold,
    ..Font::DEFAULT
};
const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..Font::DEFAULT
};

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("kushare")
        .theme(App::theme)
        .subscription(App::subscription)
        .window_size((1040.0, 720.0))
        .run()
}

// --- Configuración persistente -------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct GuiConfig {
    theme: ThemeMode,
    /// Con el tema del sistema en oscuro, usar negro puro.
    oled_when_system_dark: bool,
    /// Formas que giran mientras hay actividad.
    animations: bool,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            oled_when_system_dark: true,
            animations: true,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "kushare").map(|d| d.config_dir().join("gui.toml"))
}

impl GuiConfig {
    fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let (Some(path), Ok(text)) = (config_path(), toml::to_string_pretty(self)) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, text);
        }
    }
}

// --- Estado ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Home,
    Devices,
    Send,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonState {
    Connecting,
    NotRunning,
    Running,
}

#[derive(Debug, Clone)]
struct Pairing {
    id: String,
    name: String,
    pin: String,
}

#[derive(Debug, Clone)]
struct AppFilter {
    peer_id: String,
    peer_name: String,
    outgoing: bool,
    apps: Option<AppsReply>,
}

struct App {
    config: GuiConfig,
    system_dark: bool,
    screen: Screen,
    daemon: Option<DaemonProxy<'static>>,
    state: DaemonState,
    snapshot: Snapshot,
    bonded_bt: Vec<(String, String)>,
    pairing: Option<Pairing>,
    app_filter: Option<AppFilter>,
    send_file: Option<PathBuf>,
    toast: Option<String>,
    angle: f32,
}

#[derive(Debug, Clone)]
enum Message {
    SystemTheme(iced::theme::Mode),
    Daemon(Event),
    Refresh,
    Snapshot(Result<Snapshot, String>),
    Bonded(Vec<(String, String)>),
    Tick,
    Navigate(Screen),
    // dispositivos
    SetDirection {
        peer: String,
        outgoing: bool,
        enabled: bool,
    },
    Unpair(String),
    Scan,
    Pair(String),
    RespondPairing(bool),
    OpenApps {
        peer: String,
        name: String,
        outgoing: bool,
    },
    AppsLoaded(Result<AppsReply, String>),
    SetAppBlocked {
        app: String,
        blocked: bool,
    },
    CloseApps,
    // envío
    PickFile,
    FilePicked(Option<PathBuf>),
    SendTo {
        address: String,
        name: String,
    },
    // daemon y ajustes
    StartDaemon,
    QuitDaemon,
    SetTheme(ThemeMode),
    SetOledWhenSystemDark(bool),
    SetAnimations(bool),
    OpenDownloads,
    Done(Result<String, String>),
    DismissToast,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let app = Self {
            config: GuiConfig::load(),
            system_dark: true,
            screen: Screen::Home,
            daemon: None,
            state: DaemonState::Connecting,
            snapshot: Snapshot::default(),
            bonded_bt: Vec::new(),
            pairing: None,
            app_filter: None,
            send_file: None,
            toast: None,
            angle: 0.0,
        };
        let tasks = Task::batch([
            iced::system::theme().map(Message::SystemTheme),
            Task::perform(daemon::bonded_bluetooth(), Message::Bonded),
        ]);
        (app, tasks)
    }

    fn appearance(&self) -> Appearance {
        match self.config.theme {
            ThemeMode::Light => Appearance::Light,
            ThemeMode::Dark => Appearance::Dark,
            ThemeMode::Oled => Appearance::Oled,
            ThemeMode::System if !self.system_dark => Appearance::Light,
            ThemeMode::System if self.config.oled_when_system_dark => Appearance::Oled,
            ThemeMode::System => Appearance::Dark,
        }
    }

    fn colors(&self) -> Colors {
        style::colors(self.appearance())
    }

    fn theme(&self) -> Option<Theme> {
        Some(style::iced_theme(self.appearance()))
    }

    /// ¿Hay algo que animar? (para no redibujar sin motivo)
    fn animating(&self) -> bool {
        self.config.animations
            && (self.snapshot.scanning
                || self.snapshot.peers.iter().any(|p| p.connected)
                || self.pairing.is_some())
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            daemon::events().map(Message::Daemon),
            iced::system::theme_changes().map(Message::SystemTheme),
        ];
        if self.state == DaemonState::Running {
            // Respaldo por si se pierde alguna señal; más seguido mientras se escanea.
            let every = if self.snapshot.scanning { 1 } else { 10 };
            subs.push(iced::time::every(Duration::from_secs(every)).map(|_| Message::Refresh));
        }
        if self.animating() {
            subs.push(iced::time::every(Duration::from_millis(40)).map(|_| Message::Tick));
        }
        Subscription::batch(subs)
    }

    fn refresh(&self) -> Task<Message> {
        match &self.daemon {
            Some(d) => Task::perform(daemon::snapshot(d.clone()), Message::Snapshot),
            None => Task::none(),
        }
    }

    /// Lanza una llamada al daemon y muestra su resultado como aviso.
    fn call<F, Fut>(&self, ok: impl Into<String>, f: F) -> Task<Message>
    where
        F: FnOnce(DaemonProxy<'static>) -> Fut,
        Fut: std::future::Future<Output = zbus::Result<()>> + Send + 'static,
    {
        let Some(d) = self.daemon.clone() else {
            return Task::done(Message::Done(Err("el daemon no está corriendo".into())));
        };
        let ok = ok.into();
        let fut = f(d);
        Task::perform(
            async move { fut.await.map(|()| ok).map_err(|e| e.to_string()) },
            Message::Done,
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SystemTheme(mode) => {
                self.system_dark = !matches!(mode, iced::theme::Mode::Light);
                Task::none()
            }
            Message::Daemon(Event::Connected(proxy)) => {
                self.daemon = Some(proxy);
                self.state = DaemonState::Running;
                self.refresh()
            }
            Message::Daemon(Event::Disconnected) => {
                self.daemon = None;
                self.state = DaemonState::NotRunning;
                self.pairing = None;
                for p in &mut self.snapshot.peers {
                    p.connected = false;
                }
                Task::none()
            }
            Message::Daemon(Event::PeersChanged) | Message::Refresh => self.refresh(),
            Message::Daemon(Event::PairingRequested { id, name, pin }) => {
                self.pairing = Some(Pairing { id, name, pin });
                Task::none()
            }
            Message::Daemon(Event::PairingFinished { id, name, accepted }) => {
                if self.pairing.as_ref().is_some_and(|p| p.id == id) {
                    self.pairing = None;
                }
                self.toast = Some(if accepted {
                    format!("Emparejado con {name}")
                } else {
                    format!("No se emparejó con {name}")
                });
                self.refresh()
            }
            Message::Snapshot(Ok(snapshot)) => {
                self.snapshot = snapshot;
                Task::none()
            }
            Message::Snapshot(Err(err)) => {
                tracing::debug!(%err, "no se pudo leer el estado del daemon");
                Task::none()
            }
            Message::Bonded(list) => {
                self.bonded_bt = list;
                Task::none()
            }
            Message::Tick => {
                self.angle = (self.angle + 0.02) % std::f32::consts::TAU;
                Task::none()
            }
            Message::Navigate(screen) => {
                self.screen = screen;
                if screen == Screen::Devices {
                    return Task::batch([
                        self.refresh(),
                        Task::perform(daemon::bonded_bluetooth(), Message::Bonded),
                    ]);
                }
                Task::none()
            }
            Message::SetDirection {
                peer,
                outgoing,
                enabled,
            } => {
                let dir = if outgoing { "out" } else { "in" };
                Task::batch([
                    self.call("", move |d| async move {
                        d.set_direction(&peer, dir, enabled).await
                    }),
                    self.refresh(),
                ])
            }
            Message::Unpair(peer) => {
                self.call(
                    "Desemparejado",
                    move |d| async move { d.unpair(&peer).await },
                )
            }
            Message::Scan => {
                self.snapshot.scanning = true;
                self.call("", |d| async move { d.scan(20).await })
            }
            Message::Pair(target) => self
                .call("Esperando confirmación del PIN…", move |d| async move {
                    d.pair(&target).await.map(|_| ())
                }),
            Message::RespondPairing(accept) => {
                let Some(p) = self.pairing.take() else {
                    return Task::none();
                };
                self.call("", move |d| async move {
                    d.respond_pairing(&p.id, accept).await.map(|_| ())
                })
            }
            Message::OpenApps {
                peer,
                name,
                outgoing,
            } => {
                self.app_filter = Some(AppFilter {
                    peer_id: peer.clone(),
                    peer_name: name,
                    outgoing,
                    apps: None,
                });
                match self.daemon.clone() {
                    Some(d) => Task::perform(
                        async move { d.apps(&peer).await.map_err(|e| e.to_string()) },
                        Message::AppsLoaded,
                    ),
                    None => Task::none(),
                }
            }
            Message::AppsLoaded(result) => {
                match result {
                    Ok(apps) => {
                        if let Some(f) = &mut self.app_filter {
                            f.apps = Some(apps);
                        }
                    }
                    Err(err) => self.toast = Some(err),
                }
                Task::none()
            }
            Message::SetAppBlocked { app, blocked } => {
                let Some(f) = &mut self.app_filter else {
                    return Task::none();
                };
                if let Some(apps) = &mut f.apps {
                    let list = if f.outgoing { &mut apps.2 } else { &mut apps.3 };
                    list.retain(|a| a != &app);
                    if blocked {
                        list.push(app.clone());
                    }
                }
                let (peer, dir) = (f.peer_id.clone(), if f.outgoing { "out" } else { "in" });
                self.call("", move |d| async move {
                    d.set_app_blocked(&peer, dir, &app, blocked).await
                })
            }
            Message::CloseApps => {
                self.app_filter = None;
                Task::none()
            }
            Message::PickFile => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_file()
                        .await
                        .map(|f| f.path().to_path_buf())
                },
                Message::FilePicked,
            ),
            Message::FilePicked(file) => {
                if file.is_some() {
                    self.send_file = file;
                    self.screen = Screen::Send;
                }
                Task::none()
            }
            Message::SendTo { address, name } => {
                let Some(file) = self.send_file.clone() else {
                    return Task::none();
                };
                let label = file
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.call(
                    format!("Enviando \"{label}\" a {name}… (avisará al terminar)"),
                    move |d| async move { d.send_file_bt(&file.to_string_lossy(), &address).await },
                )
            }
            Message::StartDaemon => {
                self.state = DaemonState::Connecting;
                match daemon::start_daemon() {
                    Ok(()) => Task::none(),
                    Err(err) => {
                        self.state = DaemonState::NotRunning;
                        self.toast = Some(err);
                        Task::none()
                    }
                }
            }
            Message::QuitDaemon => self.call("Daemon detenido", |d| async move { d.quit().await }),
            Message::SetTheme(theme) => {
                self.config.theme = theme;
                self.config.save();
                Task::none()
            }
            Message::SetOledWhenSystemDark(on) => {
                self.config.oled_when_system_dark = on;
                self.config.save();
                Task::none()
            }
            Message::SetAnimations(on) => {
                self.config.animations = on;
                self.config.save();
                Task::none()
            }
            Message::OpenDownloads => {
                let dir = directories::UserDirs::new()
                    .and_then(|d| d.download_dir().map(PathBuf::from))
                    .unwrap_or_else(|| PathBuf::from("."));
                let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
                Task::none()
            }
            Message::Done(Ok(msg)) => {
                if !msg.is_empty() {
                    self.toast = Some(msg);
                }
                self.refresh()
            }
            Message::Done(Err(err)) => {
                self.toast = Some(err);
                self.refresh()
            }
            Message::DismissToast => {
                self.toast = None;
                Task::none()
            }
        }
    }

    // --- Vistas ------------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        let c = self.colors();
        let content: Element<'_, Message> = match self.screen {
            Screen::Home => self.home(c),
            Screen::Devices => self.devices(c),
            Screen::Send => self.send(c),
            Screen::Settings => self.settings(c),
        };
        let body = container(
            scrollable(
                container(content)
                    .padding(Padding::new(28.0).left(12.0))
                    .max_width(920),
            )
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill);
        let page = row![self.rail(c), body];
        let mut layers = stack![
            container(page)
                .style(style::surface(c))
                .width(Length::Fill)
                .height(Length::Fill)
        ];
        if let Some(t) = &self.toast {
            layers = layers.push(self.toast_view(c, t));
        }
        if let Some(p) = &self.pairing {
            layers = layers.push(self.pairing_dialog(c, p));
        }
        if let Some(f) = &self.app_filter {
            layers = layers.push(self.apps_dialog(c, f));
        }
        layers.into()
    }

    /// Riel de navegación.
    fn rail(&self, c: Colors) -> Element<'_, Message> {
        let item = |icon: Icon, label: &'static str, screen: Screen| {
            let selected = self.screen == screen;
            button(
                column![
                    icon.view(
                        24.0,
                        if selected {
                            c.on_secondary_container
                        } else {
                            c.on_surface_variant
                        }
                    ),
                    text(label)
                        .size(12)
                        .font(if selected { BOLD } else { Font::DEFAULT }),
                ]
                .align_x(Alignment::Center)
                .spacing(4),
            )
            .padding(Padding::from([10, 14]))
            .width(Length::Fixed(84.0))
            .style(style::button(c, ButtonKind::Nav(selected)))
            .on_press(Message::Navigate(screen))
        };
        container(
            column![
                text("ku").size(26).font(EXTRA_BOLD).color(c.primary),
                space().height(16),
                item(Icon::Home, "Inicio", Screen::Home),
                item(Icon::Devices, "Dispositivos", Screen::Devices),
                item(Icon::Send, "Enviar", Screen::Send),
                item(Icon::Settings, "Ajustes", Screen::Settings),
            ]
            .spacing(10)
            .align_x(Alignment::Center),
        )
        .padding(Padding::new(16.0).top(24.0))
        .height(Length::Fill)
        .style(style::card(c.container_low, c.on_surface, Shape::List))
        .into()
    }

    fn title<'a>(&self, c: Colors, title: &'a str, subtitle: String) -> Element<'a, Message> {
        column![
            text(title).size(44).font(EXTRA_BOLD).color(c.on_surface),
            text(subtitle).size(16).color(c.on_surface_variant),
        ]
        .spacing(2)
        .padding(Padding::default().bottom(18.0))
        .into()
    }

    fn section<'a>(&self, c: Colors, label: &'a str) -> Element<'a, Message> {
        text(label).size(16).font(BOLD).color(c.primary).into()
    }

    fn home(&self, c: Colors) -> Element<'_, Message> {
        let connected: Vec<&Peer> = self.snapshot.peers.iter().filter(|p| p.connected).collect();
        let (title, detail, icon, blob, active, highlight) = match self.state {
            DaemonState::Connecting => (
                "Iniciando…",
                "Conectando con el daemon".to_string(),
                Icon::Schedule,
                Blob::SoftBurst,
                true,
                false,
            ),
            DaemonState::NotRunning => (
                "Apagado",
                "El daemon de kushare no está corriendo.".to_string(),
                Icon::Power,
                Blob::Circle,
                false,
                false,
            ),
            DaemonState::Running if !connected.is_empty() => (
                "Conectado",
                connected
                    .iter()
                    .map(|p| format!("{} · {}", p.name, p.via()))
                    .collect::<Vec<_>>()
                    .join("   "),
                if connected.iter().any(|p| p.via() == "Bluetooth") {
                    Icon::Bluetooth
                } else {
                    Icon::Wifi
                },
                Blob::Cookie9,
                true,
                true,
            ),
            DaemonState::Running if self.snapshot.peers.is_empty() => (
                "Sin emparejar",
                "Empareja tu teléfono en Dispositivos.".to_string(),
                Icon::Devices,
                Blob::Cookie4,
                false,
                false,
            ),
            DaemonState::Running => (
                "Esperando",
                "Se conectará solo cuando el teléfono esté cerca.".to_string(),
                Icon::Schedule,
                Blob::SoftBurst,
                true,
                false,
            ),
        };
        let (bg, fg, accent, on_accent) = if highlight {
            (
                c.primary_container,
                c.on_primary_container,
                c.primary,
                c.on_primary,
            )
        } else {
            (
                c.container_high,
                c.on_surface,
                c.container_highest,
                c.on_surface_variant,
            )
        };
        let daemon_button: Element<'_, Message> = match self.state {
            DaemonState::NotRunning => button(text("Iniciar").font(SEMIBOLD))
                .padding(Padding::from([10, 22]))
                .style(style::button(c, ButtonKind::Filled))
                .on_press(Message::StartDaemon)
                .into(),
            _ => space().into(),
        };
        let hero = container(
            row![
                shape_icon(
                    icon,
                    blob,
                    accent,
                    on_accent,
                    92.0,
                    if active { self.angle } else { 0.0 }
                ),
                column![text(title).size(38).font(EXTRA_BOLD), text(detail).size(15)]
                    .spacing(4)
                    .width(Length::Fill),
                daemon_button,
            ]
            .spacing(22)
            .align_y(Alignment::Center),
        )
        .padding(28)
        .width(Length::Fill)
        .style(style::card(bg, fg, Shape::Hero));

        let action = |title: &'static str,
                      sub: &'static str,
                      icon: Icon,
                      blob: Blob,
                      bg: Color,
                      fg: Color,
                      acc: Color,
                      on_acc: Color,
                      shape: Shape,
                      msg: Message| {
            button(
                column![
                    shape_icon(icon, blob, acc, on_acc, 60.0, 0.0),
                    space().height(18),
                    text(title).size(28).font(EXTRA_BOLD),
                    text(sub).size(13),
                ]
                .spacing(2),
            )
            .padding(24)
            .width(Length::Fill)
            .height(Length::Fixed(200.0))
            .style(style::button(c, ButtonKind::Card { bg, fg, shape }))
            .on_press(msg)
        };
        let actions = row![
            action(
                "Enviar",
                "Un archivo a tu teléfono por Bluetooth",
                Icon::Send,
                Blob::Clover,
                c.tertiary_container,
                c.on_tertiary_container,
                c.tertiary,
                c.on_tertiary,
                Shape::ActionLeading,
                Message::PickFile
            ),
            action(
                "Recibidos",
                "Este PC recibe siempre; se guardan en Descargas",
                Icon::Download,
                Blob::Flower,
                c.secondary_container,
                c.on_secondary_container,
                c.secondary,
                c.on_secondary,
                Shape::ActionTrailing,
                Message::OpenDownloads
            ),
        ]
        .spacing(14);

        let devices = button(
            row![
                shape_icon(
                    Icon::Devices,
                    Blob::Cookie6,
                    c.secondary,
                    c.on_secondary,
                    54.0,
                    0.0
                ),
                column![
                    text("Dispositivos y notificaciones").size(22).font(BOLD),
                    text(match self.snapshot.peers.len() {
                        0 => "Empareja tu teléfono para sincronizar notificaciones".to_string(),
                        1 => "1 dispositivo emparejado".to_string(),
                        n => format!("{n} dispositivos emparejados"),
                    })
                    .size(14)
                    .color(c.on_surface_variant),
                ]
                .width(Length::Fill),
                text("→").size(26).color(c.on_surface_variant),
            ]
            .spacing(18)
            .align_y(Alignment::Center),
        )
        .padding(22)
        .width(Length::Fill)
        .style(style::button(
            c,
            ButtonKind::Card {
                bg: c.container,
                fg: c.on_surface,
                shape: Shape::Wide,
            },
        ))
        .on_press(Message::Navigate(Screen::Devices));

        column![
            self.title(c, "kushare", self.snapshot.device_name.clone()),
            hero,
            actions,
            devices
        ]
        .spacing(14)
        .into()
    }

    fn switch_row<'a>(
        &self,
        c: Colors,
        label: &'a str,
        sub: Option<&'a str>,
        on: bool,
        msg: impl Fn(bool) -> Message + 'a,
    ) -> Element<'a, Message> {
        let mut texts = column![text(label).size(16).font(SEMIBOLD)];
        if let Some(sub) = sub {
            texts = texts.push(text(sub).size(13).color(c.on_surface_variant));
        }
        row![
            texts.width(Length::Fill),
            toggler(on).on_toggle(msg).size(26).style(style::toggler(c))
        ]
        .align_y(Alignment::Center)
        .spacing(12)
        .padding(Padding::from([4, 0]))
        .into()
    }

    fn devices(&self, c: Colors) -> Element<'_, Message> {
        let running = self.state == DaemonState::Running;
        let mut col = column![self.title(
            c,
            "Dispositivos",
            if running {
                format!(
                    "{} conectado(s) de {}",
                    self.snapshot.peers.iter().filter(|p| p.connected).count(),
                    self.snapshot.peers.len()
                )
            } else {
                "El daemon no está corriendo".to_string()
            },
        )]
        .spacing(14);

        col = col.push(self.section(c, "Emparejados"));
        if self.snapshot.peers.is_empty() {
            col = col.push(
                text("Todavía no hay ninguno. Empareja por la red o por Bluetooth más abajo.")
                    .color(c.on_surface_variant),
            );
        }
        for p in &self.snapshot.peers {
            col = col.push(self.peer_card(c, p));
        }

        let scan_label = if self.snapshot.scanning {
            "Buscando…"
        } else {
            "Buscar"
        };
        col = col.push(
            row![
                container(self.section(c, "En esta red")).width(Length::Fill),
                button(
                    row![
                        Icon::Search.view(18.0, c.on_secondary_container),
                        text(scan_label).font(SEMIBOLD)
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                )
                .padding(Padding::from([10, 20]))
                .style(style::button(c, ButtonKind::Tonal))
                .on_press_maybe((running && !self.snapshot.scanning).then_some(Message::Scan)),
            ]
            .align_y(Alignment::Center),
        );
        if self.snapshot.discovered.is_empty() {
            col = col.push(
                text(if self.snapshot.scanning {
                    "Buscando dispositivos kushare…"
                } else {
                    "Pulsa Buscar. Si la red aísla a los dispositivos, empareja por Bluetooth."
                })
                .color(c.on_surface_variant),
            );
        }
        for (id, name, addr) in &self.snapshot.discovered {
            col = col.push(self.candidate(
                c,
                Icon::Wifi,
                name,
                addr,
                running.then(|| Message::Pair(id.clone())),
            ));
        }

        col = col.push(self.section(c, "Por Bluetooth"));
        let paired_bt: Vec<&str> = self
            .snapshot
            .peers
            .iter()
            .map(|p| p.bt_address.as_str())
            .collect();
        let candidates: Vec<&(String, String)> = self
            .bonded_bt
            .iter()
            .filter(|(a, _)| !paired_bt.contains(&a.as_str()))
            .collect();
        if candidates.is_empty() {
            col = col.push(text("No hay dispositivos Bluetooth vinculados sin emparejar (bluetoothctl pair <MAC>).").color(c.on_surface_variant));
        }
        for (addr, name) in candidates {
            col = col.push(self.candidate(
                c,
                Icon::Bluetooth,
                name,
                addr,
                running.then(|| Message::Pair(addr.clone())),
            ));
        }
        col.into()
    }

    fn candidate<'a>(
        &self,
        c: Colors,
        icon: Icon,
        name: &'a str,
        detail: &'a str,
        on_pair: Option<Message>,
    ) -> Element<'a, Message> {
        container(
            row![
                shape_icon(
                    icon,
                    Blob::Squircle,
                    c.secondary_container,
                    c.on_secondary_container,
                    46.0,
                    0.0
                ),
                column![
                    text(name).size(17).font(SEMIBOLD),
                    text(detail).size(13).color(c.on_surface_variant)
                ]
                .width(Length::Fill),
                button(text("Emparejar").font(SEMIBOLD))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press_maybe(on_pair),
            ]
            .spacing(16)
            .align_y(Alignment::Center),
        )
        .padding(16)
        .width(Length::Fill)
        .style(style::card(c.container, c.on_surface, Shape::List))
        .into()
    }

    fn peer_card<'a>(&'a self, c: Colors, p: &'a Peer) -> Element<'a, Message> {
        let (bg, fg, acc, on_acc) = if p.connected {
            (
                c.primary_container,
                c.on_primary_container,
                c.primary,
                c.on_primary,
            )
        } else {
            (
                c.container_high,
                c.on_surface,
                c.container_highest,
                c.on_surface_variant,
            )
        };
        let id = p.id.clone();
        let id2 = p.id.clone();
        let header = row![
            shape_icon(
                if p.kind == "android" {
                    Icon::Devices
                } else {
                    Icon::Laptop
                },
                if p.connected {
                    Blob::Cookie9
                } else {
                    Blob::Circle
                },
                acc,
                on_acc,
                64.0,
                if p.connected { self.angle } else { 0.0 },
            ),
            column![
                text(&p.name).size(26).font(EXTRA_BOLD),
                text(if p.connected {
                    format!("Conectado por {}", p.via())
                } else {
                    "Desconectado".to_string()
                })
                .size(14),
            ]
            .width(Length::Fill),
        ]
        .spacing(18)
        .align_y(Alignment::Center);
        let send_button: Element<'_, Message> = if p.bt_address.is_empty() {
            space().into()
        } else {
            button(
                row![
                    Icon::Send.view(16.0, c.on_secondary_container),
                    text("Enviar archivo").font(SEMIBOLD)
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([9, 18]))
            .style(style::button(c, ButtonKind::Tonal))
            .on_press(Message::PickFile)
            .into()
        };
        container(
            column![
                header,
                self.switch_row(
                    c,
                    "Recibir sus notificaciones",
                    None,
                    p.incoming,
                    move |on| Message::SetDirection {
                        peer: id.clone(),
                        outgoing: false,
                        enabled: on
                    }
                ),
                self.switch_row(
                    c,
                    "Enviarle mis notificaciones",
                    None,
                    p.outgoing,
                    move |on| Message::SetDirection {
                        peer: id2.clone(),
                        outgoing: true,
                        enabled: on
                    }
                ),
                row![
                    button(text("Apps que recibo").font(SEMIBOLD))
                        .padding(Padding::from([9, 18]))
                        .style(style::button(c, ButtonKind::Outlined))
                        .on_press(Message::OpenApps {
                            peer: p.id.clone(),
                            name: p.name.clone(),
                            outgoing: false
                        }),
                    button(text("Apps que envío").font(SEMIBOLD))
                        .padding(Padding::from([9, 18]))
                        .style(style::button(c, ButtonKind::Outlined))
                        .on_press(Message::OpenApps {
                            peer: p.id.clone(),
                            name: p.name.clone(),
                            outgoing: true
                        }),
                    send_button,
                    space().width(Length::Fill),
                    button(text("Desemparejar"))
                        .style(style::button(c, ButtonKind::Text))
                        .on_press(Message::Unpair(p.id.clone())),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            ]
            .spacing(10),
        )
        .padding(26)
        .width(Length::Fill)
        .style(style::card(bg, fg, Shape::Hero))
        .into()
    }

    fn send(&self, c: Colors) -> Element<'_, Message> {
        let file_label = self
            .send_file
            .as_ref()
            .and_then(|f| f.file_name())
            .map(|n| n.to_string_lossy().to_string());
        let header = container(
            row![
                shape_icon(
                    Icon::Send,
                    Blob::Clover,
                    c.tertiary,
                    c.on_tertiary,
                    72.0,
                    0.0
                ),
                column![
                    text(
                        file_label
                            .clone()
                            .unwrap_or_else(|| "Ningún archivo".into())
                    )
                    .size(26)
                    .font(EXTRA_BOLD),
                    text("Se envía por Bluetooth a un dispositivo emparejado.").size(14),
                ]
                .width(Length::Fill),
                button(text("Elegir archivo").font(SEMIBOLD))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::PickFile),
            ]
            .spacing(20)
            .align_y(Alignment::Center),
        )
        .padding(26)
        .width(Length::Fill)
        .style(style::card(
            c.tertiary_container,
            c.on_tertiary_container,
            Shape::ActionLeading,
        ));

        let mut col = column![
            self.title(c, "Enviar", "Archivos del PC a tus dispositivos".into()),
            header
        ]
        .spacing(14);
        col = col.push(self.section(c, "Destino"));
        let targets: Vec<&Peer> = self
            .snapshot
            .peers
            .iter()
            .filter(|p| !p.bt_address.is_empty())
            .collect();
        if targets.is_empty() {
            col = col.push(
                text("No hay dispositivos emparejados con dirección Bluetooth conocida.")
                    .color(c.on_surface_variant),
            );
        }
        for p in targets {
            let msg = (file_label.is_some() && self.daemon.is_some()).then(|| Message::SendTo {
                address: p.bt_address.clone(),
                name: p.name.clone(),
            });
            col = col.push(
                container(
                    row![
                        shape_icon(
                            Icon::Bluetooth,
                            Blob::Squircle,
                            c.secondary_container,
                            c.on_secondary_container,
                            46.0,
                            0.0
                        ),
                        column![
                            text(&p.name).size(17).font(SEMIBOLD),
                            text(&p.bt_address).size(13).color(c.on_surface_variant)
                        ]
                        .width(Length::Fill),
                        button(text("Enviar").font(SEMIBOLD))
                            .padding(Padding::from([10, 22]))
                            .style(style::button(c, ButtonKind::Filled))
                            .on_press_maybe(msg),
                    ]
                    .spacing(16)
                    .align_y(Alignment::Center),
                )
                .padding(16)
                .width(Length::Fill)
                .style(style::card(c.container, c.on_surface, Shape::List)),
            );
        }
        col.into()
    }

    fn settings(&self, c: Colors) -> Element<'_, Message> {
        let segments = row(ThemeMode::ALL.iter().enumerate().map(|(i, mode)| {
            let icon = match mode {
                ThemeMode::System => Icon::ThemeSystem,
                ThemeMode::Light => Icon::ThemeLight,
                ThemeMode::Dark => Icon::ThemeDark,
                ThemeMode::Oled => Icon::ThemeOled,
            };
            let selected = self.config.theme == *mode;
            button(
                column![
                    icon.view(22.0, if selected { c.on_primary } else { c.on_surface }),
                    text(mode.label()).size(13).font(SEMIBOLD)
                ]
                .align_x(Alignment::Center)
                .spacing(4),
            )
            .padding(Padding::from([12, 0]))
            .width(Length::Fill)
            .style(style::button(
                c,
                ButtonKind::Segment {
                    selected,
                    first: i == 0,
                    last: i == ThemeMode::ALL.len() - 1,
                },
            ))
            .on_press(Message::SetTheme(*mode))
            .into()
        }))
        .spacing(3);

        let mut theme_card = column![
            text("Tema").size(22).font(BOLD),
            text("OLED usa negro puro: en pantallas OLED los píxeles negros se apagan.")
                .size(13)
                .color(c.on_surface_variant),
            space().height(8),
            segments,
        ]
        .spacing(4);
        if self.config.theme == ThemeMode::System {
            theme_card = theme_card.push(self.switch_row(
                c,
                "Negro puro cuando el sistema esté en oscuro",
                Some("Con el tema oscuro del sistema, kushare usa el OLED."),
                self.config.oled_when_system_dark,
                Message::SetOledWhenSystemDark,
            ));
        }
        theme_card = theme_card.push(self.switch_row(
            c,
            "Animaciones",
            Some("Formas que giran mientras hay conexión o búsqueda."),
            self.config.animations,
            Message::SetAnimations,
        ));

        let daemon_card = {
            let (label, action): (&str, Element<'_, Message>) = match self.state {
                DaemonState::Running => (
                    "Corriendo",
                    button(text("Detener").font(SEMIBOLD))
                        .padding(Padding::from([10, 22]))
                        .style(style::button(c, ButtonKind::Outlined))
                        .on_press(Message::QuitDaemon)
                        .into(),
                ),
                DaemonState::NotRunning => (
                    "Detenido",
                    button(text("Iniciar").font(SEMIBOLD))
                        .padding(Padding::from([10, 22]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press(Message::StartDaemon)
                        .into(),
                ),
                DaemonState::Connecting => ("Conectando…", space().into()),
            };
            row![
                shape_icon(
                    Icon::Power,
                    Blob::Cookie4,
                    c.primary,
                    c.on_primary,
                    48.0,
                    0.0
                ),
                column![
                    text("Daemon de kushare").size(17).font(SEMIBOLD),
                    text(label).size(13).color(c.on_surface_variant),
                ]
                .width(Length::Fill),
                action,
            ]
            .spacing(16)
            .align_y(Alignment::Center)
        };

        let info = |k: &'static str, v: String| {
            row![
                text(k).color(c.on_surface_variant).width(Length::Fill),
                text(v).font(SEMIBOLD)
            ]
            .padding(Padding::from([3, 0]))
        };
        column![
            self.title(c, "Ajustes", "Apariencia y conexión".into()),
            self.section(c, "Apariencia"),
            container(theme_card)
                .padding(26)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::Wide)),
            self.section(c, "Conexión"),
            container(daemon_card)
                .padding(20)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::List)),
            self.section(c, "Acerca de"),
            container(column![
                info("Nombre", self.snapshot.device_name.clone()),
                info("Identificador", self.snapshot.device_id.clone()),
                info("Versión", env!("CARGO_PKG_VERSION").to_string()),
            ])
            .padding(22)
            .width(Length::Fill)
            .style(style::card(c.container_high, c.on_surface, Shape::List)),
        ]
        .spacing(14)
        .into()
    }

    fn dialog<'a>(&self, c: Colors, content: Element<'a, Message>) -> Element<'a, Message> {
        opaque(
            container(center(container(content).padding(32).max_width(520).style(
                style::card(c.container_high, c.on_surface, Shape::Dialog),
            )))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::scrim),
        )
    }

    fn pairing_dialog<'a>(&self, c: Colors, p: &'a Pairing) -> Element<'a, Message> {
        let pin = format!(
            "{} {}",
            &p.pin[..p.pin.len().min(3)],
            &p.pin[p.pin.len().min(3)..]
        );
        self.dialog(
            c,
            column![
                shape_icon(
                    Icon::Devices,
                    Blob::Cookie9,
                    c.primary,
                    c.on_primary,
                    72.0,
                    self.angle
                ),
                text(format!("Emparejar con {}", p.name))
                    .size(26)
                    .font(EXTRA_BOLD),
                text("Comprueba que el otro dispositivo muestra el mismo código")
                    .color(c.on_surface_variant),
                container(text(pin).size(54).font(EXTRA_BOLD))
                    .padding(Padding::from([10, 34]))
                    .style(style::card(
                        c.primary_container,
                        c.on_primary_container,
                        Shape::List
                    )),
                row![
                    button(text("Rechazar").font(SEMIBOLD))
                        .padding(Padding::from([10, 22]))
                        .style(style::button(c, ButtonKind::Text))
                        .on_press(Message::RespondPairing(false)),
                    button(text("Coincide").font(SEMIBOLD))
                        .padding(Padding::from([10, 26]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press(Message::RespondPairing(true)),
                ]
                .spacing(12),
            ]
            .spacing(16)
            .align_x(Alignment::Center)
            .into(),
        )
    }

    fn apps_dialog<'a>(&self, c: Colors, f: &'a AppFilter) -> Element<'a, Message> {
        let title = if f.outgoing {
            format!("Apps que envío a {}", f.peer_name)
        } else {
            format!("Apps que recibo de {}", f.peer_name)
        };
        let list: Element<'a, Message> = match &f.apps {
            None => text("Cargando…").into(),
            Some((local, remote, out_blocked, in_blocked)) => {
                let (apps, blocked) = if f.outgoing {
                    (local, out_blocked)
                } else {
                    (remote, in_blocked)
                };
                if apps.is_empty() {
                    text("Las apps aparecen aquí cuando notifican algo por primera vez.")
                        .color(c.on_surface_variant)
                        .into()
                } else {
                    scrollable(
                        column(apps.iter().map(|(id, name)| {
                            let allowed = !blocked.contains(id);
                            let id = id.clone();
                            checkbox(allowed)
                                .label(name.as_str())
                                .on_toggle(move |on| Message::SetAppBlocked {
                                    app: id.clone(),
                                    blocked: !on,
                                })
                                .size(20)
                                .spacing(12)
                                .into()
                        }))
                        .spacing(10),
                    )
                    .height(Length::Fixed(320.0))
                    .into()
                }
            }
        };
        self.dialog(
            c,
            column![
                text(title).size(24).font(EXTRA_BOLD),
                list,
                row![
                    space().width(Length::Fill),
                    button(text("Listo").font(SEMIBOLD))
                        .padding(Padding::from([10, 26]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press(Message::CloseApps),
                ],
            ]
            .spacing(18)
            .into(),
        )
    }

    fn toast_view<'a>(&self, c: Colors, message: &'a str) -> Element<'a, Message> {
        container(
            button(text(message).size(14).color(c.on_tertiary_container))
                .padding(Padding::from([14, 22]))
                .style(style::button(
                    c,
                    ButtonKind::Card {
                        bg: c.tertiary_container,
                        fg: c.on_tertiary_container,
                        shape: Shape::List,
                    },
                ))
                .on_press(Message::DismissToast),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::End)
        .padding(24)
        .into()
    }
}
