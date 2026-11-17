//! Página *Cámara y micrófono*: el teléfono como webcam del PC (elegir
//! cámara: frontal, traseras, teleobjetivo, visión nocturna…; motor
//! v4l2loopback o PipeWire), como micrófono del PC y el escáner de QR con su
//! historial. Si es una web se puede abrir en el navegador; si no, copiar o
//! guardar. Nada se abre solo.

use super::*;
use iced::widget::column;
use kushare_dbus::{CameraRow, MicRow, QrRow, WebcamRow};

#[derive(Debug, Clone, Default)]
pub struct CameraState {
    pub peer: String,
    pub cameras: Vec<CameraRow>,
    /// Cámara elegida para la webcam (id; vacío = la trasera por defecto).
    pub selected: String,
    pub webcam: WebcamRow,
    pub qr: Vec<QrRow>,
    pub starting: bool,
    /// Motor de la webcam: "auto", "v4l2" o "pipewire".
    pub backend: String,
    pub mic: MicRow,
    pub mic_starting: bool,
    /// Poner el micrófono del teléfono como entrada predeterminada.
    pub mic_default: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Load,
    /// Solo el estado de la webcam (cada pocos segundos).
    Poll,
    CamerasLoaded(String, Result<Vec<CameraRow>, String>),
    Status(WebcamRow, MicRow),
    Backend(String),
    MicStart,
    MicStarted(Result<String, String>),
    MicStop,
    MicDefault(bool),
    QrLoaded(Vec<QrRow>),
    Select(String),
    Start,
    Started(Result<String, String>),
    Stop,
    Scan,
    QrOpen(u32),
    QrCopy(u32),
    QrSave(u32),
    QrSaved(Result<String, String>),
    QrForget(i32),
}

