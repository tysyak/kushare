//! Página *Portapapeles*: compartir el portapapeles con el teléfono (en
//! ambos sentidos) y el historial opcional (desactivado por defecto).

use super::*;
use iced::widget::column;
use kushare_dbus::ClipRow;

#[derive(Debug, Clone, Default)]
pub struct ClipState {
    pub history: Vec<ClipRow>,
    pub loaded: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Load,
    Loaded(Vec<ClipRow>),
    Copy(String),
    Forget(usize),
    Clear,
}

impl App {
    pub(crate) fn clip_update(&mut self, msg: Msg) -> Task<Message> {
        match msg {
            Msg::Load => match self.daemon.clone() {
                Some(d) => Task::batch([
                    Task::perform(
                        async move { d.clipboard_history().await.unwrap_or_default() },
                        |h| Message::Clip(Msg::Loaded(h)),
                    ),
                    self.load_settings(),
                ]),
                None => Task::none(),
            },
            Msg::Loaded(history) => {
                self.clip.history = history;
                self.clip.loaded = true;
                Task::none()
            }
            Msg::Copy(text) => self.call(t("gui.clip.copied"), move |d| async move {
                d.clipboard_copy(&text).await
            }),
            Msg::Forget(i) => {
                if i < self.clip.history.len() {
                    self.clip.history.remove(i);
                }
                let i = i as i32;
                self.call("", move |d| async move { d.clipboard_forget(i).await })
            }
            Msg::Clear => {
                self.clip.history.clear();
                self.call(t("gui.clip.cleared"), |d| async move {
                    d.clipboard_forget(-1).await
                })
            }
        }
    }

    pub(crate) fn clip_view(&self, c: Colors) -> Element<'_, Message> {
        let sync = self.setting("clipboard-sync");
        let history_on = self.setting("clipboard-history");
        let mut col = column![
            self.title(c, t("gui.nav.clipboard"), t("gui.clip.subtitle").into()),
            container(
                column![
                    self.switch_row(
                        c,
                        t("gui.clip.sync"),
                        Some(t("gui.clip.sync_detail")),
                        sync,
                        |on| Message::SetSetting("clipboard-sync".into(), on),
                    ),
                    self.switch_row(
                        c,
                        t("gui.clip.history"),
                        Some(t("gui.clip.history_detail")),
                        history_on,
                        |on| Message::SetSetting("clipboard-history".into(), on),
                    ),
                ]
                .spacing(6)
            )
            .padding(22)
            .width(Length::Fill)
            .style(style::card(c.container_high, c.on_surface, Shape::Wide)),
        ]
        .spacing(14);

        let h = &self.clip.history;
        col = col.push(
            row![
                self.section(c, t("gui.clip.recent")),
                space().width(Length::Fill),
                button(labeled(Icon::Delete, t("gui.clip.clear"), c.primary))
                    .style(style::button(c, ButtonKind::Text))
                    .on_press_maybe((!h.is_empty()).then_some(Message::Clip(Msg::Clear))),
            ]
            .align_y(Alignment::Center),
        );
        if h.is_empty() {
            col = col.push(
                text(if history_on {
                    t("gui.clip.empty")
                } else {
                    t("gui.clip.history_off")
                })
                .color(c.on_surface_variant),
            );
        }
        for (i, (body, date, source)) in h.iter().enumerate() {
            let from = if source.is_empty() {
                t("gui.clip.from_pc").to_string()
            } else {
                source.clone()
            };
            let short: String = body.chars().take(400).collect();
            col = col.push(
                container(
                    row![
                        Icon::Clipboard.view(20.0, c.on_surface_variant),
                        column![
                            text(format!("{from} · {}", fmt_time(*date)))
                                .size(12)
                                .color(c.on_surface_variant),
                            text(short).size(14),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        button(Icon::Copy.view(18.0, c.on_secondary_container))
                            .padding(8)
                            .style(style::button(c, ButtonKind::Tonal))
                            .on_press(Message::Clip(Msg::Copy(body.clone()))),
                        button(Icon::Delete.view(18.0, c.on_surface_variant))
                            .padding(8)
                            .style(style::button(c, ButtonKind::Text))
                            .on_press(Message::Clip(Msg::Forget(i))),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([10, 14]))
                .width(Length::Fill)
                .style(style::card(c.container, c.on_surface, Shape::List)),
            );
        }
        col.into()
    }
}
