//! Diálogos (emparejar, apps, diagnóstico) y aviso emergente.

use super::*;
use iced::widget::column;

impl App {
    pub(crate) fn pairing_dialog<'a>(&self, c: Colors, p: &'a Pairing) -> Element<'a, Message> {
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

    pub(crate) fn apps_dialog<'a>(&self, c: Colors, f: &'a AppFilter) -> Element<'a, Message> {
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

    pub(crate) fn doctor_dialog<'a>(&self, c: Colors, report: &'a str) -> Element<'a, Message> {
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

    /// Aviso emergente; `away` (0–1) lo baja fuera de la ventana.
    pub(crate) fn toast_view<'a>(
        &self,
        c: Colors,
        message: &'a str,
        away: f32,
    ) -> Element<'a, Message> {
        container(
            iced::widget::float(
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
            .translate(move |b, _| iced::Vector::new(0.0, away * (b.height + 32.0))),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::End)
        .padding(24)
        .into()
    }
}
