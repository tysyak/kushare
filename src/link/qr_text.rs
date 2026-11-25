//! QR en texto (bloques Unicode, dos módulos por carácter) para la CLI y la
//! TUI. Los módulos oscuros son el glifo (`█▀▄`) y los claros el fondo: hay
//! que pintarlo en negro sobre blanco para que la cámara lo lea en
//! cualquier tema de terminal. También lo incluye la TUI (`#[path]`).

use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

/// Líneas del QR de `text`, con su margen. Vacío si no cabe en un QR.
pub fn qr_lines(text: &str) -> Vec<String> {
    let Ok(code) = QrCode::new(text.as_bytes()) else {
        return Vec::new();
    };
    code.render::<Dense1x2>()
        .dark_color(Dense1x2::Dark)
        .light_color(Dense1x2::Light)
        .quiet_zone(true)
        .build()
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_square_ish() {
        let lines = super::qr_lines("WIFI:T:WPA;S:kushare;P:abcdefgh;;");
        assert!(lines.len() > 10);
        let w = lines[0].chars().count();
        assert!(lines.iter().all(|l| l.chars().count() == w));
    }
}
