//! Lenguaje visual de la GUI, alineado con la app Android (Material 3
//! Expressive): roles de color tonales con contenedores de alto contraste,
//! modo OLED con negro puro, esquinas grandes y asimétricas según el rol del
//! contenedor, y botones que cambian de forma al pasar el ratón o pulsar.

use iced::border::Radius;
use iced::widget::container;
use iced::widget::{button as w_button, toggler as w_toggler};
use iced::{Background, Border, Color, Shadow, Theme, Vector};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
    Oled,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 4] = [
        ThemeMode::System,
        ThemeMode::Light,
        ThemeMode::Dark,
        ThemeMode::Oled,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::System => "Sistema",
            ThemeMode::Light => "Claro",
            ThemeMode::Dark => "Oscuro",
            ThemeMode::Oled => "OLED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
    Oled,
}

/// Roles de color (como los de Material 3).
#[derive(Debug, Clone, Copy)]
pub struct Colors {
    pub surface: Color,
    pub container_low: Color,
    pub container: Color,
    pub container_high: Color,
    pub container_highest: Color,
    pub on_surface: Color,
    pub on_surface_variant: Color,
    pub outline: Color,
    pub primary: Color,
    pub on_primary: Color,
    pub primary_container: Color,
    pub on_primary_container: Color,
    pub secondary: Color,
    pub on_secondary: Color,
    pub secondary_container: Color,
    pub on_secondary_container: Color,
    pub tertiary: Color,
    pub on_tertiary: Color,
    pub tertiary_container: Color,
    pub on_tertiary_container: Color,
    pub error: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::from_rgb8(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

const LIGHT: Colors = Colors {
    surface: rgb(0xFAF8FF),
    container_low: rgb(0xF3F3FC),
    container: rgb(0xEDEDF7),
    container_high: rgb(0xE7E7F1),
    container_highest: rgb(0xE1E2EC),
    on_surface: rgb(0x1A1B21),
    on_surface_variant: rgb(0x44464F),
    outline: rgb(0x757680),
    primary: rgb(0x2E5BD8),
    on_primary: rgb(0xFFFFFF),
    primary_container: rgb(0xDCE1FF),
    on_primary_container: rgb(0x001550),
    secondary: rgb(0x575E71),
    on_secondary: rgb(0xFFFFFF),
    secondary_container: rgb(0xDBE2F9),
    on_secondary_container: rgb(0x141B2C),
    tertiary: rgb(0x8B4A8E),
    on_tertiary: rgb(0xFFFFFF),
    tertiary_container: rgb(0xFFD6FB),
    on_tertiary_container: rgb(0x380B3D),
    error: rgb(0xBA1A1A),
};

const DARK: Colors = Colors {
    surface: rgb(0x121318),
    container_low: rgb(0x1A1B21),
    container: rgb(0x1E1F25),
    container_high: rgb(0x292A2F),
    container_highest: rgb(0x34343A),
    on_surface: rgb(0xE3E2E9),
    on_surface_variant: rgb(0xC5C6D0),
    outline: rgb(0x8F909A),
    primary: rgb(0xB6C4FF),
    on_primary: rgb(0x002780),
    primary_container: rgb(0x1240B0),
    on_primary_container: rgb(0xDCE1FF),
    secondary: rgb(0xBFC6DC),
    on_secondary: rgb(0x293041),
    secondary_container: rgb(0x3F4759),
    on_secondary_container: rgb(0xDBE2F9),
    tertiary: rgb(0xFFA9FE),
    on_tertiary: rgb(0x55195B),
    tertiary_container: rgb(0x703374),
    on_tertiary_container: rgb(0xFFD6FB),
    error: rgb(0xFFB4AB),
};

/// OLED: superficie negro puro y contenedores en una escalera de casi-negros.
const OLED: Colors = Colors {
    surface: Color::BLACK,
    container_low: rgb(0x0A0A0C),
    container: rgb(0x101013),
    container_high: rgb(0x17171B),
    container_highest: rgb(0x1F1F24),
    ..DARK
};

pub fn colors(appearance: Appearance) -> Colors {
    match appearance {
        Appearance::Light => LIGHT,
        Appearance::Dark => DARK,
        Appearance::Oled => OLED,
    }
}

pub fn iced_theme(appearance: Appearance) -> Theme {
    let c = colors(appearance);
    Theme::custom(
        "kushare",
        iced::theme::Palette {
            background: c.surface,
            text: c.on_surface,
            primary: c.primary,
            success: c.tertiary,
            warning: c.tertiary,
            danger: c.error,
        },
    )
}

// --- Formas ---------------------------------------------------------------

fn radius(tl: f32, tr: f32, br: f32, bl: f32) -> Radius {
    Radius {
        top_left: tl,
        top_right: tr,
        bottom_right: br,
        bottom_left: bl,
    }
}

/// Formas por rol, como en la app Android.
#[derive(Debug, Clone, Copy)]
pub enum Shape {
    /// Héroe de estado: casi píldora con una esquina recogida.
    Hero,
    /// Acción principal: esquinas grandes en una diagonal.
    ActionLeading,
    /// Acción secundaria: la diagonal opuesta.
    ActionTrailing,
    /// Fila ancha con una esquina más recta.
    Wide,
    /// Tarjeta de lista.
    List,
    /// Diálogo.
    Dialog,
}

impl Shape {
    pub fn radius(self) -> Radius {
        match self {
            Shape::Hero => radius(48.0, 48.0, 48.0, 18.0),
            Shape::ActionLeading => radius(40.0, 14.0, 40.0, 14.0),
            Shape::ActionTrailing => radius(14.0, 40.0, 14.0, 40.0),
            Shape::Wide => radius(18.0, 40.0, 40.0, 40.0),
            Shape::List => radius(26.0, 26.0, 26.0, 26.0),
            Shape::Dialog => radius(40.0, 40.0, 40.0, 14.0),
        }
    }
}

pub fn card(bg: Color, fg: Color, shape: Shape) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(fg),
        background: Some(Background::Color(bg)),
        border: Border {
            radius: shape.radius(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

pub fn surface(c: Colors) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(c.on_surface),
        background: Some(Background::Color(c.surface)),
        ..container::Style::default()
    }
}

/// Velo oscuro detrás de un diálogo.
pub fn scrim(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color {
            a: 0.55,
            ..Color::BLACK
        })),
        ..container::Style::default()
    }
}

