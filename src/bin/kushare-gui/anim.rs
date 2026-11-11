//! Animaciones cortas (entradas, salidas, cambios de página) calculadas por
//! tiempo: el estado guarda cuándo empezó cada una y la vista calcula su
//! progreso. Mientras alguna está en curso la GUI se redibuja a cada
//! fotograma (`window::frames`); al terminar, deja de hacerlo.

use std::time::{Duration, Instant};

/// Salida deslizante de una tarjeta hacia la derecha.
pub const SLIDE_OUT: Duration = Duration::from_millis(260);
/// Cierre del hueco que deja una tarjeta al irse.
pub const COLLAPSE: Duration = Duration::from_millis(200);
/// Entrada deslizante de una tarjeta nueva.
pub const ENTER: Duration = Duration::from_millis(320);
/// Desfase entre tarjetas en las animaciones en cascada.
pub const STAGGER: Duration = Duration::from_millis(45);
/// Como mucho, cuántas tarjetas se desfasan (el resto va con la última).
pub const STAGGER_MAX: u32 = 10;
/// Entrada de una página al navegar.
pub const PAGE: Duration = Duration::from_millis(240);
/// Cuánto sube el contenido de una página al entrar.
pub const PAGE_SHIFT: f32 = 18.0;
/// Entrada y salida del aviso emergente.
pub const TOAST: Duration = Duration::from_millis(220);

/// Progreso de 0 a 1 de una animación que empieza en `start`.
pub fn progress(start: Instant, now: Instant, length: Duration) -> f32 {
    if now <= start {
        return 0.0;
    }
    ((now - start).as_secs_f32() / length.as_secs_f32()).min(1.0)
}

/// Empieza rápido y frena al final (entradas).
pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Empieza despacio y acelera (salidas).
pub fn ease_in(t: f32) -> f32 {
    t * t * t
}

/// Suave en los dos extremos (cambios de tamaño).
pub fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Inicio de la `i`-ésima animación de una cascada.
pub fn staggered(now: Instant, i: usize) -> Instant {
    now + STAGGER * (i as u32).min(STAGGER_MAX)
}

/// Animaciones de la ventana: entrada de página y aviso emergente.
#[derive(Debug, Default)]
pub struct Ui {
    pub page_at: Option<Instant>,
    pub toast_at: Option<Instant>,
    /// Aviso que se está yendo: (texto, inicio de la salida).
    pub toast_leaving: Option<(String, Instant)>,
}

impl Ui {
    pub fn active(&self) -> bool {
        self.page_at.is_some() || self.toast_at.is_some() || self.toast_leaving.is_some()
    }

    /// Olvida las animaciones terminadas.
    pub fn tick(&mut self, now: Instant) {
        let done = |at: &Instant, len| progress(*at, now, len) >= 1.0;
        if self.page_at.as_ref().is_some_and(|at| done(at, PAGE)) {
            self.page_at = None;
        }
        if self.toast_at.as_ref().is_some_and(|at| done(at, TOAST)) {
            self.toast_at = None;
        }
        if self
            .toast_leaving
            .as_ref()
            .is_some_and(|(_, at)| done(at, TOAST))
        {
            self.toast_leaving = None;
        }
    }

    /// Desplazamiento vertical del contenido de la página que entra.
    pub fn page_shift(&self, now: Instant) -> f32 {
        self.page_at.map_or(0.0, |at| {
            PAGE_SHIFT * (1.0 - ease_out(progress(at, now, PAGE)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_ends_and_progress_clamps() {
        for f in [ease_out, ease_in, ease_in_out] {
            assert!(f(0.0).abs() < 1e-6);
            assert!((f(1.0) - 1.0).abs() < 1e-6);
        }
        let t0 = Instant::now();
        assert_eq!(progress(t0 + SLIDE_OUT, t0, SLIDE_OUT), 0.0);
        assert_eq!(progress(t0, t0 + SLIDE_OUT * 3, SLIDE_OUT), 1.0);
        assert_eq!(staggered(t0, 100), t0 + STAGGER * STAGGER_MAX);
    }
}