impl App {
    pub(crate) fn camera_update(&mut self, msg: Msg) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        let phone = self.phone_peers().first().map(|p| p.id.clone());
        let s = &mut self.camera;
        match msg {
            Msg::Load => {
                let mut tasks = vec![
                    load_status(d.clone()),
                    Task::perform(
                        {
                            let d = d.clone();
                            async move { d.qr_history().await.unwrap_or_default() }
                        },
                        |l| Message::Camera(Msg::QrLoaded(l)),
                    ),
                ];
                if let Some(peer) = phone
                    && (s.peer != peer || s.cameras.is_empty())
                {
                    s.peer = peer.clone();
                    tasks.push(Task::perform(
                        async move { (peer.clone(), d.cameras(&peer).await.map_err(err_text)) },
                        |(p, r)| Message::Camera(Msg::CamerasLoaded(p, r)),
                    ));
                }
                Task::batch(tasks)
            }
            Msg::Poll => load_status(d),
            Msg::CamerasLoaded(peer, result) => {
                if s.peer == peer {
                    match result {
                        Ok(list) => s.cameras = list,
                        Err(e) => self.toast = Some(e),
                    }
                }
                Task::none()
            }
            Msg::Status(st, mic) => {
                s.webcam = st;
                s.mic = mic;
                Task::none()
            }
            Msg::Backend(b) => {
                s.backend = b;
                Task::none()
            }
            Msg::MicDefault(on) => {
                s.mic_default = on;
                Task::none()
            }
            Msg::MicStart => {
                let Some(peer) = phone else {
                    return Task::none();
                };
                s.mic_starting = true;
                let default = s.mic_default;
                Task::perform(
                    async move { d.mic_start(&peer, default).await.map_err(err_text) },
                    |r| Message::Camera(Msg::MicStarted(r)),
                )
            }
            Msg::MicStarted(result) => {
                s.mic_starting = false;
                self.toast = Some(match result {
                    Ok(codec) => tf("gui.camera.mic_started", &[("codec", &codec)]),
                    Err(e) => e,
                });
                load_status(d)
            }
            Msg::MicStop => Task::batch([
                self.call(t("gui.camera.mic_stopped"), |d| async move {
                    d.mic_stop().await
                }),
                load_status(d),
            ]),
            Msg::QrLoaded(list) => {
                s.qr = list;
                Task::none()
            }
            Msg::Select(id) => {
                s.selected = id;
                Task::none()
            }
            Msg::Start => {
                let Some(peer) = phone else {
                    return Task::none();
                };
                s.starting = true;
                let camera = s.selected.clone();
                let backend = s.backend.clone();
                Task::perform(
                    async move {
                        d.webcam_start(&peer, &camera, &backend)
                            .await
                            .map_err(err_text)
                    },
                    |r| Message::Camera(Msg::Started(r)),
                )
            }
            Msg::Started(result) => {
                s.starting = false;
                self.toast = Some(match result {
                    Ok(device) => tf("gui.camera.started", &[("device", &device)]),
                    Err(e) => e,
                });
                load_status(d)
            }
            Msg::Stop => Task::batch([
                self.call(
                    t("gui.camera.stopped"),
                    |d| async move { d.webcam_stop().await },
                ),
                load_status(d),
            ]),
            Msg::Scan => {
                let Some(peer) = phone else {
                    return Task::none();
                };
                self.call(t("gui.camera.scan_opened"), move |d| async move {
                    d.qr_scan(&peer).await
                })
            }
            Msg::QrOpen(i) => self.call("", move |d| async move { d.qr_open(i).await }),
            Msg::QrCopy(i) => {
                self.call(
                    t("gui.clip.copied"),
                    move |d| async move { d.qr_copy(i).await },
                )
            }
            Msg::QrSave(i) => {
                Task::perform(async move { d.qr_save(i).await.map_err(err_text) }, |r| {
                    Message::Camera(Msg::QrSaved(r))
                })
            }
            Msg::QrSaved(result) => {
                self.toast = Some(match result {
                    Ok(path) => tf("gui.camera.qr_saved", &[("path", &path)]),
                    Err(e) => e,
                });
                Task::none()
            }
            Msg::QrForget(i) => {
                if i < 0 {
                    s.qr.clear();
                } else if (i as usize) < s.qr.len() {
                    s.qr.remove(i as usize);
                }
                self.call("", move |d| async move { d.qr_forget(i).await })
            }
        }
    }

    /// ¿Hay que vigilar el estado de la webcam (página abierta o en marcha)?
    pub(crate) fn webcam_watch(&self) -> bool {
        self.screen == Screen::Camera || !self.camera.webcam.1.is_empty() || self.mic_on()
    }

    /// ¿Está el micrófono del teléfono en marcha?
    pub(crate) fn mic_on(&self) -> bool {
        !self.camera.mic.1.is_empty()
    }

    /// Medidor de nivel: refresco rápido con la página abierta y el micro en marcha.
    pub(crate) fn mic_meter(&self) -> bool {
        self.screen == Screen::Camera && self.mic_on()
    }

    pub(crate) fn camera_view(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.camera;
        let mut col = column![self.title(c, t("gui.nav.camera"), t("gui.camera.subtitle").into())]
            .spacing(14);
        if self.phone_peers().is_empty() {
            col = col.push(self.empty_phone(c, t("gui.camera.none_detail")));
        } else {
            col = col.push(self.webcam_card(c));
            col = col.push(self.mic_card(c));
        }
        col = col.push(self.section(c, t("gui.camera.qr_title")));
        col = col.push(
            row![
                button(labeled(Icon::QrCode, t("gui.camera.scan"), c.on_primary))
                    .padding(Padding::from([10, 20]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press_maybe(
                        (!self.phone_peers().is_empty()).then_some(Message::Camera(Msg::Scan))
                    ),
                space().width(Length::Fill),
                button(labeled(Icon::Delete, t("gui.camera.qr_clear"), c.primary))
                    .padding(Padding::from([9, 18]))
                    .style(style::button(c, ButtonKind::Outlined))
                    .on_press_maybe(
                        (!s.qr.is_empty()).then_some(Message::Camera(Msg::QrForget(-1)))
                    ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
        if s.qr.is_empty() {
            col = col.push(text(t("gui.camera.qr_empty")).color(c.on_surface_variant));
        }
        for (i, (code, format, date, kind, _)) in s.qr.iter().enumerate() {
            col = col.push(self.qr_row(c, i as u32, code, format, *date, kind));
        }
        col.into()
    }

    fn webcam_card(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.camera;
        let (active, peer, _camera, device, fps, error) = &s.webcam;
        let running = !peer.is_empty();
        let status = if !running {
            t("gui.camera.off").to_string()
        } else if !error.is_empty() {
            tf("gui.camera.error", &[("error", error)])
        } else if *active {
            tf("gui.camera.on", &[("device", device), ("fps", fps)])
        } else {
            t("gui.camera.starting").to_string()
        };
        // Cámaras como botones conectados que se reparten en filas.
        let mut chips = row![].spacing(6);
        for (id, label, _) in &s.cameras {
            let selected = s.selected == *id
                || (s.selected.is_empty() && s.cameras.first().is_some_and(|f| f.0 == *id));
            chips = chips.push(
                button(text(label.clone()).size(13).font(SEMIBOLD))
                    .padding(Padding::from([8, 14]))
                    .style(style::button(
                        c,
                        if selected {
                            ButtonKind::Filled
                        } else {
                            ButtonKind::Tonal
                        },
                    ))
                    .on_press_maybe((!running).then(|| Message::Camera(Msg::Select(id.clone())))),
            );
        }
        let action = if running {
            button(labeled(Icon::Stop, t("gui.camera.stop"), c.on_primary))
                .padding(Padding::from([10, 22]))
                .style(style::button(c, ButtonKind::Filled))
                .on_press(Message::Camera(Msg::Stop))
        } else {
            button(labeled(Icon::Camera, t("gui.camera.start"), c.on_primary))
                .padding(Padding::from([10, 22]))
                .style(style::button(c, ButtonKind::Filled))
                .on_press_maybe((!s.starting).then_some(Message::Camera(Msg::Start)))
        };
        container(
            column![
                row![
                    shape_icon(
                        Icon::Camera,
                        Blob::Cookie9,
                        if *active {
                            c.primary
                        } else {
                            c.secondary_container
                        },
                        if *active {
                            c.on_primary
                        } else {
                            c.on_secondary_container
                        },
                        52.0,
                        if *active { self.angle } else { 0.0 }
                    ),
                    column![
                        text(t("gui.camera.webcam")).size(20).font(BOLD),
                        text(status).size(13).color(c.on_surface_variant),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    action,
                ]
                .spacing(16)
                .align_y(Alignment::Center),
                chips.wrap().vertical_spacing(6),
                self.backend_picker(c, running),
                text(t("gui.camera.webcam_detail"))
                    .size(12)
                    .color(c.on_surface_variant),
            ]
            .spacing(12),
        )
        .padding(20)
        .width(Length::Fill)
        .style(style::card(c.container_high, c.on_surface, Shape::Wide))
        .into()
    }

    fn backend_picker(&self, c: Colors, running: bool) -> Element<'_, Message> {
        let current = if self.camera.backend.is_empty() {
            "auto"
        } else {
            self.camera.backend.as_str()
        };
        let mut r = row![
            text(t("gui.camera.backend"))
                .size(13)
                .color(c.on_surface_variant)
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        for (value, key) in [
            ("auto", "gui.camera.backend_auto"),
            ("v4l2", "gui.camera.backend_v4l2"),
            ("pipewire", "gui.camera.backend_pipewire"),
        ] {
            r = r.push(
                button(text(t(key)).size(12).font(SEMIBOLD))
                    .padding(Padding::from([6, 12]))
                    .style(style::button(
                        c,
                        if current == value {
                            ButtonKind::Tonal
                        } else {
                            ButtonKind::Text
                        },
                    ))
                    .on_press_maybe(
                        (!running).then(|| Message::Camera(Msg::Backend(value.to_string()))),
                    ),
            );
        }
        r.wrap().vertical_spacing(4).into()
    }

    fn mic_card(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.camera;
        let (active, peer, codec, level, error, default) = &s.mic;
        let running = !peer.is_empty();
        let status = if !running {
            t("gui.camera.mic_off").to_string()
        } else if !error.is_empty() {
            tf("gui.camera.error", &[("error", error)])
        } else if *active {
            tf("gui.camera.mic_on", &[("codec", codec)])
        } else {
            t("gui.camera.mic_starting").to_string()
        };
        let action = if running {
            button(labeled(Icon::Stop, t("gui.camera.stop"), c.on_primary))
                .padding(Padding::from([10, 22]))
                .style(style::button(c, ButtonKind::Filled))
                .on_press(Message::Camera(Msg::MicStop))
        } else {
            button(labeled(Icon::Mic, t("gui.camera.mic_start"), c.on_primary))
                .padding(Padding::from([10, 22]))
                .style(style::button(c, ButtonKind::Filled))
                .on_press_maybe((!s.mic_starting).then_some(Message::Camera(Msg::MicStart)))
        };
        let on_now = *active && running;
        // Medidor de nivel: el pico del último bloque (raíz: más legible).
        let fraction = (*level as f32 / 1000.0).sqrt().clamp(0.0, 1.0);
        let meter = iced::widget::progress_bar(0.0..=1.0, fraction)
            .girth(8)
            .style(move |_: &Theme| iced::widget::progress_bar::Style {
                background: c.container_highest.into(),
                bar: c.primary.into(),
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
            });
        let toggle_default = checkbox(*default || s.mic_default)
            .label(t("gui.camera.mic_default"))
            .size(18)
            .text_size(13)
            .on_toggle_maybe((!running).then_some(|v| Message::Camera(Msg::MicDefault(v))));
        let mut col = column![
            row![
                shape_icon(
                    Icon::Mic,
                    Blob::Cookie9,
                    if on_now {
                        c.primary
                    } else {
                        c.secondary_container
                    },
                    if on_now {
                        c.on_primary
                    } else {
                        c.on_secondary_container
                    },
                    52.0,
                    if on_now { self.angle } else { 0.0 }
                ),
                column![
                    text(t("gui.camera.mic")).size(20).font(BOLD),
                    text(status).size(13).color(c.on_surface_variant),
                ]
                .spacing(2)
                .width(Length::Fill),
                action,
            ]
            .spacing(16)
            .align_y(Alignment::Center),
        ]
        .spacing(12);
        if on_now {
            col = col.push(meter);
        }
        col = col.push(toggle_default).push(
            text(t("gui.camera.mic_detail"))
                .size(12)
                .color(c.on_surface_variant),
        );
        container(col)
            .padding(20)
            .width(Length::Fill)
            .style(style::card(c.container_high, c.on_surface, Shape::Wide))
            .into()
    }

    fn qr_row<'a>(
        &self,
        c: Colors,
        i: u32,
        code: &str,
        format: &str,
        date: i64,
        kind: &str,
    ) -> Element<'a, Message> {
        let label = match kind {
            "url" => t("gui.camera.kind_url"),
            "wifi" => t("gui.camera.kind_wifi"),
            "phone" => t("gui.camera.kind_phone"),
            "email" => t("gui.camera.kind_email"),
            "contact" => t("gui.camera.kind_contact"),
            _ => t("gui.camera.kind_text"),
        };
        let short: String = code.replace('\n', " ").chars().take(160).collect();
        let small = |icon: Icon, msg: Msg, kind: ButtonKind, fg: Color| {
            button(icon.view(18.0, fg))
                .padding(8)
                .style(style::button(c, kind))
                .on_press(Message::Camera(msg))
        };
        let mut actions = row![].spacing(6);
        if kind == "url" {
            actions = actions.push(
                button(labeled(Icon::OpenInNew, t("gui.camera.open"), c.on_primary))
                    .padding(Padding::from([6, 14]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::Camera(Msg::QrOpen(i))),
            );
        }
        actions = actions
            .push(small(
                Icon::Copy,
                Msg::QrCopy(i),
                ButtonKind::Tonal,
                c.on_secondary_container,
            ))
            .push(small(
                Icon::Save,
                Msg::QrSave(i),
                ButtonKind::Tonal,
                c.on_secondary_container,
            ))
            .push(small(
                Icon::Close,
                Msg::QrForget(i as i32),
                ButtonKind::Text,
                c.on_surface_variant,
            ));
        container(
            row![
                shape_icon(
                    Icon::QrCode,
                    Blob::Circle,
                    c.secondary_container,
                    c.on_secondary_container,
                    40.0,
                    0.0
                ),
                column![
                    text(format!(
                        "{label} · {} · {}",
                        fmt_time(date),
                        format.replace('_', " ")
                    ))
                    .size(12)
                    .color(c.on_surface_variant),
                    text(short).size(14),
                ]
                .spacing(2)
                .width(Length::Fill),
                actions,
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([10, 14]))
        .width(Length::Fill)
        .style(style::card(c.container, c.on_surface, Shape::List))
        .into()
    }
}

fn load_status(d: DaemonProxy<'static>) -> Task<Message> {
    Task::perform(
        async move {
            (
                d.webcam_status().await.unwrap_or_default(),
                d.mic_status().await.unwrap_or_default(),
            )
        },
        |(st, mic)| Message::Camera(Msg::Status(st, mic)),
    )
}
