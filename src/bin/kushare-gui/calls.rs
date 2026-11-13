//! Página *Llamadas*: teclado con letras (como el de un teléfono: 2 = ABC…)
//! que busca contactos por número, por nombre o por sus teclas (T9), y el
//! registro de llamadas recientes. La llamada la hace el teléfono; el
//! audio se oye en el PC si está activado *Escuchar aquí*.

use super::*;
use iced::widget::column;
use iced::widget::text_input;
use kushare_dbus::{CallLogRow, ContactRow};

#[derive(Debug, Clone, Default)]
pub struct CallsState {
    pub peer: String,
    pub log: Vec<CallLogRow>,
    pub contacts: Vec<ContactRow>,
    pub loading: bool,
    pub error: Option<String>,
    pub input: String,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Load,
    LogLoaded(String, Result<Vec<CallLogRow>, String>),
    ContactsLoaded(String, Result<Vec<ContactRow>, String>),
    Input(String),
    Key(char),
    Backspace,
    /// Llamar a este número (vacío = lo escrito en el teclado).
    Dial(String),
    Dialed(Result<(), String>),
}

/// Teclas del marcador: (tecla, letras).
const KEYS: [(char, &str); 12] = [
    ('1', ""),
    ('2', "ABC"),
    ('3', "DEF"),
    ('4', "GHI"),
    ('5', "JKL"),
    ('6', "MNO"),
    ('7', "PQRS"),
    ('8', "TUV"),
    ('9', "WXYZ"),
    ('*', ""),
    ('0', "+"),
    ('#', ""),
];

/// Tecla de una letra (sin acentos): 'á' → '2'.
fn key_of(c: char) -> Option<char> {
    let c = fold(&c.to_string()).chars().next()?.to_ascii_uppercase();
    if c.is_ascii_digit() {
        return Some(c);
    }
    KEYS.iter()
        .find(|(_, letters)| letters.contains(c))
        .map(|(k, _)| *k)
}

/// El nombre escrito con las teclas del teléfono ("Ana" → "262").
fn t9(name: &str) -> String {
    name.chars().filter_map(key_of).collect()
}

/// Solo los dígitos de un número (para comparar "+52 55 1234" con "5551234").
fn digits(number: &str) -> String {
    number.chars().filter(char::is_ascii_digit).collect()
}

/// ¿El contacto coincide con lo escrito? Admite número, nombre o teclas.
fn matches(contact: &ContactRow, input: &str) -> bool {
    let q = input.trim();
    if q.is_empty() {
        return false;
    }
    let has_letters = q.chars().any(char::is_alphabetic);
    if has_letters {
        return fold(&contact.0).contains(&fold(q));
    }
    let qd = digits(q);
    if qd.is_empty() {
        return false;
    }
    // Por teclas: alguna palabra del nombre empieza con esas teclas, o todo el nombre.
    let by_keys = contact.0.split_whitespace().any(|w| t9(w).starts_with(&qd))
        || t9(&contact.0).starts_with(&qd);
    by_keys || contact.1.iter().any(|n| digits(n).contains(&qd))
}

impl App {
    pub(crate) fn calls_update(&mut self, msg: Msg) -> Task<Message> {
        let Some(d) = self.daemon.clone() else {
            return Task::none();
        };
        let phone = self.phone_peers().first().map(|p| p.id.clone());
        let s = &mut self.calls;
        match msg {
            Msg::Load => {
                let Some(peer) = phone else {
                    return Task::none();
                };
                if s.peer != peer {
                    *s = CallsState {
                        peer: peer.clone(),
                        ..Default::default()
                    };
                }
                if s.loading {
                    return Task::none();
                }
                s.loading = true;
                let (d2, p2) = (d.clone(), peer.clone());
                Task::batch([
                    Task::perform(
                        async move { (peer.clone(), d.call_log(&peer, 100).await.map_err(err_text)) },
                        |(p, r)| Message::Calls(Msg::LogLoaded(p, r)),
                    ),
                    Task::perform(
                        async move { (p2.clone(), d2.contacts(&p2, false).await.map_err(err_text)) },
                        |(p, r)| Message::Calls(Msg::ContactsLoaded(p, r)),
                    ),
                ])
            }
            Msg::LogLoaded(peer, result) => {
                if s.peer == peer {
                    s.loading = false;
                    match result {
                        Ok(log) => {
                            s.log = log;
                            s.error = None;
                        }
                        Err(e) => s.error = Some(e),
                    }
                }
                Task::none()
            }
            Msg::ContactsLoaded(peer, result) => {
                if s.peer == peer {
                    match result {
                        Ok(list) => s.contacts = list,
                        Err(e) => s.error = Some(e),
                    }
                }
                Task::none()
            }
            Msg::Input(v) => {
                s.input = v;
                Task::none()
            }
            Msg::Key(k) => {
                s.input.push(k);
                Task::none()
            }
            Msg::Backspace => {
                s.input.pop();
                Task::none()
            }
            Msg::Dial(number) => {
                let number = if number.is_empty() {
                    s.input.clone()
                } else {
                    number
                };
                if number.trim().is_empty() {
                    return Task::none();
                }
                let Some(peer) = phone else {
                    return Task::none();
                };
                self.toast = Some(tf("gui.calls.calling", &[("number", &number)]));
                Task::perform(
                    async move { d.dial(&peer, &number).await.map_err(err_text) },
                    |r| Message::Calls(Msg::Dialed(r)),
                )
            }
            Msg::Dialed(Ok(())) => Task::none(),
            Msg::Dialed(Err(e)) => {
                self.toast = Some(e);
                Task::none()
            }
        }
    }

