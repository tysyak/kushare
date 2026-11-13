//! Página *Mensajes*: conversaciones de SMS del teléfono, con el hilo
//! abierto a la derecha (burbujas) y un cuadro para responder. Se refresca
//! sola con la señal `SmsChanged` del daemon.

use super::*;
use iced::widget::column;
use iced::widget::text_input;
use kushare_dbus::{SmsRow, SmsThreadRow};

const THREAD_PAGE: u32 = 40;
const MESSAGE_PAGE: u32 = 60;

#[derive(Debug, Clone, Default)]
pub struct SmsState {
    pub peer: String,
    pub threads: Vec<SmsThreadRow>,
    pub total: u32,
    pub loading: bool,
    pub error: Option<String>,
    /// Conversación abierta.
    pub selected: Option<String>,
    pub messages: Vec<SmsRow>,
    pub messages_loading: bool,
    /// No hay más mensajes antiguos que pedir.
    pub messages_complete: bool,
    pub draft: String,
    /// Mensaje nuevo: número de destino (sin conversación abierta).
    pub new_to: Option<String>,
    pub sending: bool,
    pub search: String,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Load,
    Loaded(String, Result<(u32, Vec<SmsThreadRow>), String>),
    Open(String),
    MessagesLoaded {
        thread: String,
        older: bool,
        result: Result<Vec<SmsRow>, String>,
    },
    LoadOlder,
    Draft(String),
    Search(String),
    Send,
    Sent(Result<(), String>),
    New,
    NewTo(String),
    /// Volver a la lista (ventana estrecha: se ve una cosa u otra).
    Back,
    /// Señal del daemon: (peer, conversación).
    Changed(String, String),
}

