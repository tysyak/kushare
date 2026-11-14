//! Panel del teléfono, como la columna izquierda de Enlace Móvil: arriba el
//! estado (miniatura con su fondo de pantalla, nombre, conexión, batería,
//! timbre y llamada en curso) y debajo sus notificaciones, siempre a la
//! vista mientras se usa cualquier otra página. En ventanas estrechas, en
//! su lugar va una franja de estado encima de la página.

use super::*;
use iced::widget::column;

impl App {
    /// Columna lateral con el estado del teléfono y sus notificaciones.
    pub(crate) fn side_panel(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.notifs;
        let shown = s.list.len() - s.leaving.len().min(s.list.len());
        let mut col = column![self.status_card(c)].spacing(14);
        if !self.phone_peers().is_empty() {
            col = col.push(
                row![
                    text(t("gui.panel.notifications"))
                        .size(17)
                        .font(BOLD)
                        .width(Length::Fill),
                    button(text(t("gui.panel.dismiss_all")).size(13).font(SEMIBOLD))
                        .padding(Padding::from([4, 10]))
                        .style(style::button(c, ButtonKind::Text))
                        .on_press_maybe(
                            (shown > 0).then_some(Message::Notifs(notifs::Msg::DismissAll)),
                        ),
                ]
                .align_y(Alignment::Center),
            );
            col = col.push(
                scrollable(
                    container(self.notif_cards(c, true)).padding(Padding::default().right(10.0)),
                )
                .height(Length::Fill),
            );
        }
        container(col)
            .padding(Padding::new(16.0).top(24.0).right(10.0))
            .width(Length::Fixed(layout::PANEL_WIDTH))
            .height(Length::Fill)
            .into()
    }

