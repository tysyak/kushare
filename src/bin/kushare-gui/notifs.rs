//! Página *Notificaciones*: las que hay ahora mismo en el teléfono (todas,
//! también las de antes de conectar), con sus acciones, respuesta rápida
//! y descartar. Se refresca sola cuando llega o se quita una.
//!
//! Las tarjetas entran deslizándose desde la derecha y, al descartarlas
//! (aquí o en el teléfono), salen deslizándose y el hueco se cierra; al
//! descartar todas, salen en cascada.

use super::*;
use iced::widget::text_input;
use iced::widget::{column, float, sensor};
use kushare_dbus::PhoneNotificationRow;
use std::collections::HashSet;
use std::time::{Duration, Instant};

const GONE_GRACE: Duration = Duration::from_secs(3);
/// Caracteres del texto de una notificación que se ven en el panel.
const PANEL_BODY_CHARS: usize = 140;

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
    /// Las que se van → inicio de su salida. Siguen en `list` hasta que
    /// termina la animación.
    pub leaving: HashMap<String, Instant>,
    /// Las recién llegadas → inicio de su entrada.
    pub entering: HashMap<String, Instant>,
    /// Alto de cada tarjeta, para cerrar el hueco al quitarla.
    pub heights: HashMap<String, f32>,
    /// Recién quitadas → cuándo: durante `GONE_GRACE` no se vuelven a
    /// mostrar aunque el teléfono aún las mande (puede tardar en quitarlas).
    /// Pasado ese tiempo, si siguen allí (no se pueden descartar), vuelven.
    pub gone: HashMap<String, Instant>,
}

impl NotifState {
    pub fn animating(&self) -> bool {
        !self.leaving.is_empty() || !self.entering.is_empty()
    }

    /// Empieza la salida de las que aún no se están yendo.
    fn leave<'k>(&mut self, keys: impl IntoIterator<Item = &'k str>, now: Instant) {
        let mut i = 0;
        for key in keys {
            if !self.leaving.contains_key(key) {
                self.entering.remove(key);
                self.leaving
                    .insert(key.to_string(), anim::staggered(now, i));
                i += 1;
            }
        }
    }
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
    /// Alto medido de una tarjeta.
    Measured(String, f32),
}

