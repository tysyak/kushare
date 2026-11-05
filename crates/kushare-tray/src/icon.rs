//! Iconos de notificación → PNG pequeño (≤ 64 px) para mandarlo por el
//! enlace. Las fuentes posibles, en orden de preferencia (igual que la
//! especificación de notificaciones de freedesktop):
//!
//! 1. hint `image-data` / `image_data` / `icon_data`: píxeles crudos;
//! 2. hint `image-path` o `app_icon`: ruta (`file://` o absoluta) o nombre
//!    de icono del tema (se busca un PNG con `freedesktop-icons`).
//!
//! Solo se aceptan PNG (los SVG del tema se ignoran: rasterizarlos
//! requeriría un motor de SVG completo).

use std::path::{Path, PathBuf};

pub const MAX_ICON_SIZE: u32 = 64;

/// Imagen RGBA8 en memoria.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Convierte el formato del hint `image-data` (`(iiibiiay)`: ancho, alto,
/// rowstride, has_alpha, bits por muestra, canales, datos) a RGBA8.
pub fn rgba_from_image_data(
    width: i32,
    height: i32,
    rowstride: i32,
    has_alpha: bool,
    bits_per_sample: i32,
    channels: i32,
    data: &[u8],
) -> Option<Rgba> {
    let (w, h, stride) = (
        usize::try_from(width).ok()?,
        usize::try_from(height).ok()?,
        usize::try_from(rowstride).ok()?,
    );
    let channels = usize::try_from(channels).ok()?;
    if bits_per_sample != 8 || w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    if channels != if has_alpha { 4 } else { 3 } || stride < w * channels {
        return None;
    }
    // La última fila puede no llevar el relleno completo del rowstride.
    if data.len() < stride * (h - 1) + w * channels {
        return None;
    }
    let mut pixels = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &data[y * stride..y * stride + w * channels];
        for px in row.chunks_exact(channels) {
            pixels.extend_from_slice(&px[..3]);
            pixels.push(if has_alpha { px[3] } else { 0xFF });
        }
    }
    Some(Rgba {
        width: w as u32,
        height: h as u32,
        pixels,
    })
}

/// Lee un PNG de disco (cualquier formato de color) como RGBA8.
pub fn rgba_from_png_file(path: &Path) -> Option<Rgba> {
    let file = std::fs::File::open(path).ok()?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let data = &buf[..frame.buffer_size()];
    let pixels = match frame.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::GrayscaleAlpha => data
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        // `ALPHA` debería haber añadido canal alfa a todo lo demás.
        _ => return None,
    };
    Some(Rgba {
        width: w,
        height: h,
        pixels,
    })
}

/// Reduce (sin agrandar) a como mucho `max` px de lado, promediando
/// bloques de píxeles (suficiente para iconos).
pub fn downscale(img: Rgba, max: u32) -> Rgba {
    if img.width <= max && img.height <= max {
        return img;
    }
    let scale = f64::from(img.width.max(img.height)) / f64::from(max);
    let (nw, nh) = (
        ((f64::from(img.width) / scale).round() as u32).max(1),
        ((f64::from(img.height) / scale).round() as u32).max(1),
    );
    let mut pixels = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        let (y0, y1) = (
            (f64::from(y) * scale) as u32,
            ((f64::from(y + 1) * scale) as u32).clamp(1, img.height),
        );
        for x in 0..nw {
            let (x0, x1) = (
                (f64::from(x) * scale) as u32,
                ((f64::from(x + 1) * scale) as u32).clamp(1, img.width),
            );
            let mut acc = [0u64; 4];
            let mut n = 0u64;
            for sy in y0..y1.max(y0 + 1) {
                for sx in x0..x1.max(x0 + 1) {
                    let i =
                        ((sy.min(img.height - 1) * img.width + sx.min(img.width - 1)) * 4) as usize;
                    for (sum, &v) in acc.iter_mut().zip(&img.pixels[i..i + 4]) {
                        *sum += u64::from(v);
                    }
                    n += 1;
                }
            }
            pixels.extend(acc.iter().map(|v| (v / n.max(1)) as u8));
        }
    }
    Rgba {
        width: nw,
        height: nh,
        pixels,
    }
}

pub fn encode_png(img: &Rgba) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, img.width, img.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&img.pixels).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// Resuelve `app_icon`/`image-path` (ruta, `file://` o nombre de icono
/// del tema) a un archivo PNG.
pub fn resolve_icon_png_path(icon: &str) -> Option<PathBuf> {
    let icon = icon.trim();
    if icon.is_empty() {
        return None;
    }
    let path = if let Some(stripped) = icon.strip_prefix("file://") {
        PathBuf::from(stripped)
    } else if icon.starts_with('/') {
        PathBuf::from(icon)
    } else {
        freedesktop_icons::lookup(icon)
            .with_size(64)
            .with_cache()
            .find()?
    };
    let is_png = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"));
    (is_png && path.is_file()).then_some(path)
}

/// PNG ≤ 64 px a partir de un nombre/ruta de icono.
pub fn png_from_icon_name(icon: &str) -> Option<Vec<u8>> {
    let img = rgba_from_png_file(&resolve_icon_png_path(icon)?)?;
    encode_png(&downscale(img, MAX_ICON_SIZE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_data_rgb_with_row_padding_becomes_rgba() {
        // 2x2 RGB con rowstride 8 (2 bytes de relleno por fila).
        let data = [
            255, 0, 0, 0, 255, 0, 9, 9, //
            0, 0, 255, 255, 255, 255,
        ];
        let img = rgba_from_image_data(2, 2, 8, false, 8, 3, &data).unwrap();
        assert_eq!(
            img.pixels,
            [
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255
            ]
        );
        assert!(rgba_from_image_data(2, 2, 8, true, 8, 3, &data).is_none());
        assert!(rgba_from_image_data(2, 2, 8, false, 8, 3, &data[..10]).is_none());
    }

    #[test]
    fn downscale_keeps_aspect_and_limit() {
        let img = Rgba {
            width: 256,
            height: 128,
            pixels: vec![200; 256 * 128 * 4],
        };
        let small = downscale(img, 64);
        assert_eq!((small.width, small.height), (64, 32));
        assert!(small.pixels.iter().all(|&p| p == 200));
    }

    #[test]
    fn png_roundtrip() {
        let img = Rgba {
            width: 3,
            height: 1,
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        };
        let png = encode_png(&img).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.png");
        std::fs::write(&path, &png).unwrap();
        let back = rgba_from_png_file(&path).unwrap();
        assert_eq!(back.pixels, img.pixels);
        assert_eq!(
            resolve_icon_png_path(&format!("file://{}", path.display())),
            Some(path)
        );
    }
}