impl App {
    pub(crate) fn sms_update(&mut self, msg: Msg) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        let phone = self.phone_peers().first().map(|p| p.id.clone());
        let s = &mut self.sms;
        match msg {
            Msg::Load => {
                let Some(peer) = phone else {
                    return Task::none();
                };
                if s.peer != peer {
                    *s = SmsState {
                        peer: peer.clone(),
                        ..Default::default()
                    };
                }
                if s.loading {
                    return Task::none();
                }
                s.loading = true;
                let mut tasks = vec![Task::perform(
                    {
                        let d = d.clone();
                        let peer = peer.clone();
                        async move {
                            let r = d.sms_threads(&peer, 0, THREAD_PAGE).await.map_err(err_text);
                            (peer, r)
                        }
                    },
                    |(peer, r)| Message::Sms(Msg::Loaded(peer, r)),
                )];
                if let Some(thread) = s.selected.clone() {
                    tasks.push(load_messages(d, peer, thread, 0, false));
                }
                Task::batch(tasks)
            }
            Msg::Loaded(peer, result) => {
                if s.peer == peer {
                    s.loading = false;
                    match result {
                        Ok((total, threads)) => {
                            s.total = total;
                            s.threads = threads;
                            s.error = None;
                        }
                        Err(e) => s.error = Some(e),
                    }
                }
                Task::none()
            }
            Msg::Open(thread) => {
                s.selected = Some(thread.clone());
                s.new_to = None;
                s.messages.clear();
                s.messages_complete = false;
                s.messages_loading = true;
                s.draft.clear();
                load_messages(d, s.peer.clone(), thread, 0, false)
            }
            Msg::MessagesLoaded {
                thread,
                older,
                result,
            } => {
                if s.selected.as_deref() != Some(thread.as_str()) {
                    return Task::none();
                }
                s.messages_loading = false;
                match result {
                    Ok(list) => {
                        if older {
                            s.messages_complete = (list.len() as u32) < MESSAGE_PAGE;
                            s.messages.extend(list);
                        } else {
                            s.messages_complete = (list.len() as u32) < MESSAGE_PAGE;
                            s.messages = list;
                        }
                    }
                    Err(e) => s.error = Some(e),
                }
                Task::none()
            }
            Msg::LoadOlder => {
                let (Some(thread), Some(oldest)) = (s.selected.clone(), s.messages.last()) else {
                    return Task::none();
                };
                if s.messages_loading || s.messages_complete {
                    return Task::none();
                }
                s.messages_loading = true;
                load_messages(d, s.peer.clone(), thread, oldest.3, true)
            }
            Msg::Draft(text) => {
                s.draft = text;
                Task::none()
            }
            Msg::Search(text) => {
                s.search = text;
                Task::none()
            }
            Msg::New => {
                s.selected = None;
                s.messages.clear();
                s.new_to = Some(String::new());
                s.draft.clear();
                Task::none()
            }
            Msg::Back => {
                s.selected = None;
                s.new_to = None;
                s.messages.clear();
                Task::none()
            }
            Msg::NewTo(to) => {
                s.new_to = Some(to);
                Task::none()
            }
            Msg::Send => {
                let address = match (&s.new_to, &s.selected) {
                    (Some(to), _) => to.trim().to_string(),
                    (None, Some(thread)) => s
                        .threads
                        .iter()
                        .find(|t| &t.0 == thread)
                        .map(|t| t.1.clone())
                        .or_else(|| {
                            s.messages
                                .iter()
                                .find(|m| !m.1.is_empty())
                                .map(|m| m.1.clone())
                        })
                        .unwrap_or_default(),
                    (None, None) => String::new(),
                };
                let body = s.draft.trim().to_string();
                if address.is_empty() || body.is_empty() || s.sending {
                    return Task::none();
                }
                s.sending = true;
                let peer = s.peer.clone();
                Task::perform(
                    async move { d.send_sms(&peer, &address, &body).await.map_err(err_text) },
                    |r| Message::Sms(Msg::Sent(r)),
                )
            }
            Msg::Sent(result) => {
                s.sending = false;
                match result {
                    Ok(()) => {
                        s.draft.clear();
                        self.toast = Some(t("gui.sms.sent").into());
                        // El teléfono avisa con SmsChanged; por si acaso, refrescar.
                        Task::done(Message::Sms(Msg::Load))
                    }
                    Err(e) => {
                        self.toast = Some(e);
                        Task::none()
                    }
                }
            }
            Msg::Changed(peer, _thread) => {
                if s.peer == peer && self.screen == Screen::Messages {
                    Task::done(Message::Sms(Msg::Load))
                } else {
                    Task::none()
                }
            }
        }
    }

    pub(crate) fn sms_view(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.sms;
        let col = column![self.title(
            c,
            t("gui.nav.messages"),
            if s.total > 0 {
                tf("gui.sms.count", &[("n", &s.total)])
            } else {
                t("gui.sms.subtitle").into()
            }
        )]
        .spacing(14);
        if self.phone_peers().is_empty() {
            return col
                .push(self.empty_phone(c, t("gui.sms.none_detail")))
                .into();
        }
        let mut col = col.push(
            row![
                text_input(t("gui.sms.search"), &s.search)
                    .on_input(|v| Message::Sms(Msg::Search(v)))
                    .padding(10)
                    .width(Length::Fill),
                button(labeled(Icon::Add, t("gui.sms.new"), c.on_primary))
                    .padding(Padding::from([9, 18]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::Sms(Msg::New)),
                button(labeled(
                    Icon::Refresh,
                    t("gui.photos.reload"),
                    c.on_secondary_container
                ))
                .padding(Padding::from([9, 18]))
                .style(style::button(c, ButtonKind::Tonal))
                .on_press_maybe((!s.loading).then_some(Message::Sms(Msg::Load))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
        if let Some(err) = &s.error {
            col = col.push(text(err).color(c.error));
        }

        // Lista de conversaciones.
        let q = fold(&s.search);
        let mut list = column![].spacing(6);
        if s.threads.is_empty() {
            list = list.push(
                text(if s.loading {
                    t("gui.common.loading")
                } else {
                    t("gui.sms.empty")
                })
                .color(c.on_surface_variant),
            );
        }
        for th in s.threads.iter().filter(|th| {
            q.is_empty()
                || fold(&th.2).contains(&q)
                || th.1.contains(&q)
                || fold(&th.3).contains(&q)
        }) {
            let (id, address, name, snippet, date, unread, _count, outgoing) = th;
            let selected = s.selected.as_deref() == Some(id.as_str());
            let who = if name.is_empty() { address } else { name };
            // Nombres sin espacios muy largos (códigos) no se pueden partir:
            // recortarlos para que no tapen la fecha.
            let who = if who.chars().count() > 32 {
                format!("{}…", who.chars().take(31).collect::<String>())
            } else {
                who.clone()
            };
            let preview: String = snippet.replace('\n', " ").chars().take(60).collect();
            let preview = if *outgoing {
                tf("gui.sms.you", &[("text", &preview)])
            } else {
                preview
            };
            list = list.push(
                button(
                    row![
                        avatar(c, &who, "", 40.0),
                        column![
                            row![
                                text(who.clone())
                                    .size(15)
                                    .font(if *unread { BOLD } else { SEMIBOLD })
                                    .width(Length::Fill),
                                text(fmt_time(*date)).size(12).color(if *unread {
                                    c.primary
                                } else {
                                    c.on_surface_variant
                                }),
                            ]
                            .spacing(6),
                            text(preview).size(13).color(c.on_surface_variant),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                )
                .padding(10)
                .width(Length::Fill)
                .style(style::button(
                    c,
                    ButtonKind::Card {
                        bg: if selected {
                            c.secondary_container
                        } else {
                            c.container
                        },
                        fg: c.on_surface,
                        shape: Shape::List,
                    },
                ))
                .on_press(Message::Sms(Msg::Open(id.clone()))),
            );
        }
        let threads = container(
            scrollable(list.padding(Padding::default().right(10.0))).height(Length::Fill),
        )
        .height(Length::Fill);
        let conversation = container(self.sms_conversation(c))
            .padding(16)
            .height(Length::Fill)
            .style(style::card(c.container_high, c.on_surface, Shape::List));
        // Ventana estrecha: la lista o la conversación abierta, no ambas.
        let open = s.selected.is_some() || s.new_to.is_some();
        let body: Element<'_, Message> = match (self.layout().compact, open) {
            (true, false) => threads.width(Length::Fill).into(),
            (true, true) => conversation.width(Length::Fill).into(),
            (false, _) => row![
                threads.width(Length::FillPortion(2)),
                conversation.width(Length::FillPortion(3))
            ]
            .spacing(14)
            .height(Length::Fill)
            .into(),
        };
        col.push(body).height(Length::Fill).into()
    }

    fn sms_conversation(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.sms;
        let back: Element<'_, Message> = if self.layout().compact {
            button(Icon::ChevronLeft.view(22.0, c.on_surface))
                .padding(6)
                .style(style::button(c, ButtonKind::Text))
                .on_press(Message::Sms(Msg::Back))
                .into()
        } else {
            space().width(0).into()
        };
        let header: Element<'_, Message> = match (&s.new_to, &s.selected) {
            (Some(to), _) => row![
                back,
                text(t("gui.sms.to")).font(SEMIBOLD),
                text_input(t("gui.sms.number"), to)
                    .on_input(|v| Message::Sms(Msg::NewTo(v)))
                    .padding(8)
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into(),
            (None, Some(thread)) => {
                let th = s.threads.iter().find(|t| &t.0 == thread);
                let name = th
                    .map(|t| {
                        if t.2.is_empty() {
                            t.1.clone()
                        } else {
                            t.2.clone()
                        }
                    })
                    .unwrap_or_default();
                let number = th.map(|t| t.1.clone()).unwrap_or_default();
                row![
                    back,
                    avatar(c, &name, "", 36.0),
                    column![
                        text(name.clone()).size(18).font(BOLD),
                        text(number.clone()).size(12).color(c.on_surface_variant)
                    ]
                    .width(Length::Fill),
                    button(Icon::Phone.view(20.0, c.on_secondary_container))
                        .padding(8)
                        .style(style::button(c, ButtonKind::Tonal))
                        .on_press_maybe(
                            (!number.is_empty())
                                .then(|| Message::Calls(super::calls::Msg::Dial(number.clone())))
                        ),
                ]
                .spacing(10)
                .align_y(Alignment::Center)
                .into()
            }
            (None, None) => {
                return center(
                    column![
                        Icon::Message.view(48.0, c.on_surface_variant),
                        text(t("gui.sms.pick")).color(c.on_surface_variant)
                    ]
                    .spacing(10)
                    .align_x(Alignment::Center),
                )
                .into();
            }
        };

        let mut bubbles = column![].spacing(8);
        if !s.messages_complete && !s.messages.is_empty() {
            bubbles = bubbles.push(
                container(
                    button(labeled(Icon::ExpandMore, t("gui.sms.older"), c.primary))
                        .style(style::button(c, ButtonKind::Text))
                        .on_press_maybe(
                            (!s.messages_loading).then_some(Message::Sms(Msg::LoadOlder)),
                        ),
                )
                .center_x(Length::Fill),
            );
        }
        if s.messages_loading && s.messages.is_empty() {
            bubbles = bubbles.push(text(t("gui.common.loading")).color(c.on_surface_variant));
        }
        for m in s.messages.iter().rev() {
            let (_id, _addr, body, date, outgoing, _read, failed) = m;
            let (bg, fg) = if *outgoing {
                (c.primary, c.on_primary)
            } else {
                (c.container_highest, c.on_surface)
            };
            let mut meta = fmt_time(*date);
            if *failed {
                meta = format!("{meta} · {}", t("gui.sms.failed"));
            }
            let bubble = container(
                column![
                    text(body.clone()).size(15).color(fg),
                    text(meta).size(11).color(Color { a: 0.75, ..fg }),
                ]
                .spacing(4),
            )
            .padding(Padding::from([8, 12]))
            .max_width(380)
            .style(style::card(bg, fg, Shape::List));
            bubbles = bubbles.push(if *outgoing {
                row![space().width(Length::Fill), bubble]
            } else {
                row![bubble, space().width(Length::Fill)]
            });
        }
        let bubbles = scrollable(bubbles)
            .anchor_bottom()
            .height(Length::Fill)
            .width(Length::Fill);

        let can_send = !s.draft.trim().is_empty() && !s.sending;
        let composer = row![
            text_input(t("gui.sms.write"), &s.draft)
                .on_input(|v| Message::Sms(Msg::Draft(v)))
                .on_submit(Message::Sms(Msg::Send))
                .padding(10)
                .width(Length::Fill),
            button(Icon::Send.view(20.0, c.on_primary))
                .padding(10)
                .style(style::button(c, ButtonKind::Filled))
                .on_press_maybe(can_send.then_some(Message::Sms(Msg::Send))),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![header, bubbles, composer].spacing(12).into()
    }
}

fn load_messages(
    d: DaemonProxy<'static>,
    peer: String,
    thread: String,
    before: i64,
    older: bool,
) -> Task<Message> {
    Task::perform(
        async move {
            let r = d
                .sms_messages(&peer, &thread, MESSAGE_PAGE, before)
                .await
                .map_err(err_text);
            (thread, r)
        },
        move |(thread, result)| {
            Message::Sms(Msg::MessagesLoaded {
                thread,
                older,
                result,
            })
        },
    )
}