impl App {
    pub(crate) fn notifs_update(&mut self, msg: Msg) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        let phone = self.phone_peers().first().map(|p| p.id.clone());
        let animate = self.config.animations;
        let now = Instant::now();
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
                        let present: HashSet<String> = list.iter().map(|n| n.0.clone()).collect();
                        s.gone
                            .retain(|k, at| present.contains(k) && *at + GONE_GRACE > now);
                        list.retain(|n| !s.gone.contains_key(&n.0));
                        let old: HashSet<&str> = s.list.iter().map(|n| n.0.as_str()).collect();
                        if animate {
                            // Las nuevas entran en cascada…
                            let new = list.iter().filter(|n| !old.contains(n.0.as_str()));
                            for (i, n) in new.enumerate() {
                                s.entering.insert(n.0.clone(), anim::staggered(now, i));
                            }
                            // …y las que ya no están (quitadas en el teléfono)
                            // se quedan hasta terminar de irse.
                            let removed: Vec<PhoneNotificationRow> = s
                                .list
                                .drain(..)
                                .filter(|n| !present.contains(&n.0))
                                .collect();
                            s.leave(removed.iter().map(|n| n.0.as_str()), now);
                            list.extend(removed);
                        }
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
                if s.leaving.contains_key(&key) {
                    return Task::none();
                }
                if animate {
                    s.leave([key.as_str()], now);
                } else {
                    s.list.retain(|n| n.0 != key);
                }
                let peer = s.peer.clone();
                self.call("", move |d| async move {
                    d.dismiss_phone_notification(&peer, &key).await
                })
            }
            Msg::DismissAll => {
                let keys: Vec<String> = s
                    .list
                    .iter()
                    .filter(|n| !s.leaving.contains_key(&n.0))
                    .map(|n| n.0.clone())
                    .collect();
                if keys.is_empty() {
                    return Task::none();
                }
                if animate {
                    s.leave(keys.iter().map(String::as_str), now);
                } else {
                    s.list.clear();
                }
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
            Msg::Measured(key, height) => {
                // Mientras se va, su alto lo marca la animación.
                if !s.leaving.contains_key(&key) {
                    s.heights.insert(key, height);
                }
                Task::none()
            }
            Msg::Changed(peer) => {
                if s.peer == peer && self.notifs_visible() {
                    Task::done(Message::Notifs(Msg::Load))
                } else {
                    Task::none()
                }
            }
        }
    }

    pub(crate) fn notifs_view(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.notifs;
        let shown = s.list.len() - s.leaving.len().min(s.list.len());
        let col = column![self.title(
            c,
            t("gui.nav.notifications"),
            if shown == 0 {
                t("gui.notifs.subtitle").into()
            } else {
                tf("gui.notifs.count", &[("n", &shown)])
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
                .on_press_maybe((shown > 0).then_some(Message::Notifs(Msg::DismissAll))),
                space().width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
        if let Some(err) = &s.error {
            col = col.push(text(err).color(c.error));
        }
        col.push(
            scrollable(
                container(self.notif_cards(c, false)).padding(Padding::default().right(14.0)),
            )
            .height(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }

    /// Tarjetas de las notificaciones (página o, `compact`, panel lateral).
    pub(crate) fn notif_cards(&self, c: Colors, compact: bool) -> Element<'_, Message> {
        let s = &self.notifs;
        let now = Instant::now();
        let shown = s.list.len() - s.leaving.len().min(s.list.len());
        let (icon_size, pad, title_size, body_size) = if compact {
            (32.0, 12, 14, 13)
        } else {
            (40.0, 14, 16, 14)
        };
        let mut col = column![];
        if shown == 0 && !s.animating() {
            col = col.push(
                text(if s.loading {
                    t("gui.common.loading")
                } else {
                    t("gui.notifs.empty")
                })
                .color(c.on_surface_variant),
            );
        }
        // Sin separación entre tarjetas: cada una lleva la suya debajo, para
        // que el hueco se cierre entero al quitarla.
        let mut cards = column![];
        for (key, _app_id, app, title, body, date, icon, actions) in &s.list {
            let icon_view: Element<'_, Message> = if icon.is_empty() {
                shape_icon(
                    Icon::Notifications,
                    Blob::Circle,
                    c.secondary_container,
                    c.on_secondary_container,
                    icon_size,
                    0.0,
                )
            } else {
                iced::widget::image(iced::widget::image::Handle::from_path(icon))
                    .width(Length::Fixed(icon_size))
                    .height(Length::Fixed(icon_size))
                    .border_radius(icon_size / 2.0)
                    .into()
            };
            let mut buttons = row![].spacing(if compact { 4 } else { 8 });
            for (id, label, is_reply) in actions {
                let msg = if *is_reply {
                    Msg::StartReply(key.clone(), id.clone())
                } else {
                    Msg::Action(key.clone(), id.clone())
                };
                let icon = if *is_reply {
                    Icon::Reply
                } else {
                    Icon::OpenInNew
                };
                let content: Element<'_, Message> = if compact {
                    row![
                        icon.view(14.0, c.primary),
                        text(label).size(12).font(SEMIBOLD)
                    ]
                    .spacing(5)
                    .align_y(Alignment::Center)
                    .into()
                } else {
                    labeled(icon, label, c.primary)
                };
                buttons = buttons.push(
                    button(content)
                        .padding(if compact {
                            Padding::from([4, 10])
                        } else {
                            Padding::from([6, 14])
                        })
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
                text(title.clone()).size(title_size).font(BOLD),
            ]
            .spacing(2);
            if !body.is_empty() {
                // En el panel, solo el principio de los textos largos.
                let body = if compact && body.chars().count() > PANEL_BODY_CHARS {
                    let cut: String = body.chars().take(PANEL_BODY_CHARS).collect();
                    format!("{}…", cut.trim_end())
                } else {
                    body.clone()
                };
                body_col = body_col.push(text(body).size(body_size));
            }
            if !actions.is_empty() {
                // En el panel, estrecho, los botones pasan a la línea siguiente.
                body_col = body_col.push(buttons.wrap().vertical_spacing(4));
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
            let card = container(
                container(
                    row![
                        icon_view,
                        body_col.width(Length::Fill),
                        button(Icon::Close.view(18.0, c.on_surface_variant))
                            .padding(8)
                            .style(style::button(c, ButtonKind::Text))
                            .on_press(Message::Notifs(Msg::Dismiss(key.clone()))),
                    ]
                    .spacing(if compact { 10 } else { 14 }),
                )
                .padding(pad)
                .width(Length::Fill)
                .style(style::card(c.container_high, c.on_surface, Shape::List)),
            )
            .padding(Padding::ZERO.bottom(if compact { 8 } else { 12 }));
            cards = cards.push(self.notif_motion(key, card.into(), now));
        }
        col.push(cards).into()
    }

    /// Envuelve una tarjeta con su animación: entra o sale deslizándose y,
    /// tras salir, su hueco se encoge hasta desaparecer.
    fn notif_motion<'a>(
        &self,
        key: &str,
        card: Element<'a, Message>,
        now: Instant,
    ) -> Element<'a, Message> {
        let s = &self.notifs;
        // Fracción del ancho (más un margen) que está desplazada a la derecha.
        let offset = if let Some(at) = s.leaving.get(key) {
            let p = anim::progress(*at, now, anim::SLIDE_OUT);
            if p >= 1.0 {
                let q = anim::progress(*at + anim::SLIDE_OUT, now, anim::COLLAPSE);
                let h = s.heights.get(key).copied().unwrap_or(0.0);
                return space().height(h * (1.0 - anim::ease_in_out(q))).into();
            }
            anim::ease_in(p)
        } else if let Some(at) = s.entering.get(key) {
            1.0 - anim::ease_out(anim::progress(*at, now, anim::ENTER))
        } else {
            0.0
        };
        let id = key.to_string();
        let measured =
            move |size: iced::Size| Message::Notifs(Msg::Measured(id.clone(), size.height));
        sensor(float(card).translate(move |b, _| iced::Vector::new(offset * (b.width + 40.0), 0.0)))
            .key(key.to_string())
            .on_show(measured.clone())
            .on_resize(measured)
            .into()
    }

    /// Quita las que terminaron de irse y olvida las entradas terminadas.
    pub(crate) fn notifs_frame(&mut self) {
        let now = Instant::now();
        let s = &mut self.notifs;
        let done: Vec<String> = s
            .leaving
            .iter()
            .filter(|(_, at)| now >= **at + anim::SLIDE_OUT + anim::COLLAPSE)
            .map(|(k, _)| k.clone())
            .collect();
        for key in done {
            s.leaving.remove(&key);
            s.heights.remove(&key);
            s.list.retain(|n| n.0 != key);
            if s.reply.as_ref().is_some_and(|r| r.0 == key) {
                s.reply = None;
            }
            s.gone.insert(key, now);
        }
        s.entering.retain(|_, at| now < *at + anim::ENTER);
    }
}
