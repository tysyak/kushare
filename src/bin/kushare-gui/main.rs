//! `kushare-gui`: interfaz gráfica de escritorio de kushare (iced), con lo
//! mismo que la app Android: estado del enlace, dispositivos (emparejar con
//! PIN, direcciones de notificaciones, filtros por app), enviar archivos y
//! ajustes de tema (Sistema, Claro, Oscuro, OLED). No hace red por su
//! cuenta: controla al daemon `kushare` por D-Bus.

mod calls;
mod clip;
mod daemon;
mod icons;
mod notifs;
mod sms;
mod style;
mod viewer;

use daemon::{Event, Peer, Snapshot};
use iced::font::Weight;
use iced::widget::{
    button, center, checkbox, column, container, opaque, row, scrollable, space, stack, text,
    toggler,
};
use iced::{Alignment, Color, Element, Font, Length, Padding, Subscription, Task, Theme};
use icons::{Blob, Icon, shape_icon};
use kushare_dbus::{AppsReply, DaemonProxy, OutputRow, UpdateRow};
use kushare_i18n::{t, tf};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// Contenido de botón con icono y texto (el icono del color del texto).
fn labeled<'a>(icon: Icon, label: &'a str, fg: Color) -> Element<'a, Message> {
    row![icon.view(18.0, fg), text(label).font(SEMIBOLD)]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

// --- Estado ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Home,
    Devices,
    Phone,
    Messages,
    Calls,
    Photos,
    Notifications,
    Clipboard,
    Send,
    Settings,
}

/// Galería del teléfono cargada por páginas.
#[derive(Debug, Clone, Default)]
struct Gallery {
    peer: String,
    total: u32,
    photos: Vec<kushare_dbus::PhotoRow>,
    loading: bool,
    /// Hay una consulta de fotos nuevas en curso (no se solapan).
    polling: bool,
    /// Llegó otro aviso de fotos nuevas mientras se consultaba: repetir.
    poll_again: bool,
    /// Fotos recién llegadas → cuándo, para animar su entrada.
    fresh: std::collections::HashMap<String, std::time::Instant>,
    error: Option<String>,
}

const PHOTO_PAGE: u32 = 30;
/// Respaldo: el teléfono avisa al instante de las fotos nuevas
/// (`PhotosChanged`); esto solo cubre avisos perdidos o teléfonos antiguos.
const PHOTO_POLL: Duration = Duration::from_secs(60);
/// Duración de la animación de entrada de una foto nueva.
const PHOTO_FADE: Duration = Duration::from_millis(700);
const PHOTO_HIGHLIGHT: Duration = Duration::from_millis(3000);
/// Cuántas de las más recientes se piden en cada consulta: pocas, para que
/// sea barato incluso por Bluetooth (cada una lleva su miniatura).
const PHOTO_POLL_LIMIT: u32 = 4;

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
    /// Informe de `kushare doctor` (None: no se ha pedido; Some(""): en curso).
    doctor: Option<String>,
    gallery: Gallery,
    sms: sms::SmsState,
    calls: calls::CallsState,
    notifs: notifs::NotifState,
    clip: clip::ClipState,
    viewer: Option<viewer::Viewer>,
    /// Ajustes del daemon: (nombre, activado).
    daemon_settings: Vec<(String, bool)>,
    update: UpdateStatus,
    /// Salidas y entradas de audio de este PC.
    local_audio: (Vec<OutputRow>, Vec<OutputRow>),
    /// Fondo de pantalla de cada teléfono: (ruta o "", colores ARGB).
    wallpapers: HashMap<String, (String, Vec<u32>)>,
}

/// Estado de la búsqueda de actualizaciones (Ajustes).
#[derive(Debug, Clone, Default)]
struct UpdateStatus {
    row: Option<UpdateRow>,
    checking: bool,
    downloading: bool,
    /// Paquete ya descargado y verificado.
    downloaded: Option<String>,
    installing: bool,
    error: Option<String>,
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
    RunDoctor,
    DoctorDone(String),
    CloseDoctor,
    // teléfono
    Media {
        peer: String,
        player: String,
        action: &'static str,
    },
    SetOutput {
        peer: String,
        output: String,
    },
    SetRinger {
        peer: String,
        mode: &'static str,
    },
    CallCommand {
        peer: String,
        action: &'static str,
    },
    PhoneAudio {
        peer: String,
        enable: bool,
    },
    // fotos
    LoadPhotos {
        peer: String,
        more: bool,
    },
    PhotosLoaded {
        peer: String,
        /// Recarga completa: sustituye al mosaico (que sigue visible mientras tanto).
        replace: bool,
        result: Result<(u32, Vec<kushare_dbus::PhotoRow>), String>,
    },
    FetchPhoto {
        peer: String,
        id: String,
    },
    PollPhotos,
    PhotosPolled {
        peer: String,
        /// Cuántas se pidieron (si todas son nuevas, se pide una ventana mayor).
        limit: u32,
        result: Result<(u32, Vec<kushare_dbus::PhotoRow>), String>,
    },
    CachedPhotos {
        peer: String,
        photos: (u32, Vec<kushare_dbus::PhotoRow>),
    },
    Sms(sms::Msg),
    Calls(calls::Msg),
    Notifs(notifs::Msg),
    Clip(clip::Msg),
    Viewer(viewer::Msg),
    /// Abrir *Mensajes* con un SMS nuevo (o la conversación) para este número.
    OpenSmsTo(String),
    // ajustes del daemon y actualizaciones
    SettingsLoaded(Vec<(String, bool)>),
    SetSetting(String, bool),
    CheckUpdate(bool),
    UpdateChecked(Result<UpdateRow, String>),
    DownloadUpdate,
    UpdateDownloaded(Result<String, String>),
    InstallUpdate,
    UpdateInstalled(Result<(), String>),
    SetLanguage(&'static str),
    // audio del PC y fondo de pantalla
    LoadLocalAudio,
    LocalAudioLoaded((Vec<OutputRow>, Vec<OutputRow>)),
    SetLocalAudio {
        input: bool,
        id: String,
    },
    LoadWallpaper(String, bool),
    WallpaperLoaded(String, Result<(String, Vec<u32>), String>),
    Key(iced::keyboard::Key),
    OpenUrl(String),
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let app = Self {
            config: GuiConfig::load(),
            system_dark: true,
            // KUSHARE_GUI_SCREEN=messages|calls|photos|… abre esa página
            // (útil para capturas y pruebas).
            screen: match std::env::var("KUSHARE_GUI_SCREEN").as_deref() {
                Ok("phone") => Screen::Phone,
                Ok("messages") => Screen::Messages,
                Ok("calls") => Screen::Calls,
                Ok("photos") => Screen::Photos,
                Ok("notifications") => Screen::Notifications,
                Ok("clipboard") => Screen::Clipboard,
                Ok("devices") => Screen::Devices,
                Ok("settings") => Screen::Settings,
                _ => Screen::Home,
            },
            daemon: None,
            state: DaemonState::Connecting,
            snapshot: Snapshot::default(),
            bonded_bt: Vec::new(),
            pairing: None,
            app_filter: None,
            send_file: None,
            toast: None,
            angle: 0.0,
            doctor: None,
            gallery: Gallery::default(),
            sms: Default::default(),
            calls: Default::default(),
            notifs: Default::default(),
            clip: Default::default(),
            viewer: None,
            daemon_settings: Vec::new(),
            update: UpdateStatus::default(),
            local_audio: Default::default(),
            wallpapers: HashMap::new(),
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
                || self.pairing.is_some()
                || !self.gallery.fresh.is_empty())
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
        if self.screen == Screen::Photos && !self.gallery.peer.is_empty() && self.gallery.total > 0
        {
            subs.push(iced::time::every(PHOTO_POLL).map(|_| Message::PollPhotos));
        }
        if self.animating() {
            subs.push(iced::time::every(Duration::from_millis(40)).map(|_| Message::Tick));
        }
        if self.viewer.is_some() {
            subs.push(iced::keyboard::listen().filter_map(|event| match event {
                iced::keyboard::Event::KeyPressed { key, .. } => Some(Message::Key(key)),
                _ => None,
            }));
        }
        if self.screen == Screen::Notifications && self.state == DaemonState::Running {
            subs.push(
                iced::time::every(Duration::from_secs(30))
                    .map(|_| Message::Notifs(notifs::Msg::Load)),
            );
        }
        Subscription::batch(subs)
    }

