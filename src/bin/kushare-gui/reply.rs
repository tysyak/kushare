//! Ventana para responder a una notificación del teléfono
//! (`kushare-gui --reply <título> <texto>`): el mensaje y un cuadro de
//! texto de varias líneas. El daemon la abre cuando el servidor de
//! notificaciones no tiene respuesta en línea (o es muy pequeña). Escribe
//! la respuesta en la salida estándar y termina con 0; si se cancela, con 1.

use super::*;
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{column, text_editor};

pub fn run(title: String, body: String) -> iced::Result {
    let config = GuiConfig::load();
    // Sin el tema del sistema a mano: «Sistema» va como oscuro, que es lo
    // que usa casi todo el mundo con kushare; los temas fijos se respetan.
    let appearance = match config.theme {
        ThemeMode::Light => Appearance::Light,
        ThemeMode::Dark => Appearance::Dark,
        ThemeMode::Oled => Appearance::Oled,
        ThemeMode::System if config.oled_when_system_dark => Appearance::Oled,
        ThemeMode::System => Appearance::Dark,
    };
    iced::application(
        move || Reply {
            title: title.clone(),
            body: body.clone(),
            content: text_editor::Content::new(),
            appearance,
        },
        Reply::update,
        Reply::view,
    )
    .title(|r: &Reply| tf("gui.reply.window_title", &[("title", &r.title)]))
    .theme(|r: &Reply| Some(style::iced_theme(r.appearance)))
    .subscription(|_| {
        iced::keyboard::listen().filter_map(|event| match event {
            iced::keyboard::Event::KeyPressed { key, modifiers, .. } => match key.as_ref() {
                Key::Named(Named::Escape) => Some(ReplyMsg::Cancel),
                Key::Named(Named::Enter) if modifiers.contains(Modifiers::CTRL) => {
                    Some(ReplyMsg::Send)
                }
                _ => None,
            },
            _ => None,
        })
    })
    .window(iced::window::Settings {
        size: iced::Size::new(520.0, 380.0),
        min_size: Some(iced::Size::new(360.0, 280.0)),
        // Propio para que el compositor pueda dejarla flotando (Hyprland:
        // `windowrule = float, class:^(org.kushare.reply)$`).
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: "org.kushare.reply".into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .run()
}

struct Reply {
    title: String,
    body: String,
    content: text_editor::Content,
    appearance: Appearance,
}

#[derive(Debug, Clone)]
enum ReplyMsg {
    Edit(text_editor::Action),
    Send,
    Cancel,
}

impl Reply {
    fn update(&mut self, msg: ReplyMsg) -> Task<ReplyMsg> {
        match msg {
            ReplyMsg::Edit(action) => {
                self.content.perform(action);
                Task::none()
            }
            ReplyMsg::Send => {
                let reply = self.content.text();
                let reply = reply.trim();
                if reply.is_empty() {
                    return Task::none();
                }
                print!("{reply}");
                std::process::exit(0);
            }
            ReplyMsg::Cancel => std::process::exit(1),
        }
    }

    fn view(&self) -> Element<'_, ReplyMsg> {
        let c = style::colors(self.appearance);
        let can_send = !self.content.text().trim().is_empty();
        container(
            column![
                text(self.title.clone()).size(18).font(BOLD),
                scrollable(text(self.body.clone()).size(14).color(c.on_surface_variant))
                    .height(Length::Shrink),
                text_editor(&self.content)
                    .placeholder(t("gui.reply.placeholder"))
                    .on_action(ReplyMsg::Edit)
                    .height(Length::Fill)
                    .padding(12)
                    .size(15)
                    .style(move |_: &Theme, status| text_editor::Style {
                        background: c.container_high.into(),
                        border: iced::Border {
                            radius: 18.0.into(),
                            width: if matches!(status, text_editor::Status::Focused { .. }) {
                                2.0
                            } else {
                                1.0
                            },
                            color: if matches!(status, text_editor::Status::Focused { .. }) {
                                c.primary
                            } else {
                                c.outline
                            },
                        },
                        placeholder: c.on_surface_variant,
                        value: c.on_surface,
                        selection: c.primary_container,
                    }),
                row![
                    text(t("gui.reply.hint"))
                        .size(12)
                        .color(c.on_surface_variant),
                    space().width(Length::Fill),
                    button(text(t("gui.common.cancel")).font(SEMIBOLD))
                        .padding(Padding::from([10, 18]))
                        .style(style::button(c, ButtonKind::Text))
                        .on_press(ReplyMsg::Cancel),
                    button(
                        row![
                            Icon::Send.view(18.0, c.on_primary),
                            text(t("gui.reply.send")).font(SEMIBOLD)
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center)
                    )
                    .padding(Padding::from([10, 20]))
                    .style(style::button(c, ButtonKind::Filled))
                    .on_press_maybe(can_send.then_some(ReplyMsg::Send)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(12),
        )
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(c.surface.into()),
            text_color: Some(c.on_surface),
            ..Default::default()
        })
        .into()
    }
}
