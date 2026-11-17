//! Página Teléfono: fondo de pantalla, timbre, multimedia, audio y llamada.

use super::*;
use iced::widget::column;

impl App {
    /// Cabecera de la tarjeta del teléfono sobre su fondo de pantalla (o un
    /// degradado con sus colores, si Android no deja leer la imagen).
    pub(crate) fn wallpaper_banner<'a>(
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
    pub(crate) fn local_audio_card(&self, c: Colors) -> Element<'_, Message> {
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

    pub(crate) fn phone_card<'a>(&'a self, c: Colors, p: &'a Peer) -> Element<'a, Message> {
        let Some((battery, ringer, call, players, selectable, outputs, _)) =
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
            button(Icon::RingVolume.view(18.0, c.on_surface))
                .padding(8)
                .style(style::button(c, ButtonKind::Text))
                .on_press(Message::FindPhone {
                    peer: p.id.clone(),
                    ring: !self.snapshot.states.get(&p.id).is_some_and(|st| st.6),
                }),
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
}