    /// Miniatura del teléfono con su fondo de pantalla (o sus colores).
    fn phone_thumb<'a>(&self, c: Colors, peer: &str, w: f32, h: f32) -> Element<'a, Message> {
        let radius = w * 0.16;
        let argb = |v: u32| Color::from_rgb8((v >> 16) as u8, (v >> 8) as u8, v as u8);
        let screen: Element<'a, Message> = match self.wallpapers.get(peer) {
            Some((path, _)) if !path.is_empty() => {
                iced::widget::image(iced::widget::image::Handle::from_path(path))
                    .width(Length::Fixed(w))
                    .height(Length::Fixed(h))
                    .content_fit(iced::ContentFit::Cover)
                    .border_radius(radius)
                    .into()
            }
            other => {
                let colors = other.map(|(_, cs)| cs.as_slice()).unwrap_or_default();
                let (a, b) = match colors {
                    [] => (c.primary_container, c.tertiary_container),
                    [x] => (argb(*x), c.primary),
                    [x, y, ..] => (argb(*x), argb(*y)),
                };
                container(space())
                    .width(Length::Fixed(w))
                    .height(Length::Fixed(h))
                    .style(move |_: &Theme| container::Style {
                        background: Some(
                            iced::gradient::Linear::new(0.8)
                                .add_stop(0.0, a)
                                .add_stop(1.0, b)
                                .into(),
                        ),
                        border: iced::Border {
                            radius: radius.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .into()
            }
        };
        // Marco oscuro, como el borde del teléfono.
        container(screen)
            .padding(w * 0.05)
            .style(move |_: &Theme| container::Style {
                background: Some(Color::from_rgb8(0x1C, 0x1B, 0x1F).into()),
                border: iced::Border {
                    radius: (radius + w * 0.05).into(),
                    width: 1.0,
                    color: c.outline,
                },
                ..Default::default()
            })
            .into()
    }

    /// Conexión y batería en una línea: iconos y texto.
    fn status_line<'a>(&self, c: Colors, p: &Peer) -> Element<'a, Message> {
        let via = p.via();
        let mut line = row![].spacing(6).align_y(Alignment::Center);
        if via.contains("Bluetooth") {
            line = line.push(Icon::Bluetooth.view(16.0, c.on_surface_variant));
        }
        if via.contains("Wi-Fi") {
            line = line.push(Icon::Wifi.view(16.0, c.on_surface_variant));
        }
        if let Some((battery, ..)) = self.snapshot.states.get(&p.id)
            && battery.0
        {
            let low = battery.1 <= 15 && !battery.2;
            line = line
                .push(Icon::Battery.view(16.0, if low { c.error } else { c.on_surface_variant }))
                .push(
                    text(format!(
                        "{} %{}",
                        battery.1,
                        if battery.2 { " ⚡" } else { "" }
                    ))
                    .size(13)
                    .color(if low { c.error } else { c.on_surface_variant }),
                );
        }
        line.into()
    }

    /// Tarjeta de estado del panel (imagen de referencia: Enlace Móvil).
    fn status_card(&self, c: Colors) -> Element<'_, Message> {
        fn card<'a>(c: Colors, content: Element<'a, Message>) -> Element<'a, Message> {
            container(content)
                .padding(14)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::Wide))
                .into()
        }
        let Some(p) = self.phone_peers().into_iter().next() else {
            return card(
                c,
                row![
                    shape_icon(
                        Icon::Phone,
                        Blob::Circle,
                        c.container_highest,
                        c.on_surface_variant,
                        52.0,
                        0.0
                    ),
                    column![
                        text(t("gui.panel.disconnected")).size(16).font(BOLD),
                        text(t("gui.panel.disconnected_detail"))
                            .size(13)
                            .color(c.on_surface_variant),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                ]
                .spacing(14)
                .align_y(Alignment::Center)
                .into(),
            );
        };
        let state = self.snapshot.states.get(&p.id);
        let header = row![
            self.phone_thumb(c, &p.id, 58.0, 112.0),
            column![
                row![
                    text(&p.name).size(17).font(BOLD).width(Length::Fill),
                    button(Icon::OpenInNew.view(18.0, c.on_surface_variant))
                        .padding(6)
                        .style(style::button(c, ButtonKind::Text))
                        .on_press(Message::Navigate(Screen::Phone)),
                ]
                .align_y(Alignment::Center),
                self.status_line(c, p),
                row![
                    text("●").size(12).color(c.primary),
                    text(t("gui.panel.connected")).size(13),
                    text(format!("· {}", p.via()))
                        .size(13)
                        .color(c.on_surface_variant),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            ]
            .spacing(6)
            .width(Length::Fill),
        ]
        .spacing(14)
        .align_y(Alignment::Center);
        let mut col = column![header].spacing(12);

        // Lo que suena ahora (entre el estado y los botones de timbre).
        if let Some((_, _, _, players, ..)) = state
            && let Some(media) = self.media_card(c, &p.id, players)
        {
            col = col.push(media);
        }

        // Timbre: tres botones conectados, como en Enlace Móvil.
        if let Some((_, ringer, ..)) = state
            && !ringer.is_empty()
        {
            let modes = [
                ("normal", Icon::Volume),
                ("vibrate", Icon::Vibration),
                ("silent", Icon::VolumeOff),
            ];
            let seg = row(modes.iter().enumerate().map(|(i, (mode, icon))| {
                let selected = ringer == mode;
                button(
                    container(icon.view(20.0, if selected { c.on_primary } else { c.on_surface }))
                        .center_x(Length::Fill),
                )
                .padding(Padding::from([8, 0]))
                .width(Length::Fill)
                .style(style::button(
                    c,
                    ButtonKind::Segment {
                        selected,
                        first: i == 0,
                        last: i == modes.len() - 1,
                    },
                ))
                .on_press(Message::SetRinger {
                    peer: p.id.clone(),
                    mode,
                })
                .into()
            }))
            .spacing(2);
            col = col.push(seg);
        }

        // Encontrar el teléfono: suena aunque esté en silencio.
        let finding = state.is_some_and(|st| st.6);
        col = col.push(
            button(
                row![
                    Icon::RingVolume.view(
                        18.0,
                        if finding {
                            c.on_primary
                        } else {
                            c.on_secondary_container
                        }
                    ),
                    text(if finding {
                        t("gui.panel.find_stop")
                    } else {
                        t("gui.panel.find")
                    })
                    .size(14)
                    .font(SEMIBOLD),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([8, 14]))
            .width(Length::Fill)
            .style(style::button(
                c,
                if finding {
                    ButtonKind::Filled
                } else {
                    ButtonKind::Tonal
                },
            ))
            .on_press(Message::FindPhone {
                peer: p.id.clone(),
                ring: !finding,
            }),
        );

        // Llamada en curso: quién y los botones para atender o colgar.
        if let Some((_, _, call, ..)) = state
            && call.0 != "idle"
        {
            let who = if call.2.is_empty() { &call.1 } else { &call.2 };
            let peer = p.id.clone();
            let action = |icon: Icon, kind: ButtonKind, fg: Color, action: &'static str| {
                button(icon.view(20.0, fg))
                    .padding(8)
                    .style(style::button(c, kind))
                    .on_press(Message::CallCommand {
                        peer: peer.clone(),
                        action,
                    })
            };
            let mut buttons = row![].spacing(6);
            if call.0 == "ringing" {
                buttons = buttons
                    .push(action(
                        Icon::Phone,
                        ButtonKind::Filled,
                        c.on_primary,
                        "answer",
                    ))
                    .push(action(
                        Icon::CallEnd,
                        ButtonKind::Tonal,
                        c.on_secondary_container,
                        "reject",
                    ));
            } else {
                buttons = buttons.push(action(
                    Icon::CallEnd,
                    ButtonKind::Filled,
                    c.on_primary,
                    "hangup",
                ));
            }
            col = col.push(
                container(
                    row![
                        avatar(c, who, "", 36.0),
                        column![
                            text(who.clone()).size(14).font(BOLD),
                            text(match call.0.as_str() {
                                "ringing" => t("gui.calls.incoming"),
                                "dialing" => t("gui.calls.dialing"),
                                _ => t("gui.calls.active"),
                            })
                            .size(12),
                        ]
                        .width(Length::Fill),
                        buttons,
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .padding(10)
                .style(style::card(
                    c.tertiary_container,
                    c.on_tertiary_container,
                    Shape::List,
                )),
            );
        }
        card(c, col.into())
    }

    /// Reproductor activo (o el último en pausa) con su carátula, controles
    /// y barra de progreso, como en el panel de Android.
    pub(crate) fn media_card<'a>(
        &self,
        c: Colors,
        peer: &str,
        players: &'a [kushare_dbus::PlayerRow],
    ) -> Option<Element<'a, Message>> {
        let p = players
            .iter()
            .find(|p| p.5 == "playing")
            .or_else(|| players.iter().find(|p| !p.2.is_empty()))?;
        let (id, name, title, artist, _album, st, can_play, can_pause, can_next, can_prev, prog) =
            p;
        let (art, position, duration, at) = prog;
        let playing = st == "playing";
        let art_view: Element<'a, Message> = if art.is_empty() {
            container(Icon::Headphones.view(26.0, c.on_secondary_container))
                .center(Length::Fixed(52.0))
                .style(move |_: &Theme| container::Style {
                    background: Some(c.secondary_container.into()),
                    border: iced::Border {
                        radius: 10.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into()
        } else {
            iced::widget::image(iced::widget::image::Handle::from_path(art))
                .width(Length::Fixed(52.0))
                .height(Length::Fixed(52.0))
                .content_fit(iced::ContentFit::Cover)
                .border_radius(10.0)
                .into()
        };
        let media = |action: &'static str| Message::Media {
            peer: peer.to_string(),
            player: id.clone(),
            action,
        };
        let control = |icon: Icon, size: f32, msg: Option<Message>, filled: bool| {
            button(
                container(icon.view(size, if filled { c.on_primary } else { c.on_surface }))
                    .center(Length::Fixed(size + 10.0)),
            )
            .padding(2)
            .style(style::button(
                c,
                if filled {
                    ButtonKind::Filled
                } else {
                    ButtonKind::Text
                },
            ))
            .on_press_maybe(msg)
        };
        let subtitle = if artist.is_empty() { name } else { artist };
        let mut col = column![
            row![
                art_view,
                column![
                    text(if title.is_empty() { name } else { title })
                        .size(14)
                        .font(BOLD),
                    text(subtitle.clone()).size(12).color(c.on_surface_variant),
                ]
                .spacing(2)
                .width(Length::Fill),
                control(
                    Icon::Previous,
                    18.0,
                    can_prev.then(|| media("previous")),
                    false
                ),
                control(
                    if playing { Icon::Pause } else { Icon::Play },
                    20.0,
                    (if playing { *can_pause } else { *can_play }).then(|| media("play-pause")),
                    true,
                ),
                control(Icon::Next, 18.0, can_next.then(|| media("next")), false),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(8);
        // Progreso: extrapolado desde que llegó mientras suena.
        if *duration > 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            let pos = if playing {
                position + (now - at).max(0)
            } else {
                *position
            }
            .clamp(0, *duration);
            let fmt = |ms: i64| format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60);
            col = col
                .push(
                    iced::widget::progress_bar(0.0..=*duration as f32, pos as f32)
                        .girth(4)
                        .style(move |_: &Theme| iced::widget::progress_bar::Style {
                            background: c.container_highest.into(),
                            bar: c.primary.into(),
                            border: iced::Border {
                                radius: 2.0.into(),
                                ..Default::default()
                            },
                        }),
                )
                .push(row![
                    text(fmt(pos)).size(11).color(c.on_surface_variant),
                    space().width(Length::Fill),
                    text(format!("-{}", fmt(duration - pos)))
                        .size(11)
                        .color(c.on_surface_variant),
                ]);
        }
        Some(
            container(col)
                .padding(10)
                .width(Length::Fill)
                .style(style::card(c.container, c.on_surface, Shape::List))
                .into(),
        )
    }

    /// Franja de estado para ventanas sin panel: el teléfono y cuántas
    /// notificaciones tiene (al pulsarla se abren).
    pub(crate) fn status_strip(&self, c: Colors) -> Element<'_, Message> {
        let Some(p) = self.phone_peers().into_iter().next() else {
            return space().into();
        };
        let s = &self.notifs;
        let count = s.list.len() - s.leaving.len().min(s.list.len());
        container(
            row![
                self.phone_thumb(c, &p.id, 22.0, 42.0),
                column![text(&p.name).size(15).font(BOLD), self.status_line(c, p)]
                    .spacing(2)
                    .width(Length::Fill),
                button(
                    row![
                        Icon::Notifications.view(18.0, c.on_secondary_container),
                        text(count.to_string()).size(14).font(BOLD),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([6, 12]))
                .style(style::button(c, ButtonKind::Tonal))
                .on_press(Message::Navigate(Screen::Notifications)),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([8, 14]))
        .width(Length::Fill)
        .style(style::card(c.container_high, c.on_surface, Shape::List))
        .into()
    }
}
