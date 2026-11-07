//! Página *Notificaciones*: las que hay ahora mismo en el teléfono (todas,
//! también las de antes de conectar), con sus acciones, respuesta rápida
//! y descartar. Se refresca sola cuando llega o se quita una.

use super::*;
use iced::widget::column;
use iced::widget::text_input;
use kushare_dbus::PhoneNotificationRow;

#[derive(Debug, Clone, Default)]
pub struct NotifState {
    pub peer: String,
    pub list: Vec<PhoneNotificationRow>,
    pub loading: bool,
    /// Llegó un cambio mientras se cargaba: volver a pedir.
    pub again: bool,
    pub error: Option<String>,
    /// Respuesta en curso: (key, acción, texto).
    pub reply: Option<(String, String, String)>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Load,
    Loaded(String, Result<Vec<PhoneNotificationRow>, String>),
    Dismiss(String),
    DismissAll,
    Action(String, String),
    StartReply(String, String),
    ReplyText(String),
    SendReply,
    CancelReply,
    Changed(String),
}

impl App {
    pub(crate) fn notifs_update(&mut self, msg: Msg) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        let phone = self.phone_peers().first().map(|p| p.id.clone());
        let s = &mut self.notifs;
        match msg {
            Msg::Load => {
                let Some(peer) = phone else {
                    return Task::none();
                };
                if s.peer != peer {
                    *s = NotifState {
                        peer: peer.clone(),
                        ..Default::default()
                    };
                }
                if s.loading {
                    s.again = true;
                    return Task::none();
                }
                s.loading = true;
                Task::perform(
                    async move {
                        (
                            peer.clone(),
                            d.phone_notifications(&peer).await.map_err(err_text),
                        )
                    },
                    |(p, r)| Message::Notifs(Msg::Loaded(p, r)),
                )
            }
            Msg::Loaded(peer, result) => {
                if s.peer != peer {
                    return Task::none();
                }
                s.loading = false;
                match result {
                    Ok(mut list) => {
                        list.sort_by_key(|n| std::cmp::Reverse(n.5));
                        s.list = list;
                        s.error = None;
                    }
                    Err(e) => s.error = Some(e),
                }
                if std::mem::take(&mut s.again) {
                    return Task::done(Message::Notifs(Msg::Load));
                }
                Task::none()
            }
            Msg::Dismiss(key) => {
                s.list.retain(|n| n.0 != key);
                let peer = s.peer.clone();
                self.call("", move |d| async move {
                    d.dismiss_phone_notification(&peer, &key).await
                })
            }
            Msg::DismissAll => {
                let keys: Vec<String> = s.list.drain(..).map(|n| n.0).collect();
                let peer = s.peer.clone();
                self.call(t("gui.notifs.dismissed_all"), move |d| async move {
                    for key in keys {
                        d.dismiss_phone_notification(&peer, &key).await?;
                    }
                    Ok(())
                })
            }
            Msg::Action(key, action) => {
                let peer = s.peer.clone();
                self.call(t("gui.notifs.done"), move |d| async move {
                    d.phone_notification_action(&peer, &key, &action).await
                })
            }
            Msg::StartReply(key, action) => {
                s.reply = Some((key, action, String::new()));
                Task::none()
            }
            Msg::ReplyText(v) => {
                if let Some(r) = &mut s.reply {
                    r.2 = v;
                }
                Task::none()
            }
            Msg::CancelReply => {
                s.reply = None;
                Task::none()
            }
            Msg::SendReply => {
                let Some((key, action, text)) = s.reply.take() else {
                    return Task::none();
                };
                if text.trim().is_empty() {
                    return Task::none();
                }
                let peer = s.peer.clone();
                self.call(t("gui.notifs.replied"), move |d| async move {
                    d.phone_notification_reply(&peer, &key, &action, &text)
                        .await
                })
            }
            Msg::Changed(peer) => {
                if s.peer == peer && self.screen == Screen::Notifications {
                    Task::done(Message::Notifs(Msg::Load))
                } else {
                    Task::none()
                }
            }
        }
    }

    pub(crate) fn notifs_view(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.notifs;
        let col = column![self.title(
            c,
            t("gui.nav.notifications"),
            if s.list.is_empty() {
                t("gui.notifs.subtitle").into()
            } else {
                tf("gui.notifs.count", &[("n", &s.list.len())])
            }
        )]
        .spacing(14);
        if self.phone_peers().is_empty() {
            return col
                .push(self.empty_phone(c, t("gui.notifs.none_detail")))
                .into();
        }
        let mut col = col.push(
            row![
                button(labeled(
                    Icon::Refresh,
                    t("gui.photos.reload"),
                    c.on_secondary_container
                ))
                .padding(Padding::from([9, 18]))
                .style(style::button(c, ButtonKind::Tonal))
                .on_press_maybe((!s.loading).then_some(Message::Notifs(Msg::Load))),
                button(labeled(
                    Icon::Delete,
                    t("gui.notifs.dismiss_all"),
                    c.primary
                ))
                .padding(Padding::from([9, 18]))
                .style(style::button(c, ButtonKind::Outlined))
                .on_press_maybe((!s.list.is_empty()).then_some(Message::Notifs(Msg::DismissAll))),
                space().width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
        if let Some(err) = &s.error {
            col = col.push(text(err).color(c.error));
        }
        if s.list.is_empty() {
            col = col.push(
                text(if s.loading {
                    t("gui.common.loading")
                } else {
                    t("gui.notifs.empty")
                })
                .color(c.on_surface_variant),
            );
        }
        for (key, _app_id, app, title, body, date, icon, actions) in &s.list {
            let icon_view: Element<'_, Message> = if icon.is_empty() {
                shape_icon(
                    Icon::Notifications,
                    Blob::Circle,
                    c.secondary_container,
                    c.on_secondary_container,
                    40.0,
                    0.0,
                )
            } else {
                iced::widget::image(iced::widget::image::Handle::from_path(icon))
                    .width(Length::Fixed(40.0))
                    .height(Length::Fixed(40.0))
                    .border_radius(20.0)
                    .into()
            };
            let mut buttons = row![].spacing(8);
            for (id, label, is_reply) in actions {
                let msg = if *is_reply {
                    Msg::StartReply(key.clone(), id.clone())
                } else {
                    Msg::Action(key.clone(), id.clone())
                };
                buttons = buttons.push(
                    button(labeled(
                        if *is_reply {
                            Icon::Reply
                        } else {
                            Icon::OpenInNew
                        },
                        label,
                        c.primary,
                    ))
                    .padding(Padding::from([6, 14]))
                    .style(style::button(c, ButtonKind::Outlined))
                    .on_press(Message::Notifs(msg)),
                );
            }
            let mut body_col = column![
                row![
                    text(app.clone()).size(12).color(c.on_surface_variant),
                    text("·").size(12).color(c.on_surface_variant),
                    text(fmt_time(*date)).size(12).color(c.on_surface_variant),
                ]
                .spacing(6),
                text(title.clone()).size(16).font(BOLD),
            ]
            .spacing(2);
            if !body.is_empty() {
                body_col = body_col.push(text(body.clone()).size(14));
            }
            if !actions.is_empty() {
                body_col = body_col.push(buttons);
            }
            if let Some((rkey, _, draft)) = &s.reply
                && rkey == key
            {
                body_col = body_col.push(
                    row![
                        text_input(t("gui.notifs.reply_hint"), draft)
                            .on_input(|v| Message::Notifs(Msg::ReplyText(v)))
                            .on_submit(Message::Notifs(Msg::SendReply))
                            .padding(8)
                            .width(Length::Fill),
                        button(Icon::Send.view(18.0, c.on_primary))
                            .padding(8)
                            .style(style::button(c, ButtonKind::Filled))
                            .on_press(Message::Notifs(Msg::SendReply)),
                        button(Icon::Close.view(18.0, c.on_surface_variant))
                            .padding(8)
                            .style(style::button(c, ButtonKind::Text))
                            .on_press(Message::Notifs(Msg::CancelReply)),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center),
                );
            }
            col = col.push(
                container(
                    row![
                        icon_view,
                        body_col.width(Length::Fill),
                        button(Icon::Close.view(18.0, c.on_surface_variant))
                            .padding(8)
                            .style(style::button(c, ButtonKind::Text))
                            .on_press(Message::Notifs(Msg::Dismiss(key.clone()))),
                    ]
                    .spacing(14),
                )
                .padding(14)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::List)),
            );
        }
        col.into()
    }
}
