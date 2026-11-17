//! Inicio y Dispositivos: estado del enlace, teléfonos emparejados y encontrados.

use super::*;
use iced::widget::column;

impl App {
    pub(crate) fn home(&self, c: Colors) -> Element<'_, Message> {
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
                if connected.iter().any(|p| p.via().starts_with("Bluetooth")) {
                    Icon::Bluetooth
                } else {
                    Icon::Wifi
                },
                Blob::Cookie9,
                // Estado estable: sin giro (ver `animating`).
                false,
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

    pub(crate) fn switch_row<'a>(
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

    pub(crate) fn devices(&self, c: Colors) -> Element<'_, Message> {
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

    pub(crate) fn candidate<'a>(
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

    pub(crate) fn peer_card<'a>(&'a self, c: Colors, p: &'a Peer) -> Element<'a, Message> {
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
                0.0,
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
    pub(crate) fn phone_peers(&self) -> Vec<&Peer> {
        self.snapshot
            .peers
            .iter()
            .filter(|p| p.connected && p.kind == "android")
            .collect()
    }

    pub(crate) fn empty_phone<'a>(&self, c: Colors, what: &'a str) -> Element<'a, Message> {
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

    pub(crate) fn icon_button<'a>(
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

    pub(crate) fn phone(&self, c: Colors) -> Element<'_, Message> {
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
}
