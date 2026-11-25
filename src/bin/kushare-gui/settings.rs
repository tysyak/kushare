//! Páginas Enviar y Ajustes, y el aviso de actualizaciones.

use super::*;
use iced::widget::column;

impl App {
    pub(crate) fn send(&self, c: Colors) -> Element<'_, Message> {
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

    pub(crate) fn settings(&self, c: Colors) -> Element<'_, Message> {
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
        // Posición del menú de pestañas.
        let nav_seg = row(layout::NavPosition::ALL.iter().enumerate().map(|(i, nav)| {
            let selected = self.config.nav == *nav;
            button(
                text(nav.label())
                    .size(13)
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
                    last: i == layout::NavPosition::ALL.len() - 1,
                },
            ))
            .on_press(Message::SetNav(*nav))
            .into()
        }))
        .spacing(3);
        theme_card = theme_card
            .push(space().height(6))
            .push(text(t("gui.settings.nav")).size(16).font(SEMIBOLD))
            .push(
                text(t("gui.settings.nav_detail"))
                    .size(13)
                    .color(c.on_surface_variant),
            )
            .push(space().height(4))
            .push(nav_seg)
            .push(self.switch_row(
                c,
                t("gui.settings.side_panel"),
                Some(t("gui.settings.side_panel_detail")),
                self.config.side_panel,
                Message::SetSidePanel,
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
                t("gui.settings.hybrid"),
                Some(t("gui.settings.hybrid_detail")),
                self.setting("hybrid"),
                |on| Message::SetSetting("hybrid".into(), on),
            ),
            self.switch_row(
                c,
                t("gui.settings.dynamic_lock"),
                Some(t("gui.settings.dynamic_lock_detail")),
                self.setting("dynamic-lock"),
                |on| Message::SetSetting("dynamic-lock".into(), on),
            ),
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
                t("gui.settings.photo_notify"),
                Some(t("gui.settings.photo_notify_detail")),
                self.setting("photo-notify"),
                |on| Message::SetSetting("photo-notify".into(), on),
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
            self.section(c, t("gui.hotspot.section")),
            container(self.hotspot_card(c))
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

    pub(crate) fn update_card(&self, c: Colors) -> Element<'_, Message> {
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

    pub(crate) fn dialog<'a>(
        &self,
        c: Colors,
        content: Element<'a, Message>,
    ) -> Element<'a, Message> {
        opaque(
            container(center(container(content).padding(32).max_width(520).style(
                style::card(c.container_high, c.on_surface, Shape::Dialog),
            )))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(style::scrim),
        )
    }
}