// --- Botones ---------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum ButtonKind {
    Filled,
    Tonal,
    Outlined,
    Text,
    /// Elemento del riel de navegación (seleccionado o no).
    Nav(bool),
    /// Segmento de un grupo de botones conectados.
    Segment {
        selected: bool,
        first: bool,
        last: bool,
    },
    /// Superficie pulsable (tarjetas de acción): solo cambia el realce.
    Card {
        bg: Color,
        fg: Color,
        shape: Shape,
    },
}

/// Botones "expresivos": la forma se estrecha al pulsar (de píldora a
/// esquinas de 12 px) y el color se realza al pasar el ratón.
pub fn button(c: Colors, kind: ButtonKind) -> impl Fn(&Theme, w_button::Status) -> w_button::Style {
    move |_, status| {
        let hovered = matches!(status, w_button::Status::Hovered);
        let pressed = matches!(status, w_button::Status::Pressed);
        let disabled = matches!(status, w_button::Status::Disabled);
        let pill = if pressed {
            radius(12.0, 12.0, 12.0, 12.0)
        } else {
            radius(999.0, 999.0, 999.0, 999.0)
        };
        let (bg, fg, border_color, border_width, r) = match kind {
            ButtonKind::Filled => (Some(c.primary), c.on_primary, Color::TRANSPARENT, 0.0, pill),
            ButtonKind::Tonal => (
                Some(c.secondary_container),
                c.on_secondary_container,
                Color::TRANSPARENT,
                0.0,
                pill,
            ),
            ButtonKind::Outlined => (None, c.primary, c.outline, 1.0, pill),
            ButtonKind::Text => (None, c.primary, Color::TRANSPARENT, 0.0, pill),
            ButtonKind::Nav(selected) => (
                selected.then_some(c.secondary_container),
                if selected {
                    c.on_secondary_container
                } else {
                    c.on_surface_variant
                },
                Color::TRANSPARENT,
                0.0,
                if pressed {
                    radius(14.0, 14.0, 14.0, 14.0)
                } else {
                    radius(999.0, 999.0, 999.0, 999.0)
                },
            ),
            ButtonKind::Segment {
                selected,
                first,
                last,
            } => {
                // Conectados: extremos redondeados por fuera; el elegido es una píldora.
                let outer = 999.0;
                let inner = if pressed { 6.0 } else { 10.0 };
                let r = if selected {
                    radius(outer, outer, outer, outer)
                } else {
                    radius(
                        if first { outer } else { inner },
                        if last { outer } else { inner },
                        if last { outer } else { inner },
                        if first { outer } else { inner },
                    )
                };
                (
                    Some(if selected {
                        c.primary
                    } else {
                        c.container_highest
                    }),
                    if selected { c.on_primary } else { c.on_surface },
                    Color::TRANSPARENT,
                    0.0,
                    r,
                )
            }
            ButtonKind::Card { bg, fg, shape } => {
                (Some(bg), fg, Color::TRANSPARENT, 0.0, shape.radius())
            }
        };
        let bg = bg.map(|bg| {
            if disabled {
                Color { a: 0.38, ..bg }
            } else if pressed {
                mix(bg, fg, 0.16)
            } else if hovered {
                mix(bg, fg, 0.08)
            } else {
                bg
            }
        });
        let shadow = match (kind, hovered && !disabled) {
            (ButtonKind::Card { .. } | ButtonKind::Filled, true) => Shadow {
                color: Color {
                    a: 0.25,
                    ..Color::BLACK
                },
                offset: Vector::new(0.0, 4.0),
                blur_radius: 14.0,
            },
            _ => Shadow::default(),
        };
        w_button::Style {
            background: bg.map(Background::Color),
            text_color: if disabled {
                Color { a: 0.38, ..fg }
            } else {
                fg
            },
            border: Border {
                color: border_color,
                width: border_width,
                radius: r,
            },
            shadow,
            snap: true,
        }
    }
}

/// Mezcla `b` sobre `a` con opacidad `t` (capa de estado de Material).
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a,
    }
}

pub fn toggler(c: Colors) -> impl Fn(&Theme, w_toggler::Status) -> w_toggler::Style {
    move |_, status| {
        let on = matches!(
            status,
            w_toggler::Status::Active { is_toggled: true }
                | w_toggler::Status::Hovered { is_toggled: true }
        );
        w_toggler::Style {
            background: Background::Color(if on { c.primary } else { c.container_highest }),
            background_border_width: if on { 0.0 } else { 2.0 },
            background_border_color: c.outline,
            foreground: Background::Color(if on { c.on_primary } else { c.outline }),
            foreground_border_width: 0.0,
            foreground_border_color: Color::TRANSPARENT,
            text_color: Some(c.on_surface),
            border_radius: None,
            padding_ratio: 0.15,
        }
    }
}
