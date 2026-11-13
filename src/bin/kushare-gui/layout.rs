//! Disposición adaptativa: dónde va el menú de pestañas y qué cabe según el
//! tamaño de la ventana (panel del teléfono al lado, modo compacto).

use serde::{Deserialize, Serialize};

/// Dónde van las pestañas del menú.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NavPosition {
    #[default]
    Left,
    Top,
    Bottom,
    Right,
}

impl NavPosition {
    pub const ALL: [NavPosition; 4] = [
        NavPosition::Left,
        NavPosition::Top,
        NavPosition::Bottom,
        NavPosition::Right,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NavPosition::Left => kushare_i18n::t("gui.settings.nav_left"),
            NavPosition::Top => kushare_i18n::t("gui.settings.nav_top"),
            NavPosition::Bottom => kushare_i18n::t("gui.settings.nav_bottom"),
            NavPosition::Right => kushare_i18n::t("gui.settings.nav_right"),
        }
    }

    /// Riel vertical (izquierda/derecha) o fila de pestañas (arriba/abajo).
    pub fn vertical(self) -> bool {
        matches!(self, NavPosition::Left | NavPosition::Right)
    }
}

/// Ancho del riel vertical (botones de 104 px + márgenes).
pub const RAIL_WIDTH: f32 = 136.0;
/// Ancho del panel del teléfono (estado + notificaciones).
pub const PANEL_WIDTH: f32 = 340.0;
/// La página necesita al menos esto para que quepa el panel a su lado.
pub const PAGE_MIN_WITH_PANEL: f32 = 600.0;
/// Por debajo de este ancho, la página va en modo compacto.
pub const COMPACT_BELOW: f32 = 640.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// Se ve el panel del teléfono al lado de la página.
    pub panel: bool,
    /// Página estrecha: márgenes y títulos menores, columnas apiladas.
    pub compact: bool,
    /// Ancho que le queda a la página.
    pub page_width: f32,
}

pub fn compute(width: f32, nav: NavPosition, panel_enabled: bool) -> Layout {
    let available = width - if nav.vertical() { RAIL_WIDTH } else { 0.0 };
    let panel = panel_enabled && available >= PANEL_WIDTH + PAGE_MIN_WITH_PANEL;
    let page_width = available - if panel { PANEL_WIDTH } else { 0.0 };
    Layout {
        panel,
        compact: page_width < COMPACT_BELOW,
        page_width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_and_compact_depend_on_width_and_nav() {
        // Ventana ancha: panel al lado y página normal.
        let wide = compute(1400.0, NavPosition::Left, true);
        assert!(wide.panel && !wide.compact);
        assert_eq!(wide.page_width, 1400.0 - RAIL_WIDTH - PANEL_WIDTH);
        // Con el menú arriba queda más ancho: el panel cabe antes.
        assert!(!compute(1050.0, NavPosition::Left, true).panel);
        assert!(compute(1050.0, NavPosition::Top, true).panel);
        // Desactivado en Ajustes: nunca.
        assert!(!compute(2000.0, NavPosition::Left, false).panel);
        // Ventana estrecha: sin panel y compacta.
        let narrow = compute(600.0, NavPosition::Left, true);
        assert!(!narrow.panel && narrow.compact);
    }

    #[test]
    fn nav_position_round_trips() {
        #[derive(Serialize, Deserialize)]
        struct C {
            nav: NavPosition,
        }
        for nav in NavPosition::ALL {
            let text = toml::to_string(&C { nav }).unwrap();
            assert_eq!(toml::from_str::<C>(&text).unwrap().nav, nav);
        }
        assert_eq!(NavPosition::default(), NavPosition::Left);
    }
}