    pub(crate) fn calls_view(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.calls;
        let compact = self.layout().compact;
        let col =
            column![self.title(c, t("gui.nav.calls"), t("gui.calls.subtitle").into())].spacing(14);
        if self.phone_peers().is_empty() {
            return col
                .push(self.empty_phone(c, t("gui.calls.none_detail")))
                .into();
        }
        let mut col = col;
        if let Some(err) = &s.error {
            col = col.push(text(err).color(c.error));
        }
        // Como en Enlace Móvil: los recientes a la izquierda, ocupando el
        // alto, y el teclado a la derecha. En ventanas estrechas, apilados.
        if compact {
            return col
                .push(self.dial_pad(c))
                .push(self.recent_calls(c, false))
                .into();
        }
        col.push(
            row![
                container(self.recent_calls(c, true))
                    .width(Length::FillPortion(3))
                    .height(Length::Fill),
                container(scrollable(self.dial_pad(c)).height(Length::Fill))
                    .width(Length::FillPortion(2))
                    .height(Length::Fill),
            ]
            .spacing(16)
            .height(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }

    /// Teclado, botón de llamar y contactos que coinciden con lo escrito.
    fn dial_pad(&self, c: Colors) -> Element<'_, Message> {
        let s = &self.calls;
        let display = row![
            text_input(t("gui.calls.type"), &s.input)
                .on_input(|v| Message::Calls(Msg::Input(v)))
                .on_submit(Message::Calls(Msg::Dial(String::new())))
                .size(26)
                .padding(10)
                .width(Length::Fill),
            button(Icon::Backspace.view(22.0, c.on_surface_variant))
                .padding(10)
                .style(style::button(c, ButtonKind::Text))
                .on_press_maybe((!s.input.is_empty()).then_some(Message::Calls(Msg::Backspace))),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let mut pad = column![display].spacing(10);

        // Contactos que coinciden (por número, nombre o teclas), arriba del
        // teclado para verlos mientras se escribe.
        if !s.input.is_empty() {
            let found: Vec<&ContactRow> = s
                .contacts
                .iter()
                .filter(|ct| matches(ct, &s.input))
                .take(4)
                .collect();
            for ct in found {
                for n in ct.1.iter().take(2) {
                    pad = pad.push(self.call_row(c, &ct.0, &ct.3, None, n.clone(), n.clone()));
                }
            }
        }
        for chunk in KEYS.chunks(3) {
            let mut r = row![].spacing(10);
            for (key, letters) in chunk {
                r = r.push(
                    button(
                        column![
                            text(key.to_string()).size(26).font(BOLD),
                            text(*letters).size(11).color(c.on_surface_variant),
                        ]
                        .align_x(Alignment::Center)
                        .width(Length::Fill),
                    )
                    .padding(Padding::from([10, 0]))
                    .width(Length::FillPortion(1))
                    .style(style::button(
                        c,
                        ButtonKind::Card {
                            bg: c.container_highest,
                            fg: c.on_surface,
                            shape: Shape::List,
                        },
                    ))
                    .on_press(Message::Calls(Msg::Key(*key))),
                );
            }
            pad = pad.push(r);
        }
        let number = super::dialable_preview(&s.input);
        pad = pad.push(
            button(
                row![
                    Icon::Phone.view(22.0, c.on_primary),
                    text(if number.is_empty() {
                        t("gui.calls.call").to_string()
                    } else {
                        tf("gui.calls.call_number", &[("number", &number)])
                    })
                    .font(BOLD)
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([14, 22]))
            .width(Length::Fill)
            .style(style::button(c, ButtonKind::Filled))
            .on_press_maybe(
                (!number.is_empty()).then_some(Message::Calls(Msg::Dial(String::new()))),
            ),
        );
        container(pad)
            .padding(18)
            .width(Length::Fill)
            .style(style::card(c.container_high, c.on_surface, Shape::Wide))
            .into()
    }

    /// Llamadas recientes; `fill`: lista desplazable que llena el alto.
    fn recent_calls(&self, c: Colors, fill: bool) -> Element<'_, Message> {
        let s = &self.calls;
        let header = row![
            self.section(c, t("gui.calls.recent")),
            space().width(Length::Fill),
            button(labeled(Icon::Refresh, t("gui.photos.reload"), c.primary))
                .style(style::button(c, ButtonKind::Text))
                .on_press_maybe((!s.loading).then_some(Message::Calls(Msg::Load))),
        ]
        .align_y(Alignment::Center);
        let mut list = column![].spacing(6);
        if s.log.is_empty() {
            list = list.push(
                text(if s.loading {
                    t("gui.common.loading")
                } else {
                    t("gui.calls.empty")
                })
                .color(c.on_surface_variant),
            );
        }
        for (number, name, date, duration, kind, photo) in &s.log {
            let (icon, color, label) = match kind.as_str() {
                "outgoing" => (Icon::CallMade, c.primary, t("gui.calls.kind_outgoing")),
                "missed" => (Icon::CallMissed, c.error, t("gui.calls.kind_missed")),
                "rejected" => (Icon::CallMissed, c.error, t("gui.calls.kind_rejected")),
                "blocked" => (Icon::CallMissed, c.error, t("gui.calls.kind_blocked")),
                "voicemail" => (
                    Icon::CallReceived,
                    c.tertiary,
                    t("gui.calls.kind_voicemail"),
                ),
                _ => (Icon::CallReceived, c.tertiary, t("gui.calls.kind_incoming")),
            };
            let who = if name.is_empty() { number } else { name };
            let mut detail = fmt_time(*date);
            if *duration > 0 {
                detail = format!("{detail} · {}:{:02}", duration / 60, duration % 60);
            }
            if !name.is_empty() {
                detail = format!("{number} · {detail}");
            }
            list = list.push(self.call_row(
                c,
                who,
                photo,
                Some((icon, color, label)),
                detail,
                number.clone(),
            ));
        }
        if fill {
            column![
                header,
                scrollable(list.padding(Padding::default().right(14.0))).height(Length::Fill)
            ]
            .spacing(6)
            .height(Length::Fill)
            .into()
        } else {
            column![header, list].spacing(6).into()
        }
    }

    /// Fila de una llamada o un contacto: avatar (foto o iniciales), nombre,
    /// tipo de llamada (si lo hay) y botones de mensaje y llamar.
    fn call_row<'a>(
        &self,
        c: Colors,
        title: &str,
        photo: &str,
        kind: Option<(Icon, Color, &'static str)>,
        detail: String,
        number: String,
    ) -> Element<'a, Message> {
        let mut texts = column![text(title.to_string()).size(15).font(SEMIBOLD)].spacing(1);
        if let Some((icon, color, label)) = kind {
            texts = texts.push(
                row![icon.view(14.0, color), text(label).size(12).color(color)]
                    .spacing(4)
                    .align_y(Alignment::Center),
            );
        }
        texts = texts.push(text(detail).size(12).color(c.on_surface_variant));
        container(
            row![
                avatar(c, title, photo, 40.0),
                texts.width(Length::Fill),
                button(Icon::Message.view(18.0, c.on_secondary_container))
                    .padding(8)
                    .style(style::button(c, ButtonKind::Tonal))
                    .on_press(Message::OpenSmsTo(number.clone())),
                button(Icon::Phone.view(18.0, c.on_primary))
                    .padding(8)
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press(Message::Calls(Msg::Dial(number))),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([8, 12]))
        .width(Length::Fill)
        .style(style::card(c.container, c.on_surface, Shape::List))
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contacts_match_by_keys_name_or_number() {
        let ana: ContactRow = (
            "Ana López".into(),
            vec!["+52 55 1234 5678".into()],
            false,
            String::new(),
        );
        assert!(matches(&ana, "262")); // A-N-A
        assert!(matches(&ana, "567")); // L-O-P (segunda palabra)
        assert!(matches(&ana, "lopez")); // sin acento
        assert!(matches(&ana, "12345"));
        assert!(!matches(&ana, "999"));
        assert!(!matches(&ana, ""));
    }
}
