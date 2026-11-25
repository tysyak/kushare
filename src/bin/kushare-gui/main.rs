//! `kushare-gui`: interfaz gráfica de escritorio de kushare (iced), con lo
//! mismo que la app Android: estado del enlace, dispositivos (emparejar con
//! PIN, direcciones de notificaciones, filtros por app), enviar archivos y
//! ajustes de tema (Sistema, Claro, Oscuro, OLED). No hace red por su
//! cuenta: controla al daemon `kushare` por D-Bus.

mod anim;
mod calls;
mod camera;
mod clip;
mod daemon;
mod dialogs;
mod gallery;
mod home;
mod hotspot;
mod icons;
mod layout;
mod notifs;
mod panel;
mod phone;
mod reply;
mod settings;
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
use icons::{Blob, Icon, avatar, shape_icon};
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

/// Tamaño inicial: cabe el panel del teléfono al lado de la página.
const WINDOW_SIZE: iced::Size = iced::Size::new(1280.0, 800.0);

fn main() -> iced::Result {
    // `kushare-gui --reply <título> <texto>`: ventana para responder a una
    // notificación del teléfono (la abre el daemon).
    let args: Vec<String> = std::env::args().collect();
    // Sin clap: la ventana no tiene más opciones que estas.
    match args.get(1).map(String::as_str) {
        Some("-h" | "--help") => {
            println!(
                "Ventana de kushare (archivos, notificaciones y teléfono).\n\n\
                 Uso: kushare-gui\n\
                 \x20    kushare-gui --reply <TÍTULO> <TEXTO>\n\n\
                 KUSHARE_GUI_SCREEN=<página> abre esa página. Más en `man kushare-gui`."
            );
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("kushare-gui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    if args.get(1).map(String::as_str) == Some("--reply") {
        let title = args.get(2).cloned().unwrap_or_default();
        let body = args.get(3).cloned().unwrap_or_default();
        return reply::run(title, body);
    }
    iced::application(App::new, App::update, App::view)
        .title("kushare")
        .theme(App::theme)
        .subscription(App::subscription)
        .window(iced::window::Settings {
            size: WINDOW_SIZE,
            min_size: Some(iced::Size::new(480.0, 480.0)),
            ..Default::default()
        })
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
    /// Dónde van las pestañas del menú.
    nav: layout::NavPosition,
    /// Panel del teléfono (estado + notificaciones) al lado de la página,
    /// si cabe.
    side_panel: bool,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            oled_when_system_dark: true,
            animations: true,
            nav: layout::NavPosition::Left,
            side_panel: true,
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
    Camera,
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
    /// Desde cuándo hay una llamada activa (para el contador de la barra).
    call_since: Option<std::time::Instant>,
    angle: f32,
    /// Informe de `kushare doctor` (None: no se ha pedido; Some(""): en curso).
    doctor: Option<String>,
    gallery: Gallery,
    sms: sms::SmsState,
    calls: calls::CallsState,
    notifs: notifs::NotifState,
    clip: clip::ClipState,
    camera: camera::CameraState,
    hotspot: hotspot::HotspotState,
    viewer: Option<viewer::Viewer>,
    /// Ajustes del daemon: (nombre, activado).
    daemon_settings: Vec<(String, bool)>,
    update: UpdateStatus,
    /// Salidas y entradas de audio de este PC.
    local_audio: (Vec<OutputRow>, Vec<OutputRow>),
    /// Fondo de pantalla de cada teléfono: (ruta o "", colores ARGB).
    wallpapers: HashMap<String, (String, Vec<u32>)>,
    /// Animaciones de la ventana (entrada de página, aviso emergente).
    ui: anim::Ui,
    /// Tamaño actual de la ventana (para la disposición adaptativa).
    win: iced::Size,
    /// Cuántos avisos emergentes se han mostrado: cada uno se cierra solo
    /// a los `TOAST_TIME`, salvo que ya lo haya sustituido otro.
    toast_seq: u64,
}

/// Cuánto se queda a la vista un aviso emergente.
const TOAST_TIME: Duration = Duration::from_secs(5);

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
    /// Fotograma mientras hay animaciones cortas en curso.
    Frame,
    /// Cambió el tamaño de la ventana.
    Resized(iced::Size),
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
    SetNav(layout::NavPosition),
    SetSidePanel(bool),
    OpenDownloads,
    Done(Result<String, String>),
    DismissToast,
    /// Se cumplió el tiempo del aviso emergente número n.
    ToastExpired(u64),
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
    /// Hacer sonar el teléfono para encontrarlo (`ring = false`: pararlo).
    FindPhone {
        peer: String,
        ring: bool,
    },
    /// Cada segundo mientras suena algo (barra de progreso).
    Clock,
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
    Camera(camera::Msg),
    Hotspot(hotspot::Msg),
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
                Ok("camera") => Screen::Camera,
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
            call_since: None,
            angle: 0.0,
            doctor: None,
            gallery: Gallery::default(),
            sms: Default::default(),
            calls: Default::default(),
            notifs: Default::default(),
            clip: Default::default(),
            camera: Default::default(),
            hotspot: Default::default(),
            viewer: None,
            daemon_settings: Vec::new(),
            update: UpdateStatus::default(),
            ui: anim::Ui::default(),
            win: WINDOW_SIZE,
            toast_seq: 0,
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

    /// Qué cabe en la ventana ahora mismo.
    fn layout(&self) -> layout::Layout {
        layout::compute(self.win.width, self.config.nav, self.config.side_panel)
    }

    /// ¿Se ven las notificaciones? En su página, en el panel o (cuántas
    /// hay) en la franja de estado: con un teléfono conectado, siempre.
    fn notifs_visible(&self) -> bool {
        self.screen == Screen::Notifications || !self.phone_peers().is_empty()
    }

    /// Página que se dibuja: con el panel a la vista, *Notificaciones* ya
    /// está en él, así que se muestra *Inicio*.
    fn shown_screen(&self) -> Screen {
        if self.screen == Screen::Notifications && self.layout().panel {
            Screen::Home
        } else {
            self.screen
        }
    }

    /// Aplica un cambio que puede hacer aparecer el panel; si aparece,
    /// carga sus notificaciones.
    fn panel_appeared(&mut self, change: impl FnOnce(&mut Self)) -> Task<Message> {
        let before = self.layout().panel;
        change(self);
        if !before && self.layout().panel {
            Task::done(Message::Notifs(notifs::Msg::Load))
        } else {
            Task::none()
        }
    }

    /// ¿Hay algo que animar? (para no redibujar sin motivo)
    fn animating(&self) -> bool {
        // Solo lo que es pasajero: buscar, emparejar, fotos recién llegadas,
        // webcam o micrófono en marcha. «Conectado» es un estado estable y no
        // gira (animarlo siempre costaba ~2,6 % de CPU y ~400 despertares/s).
        self.config.animations
            && (self.snapshot.scanning
                || self.state == DaemonState::Connecting
                || self.pairing.is_some()
                || !self.gallery.fresh.is_empty()
                || (self.camera.webcam.0 && self.screen == Screen::Camera)
                || (self.mic_on() && self.screen == Screen::Camera))
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
        subs.push(iced::window::resize_events().map(|(_, size)| Message::Resized(size)));
        if self.config.animations && (self.ui.active() || self.notifs.animating()) {
            subs.push(iced::window::frames().map(|_| Message::Frame));
        }
        // Estado de la webcam (fps) mientras se ve su página o está en marcha.
        if self.webcam_watch() && self.state == DaemonState::Running {
            // Con el micrófono a la vista, más a menudo para el medidor.
            let every = if self.mic_meter() {
                Duration::from_millis(200)
            } else {
                Duration::from_secs(2)
            };
            subs.push(iced::time::every(every).map(|_| Message::Camera(camera::Msg::Poll)));
        }
        // La barra de progreso de lo que suena avanza cada segundo.
        let playing = self
            .snapshot
            .states
            .values()
            .any(|st| st.3.iter().any(|p| p.5 == "playing" && p.10.2 > 0));
        if playing || self.call_since.is_some() {
            subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Clock));
        }
        if self.viewer.is_some() {
            subs.push(iced::keyboard::listen().filter_map(|event| match event {
                iced::keyboard::Event::KeyPressed { key, .. } => Some(Message::Key(key)),
                _ => None,
            }));
        }
        if self.notifs_visible() && self.state == DaemonState::Running {
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

    /// Atiende el mensaje y anima lo que haya cambiado de golpe: la página
    /// (al navegar) y el aviso emergente (al aparecer o irse).
    fn update(&mut self, message: Message) -> Task<Message> {
        let (screen, toast) = (self.screen, self.toast.clone());
        let mut task = self.handle(message);
        if self.toast.is_some() && self.toast != toast {
            self.toast_seq += 1;
            let seq = self.toast_seq;
            task = Task::batch([
                task,
                Task::perform(tokio::time::sleep(TOAST_TIME), move |_| {
                    Message::ToastExpired(seq)
                }),
            ]);
        }
        if self.config.animations {
            let now = std::time::Instant::now();
            if self.screen != screen {
                self.ui.page_at = Some(now);
            }
            if self.toast != toast {
                match (&self.toast, toast) {
                    (Some(_), _) => {
                        self.ui.toast_at = Some(now);
                        self.ui.toast_leaving = None;
                    }
                    (None, Some(old)) => self.ui.toast_leaving = Some((old, now)),
                    (None, None) => {}
                }
            }
        }
        task
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
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
            Message::Daemon(Event::QrChanged) => Task::done(Message::Camera(camera::Msg::Load)),
            Message::Daemon(Event::HotspotChanged) => {
                Task::done(Message::Hotspot(hotspot::Msg::Load))
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
                // Contador de la barra de llamada: empieza al contestar.
                let active = self
                    .phone_call()
                    .is_some_and(|(_, call)| call.0 == "active");
                match (active, self.call_since) {
                    (true, None) => self.call_since = Some(std::time::Instant::now()),
                    (false, Some(_)) => self.call_since = None,
                    _ => {}
                }
                // El panel muestra las notificaciones del teléfono conectado:
                // cargarlas en cuanto lo hay (o si cambió de teléfono).
                let phone = self.phone_peers().first().map(|p| p.id.clone());
                let mut tasks = Vec::new();
                if let Some(peer) = phone {
                    // El fondo de pantalla se ve siempre (panel o franja de estado).
                    if !self.wallpapers.contains_key(&peer) {
                        self.wallpapers.insert(peer.clone(), Default::default());
                        tasks.push(Task::done(Message::LoadWallpaper(peer.clone(), false)));
                    }
                    if self.notifs_visible() && peer != self.notifs.peer {
                        tasks.push(Task::done(Message::Notifs(notifs::Msg::Load)));
                    }
                }
                Task::batch(tasks)
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
            Message::Frame => {
                self.ui.tick(std::time::Instant::now());
                self.notifs_frame();
                Task::none()
            }
            Message::Resized(size) => self.panel_appeared(|app| app.win = size),
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
                    Screen::Camera => return Task::done(Message::Camera(camera::Msg::Load)),
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
                            Task::done(Message::Hotspot(hotspot::Msg::Load)),
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
            Message::SetNav(nav) => {
                self.config.nav = nav;
                self.config.save();
                self.panel_appeared(|app| app.config.nav = nav)
            }
            Message::SetSidePanel(on) => {
                self.config.side_panel = on;
                self.config.save();
                self.panel_appeared(|app| app.config.side_panel = on)
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
            Message::ToastExpired(seq) => {
                if seq == self.toast_seq {
                    self.toast = None;
                }
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
            Message::FindPhone { peer, ring } => self.call(
                if ring {
                    t("gui.panel.find_started")
                } else {
                    ""
                },
                move |d| async move { d.find_phone(&peer, ring).await },
            ),
            Message::Clock => Task::none(),
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
            Message::Camera(m) => self.camera_update(m),
            Message::Hotspot(m) => self.hotspot_update(m),
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
                    Err(e) => {
                        tracing::debug!(%e, "sin fondo de pantalla");
                        // Vacío: no volver a pedirlo en cada refresco.
                        self.wallpapers.entry(peer).or_default();
                    }
                }
                Task::none()
            }
        }
    }

    // --- Vistas ------------------------------------------------------------

    fn view(&self) -> Element<'_, Message> {
        let c = self.colors();
        let lay = self.layout();
        let screen = self.shown_screen();
        let content: Element<'_, Message> = match screen {
            Screen::Home => self.home(c),
            Screen::Devices => self.devices(c),
            Screen::Phone => self.phone(c),
            Screen::Messages => self.sms_view(c),
            Screen::Calls => self.calls_view(c),
            Screen::Photos => self.photos(c),
            Screen::Notifications => self.notifs_view(c),
            Screen::Clipboard => self.clip_view(c),
            Screen::Camera => self.camera_view(c),
            Screen::Send => self.send(c),
            Screen::Settings => self.settings(c),
        };
        // La página usa todo el ancho que le queda; las que llevan sus
        // propias listas desplazables (Mensajes, Llamadas, Notificaciones)
        // llenan el alto en lugar de ir dentro de un desplazable.
        let pad = if lay.compact { 16.0 } else { 28.0 };
        let shift = self.ui.page_shift(std::time::Instant::now());
        let inner = container(content)
            .padding(Padding::new(pad).top(pad + shift))
            .width(Length::Fill);
        let fills = match screen {
            Screen::Messages | Screen::Notifications => true,
            Screen::Calls => !lay.compact,
            _ => false,
        };
        let page: Element<'_, Message> = if fills {
            inner.height(Length::Fill).into()
        } else {
            scrollable(inner).height(Length::Fill).into()
        };
        // Llamada en curso: barra fija arriba en todas las páginas.
        let mut body = column![];
        if let Some(bar) = self.call_bar(c) {
            body = body.push(container(bar).padding(Padding::new(pad).bottom(0.0).top(12.0)));
        }
        // Sin panel, el estado del teléfono va en una franja encima.
        if !lay.panel && screen != Screen::Phone && !self.phone_peers().is_empty() {
            body = body.push(
                container(self.status_strip(c)).padding(Padding::new(pad).bottom(0.0).top(12.0)),
            );
        }
        let body = body.push(page).width(Length::Fill).height(Length::Fill);

        use layout::NavPosition as Nav;
        let main: Element<'_, Message> = match self.config.nav {
            Nav::Top => column![self.tabs(c), body].into(),
            Nav::Bottom => column![body, self.tabs(c)].into(),
            Nav::Left | Nav::Right => body.into(),
        };
        let mut page = row![];
        if self.config.nav == Nav::Left {
            page = page.push(self.rail(c));
        }
        if lay.panel {
            page = page.push(self.side_panel(c));
        }
        page = page.push(main);
        if self.config.nav == Nav::Right {
            page = page.push(self.rail(c));
        }
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
        let now = std::time::Instant::now();
        if let Some(t) = &self.toast {
            // Entra desde abajo.
            let p = self
                .ui
                .toast_at
                .map_or(1.0, |at| anim::progress(at, now, anim::TOAST));
            layers = layers.push(self.toast_view(c, t, 1.0 - anim::ease_out(p)));
        } else if let Some((t, at)) = &self.ui.toast_leaving {
            // Se va hacia abajo.
            let p = anim::progress(*at, now, anim::TOAST);
            layers = layers.push(self.toast_view(c, t, anim::ease_in(p)));
        }
        layers.into()
    }

    /// Pestañas del menú (sin *Notificaciones* si se ven en el panel).
    fn nav_items(&self) -> Vec<(Icon, &'static str, Screen)> {
        let mut items = vec![
            (Icon::Home, t("gui.nav.home"), Screen::Home),
            (Icon::Phone, t("gui.nav.phone"), Screen::Phone),
            (Icon::Message, t("gui.nav.messages"), Screen::Messages),
            (Icon::Dialpad, t("gui.nav.calls"), Screen::Calls),
            (Icon::Photo, t("gui.nav.photos"), Screen::Photos),
            (
                Icon::Notifications,
                t("gui.nav.notifications"),
                Screen::Notifications,
            ),
            (Icon::Clipboard, t("gui.nav.clipboard"), Screen::Clipboard),
            (Icon::Camera, t("gui.nav.camera"), Screen::Camera),
            (Icon::Send, t("gui.nav.send"), Screen::Send),
            (Icon::Devices, t("gui.nav.devices"), Screen::Devices),
            (Icon::Settings, t("gui.nav.settings"), Screen::Settings),
        ];
        if self.layout().panel {
            items.retain(|(_, _, s)| *s != Screen::Notifications);
        }
        items
    }

    /// Pestañas en fila (menú arriba o abajo), como en Enlace Móvil.
    fn tabs(&self, c: Colors) -> Element<'_, Message> {
        let shown = self.shown_screen();
        let mut tabs = row![
            text("ku").size(22).font(EXTRA_BOLD).color(c.primary),
            space().width(8)
        ]
        .spacing(4)
        .align_y(Alignment::Center);
        for (icon, label, screen) in self.nav_items() {
            let selected = shown == screen;
            let fg = if selected {
                c.on_secondary_container
            } else {
                c.on_surface_variant
            };
            tabs = tabs.push(
                button(
                    row![
                        icon.view(20.0, fg),
                        text(label)
                            .size(14)
                            .font(if selected { BOLD } else { SEMIBOLD })
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([8, 14]))
                .style(style::button(c, ButtonKind::Nav(selected)))
                .on_press(Message::Navigate(screen)),
            );
        }
        container(
            scrollable(tabs.padding(Padding::from([0, 4])))
                // Desplazable en ventanas estrechas, sin barra visible.
                .direction(iced::widget::scrollable::Direction::Horizontal(
                    iced::widget::scrollable::Scrollbar::hidden(),
                )),
        )
        .padding(Padding::from([10, 12]))
        .width(Length::Fill)
        .style(style::card(c.container_low, c.on_surface, Shape::List))
        .into()
    }

    /// Riel de navegación (menú a la izquierda o a la derecha).
    fn rail(&self, c: Colors) -> Element<'_, Message> {
        let shown = self.shown_screen();
        let item = |icon: Icon, label: &'static str, screen: Screen| {
            let selected = shown == screen;
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
                ]
                .extend(
                    self.nav_items()
                        .into_iter()
                        .map(|(icon, label, screen)| item(icon, label, screen).into()),
                )
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
            text(title)
                .size(if self.layout().compact { 30 } else { 44 })
                .font(EXTRA_BOLD)
                .color(c.on_surface),
            text(subtitle).size(16).color(c.on_surface_variant),
        ]
        .spacing(2)
        .padding(Padding::default().bottom(18.0))
        .into()
    }

    fn section<'a>(&self, c: Colors, label: &'a str) -> Element<'a, Message> {
        text(label).size(16).font(BOLD).color(c.primary).into()
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
            .unwrap_or(!matches!(name, "clipboard-history" | "dynamic-lock"))
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