    /// Pide las `limit` fotos más recientes para añadir las nuevas al
    /// principio del mosaico (sin recargarlo).
    fn poll_photos(&mut self, limit: u32) -> Task<Message> {
        let g = &mut self.gallery;
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        if g.peer.is_empty() || g.loading {
            return Task::none();
        }
        if g.polling {
            g.poll_again = true;
            return Task::none();
        }
        g.polling = true;
        let peer = g.peer.clone();
        Task::perform(
            async move {
                let result = d.photos(&peer, 0, limit).await.map_err(|e| e.to_string());
                (peer, result)
            },
            move |(peer, result)| Message::PhotosPolled {
                peer,
                limit,
                result,
            },
        )
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
            return Task::done(Message::Done(Err(t("gui.error.no_daemon").into())));
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
                let screen = self.screen;
                Task::batch([
                    self.refresh(),
                    self.load_settings(),
                    Task::done(Message::CheckUpdate(false)),
                ])
                .chain(Task::done(Message::Navigate(screen)))
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
            Message::Daemon(Event::SmsChanged(peer, thread)) => {
                Task::done(Message::Sms(sms::Msg::Changed(peer, thread)))
            }
            Message::Daemon(Event::NotificationsChanged(peer)) => {
                Task::done(Message::Notifs(notifs::Msg::Changed(peer)))
            }
            Message::Daemon(Event::ClipboardChanged) => {
                if self.screen == Screen::Clipboard {
                    Task::done(Message::Clip(clip::Msg::Load))
                } else {
                    Task::none()
                }
            }
            Message::Daemon(Event::PhotosChanged(peer)) => {
                if self.gallery.peer == peer && self.gallery.total > 0 {
                    Task::done(Message::PollPhotos)
                } else {
                    Task::none()
                }
            }
            Message::Daemon(Event::PairingRequested { id, name, pin }) => {
                self.pairing = Some(Pairing { id, name, pin });
                Task::none()
            }
            Message::Daemon(Event::PairingFinished { id, name, accepted }) => {
                if self.pairing.as_ref().is_some_and(|p| p.id == id) {
                    self.pairing = None;
                }
                self.toast = Some(if accepted {
                    tf("gui.toast.paired", &[("name", &name)])
                } else {
                    tf("gui.toast.not_paired", &[("name", &name)])
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
                self.gallery
                    .fresh
                    .retain(|_, at| at.elapsed() < PHOTO_HIGHLIGHT);
                Task::none()
            }
            Message::Navigate(screen) => {
                self.screen = screen;
                let phone = self.phone_peers().first().map(|p| p.id.clone());
                if screen == Screen::Photos
                    && self.gallery.photos.is_empty()
                    && !self.gallery.loading
                    && let Some(peer) = phone.clone()
                {
                    // Primero la galería de la caché (al instante), luego la real.
                    let Some(d) = self.daemon.clone() else {
                        return Task::none();
                    };
                    let p2 = peer.clone();
                    return Task::batch([Task::perform(
                        async move { d.cached_photos(&p2).await.unwrap_or_default() },
                        move |photos| Message::CachedPhotos {
                            peer: peer.clone(),
                            photos,
                        },
                    )]);
                }
                match screen {
                    Screen::Messages => return Task::done(Message::Sms(sms::Msg::Load)),
                    Screen::Calls => return Task::done(Message::Calls(calls::Msg::Load)),
                    Screen::Notifications => {
                        return Task::done(Message::Notifs(notifs::Msg::Load));
                    }
                    Screen::Clipboard => return Task::done(Message::Clip(clip::Msg::Load)),
                    Screen::Phone => {
                        let mut tasks = vec![Task::done(Message::LoadLocalAudio)];
                        for p in self.phone_peers() {
                            if !self.wallpapers.contains_key(&p.id) {
                                tasks.push(Task::done(Message::LoadWallpaper(p.id.clone(), false)));
                            }
                        }
                        return Task::batch(tasks);
                    }
                    Screen::Settings => {
                        return Task::batch([
                            self.load_settings(),
                            Task::done(Message::CheckUpdate(false)),
                        ]);
                    }
                    _ => {}
                }
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
            Message::Unpair(peer) => self.call(t("gui.toast.unpaired"), move |d| async move {
                d.unpair(&peer).await
            }),
            Message::Scan => {
                self.snapshot.scanning = true;
                self.call("", |d| async move { d.scan(20).await })
            }
            Message::Pair(target) => self.call(t("gui.toast.waiting_pin"), move |d| async move {
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
                    tf("gui.toast.sending", &[("file", &label), ("name", &name)]),
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
            Message::QuitDaemon => {
                self.call(
                    t("gui.toast.daemon_stopped"),
                    |d| async move { d.quit().await },
                )
            }
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
            Message::RunDoctor => {
                self.doctor = Some(String::new());
                Task::perform(daemon::doctor(), Message::DoctorDone)
            }
            Message::DoctorDone(report) => {
                self.doctor = Some(report);
                Task::none()
            }
            Message::CloseDoctor => {
                self.doctor = None;
                Task::none()
            }
            Message::Media {
                peer,
                player,
                action,
            } => self.call("", move |d| async move {
                d.media_command(&peer, &player, action).await
            }),
            Message::SetOutput { peer, output } => self.call("", move |d| async move {
                d.set_audio_output(&peer, &output).await
            }),
            Message::SetRinger { peer, mode } => {
                self.call("", move |d| async move { d.set_ringer(&peer, mode).await })
            }
            Message::CallCommand { peer, action } => {
                self.call(
                    "",
                    move |d| async move { d.call_command(&peer, action).await },
                )
            }
            Message::PhoneAudio { peer, enable } => {
                let Some(d) = self.daemon.clone() else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        d.phone_audio(&peer, enable)
                            .await
                            .map(|p| {
                                if enable {
                                    tf(
                                        "gui.toast.phone_audio_on",
                                        &[("profiles", &p.join(t("gui.toast.and")))],
                                    )
                                } else {
                                    t("gui.toast.phone_audio_off").to_string()
                                }
                            })
                            .map_err(|e| e.to_string())
                    },
                    Message::Done,
                )
            }
            Message::LoadPhotos { peer, more } => {
                let Some(d) = self.daemon.clone() else {
                    return Task::none();
                };
                if self.gallery.peer != peer {
                    self.gallery = Gallery {
                        peer: peer.clone(),
                        ..Default::default()
                    };
                }
                if self.gallery.loading {
                    return Task::none();
                }
                // Recarga completa: el mosaico actual sigue visible hasta que
                // llega el nuevo (nada de pantalla vacía).
                let replace = !more;
                self.gallery.loading = true;
                let (offset, limit) = if replace {
                    (0, (self.gallery.photos.len() as u32).max(PHOTO_PAGE))
                } else {
                    (self.gallery.photos.len() as u32, PHOTO_PAGE)
                };
                Task::perform(
                    async move {
                        let result = d
                            .photos(&peer, offset, limit)
                            .await
                            .map_err(|e| e.to_string());
                        (peer, result)
                    },
                    move |(peer, result)| Message::PhotosLoaded {
                        peer,
                        replace,
                        result,
                    },
                )
            }
            Message::PhotosLoaded {
                peer,
                replace,
                result,
            } => {
                if self.gallery.peer == peer {
                    self.gallery.loading = false;
                    match result {
                        Ok((total, photos)) => {
                            self.gallery.total = total;
                            if replace {
                                self.gallery.photos = photos;
                            } else {
                                self.gallery.photos.extend(photos);
                            }
                            self.gallery.error = None;
                        }
                        Err(err) => self.gallery.error = Some(err),
                    }
                }
                Task::none()
            }
            Message::PollPhotos => self.poll_photos(PHOTO_POLL_LIMIT),
            Message::PhotosPolled {
                peer,
                limit,
                result,
            } => {
                let g = &mut self.gallery;
                if g.peer != peer {
                    return Task::none();
                }
                g.polling = false;
                // Un fallo puntual (teléfono fuera de alcance) no borra nada.
                let Ok((total, latest)) = result else {
                    return Task::none();
                };
                let known: std::collections::HashSet<&str> =
                    g.photos.iter().map(|p| p.0.as_str()).collect();
                let new: Vec<_> = latest
                    .iter()
                    .filter(|p| !known.contains(p.0.as_str()))
                    .cloned()
                    .collect();
                if new.len() == latest.len() && !latest.is_empty() && !g.photos.is_empty() {
                    // Todas las consultadas son nuevas: puede haber más. Pedir
                    // una ventana mayor; si tampoco alcanza, recargar entero.
                    return if limit < PHOTO_PAGE {
                        self.poll_photos(PHOTO_PAGE)
                    } else {
                        Task::done(Message::LoadPhotos { peer, more: false })
                    };
                }
                if !new.is_empty() {
                    // Las nuevas van al principio, animadas; las cargadas se
                    // conservan y t("gui.photos.load_more") sigue cuadrando (todo se desplazó igual).
                    let now = std::time::Instant::now();
                    for p in &new {
                        g.fresh.insert(p.0.clone(), now);
                    }
                    self.toast = Some(match new.len() {
                        1 => t("gui.photos.new_one").to_string(),
                        n => tf("gui.photos.new_many", &[("n", &n)]),
                    });
                    let mut photos = new;
                    photos.append(&mut g.photos);
                    g.photos = photos;
                }
                g.total = total;
                if std::mem::take(&mut g.poll_again) {
                    return self.poll_photos(PHOTO_POLL_LIMIT);
                }
                Task::none()
            }
            Message::FetchPhoto { peer, id } => {
                if self.gallery.peer != peer {
                    return Task::none();
                }
                match self.gallery.photos.iter().position(|p| p.0 == id) {
                    Some(index) => Task::done(Message::Viewer(viewer::Msg::Open(index))),
                    None => Task::none(),
                }
            }
            Message::CachedPhotos { peer, photos } => {
                if self.gallery.peer != peer {
                    self.gallery = Gallery {
                        peer: peer.clone(),
                        ..Default::default()
                    };
                }
                if self.gallery.photos.is_empty() {
                    self.gallery.total = photos.0;
                    self.gallery.photos = photos.1;
                }
                Task::done(Message::LoadPhotos { peer, more: false })
            }
            Message::Sms(m) => self.sms_update(m),
            Message::Calls(m) => self.calls_update(m),
            Message::Notifs(m) => self.notifs_update(m),
            Message::Clip(m) => self.clip_update(m),
            Message::Viewer(m) => self.viewer_update(m),
            Message::Key(key) => {
                use iced::keyboard::{Key, key::Named};
                let msg = match key {
                    Key::Named(Named::ArrowLeft) => viewer::Msg::Prev,
                    Key::Named(Named::ArrowRight) => viewer::Msg::Next,
                    Key::Named(Named::Escape) => viewer::Msg::Close,
                    _ => return Task::none(),
                };
                self.viewer_update(msg)
            }
            Message::OpenSmsTo(number) => {
                self.screen = Screen::Messages;
                let wanted = calls_digits(&number);
                let thread = self
                    .sms
                    .threads
                    .iter()
                    .find(|t| !wanted.is_empty() && calls_digits(&t.1).ends_with(&wanted))
                    .map(|t| t.0.clone());
                let open = match thread {
                    Some(thread) => Task::done(Message::Sms(sms::Msg::Open(thread))),
                    None => Task::batch([
                        Task::done(Message::Sms(sms::Msg::New)),
                        Task::done(Message::Sms(sms::Msg::NewTo(number))),
                    ]),
                };
                if self.sms.threads.is_empty() {
                    Task::batch([Task::done(Message::Sms(sms::Msg::Load)), open])
                } else {
                    open
                }
            }
            Message::OpenUrl(url) => {
                if url.starts_with("https://") {
                    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
                }
                Task::none()
            }
            Message::SettingsLoaded(list) => {
                self.daemon_settings = list;
                Task::none()
            }
            Message::SetSetting(name, on) => {
                if let Some(slot) = self.daemon_settings.iter_mut().find(|(n, _)| *n == name) {
                    slot.1 = on;
                }
                if name == "clipboard-history" && !on {
                    self.clip.history.clear();
                }
                self.call("", move |d| async move { d.set_setting(&name, on).await })
            }
            Message::CheckUpdate(force) => {
                let Some(d) = self.daemon.clone() else {
                    return Task::none();
                };
                if self.update.checking {
                    return Task::none();
                }
                self.update.checking = true;
                Task::perform(
                    async move { d.check_update(force).await.map_err(err_text) },
                    Message::UpdateChecked,
                )
            }
            Message::UpdateChecked(result) => {
                self.update.checking = false;
                match result {
                    Ok(row) => {
                        self.update.row = Some(row);
                        self.update.error = None;
                    }
                    Err(e) => self.update.error = Some(e),
                }
                Task::none()
            }
            Message::DownloadUpdate => {
                let Some(d) = self.daemon.clone() else {
                    return Task::none();
                };
                self.update.downloading = true;
                self.update.error = None;
                Task::perform(
                    async move { d.download_update().await.map_err(err_text) },
                    Message::UpdateDownloaded,
                )
            }
            Message::UpdateDownloaded(result) => {
                self.update.downloading = false;
                match result {
                    Ok(path) => self.update.downloaded = Some(path),
                    Err(e) => self.update.error = Some(e),
                }
                Task::none()
            }
            Message::InstallUpdate => {
                let Some(path) = self.update.downloaded.clone() else {
                    return Task::none();
                };
                self.update.installing = true;
                Task::perform(install_package(path), Message::UpdateInstalled)
            }
            Message::UpdateInstalled(result) => {
                self.update.installing = false;
                match result {
                    Ok(()) => {
                        self.update.downloaded = None;
                        self.toast = Some(t("gui.update.installed").into());
                    }
                    Err(e) => self.update.error = Some(e),
                }
                Task::none()
            }
            Message::SetLanguage(lang) => {
                kushare_i18n::set_saved_language(lang);
                self.toast = Some(t("gui.settings.language_restart").into());
                Task::none()
            }
            Message::LoadLocalAudio => match self.daemon.clone() {
                Some(d) => Task::perform(
                    async move { d.local_audio().await.unwrap_or_default() },
                    Message::LocalAudioLoaded,
                ),
                None => Task::none(),
            },
            Message::LocalAudioLoaded(audio) => {
                self.local_audio = audio;
                Task::none()
            }
            Message::SetLocalAudio { input, id } => {
                let list = if input {
                    &mut self.local_audio.1
                } else {
                    &mut self.local_audio.0
                };
                for o in list.iter_mut() {
                    o.2 = o.0 == id;
                }
                let kind = if input { "input" } else { "output" };
                let Some(d) = self.daemon.clone() else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        let r = d.set_local_audio(kind, &id).await.map_err(err_text);
                        (r, d.local_audio().await.unwrap_or_default())
                    },
                    |(r, audio)| match r {
                        Ok(()) => Message::LocalAudioLoaded(audio),
                        Err(e) => Message::Done(Err(e)),
                    },
                )
            }
            Message::LoadWallpaper(peer, refresh) => match self.daemon.clone() {
                Some(d) => Task::perform(
                    async move {
                        let r = d.wallpaper(&peer, refresh).await.map_err(err_text);
                        (peer, r)
                    },
                    |(peer, r)| Message::WallpaperLoaded(peer, r),
                ),
                None => Task::none(),
            },
            Message::WallpaperLoaded(peer, result) => {
                match result {
                    Ok(w) => {
                        self.wallpapers.insert(peer, w);
                    }
                    Err(e) => tracing::debug!(%e, "sin fondo de pantalla"),
                }
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
            Screen::Phone => self.phone(c),
            Screen::Messages => self.sms_view(c),
            Screen::Calls => self.calls_view(c),
            Screen::Photos => self.photos(c),
            Screen::Notifications => self.notifs_view(c),
            Screen::Clipboard => self.clip_view(c),
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
        if let Some(p) = &self.pairing {
            layers = layers.push(self.pairing_dialog(c, p));
        }
        if let Some(f) = &self.app_filter {
            layers = layers.push(self.apps_dialog(c, f));
        }
        if let Some(report) = &self.doctor {
            layers = layers.push(self.doctor_dialog(c, report));
        }
        if let Some(v) = &self.viewer {
            layers = layers.push(self.viewer_view(c, v));
        }
        if let Some(t) = &self.toast {
            layers = layers.push(self.toast_view(c, t));
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
                        .font(if selected { BOLD } else { Font::DEFAULT })
                        .align_x(iced::alignment::Horizontal::Center)
                        .width(Length::Fill),
                ]
                .align_x(Alignment::Center)
                .width(Length::Fill)
                .spacing(4),
            )
            // Ancho para la etiqueta más larga ("Dispositivos") sin que
            // descentre el icono.
            .padding(Padding::from([7, 4]))
            .width(Length::Fixed(104.0))
            .style(style::button(c, ButtonKind::Nav(selected)))
            .on_press(Message::Navigate(screen))
        };
        container(
            scrollable(
                column![
                    text("ku").size(26).font(EXTRA_BOLD).color(c.primary),
                    space().height(8),
                    item(Icon::Home, t("gui.nav.home"), Screen::Home),
                    item(Icon::Phone, t("gui.nav.phone"), Screen::Phone),
                    item(Icon::Message, t("gui.nav.messages"), Screen::Messages),
                    item(Icon::Dialpad, t("gui.nav.calls"), Screen::Calls),
                    item(Icon::Photo, t("gui.nav.photos"), Screen::Photos),
                    item(
                        Icon::Notifications,
                        t("gui.nav.notifications"),
                        Screen::Notifications
                    ),
                    item(Icon::Clipboard, t("gui.nav.clipboard"), Screen::Clipboard),
                    item(Icon::Send, t("gui.nav.send"), Screen::Send),
                    item(Icon::Devices, t("gui.nav.devices"), Screen::Devices),
                    item(Icon::Settings, t("gui.nav.settings"), Screen::Settings),
                ]
                .spacing(4)
                .align_x(Alignment::Center),
            )
            // Desplazable para ventanas bajas, sin barra visible.
            .direction(iced::widget::scrollable::Direction::Vertical(
                iced::widget::scrollable::Scrollbar::hidden(),
            )),
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
                t("gui.home.starting"),
                t("gui.home.connecting_daemon").to_string(),
                Icon::Schedule,
                Blob::SoftBurst,
                true,
                false,
            ),
            DaemonState::NotRunning => (
                t("gui.home.off"),
                t("gui.home.off_detail").to_string(),
                Icon::Power,
                Blob::Circle,
                false,
                false,
            ),
            DaemonState::Running if !connected.is_empty() => (
                t("gui.home.connected"),
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
                t("gui.home.unpaired"),
                t("gui.home.unpaired_detail").to_string(),
                Icon::Devices,
                Blob::Cookie4,
                false,
                false,
            ),
            DaemonState::Running => (
                t("gui.home.waiting"),
                t("gui.home.waiting_detail").to_string(),
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
            DaemonState::NotRunning => {
                button(labeled(Icon::PlayCircle, t("gui.home.start"), c.on_primary))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::StartDaemon)
                    .into()
            }
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
                t("gui.nav.send"),
                t("gui.home.send_detail"),
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
                t("gui.home.received"),
                t("gui.home.received_detail"),
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
                    text(t("gui.home.devices_title")).size(22).font(BOLD),
                    text(match self.snapshot.peers.len() {
                        0 => t("gui.home.devices_none").to_string(),
                        1 => t("gui.home.devices_one").to_string(),
                        n => tf("gui.home.devices_many", &[("n", &n)]),
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
            t("gui.nav.devices"),
            if running {
                tf(
                    "gui.devices.connected_of",
                    &[
                        (
                            "connected",
                            &self.snapshot.peers.iter().filter(|p| p.connected).count(),
                        ),
                        ("total", &self.snapshot.peers.len()),
                    ],
                )
            } else {
                t("gui.devices.no_daemon").to_string()
            },
        )]
        .spacing(14);

        col = col.push(self.section(c, t("gui.devices.paired")));
        if self.snapshot.peers.is_empty() {
            col = col.push(text(t("gui.devices.none_yet")).color(c.on_surface_variant));
        }
        for p in &self.snapshot.peers {
            col = col.push(self.peer_card(c, p));
        }

        let scan_label = if self.snapshot.scanning {
            t("gui.devices.searching")
        } else {
            t("gui.devices.search")
        };
        col = col.push(
            row![
                container(self.section(c, t("gui.devices.on_network"))).width(Length::Fill),
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
                    t("gui.devices.searching_detail")
                } else {
                    t("gui.devices.search_hint")
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

        col = col.push(self.section(c, t("gui.devices.by_bluetooth")));
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
            col = col.push(text(t("gui.devices.no_bonded")).color(c.on_surface_variant));
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
                button(labeled(Icon::Check, t("gui.devices.pair"), c.on_primary))
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
                    tf("gui.devices.connected_via", &[("via", &p.via())])
                } else {
                    t("gui.devices.disconnected").to_string()
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
                    text(t("gui.devices.send_file")).font(SEMIBOLD)
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
                    t("gui.devices.receive_notifications"),
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
                    t("gui.devices.send_notifications"),
                    None,
                    p.outgoing,
                    move |on| Message::SetDirection {
                        peer: id2.clone(),
                        outgoing: true,
                        enabled: on
                    }
                ),
                row![
                    button(labeled(Icon::Apps, t("gui.devices.apps_in"), c.primary))
                        .padding(Padding::from([9, 18]))
                        .style(style::button(c, ButtonKind::Outlined))
                        .on_press(Message::OpenApps {
                            peer: p.id.clone(),
                            name: p.name.clone(),
                            outgoing: false
                        }),
                    button(labeled(Icon::Apps, t("gui.devices.apps_out"), c.primary))
                        .padding(Padding::from([9, 18]))
                        .style(style::button(c, ButtonKind::Outlined))
                        .on_press(Message::OpenApps {
                            peer: p.id.clone(),
                            name: p.name.clone(),
                            outgoing: true
                        }),
                    send_button,
                    space().width(Length::Fill),
                    button(labeled(Icon::LinkOff, t("gui.devices.unpair"), c.primary))
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

    /// Peers conectados que son teléfonos (con funciones de teléfono).
    fn phone_peers(&self) -> Vec<&Peer> {
        self.snapshot
            .peers
            .iter()
            .filter(|p| p.connected && p.kind == "android")
            .collect()
    }

    fn empty_phone<'a>(&self, c: Colors, what: &'a str) -> Element<'a, Message> {
        container(
            column![
                shape_icon(
                    Icon::Phone,
                    Blob::Circle,
                    c.container_highest,
                    c.on_surface_variant,
                    72.0,
                    0.0
                ),
                text(t("gui.phone.none")).size(22).font(EXTRA_BOLD),
                text(what).size(14).color(c.on_surface_variant),
            ]
            .spacing(10)
            .align_x(Alignment::Center),
        )
        .padding(40)
        .width(Length::Fill)
        .center_x(Length::Fill)
        .style(style::card(c.container_high, c.on_surface, Shape::Hero))
        .into()
    }

    fn icon_button<'a>(
        &self,
        c: Colors,
        icon: Icon,
        kind: ButtonKind,
        msg: Option<Message>,
    ) -> Element<'a, Message> {
        let fg = match kind {
            ButtonKind::Filled => c.on_primary,
            _ => c.on_secondary_container,
        };
        button(icon.view(22.0, fg))
            .padding(10)
            .style(style::button(c, kind))
            .on_press_maybe(msg)
            .into()
    }

    fn phone(&self, c: Colors) -> Element<'_, Message> {
        let phones = self.phone_peers();
        let mut col =
            column![self.title(c, t("gui.nav.phone"), t("gui.phone.subtitle").into())].spacing(16);
        if phones.is_empty() {
            return col
                .push(self.empty_phone(c, t("gui.phone.none_detail")))
                .push(self.local_audio_card(c))
                .into();
        }
        for p in phones {
            col = col.push(self.phone_card(c, p));
        }
        col.push(self.local_audio_card(c)).into()
    }

    /// Cabecera de la tarjeta del teléfono sobre su fondo de pantalla (o un
    /// degradado con sus colores, si Android no deja leer la imagen).
    fn wallpaper_banner<'a>(
        &self,
        c: Colors,
        peer: &str,
        content: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let Some((path, colors)) = self.wallpapers.get(peer) else {
            return content;
        };
        let argb = |v: u32| Color::from_rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8);
        let fg = Color::WHITE;
        let content = container(content)
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fixed(180.0))
            .align_y(Alignment::End)
            .style(move |_: &Theme| container::Style {
                text_color: Some(fg),
                background: Some(
                    iced::gradient::Linear::new(0.0)
                        .add_stop(
                            0.0,
                            Color {
                                a: 0.75,
                                ..Color::BLACK
                            },
                        )
                        .add_stop(
                            0.6,
                            Color {
                                a: 0.0,
                                ..Color::BLACK
                            },
                        )
                        .into(),
                ),
                border: iced::Border {
                    radius: 22.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        let backdrop: Element<'a, Message> = if !path.is_empty() {
            iced::widget::image(iced::widget::image::Handle::from_path(path))
                .width(Length::Fill)
                .height(Length::Fixed(180.0))
                .content_fit(iced::ContentFit::Cover)
                .border_radius(22.0)
                .into()
        } else if !colors.is_empty() {
            let a = argb(colors[0]);
            let b = colors.get(1).map_or(c.primary, |v| argb(*v));
            container(space())
                .width(Length::Fill)
                .height(Length::Fixed(180.0))
                .style(move |_: &Theme| container::Style {
                    background: Some(
                        iced::gradient::Linear::new(0.8)
                            .add_stop(0.0, a)
                            .add_stop(1.0, b)
                            .into(),
                    ),
                    border: iced::Border {
                        radius: 22.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into()
        } else {
            return content.into();
        };
        stack![backdrop, content].into()
    }

    /// Salidas y entradas (micrófono, p. ej. para las llamadas) de este PC.
    fn local_audio_card(&self, c: Colors) -> Element<'_, Message> {
        let (outputs, inputs) = &self.local_audio;
        let list = |rows: &[OutputRow], input: bool, icon: Icon| {
            let mut col = column![].spacing(2);
            for (id, name, active) in rows {
                col = col.push(
                    button(
                        row![
                            icon.view(
                                18.0,
                                if *active {
                                    c.primary
                                } else {
                                    c.on_surface_variant
                                }
                            ),
                            text(name.clone()).size(15).font(if *active {
                                BOLD
                            } else {
                                Font::DEFAULT
                            }),
                            space().width(Length::Fill),
                            if *active {
                                Icon::Check.view(18.0, c.primary)
                            } else {
                                space().into()
                            },
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::from([8, 14]))
                    .width(Length::Fill)
                    .style(style::button(c, ButtonKind::Text))
                    .on_press_maybe((!active).then(|| {
                        Message::SetLocalAudio {
                            input,
                            id: id.clone(),
                        }
                    })),
                );
            }
            if rows.is_empty() {
                col = col.push(
                    text(t("gui.audio.none"))
                        .size(13)
                        .color(c.on_surface_variant),
                );
            }
            col
        };
        container(
            column![
                row![
                    text(t("gui.audio.title"))
                        .size(22)
                        .font(BOLD)
                        .width(Length::Fill),
                    button(Icon::Refresh.view(18.0, c.on_surface_variant))
                        .padding(8)
                        .style(style::button(c, ButtonKind::Text))
                        .on_press(Message::LoadLocalAudio),
                ]
                .align_y(Alignment::Center),
                text(t("gui.audio.detail"))
                    .size(13)
                    .color(c.on_surface_variant),
                self.section(c, t("gui.audio.outputs")),
                list(outputs, false, Icon::Volume),
                self.section(c, t("gui.audio.inputs")),
                list(inputs, true, Icon::Mic),
            ]
            .spacing(8),
        )
        .padding(26)
        .width(Length::Fill)
        .style(style::card(c.container_high, c.on_surface, Shape::Wide))
        .into()
    }

    fn phone_card<'a>(&'a self, c: Colors, p: &'a Peer) -> Element<'a, Message> {
        let Some((battery, ringer, call, players, selectable, outputs)) =
            self.snapshot.states.get(&p.id)
        else {
            return text(t("gui.common.loading")).into();
        };
        let peer = p.id.clone();
        // Cabecera: nombre y batería.
        let battery_text = if battery.0 {
            format!(
                "{} %{}",
                battery.1,
                if battery.2 {
                    t("gui.phone.charging_suffix")
                } else {
                    ""
                }
            )
        } else {
            t("gui.phone.battery_unknown").into()
        };
        let low = battery.0 && battery.1 <= 15 && !battery.2;
        let header_row = row![
            shape_icon(
                Icon::Battery,
                Blob::Cookie9,
                if low { c.error } else { c.primary },
                c.on_primary,
                64.0,
                0.0
            ),
            column![
                text(&p.name).size(26).font(EXTRA_BOLD),
                text(battery_text).size(15),
            ]
            .width(Length::Fill),
            button(Icon::Refresh.view(18.0, c.on_surface))
                .padding(8)
                .style(style::button(c, ButtonKind::Text))
                .on_press(Message::LoadWallpaper(p.id.clone(), true)),
        ]
        .spacing(18)
        .align_y(Alignment::Center);
        let header = self.wallpaper_banner(c, &p.id, header_row.into());

        let mut body = column![header].spacing(14);

        // Llamada en curso.
        if call.0 != "idle" {
            let who = if call.2.is_empty() {
                call.1.clone()
            } else {
                call.2.clone()
            };
            let label = match call.0.as_str() {
                "ringing" => tf("gui.phone.call_incoming", &[("who", &who)]),
                "dialing" => tf("gui.phone.call_dialing", &[("who", &who)]),
                _ => tf("gui.phone.call_active", &[("who", &who)]),
            };
            let mut buttons = row![].spacing(10);
            if call.0 == "ringing" {
                buttons = buttons.push(
                    button(labeled(Icon::Phone, t("gui.phone.answer"), c.on_primary))
                        .padding(Padding::from([9, 18]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press(Message::CallCommand {
                            peer: peer.clone(),
                            action: "answer",
                        }),
                );
                buttons = buttons.push(
                    button(labeled(
                        Icon::Close,
                        t("gui.phone.reject"),
                        c.on_secondary_container,
                    ))
                    .padding(Padding::from([9, 18]))
                    .style(style::button(c, ButtonKind::Tonal))
                    .on_press(Message::CallCommand {
                        peer: peer.clone(),
                        action: "reject",
                    }),
                );
            } else {
                buttons = buttons.push(
                    button(labeled(Icon::CallEnd, t("gui.phone.hangup"), c.on_primary))
                        .padding(Padding::from([9, 18]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press(Message::CallCommand {
                            peer: peer.clone(),
                            action: "hangup",
                        }),
                );
                buttons = buttons.push(
                    button(labeled(
                        if call.3 { Icon::Mic } else { Icon::MicOff },
                        if call.3 {
                            t("gui.phone.unmute")
                        } else {
                            t("gui.phone.mute")
                        },
                        c.on_secondary_container,
                    ))
                    .padding(Padding::from([9, 18]))
                    .style(style::button(c, ButtonKind::Tonal))
                    .on_press(Message::CallCommand {
                        peer: peer.clone(),
                        action: if call.3 { "unmute" } else { "mute" },
                    }),
                );
            }
            body = body.push(
                container(
                    column![
                        row![
                            Icon::Phone.view(20.0, c.on_tertiary_container),
                            text(label).size(18).font(BOLD)
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                        buttons
                    ]
                    .spacing(10),
                )
                .padding(18)
                .width(Length::Fill)
                .style(style::card(
                    c.tertiary_container,
                    c.on_tertiary_container,
                    Shape::List,
                )),
            );
        }

        // Modo de timbre (grupo de botones conectados).
        if !ringer.is_empty() {
            let modes = [
                ("normal", Icon::Volume, t("gui.phone.ringer_normal")),
                ("vibrate", Icon::Vibration, t("gui.phone.ringer_vibrate")),
                ("silent", Icon::VolumeOff, t("gui.phone.ringer_silent")),
            ];
            let mut seg = row![].spacing(2);
            for (i, (mode, icon, label)) in modes.iter().enumerate() {
                seg = seg.push(
                    button(labeled(
                        *icon,
                        label,
                        if ringer == mode {
                            c.on_primary
                        } else {
                            c.on_surface
                        },
                    ))
                    .padding(Padding::from([9, 18]))
                    .style(style::button(
                        c,
                        ButtonKind::Segment {
                            selected: ringer == mode,
                            first: i == 0,
                            last: i == modes.len() - 1,
                        },
                    ))
                    .on_press(Message::SetRinger {
                        peer: peer.clone(),
                        mode,
                    }),
                );
            }
            body = body.push(self.section(c, t("gui.phone.ringer"))).push(seg);
        }

        // Reproductores.
        body = body.push(
            row![
                self.section(c, t("gui.phone.media")),
                space().width(Length::Fill),
                button(labeled(Icon::Pause, t("gui.phone.pause_all"), c.primary))
                    .padding(Padding::from([7, 16]))
                    .style(style::button(c, ButtonKind::Outlined))
                    .on_press_maybe(players.iter().any(|p| p.5 == "playing").then(|| {
                        Message::Media {
                            peer: peer.clone(),
                            player: String::new(),
                            action: "pause",
                        }
                    })),
            ]
            .align_y(Alignment::Center),
        );
        if players.is_empty() {
            body = body.push(
                text(t("gui.phone.nothing_playing"))
                    .size(14)
                    .color(c.on_surface_variant),
            );
        }
        for pl in players {
            let playing = pl.5 == "playing";
            let media = |action: &'static str| Message::Media {
                peer: peer.clone(),
                player: pl.0.clone(),
                action,
            };
            let what = [pl.2.as_str(), pl.3.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" — ");
            body = body.push(
                container(
                    row![
                        column![
                            text(&pl.1).size(13).color(c.on_surface_variant),
                            text(if what.is_empty() {
                                "—".to_string()
                            } else {
                                what
                            })
                            .size(17)
                            .font(SEMIBOLD),
                        ]
                        .width(Length::Fill),
                        self.icon_button(
                            c,
                            Icon::Previous,
                            ButtonKind::Tonal,
                            pl.9.then(|| media("previous"))
                        ),
                        self.icon_button(
                            c,
                            if playing { Icon::Pause } else { Icon::Play },
                            ButtonKind::Filled,
                            Some(media("play-pause")),
                        ),
                        self.icon_button(
                            c,
                            Icon::Next,
                            ButtonKind::Tonal,
                            pl.8.then(|| media("next"))
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([12, 18]))
                .width(Length::Fill)
                .style(style::card(c.container_highest, c.on_surface, Shape::List)),
            );
        }

        // Audio del teléfono en el PC (Bluetooth A2DP + HFP).
        if !p.bt_address.is_empty() {
            body = body.push(self.section(c, t("gui.phone.audio_here"))).push(
                row![
                    text(t("gui.phone.audio_here_detail"))
                        .size(14)
                        .color(c.on_surface_variant)
                        .width(Length::Fill),
                    button(labeled(
                        Icon::Headphones,
                        t("gui.phone.listen_here"),
                        c.on_secondary_container
                    ))
                    .padding(Padding::from([9, 18]))
                    .style(style::button(c, ButtonKind::Tonal))
                    .on_press(Message::PhoneAudio {
                        peer: peer.clone(),
                        enable: true
                    }),
                    button(labeled(Icon::Undo, t("gui.phone.give_back"), c.primary))
                        .style(style::button(c, ButtonKind::Text))
                        .on_press(Message::PhoneAudio {
                            peer: peer.clone(),
                            enable: false
                        }),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }

        // Salidas de audio del teléfono (informativo si no se pueden elegir).
        if !outputs.is_empty() {
            body = body.push(self.section(c, t("gui.phone.output")));
            for (id, name, active) in outputs {
                let msg = (*selectable && !active).then(|| Message::SetOutput {
                    peer: peer.clone(),
                    output: id.clone(),
                });
                body = body.push(
                    button(
                        row![
                            Icon::Volume.view(
                                18.0,
                                if *active {
                                    c.primary
                                } else {
                                    c.on_surface_variant
                                }
                            ),
                            text(name)
                                .size(15)
                                .font(if *active { BOLD } else { Font::DEFAULT }),
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::from([8, 14]))
                    .width(Length::Fill)
                    .style(style::button(c, ButtonKind::Text))
                    .on_press_maybe(msg),
                );
            }
            if !selectable {
                body = body.push(
                    text(t("gui.phone.output_readonly"))
                        .size(13)
                        .color(c.on_surface_variant),
                );
            }
        }

        container(body)
            .padding(26)
            .width(Length::Fill)
            .style(style::card(c.container_high, c.on_surface, Shape::Hero))
            .into()
    }

    fn photos(&self, c: Colors) -> Element<'_, Message> {
        let phones = self.phone_peers();
        let g = &self.gallery;
        let subtitle = if g.total > 0 {
            tf(
                "gui.photos.count",
                &[("shown", &g.photos.len()), ("total", &g.total)],
            )
        } else {
            t("gui.photos.subtitle").into()
        };
        let mut col = column![self.title(c, t("gui.nav.photos"), subtitle)].spacing(16);
        let Some(phone) = phones.iter().find(|p| p.id == g.peer).or(phones.first()) else {
            return col
                .push(self.empty_phone(c, t("gui.photos.none_detail")))
                .into();
        };
        if g.peer != phone.id && !g.loading {
            col = col.push(
                button(text(tf("gui.photos.view_of", &[("name", &phone.name)])).font(SEMIBOLD))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::LoadPhotos {
                        peer: phone.id.clone(),
                        more: false,
                    }),
            );
            return col.into();
        }
        if let Some(err) = &g.error {
            col = col.push(text(err).color(c.error));
        }
        col = col.push(self.photos_toolbar(c));
        const COLUMNS: usize = 5;
        for chunk in g.photos.chunks(COLUMNS) {
            let mut r = row![].spacing(10);
            for (id, _name, _date, _w, _h, _size, thumb, _original) in chunk {
                // Entrada animada de las fotos nuevas: aparecen (opacidad) y
                // llevan un borde del color primario que se desvanece.
                let age = g.fresh.get(id).map(|at| at.elapsed());
                let opacity = age.map_or(1.0, |a| {
                    (a.as_secs_f32() / PHOTO_FADE.as_secs_f32()).clamp(0.0, 1.0)
                });
                let glow = age.map_or(0.0, |a| {
                    1.0 - (a.as_secs_f32() / PHOTO_HIGHLIGHT.as_secs_f32()).clamp(0.0, 1.0)
                });
                let tile = container(
                    iced::widget::image(iced::widget::image::Handle::from_path(thumb))
                        .width(Length::Fill)
                        .height(Length::Fixed(150.0))
                        .content_fit(iced::ContentFit::Cover)
                        .opacity(opacity),
                )
                .padding(if glow > 0.0 { 3 } else { 0 })
                .style(move |_: &Theme| container::Style {
                    border: iced::Border {
                        color: Color {
                            a: glow,
                            ..c.primary
                        },
                        width: if glow > 0.0 { 3.0 } else { 0.0 },
                        radius: 14.0.into(),
                    },
                    ..Default::default()
                });
                r = r.push(
                    button(tile)
                        .padding(0)
                        .width(Length::FillPortion(1))
                        .style(style::button(
                            c,
                            ButtonKind::Card {
                                bg: c.container_high,
                                fg: c.on_surface,
                                shape: Shape::List,
                            },
                        ))
                        .on_press(Message::FetchPhoto {
                            peer: g.peer.clone(),
                            id: id.clone(),
                        }),
                );
            }
            for _ in chunk.len()..COLUMNS {
                r = r.push(space().width(Length::FillPortion(1)));
            }
            col = col.push(r);
        }
        if g.photos.is_empty() && g.loading {
            col = col.push(text(t("gui.common.loading")).color(c.on_surface_variant));
        }
        // "Cargar más" también al final, para no tener que volver arriba.
        if !g.loading && (g.photos.len() as u32) < g.total {
            col = col.push(
                container(
                    button(labeled(
                        Icon::ExpandMore,
                        t("gui.photos.load_more"),
                        c.on_secondary_container,
                    ))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Tonal))
                    .on_press(Message::LoadPhotos {
                        peer: g.peer.clone(),
                        more: true,
                    }),
                )
                .center_x(Length::Fill),
            );
        }
        col.into()
    }

    /// Barra de acciones de la página Fotos (arriba).
    fn photos_toolbar(&self, c: Colors) -> Element<'_, Message> {
        let g = &self.gallery;
        let status: Element<'_, Message> = if g.loading && !g.photos.is_empty() {
            row![
                Icon::Refresh.view(16.0, c.on_surface_variant),
                text(t("gui.photos.updating")).color(c.on_surface_variant)
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        } else {
            space().into()
        };
        row![
            button(labeled(
                Icon::Refresh,
                t("gui.photos.reload"),
                c.on_secondary_container
            ))
            .padding(Padding::from([9, 18]))
            .style(style::button(c, ButtonKind::Tonal))
            .on_press_maybe((!g.loading).then(|| Message::LoadPhotos {
                peer: g.peer.clone(),
                more: false,
            })),
            button(labeled(
                Icon::ExpandMore,
                t("gui.photos.load_more"),
                c.primary
            ))
            .padding(Padding::from([9, 18]))
            .style(style::button(c, ButtonKind::Outlined))
            .on_press_maybe((!g.loading && (g.photos.len() as u32) < g.total).then(|| {
                Message::LoadPhotos {
                    peer: g.peer.clone(),
                    more: true,
                }
            })),
            button(labeled(
                Icon::Folder,
                t("gui.common.open_downloads"),
                c.primary
            ))
            .padding(Padding::from([9, 18]))
            .style(style::button(c, ButtonKind::Outlined))
            .on_press(Message::OpenDownloads),
            space().width(Length::Fill),
            status,
        ]
        .spacing(10)
        .align_y(Alignment::Center)
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
                            .unwrap_or_else(|| t("gui.send.no_file").into())
                    )
                    .size(26)
                    .font(EXTRA_BOLD),
                    text(t("gui.send.hint")).size(14),
                ]
                .width(Length::Fill),
                button(labeled(Icon::Attach, t("gui.send.pick"), c.on_primary))
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
            self.title(c, t("gui.nav.send"), t("gui.send.subtitle").into()),
            header
        ]
        .spacing(14);
        col = col.push(self.section(c, t("gui.send.target")));
        let targets: Vec<&Peer> = self
            .snapshot
            .peers
            .iter()
            .filter(|p| !p.bt_address.is_empty())
            .collect();
        if targets.is_empty() {
            col = col.push(text(t("gui.send.no_target")).color(c.on_surface_variant));
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
                        button(labeled(Icon::Send, t("gui.nav.send"), c.on_primary))
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
                .width(Length::Fill)
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
            text(t("gui.settings.theme")).size(22).font(BOLD),
            text(t("gui.settings.theme_detail"))
                .size(13)
                .color(c.on_surface_variant),
            space().height(8),
            segments,
        ]
        .spacing(4);
        if self.config.theme == ThemeMode::System {
            theme_card = theme_card.push(self.switch_row(
                c,
                t("gui.settings.oled_system"),
                Some(t("gui.settings.oled_system_detail")),
                self.config.oled_when_system_dark,
                Message::SetOledWhenSystemDark,
            ));
        }
        theme_card = theme_card.push(self.switch_row(
            c,
            t("gui.settings.animations"),
            Some(t("gui.settings.animations_detail")),
            self.config.animations,
            Message::SetAnimations,
        ));

        let daemon_card = {
            let (label, action): (&str, Element<'_, Message>) = match self.state {
                DaemonState::Running => (
                    t("gui.settings.daemon_running"),
                    button(labeled(Icon::Stop, t("gui.settings.stop"), c.primary))
                        .padding(Padding::from([10, 22]))
                        .style(style::button(c, ButtonKind::Outlined))
                        .on_press(Message::QuitDaemon)
                        .into(),
                ),
                DaemonState::NotRunning => (
                    t("gui.settings.daemon_stopped"),
                    button(labeled(Icon::PlayCircle, t("gui.home.start"), c.on_primary))
                        .padding(Padding::from([10, 22]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press(Message::StartDaemon)
                        .into(),
                ),
                DaemonState::Connecting => (t("gui.settings.daemon_connecting"), space().into()),
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
                    text(t("gui.settings.daemon")).size(17).font(SEMIBOLD),
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
        // Idioma (se aplica al volver a abrir).
        let saved = kushare_i18n::saved_language();
        let langs: [(&'static str, &str); 3] = [
            ("", t("gui.settings.language_system")),
            ("es", "Español"),
            ("en", "English"),
        ];
        let lang_seg = row(langs.iter().enumerate().map(|(i, (code, label))| {
            let selected = saved == *code;
            button(
                text(*label)
                    .size(14)
                    .font(SEMIBOLD)
                    .width(Length::Fill)
                    .align_x(iced::alignment::Horizontal::Center),
            )
            .padding(Padding::from([10, 0]))
            .width(Length::Fill)
            .style(style::button(
                c,
                ButtonKind::Segment {
                    selected,
                    first: i == 0,
                    last: i == langs.len() - 1,
                },
            ))
            .on_press(Message::SetLanguage(code))
            .into()
        }))
        .spacing(3);
        theme_card = theme_card.push(space().height(8)).push(
            row![
                Icon::Language.view(20.0, c.on_surface_variant),
                text(t("gui.settings.language")).size(17).font(SEMIBOLD)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        theme_card = theme_card.push(lang_seg);

        let features_card = column![
            self.switch_row(
                c,
                t("gui.settings.prefetch"),
                Some(t("gui.settings.prefetch_detail")),
                self.setting("photo-prefetch"),
                |on| Message::SetSetting("photo-prefetch".into(), on),
            ),
            self.switch_row(
                c,
                t("gui.clip.sync"),
                Some(t("gui.clip.sync_detail")),
                self.setting("clipboard-sync"),
                |on| Message::SetSetting("clipboard-sync".into(), on),
            ),
            self.switch_row(
                c,
                t("gui.settings.receive_confirm"),
                Some(t("gui.settings.receive_confirm_detail")),
                self.setting("receive-confirm"),
                |on| Message::SetSetting("receive-confirm".into(), on),
            ),
            self.switch_row(
                c,
                t("gui.settings.update_check"),
                Some(t("gui.settings.update_check_detail")),
                self.setting("update-check"),
                |on| Message::SetSetting("update-check".into(), on),
            ),
        ]
        .spacing(6);

        column![
            self.title(c, t("gui.nav.settings"), t("gui.settings.subtitle").into()),
            self.section(c, t("gui.settings.appearance")),
            container(theme_card)
                .padding(26)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::Wide)),
            self.section(c, t("gui.settings.connection")),
            container(daemon_card)
                .padding(20)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::List)),
            self.section(c, t("gui.settings.features")),
            container(features_card)
                .padding(22)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::List)),
            self.section(c, t("gui.update.title")),
            container(self.update_card(c))
                .padding(20)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::List)),
            self.section(c, t("gui.settings.diagnostics")),
            container(
                row![
                    shape_icon(
                        Icon::Search,
                        Blob::Cookie6,
                        c.tertiary,
                        c.on_tertiary,
                        48.0,
                        0.0
                    ),
                    column![
                        text(t("gui.settings.check")).size(17).font(SEMIBOLD),
                        text(if self.snapshot.link_port != 0 {
                            tf(
                                "gui.settings.doctor_port",
                                &[("port", &self.snapshot.link_port)],
                            )
                        } else {
                            t("gui.settings.check_detail").to_string()
                        })
                        .size(13)
                        .color(c.on_surface_variant),
                    ]
                    .width(Length::Fill),
                    button(labeled(
                        Icon::Health,
                        t("gui.settings.diagnose"),
                        c.on_secondary_container
                    ))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Tonal))
                    .on_press(Message::RunDoctor),
                ]
                .spacing(16)
                .align_y(Alignment::Center),
            )
            .padding(20)
            .width(Length::Fill)
            .style(style::card(c.container_high, c.on_surface, Shape::List)),
            self.section(c, t("gui.settings.about")),
            container(column![
                info(t("gui.settings.name"), self.snapshot.device_name.clone()),
                info(t("gui.settings.id"), self.snapshot.device_id.clone()),
                info(
                    t("gui.settings.version"),
                    env!("CARGO_PKG_VERSION").to_string()
                ),
            ])
            .padding(22)
            .width(Length::Fill)
            .style(style::card(c.container_high, c.on_surface, Shape::List)),
        ]
        .spacing(14)
        .into()
    }

    fn update_card(&self, c: Colors) -> Element<'_, Message> {
        let u = &self.update;
        let (title, detail, action): (String, String, Element<'_, Message>) = match &u.row {
            _ if u.checking => (
                t("gui.update.checking").into(),
                String::new(),
                space().into(),
            ),
            Some((current, latest, page, asset, _url)) if crate::is_newer(latest, current) => {
                let title = tf("gui.update.available", &[("version", latest)]);
                let page = page.clone();
                let action: Element<'_, Message> = if let Some(path) = &u.downloaded {
                    row![
                        text(path.clone()).size(12).color(c.on_surface_variant),
                        button(labeled(Icon::Update, t("gui.update.install"), c.on_primary))
                            .padding(Padding::from([10, 22]))
                            .style(style::button(c, ButtonKind::Filled))
                            .on_press_maybe((!u.installing).then_some(Message::InstallUpdate)),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .into()
                } else if asset.is_empty() {
                    button(labeled(
                        Icon::OpenInNew,
                        t("gui.update.open_page"),
                        c.on_primary,
                    ))
                    .padding(Padding::from([10, 22]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::OpenUrl(page))
                    .into()
                } else {
                    row![
                        button(labeled(Icon::OpenInNew, t("gui.update.notes"), c.primary))
                            .style(style::button(c, ButtonKind::Text))
                            .on_press(Message::OpenUrl(page)),
                        button(labeled(
                            Icon::Download,
                            t("gui.update.download"),
                            c.on_primary
                        ))
                        .padding(Padding::from([10, 22]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press_maybe((!u.downloading).then_some(Message::DownloadUpdate)),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
                };
                let detail = if u.downloading {
                    t("gui.update.downloading").to_string()
                } else if u.installing {
                    t("gui.update.installing").to_string()
                } else {
                    tf("gui.update.current", &[("version", current)])
                };
                (title, detail, action)
            }
            Some((current, ..)) => (
                t("gui.update.up_to_date").into(),
                tf("gui.update.current", &[("version", current)]),
                button(labeled(
                    Icon::Refresh,
                    t("gui.update.check"),
                    c.on_secondary_container,
                ))
                .padding(Padding::from([10, 22]))
                .style(style::button(c, ButtonKind::Tonal))
                .on_press(Message::CheckUpdate(true))
                .into(),
            ),
            None => (
                t("gui.update.unknown").into(),
                tf(
                    "gui.update.current",
                    &[("version", &env!("CARGO_PKG_VERSION"))],
                ),
                button(labeled(
                    Icon::Refresh,
                    t("gui.update.check"),
                    c.on_secondary_container,
                ))
                .padding(Padding::from([10, 22]))
                .style(style::button(c, ButtonKind::Tonal))
                .on_press(Message::CheckUpdate(true))
                .into(),
            ),
        };
        let mut texts = column![text(title).size(17).font(SEMIBOLD)];
        if !detail.is_empty() {
            texts = texts.push(text(detail).size(13).color(c.on_surface_variant));
        }
        if let Some(err) = &u.error {
            texts = texts.push(text(err.clone()).size(13).color(c.error));
        }
        row![
            shape_icon(
                Icon::Update,
                Blob::Flower,
                c.secondary,
                c.on_secondary,
                48.0,
                0.0
            ),
            texts.width(Length::Fill),
            action,
        ]
        .spacing(16)
        .align_y(Alignment::Center)
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
                text(tf("gui.pairing.title", &[("name", &p.name)]))
                    .size(26)
                    .font(EXTRA_BOLD),
                text(t("gui.pairing.hint")).color(c.on_surface_variant),
                container(text(pin).size(54).font(EXTRA_BOLD))
                    .padding(Padding::from([10, 34]))
                    .style(style::card(
                        c.primary_container,
                        c.on_primary_container,
                        Shape::List
                    )),
                row![
                    button(labeled(Icon::Close, t("gui.phone.reject"), c.primary))
                        .padding(Padding::from([10, 22]))
                        .style(style::button(c, ButtonKind::Text))
                        .on_press(Message::RespondPairing(false)),
                    button(labeled(Icon::Check, t("gui.pairing.matches"), c.on_primary))
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
            tf("gui.apps.title_out", &[("name", &f.peer_name)])
        } else {
            tf("gui.apps.title_in", &[("name", &f.peer_name)])
        };
        let list: Element<'a, Message> = match &f.apps {
            None => text(t("gui.common.loading")).into(),
            Some((local, remote, out_blocked, in_blocked)) => {
                let (apps, blocked) = if f.outgoing {
                    (local, out_blocked)
                } else {
                    (remote, in_blocked)
                };
                if apps.is_empty() {
                    text(t("gui.apps.hint")).color(c.on_surface_variant).into()
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
                    button(labeled(Icon::Check, t("gui.common.done"), c.on_primary))
                        .padding(Padding::from([10, 26]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press(Message::CloseApps),
                ],
            ]
            .spacing(18)
            .into(),
        )
    }

    fn doctor_dialog<'a>(&self, c: Colors, report: &'a str) -> Element<'a, Message> {
        let body: Element<'a, Message> = if report.is_empty() {
            text(t("gui.settings.diagnosing"))
                .color(c.on_surface_variant)
                .into()
        } else {
            scrollable(text(report).font(Font::MONOSPACE).size(13))
                .height(Length::Fixed(380.0))
                .into()
        };
        self.dialog(
            c,
            column![
                text(t("gui.settings.diagnostics"))
                    .size(26)
                    .font(EXTRA_BOLD),
                body,
                row![
                    space().width(Length::Fill),
                    button(labeled(Icon::Close, t("gui.common.close"), c.on_primary))
                        .padding(Padding::from([10, 26]))
                        .style(style::button(c, ButtonKind::Filled))
                        .on_press(Message::CloseDoctor),
                ],
            ]
            .spacing(16)
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

// --- Utilidades compartidas por las páginas --------------------------------

impl App {
    fn load_settings(&self) -> Task<Message> {
        match self.daemon.clone() {
            Some(d) => Task::perform(
                async move { d.settings().await.unwrap_or_default() },
                Message::SettingsLoaded,
            ),
            None => Task::none(),
        }
    }

    /// Valor de un ajuste del daemon (los predeterminados si aún no se leyó).
    fn setting(&self, name: &str) -> bool {
        self.daemon_settings
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, on)| *on)
            .unwrap_or(name != "clipboard-history")
    }
}

/// Mensaje de error del daemon sin el prefijo de D-Bus.
fn err_text(err: zbus::Error) -> String {
    match err {
        zbus::Error::MethodError(_, Some(detail), _) => detail,
        zbus::Error::FDO(e) => match *e {
            zbus::fdo::Error::Failed(detail) | zbus::fdo::Error::InvalidArgs(detail) => detail,
            other => other.to_string(),
        },
        other => other.to_string(),
    }
}

/// Fecha local corta: "14:03" si es de hoy, "7 nov 14:03" si es de este
/// año, "07/11/2025" si no.
fn fmt_time(ms: i64) -> String {
    fn local(secs: i64) -> libc::tm {
        let t: libc::time_t = secs as libc::time_t;
        // SAFETY: localtime_r solo escribe en `tm`, que es nuestro.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&t, &mut tm);
            tm
        }
    }
    if ms <= 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let (d, n) = (local(ms / 1000), local(now));
    if d.tm_year == n.tm_year && d.tm_yday == n.tm_yday {
        format!("{:02}:{:02}", d.tm_hour, d.tm_min)
    } else if d.tm_year == n.tm_year {
        let month = month_name(d.tm_mon);
        format!("{} {month} {:02}:{:02}", d.tm_mday, d.tm_hour, d.tm_min)
    } else {
        format!("{:02}/{:02}/{}", d.tm_mday, d.tm_mon + 1, d.tm_year + 1900)
    }
}

fn month_name(month: i32) -> &'static str {
    match month {
        0 => t("gui.month.1"),
        1 => t("gui.month.2"),
        2 => t("gui.month.3"),
        3 => t("gui.month.4"),
        4 => t("gui.month.5"),
        5 => t("gui.month.6"),
        6 => t("gui.month.7"),
        7 => t("gui.month.8"),
        8 => t("gui.month.9"),
        9 => t("gui.month.10"),
        10 => t("gui.month.11"),
        _ => t("gui.month.12"),
    }
}

/// Minúsculas y sin acentos, para buscar ("López" → "lopez").
fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

/// Lo escrito en el marcador tal como se marcará (letras → teclas).
fn dialable_preview(input: &str) -> String {
    input
        .trim()
        .chars()
        .filter_map(
            |c| match fold(&c.to_string()).chars().next()?.to_ascii_uppercase() {
                d @ ('0'..='9' | '+' | '*' | '#') => Some(d),
                'A'..='C' => Some('2'),
                'D'..='F' => Some('3'),
                'G'..='I' => Some('4'),
                'J'..='L' => Some('5'),
                'M'..='O' => Some('6'),
                'P'..='S' => Some('7'),
                'T'..='V' => Some('8'),
                'W'..='Z' => Some('9'),
                _ => None,
            },
        )
        .collect()
}

/// Últimos dígitos de un número, para emparejar "+52 55 1234 5678" con "5512345678".
fn calls_digits(number: &str) -> String {
    let d: String = number.chars().filter(char::is_ascii_digit).collect();
    d[d.len().saturating_sub(10)..].to_string()
}

/// Instala el paquete descargado con `pkexec` (pide la contraseña con el
/// agente de polkit del escritorio).
async fn install_package(path: String) -> Result<(), String> {
    let kind = if path.ends_with(".pkg.tar.zst") {
        "arch"
    } else if path.ends_with(".deb") {
        "deb"
    } else {
        "rpm"
    };
    let mut cmd: Vec<String> = match kind {
        "arch" => vec!["pkexec", "pacman", "-U", "--noconfirm"],
        "deb" => vec!["pkexec", "apt-get", "install", "-y"],
        _ if std::path::Path::new("/usr/bin/dnf").exists() => {
            vec!["pkexec", "dnf", "install", "-y"]
        }
        _ if std::path::Path::new("/usr/bin/zypper").exists() => {
            vec!["pkexec", "zypper", "--non-interactive", "install"]
        }
        _ => vec!["pkexec", "rpm", "-U"],
    }
    .into_iter()
    .map(str::to_string)
    .collect();
    cmd.push(path);
    let out = tokio::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .output()
        .await
        .map_err(|e| tf("gui.update.no_pkexec", &[("error", &e)]))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(tf(
            "gui.update.install_failed",
            &[("error", &err.lines().last().unwrap_or_default().to_string())],
        ))
    }
}

/// ¿`latest` es posterior a `current`? (x.y.z numérico).
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(latest) > parse(current)
}
