//! El teléfono como webcam: los fotogramas JPEG que manda por el enlace de
//! datos Wi-Fi se descodifican (Rust puro, `zune-jpeg`) y se escriben en
//! una cámara virtual **v4l2loopback** (`/dev/videoN`), que ven todas las
//! apps (navegadores, OBS, Zoom…). Sirve cualquier variante del módulo,
//! también la de DroidCam (`v4l2loopback_dc`, tamaño fijo y YU12).

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

/// Formatos V4L2 (FourCC).
const YU12: u32 = u32::from_le_bytes(*b"YU12");
const YUYV: u32 = u32::from_le_bytes(*b"YUYV");
const V4L2_BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_FIELD_NONE: u32 = 1;
/// Tamaño por defecto si el módulo deja elegirlo.
const DEFAULT_SIZE: (u32, u32) = (1280, 720);

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

/// `struct v4l2_format` (208 bytes en 64 bits: la unión va alineada a 8).
#[repr(C)]
struct Format {
    type_: u32,
    _pad: u32,
    pix: PixFormat,
    _rest: [u8; 200 - std::mem::size_of::<PixFormat>()],
}

const fn iowr(nr: u64, size: usize) -> u64 {
    (3 << 30) | ((size as u64) << 16) | ((b'V' as u64) << 8) | nr
}
const VIDIOC_G_FMT: u64 = iowr(4, std::mem::size_of::<Format>());
const VIDIOC_S_FMT: u64 = iowr(5, std::mem::size_of::<Format>());

/// La primera cámara virtual v4l2loopback (por su nombre en sysfs).
pub fn find_device() -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .filter(|e| {
            let name = std::fs::read_to_string(e.path().join("name"))
                .unwrap_or_default()
                .to_lowercase();
            name.contains("loopback") || name.contains("droidcam") || name.contains("kushare")
        })
        .map(|e| Path::new("/dev").join(e.file_name()))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Un fotograma recibido del teléfono.
pub struct Frame {
    pub jpeg: Vec<u8>,
    pub rotation: u32,
    pub mirror: bool,
}

/// Escritor de la cámara virtual en su propio hilo. Si va atrasado, los
/// fotogramas nuevos se descartan (el teléfono ya limita los pendientes).
pub struct Sink {
    tx: mpsc::SyncSender<Frame>,
    pub device: PathBuf,
    pub width: u32,
    pub height: u32,
    pub frames: Arc<AtomicU64>,
}

impl Sink {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| anyhow::anyhow!("no se pudo abrir {}: {e}", path.display()))?;
        let pix = negotiate(&file)?;
        let (width, height, fourcc) = (pix.width, pix.height, pix.pixelformat);
        tracing::info!(device = %path.display(), width, height, "webcam virtual lista");
        let (tx, rx) = mpsc::sync_channel::<Frame>(1);
        let frames = Arc::new(AtomicU64::new(0));
        let counter = frames.clone();
        std::thread::Builder::new()
            .name("kushare-webcam".into())
            .spawn(move || writer(file, rx, width, height, fourcc, counter))?;
        Ok(Self {
            tx,
            device: path.to_path_buf(),
            width,
            height,
            frames,
        })
    }

    pub fn push(&self, frame: Frame) {
        let _ = self.tx.try_send(frame);
    }
}

/// Formato de salida: el que tenga el dispositivo (DroidCam lo fija) o,
/// si se puede elegir, 1280×720 YU12.
fn negotiate(file: &File) -> anyhow::Result<PixFormat> {
    let fd = file.as_raw_fd();
    let mut fmt = Format {
        type_: V4L2_BUF_TYPE_VIDEO_OUTPUT,
        _pad: 0,
        pix: PixFormat::default(),
        _rest: [0; 200 - std::mem::size_of::<PixFormat>()],
    };
    // SAFETY: `fmt` es un `struct v4l2_format` válido y vive durante la llamada.
    let got = unsafe { libc::ioctl(fd, VIDIOC_G_FMT as _, &mut fmt) } == 0;
    let usable = got
        && fmt.pix.width > 0
        && fmt.pix.height > 0
        && matches!(fmt.pix.pixelformat, YU12 | YUYV);
    if usable {
        return Ok(fmt.pix);
    }
    // La variante de DroidCam (`v4l2loopback_dc`) solo se presenta como
    // captura, con tamaño y formato fijos, pero admite `write()`.
    fmt.type_ = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    // SAFETY: igual que arriba.
    let capture = unsafe { libc::ioctl(fd, VIDIOC_G_FMT as _, &mut fmt) } == 0;
    if capture
        && fmt.pix.width > 0
        && fmt.pix.height > 0
        && matches!(fmt.pix.pixelformat, YU12 | YUYV)
    {
        return Ok(fmt.pix);
    }
    {
        fmt.type_ = V4L2_BUF_TYPE_VIDEO_OUTPUT;
        fmt.pix = PixFormat {
            width: DEFAULT_SIZE.0,
            height: DEFAULT_SIZE.1,
            pixelformat: YU12,
            field: V4L2_FIELD_NONE,
            bytesperline: DEFAULT_SIZE.0,
            sizeimage: DEFAULT_SIZE.0 * DEFAULT_SIZE.1 * 3 / 2,
            ..Default::default()
        };
        // SAFETY: igual que arriba.
        if unsafe { libc::ioctl(fd, VIDIOC_S_FMT as _, &mut fmt) } != 0 {
            anyhow::bail!(
                "la cámara virtual no acepta el formato: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    if !matches!(fmt.pix.pixelformat, YU12 | YUYV) {
        anyhow::bail!("formato de la cámara virtual no soportado");
    }
    Ok(fmt.pix)
}

fn writer(
    mut file: File,
    rx: mpsc::Receiver<Frame>,
    width: u32,
    height: u32,
    fourcc: u32,
    frames: Arc<AtomicU64>,
) {
    let mut out = Vec::new();
    while let Ok(frame) = rx.recv() {
        let Some((pixels, w, h)) = decode(&frame.jpeg) else {
            continue;
        };
        let img = Image {
            pixels: &pixels,
            width: w,
            height: h,
        };
        if fourcc == YUYV {
            to_yuyv(&img, frame.rotation, frame.mirror, width, height, &mut out);
        } else {
            to_i420(&img, frame.rotation, frame.mirror, width, height, &mut out);
        }
        if let Err(err) = file.write_all(&out) {
            tracing::warn!(%err, "no se pudo escribir en la cámara virtual");
            break;
        }
        frames.fetch_add(1, Ordering::Relaxed);
    }
}

/// JPEG → YCbCr entrelazado (3 bytes por píxel).
fn decode(jpeg: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::YCbCr);
    let mut decoder = JpegDecoder::new_with_options(jpeg, options);
    let pixels = decoder.decode().ok()?;
    let (w, h) = decoder.dimensions()?;
    (pixels.len() >= w * h * 3).then_some((pixels, w, h))
}

struct Image<'a> {
    pixels: &'a [u8],
    width: usize,
    height: usize,
}

/// Para cada píxel de salida, el de la imagen (girada y encajada sin
/// deformar, con bandas negras) o `None` si cae en una banda.
struct Mapper {
    out_w: usize,
    out_h: usize,
    src_w: usize,
    src_h: usize,
    rot: u32,
    mirror: bool,
    scale: f32,
    off_x: f32,
    off_y: f32,
}

impl Mapper {
    fn new(img: &Image, rotation: u32, mirror: bool, out_w: u32, out_h: u32) -> Self {
        let rot = (rotation / 90 % 4) * 90;
        let (rw, rh) = if rot.is_multiple_of(180) {
            (img.width, img.height)
        } else {
            (img.height, img.width)
        };
        let scale = (out_w as f32 / rw as f32).min(out_h as f32 / rh as f32);
        Self {
            out_w: out_w as usize,
            out_h: out_h as usize,
            src_w: img.width,
            src_h: img.height,
            rot,
            mirror,
            scale,
            off_x: (out_w as f32 - rw as f32 * scale) / 2.0,
            off_y: (out_h as f32 - rh as f32 * scale) / 2.0,
        }
    }

    fn source(&self, x: usize, y: usize) -> Option<usize> {
        let fx = (x as f32 - self.off_x) / self.scale;
        let fy = (y as f32 - self.off_y) / self.scale;
        if fx < 0.0 || fy < 0.0 {
            return None;
        }
        let (mut rx, ry) = (fx as usize, fy as usize);
        let (rw, rh) = if self.rot.is_multiple_of(180) {
            (self.src_w, self.src_h)
        } else {
            (self.src_h, self.src_w)
        };
        if rx >= rw || ry >= rh {
            return None;
        }
        if self.mirror {
            rx = rw - 1 - rx;
        }
        // Coordenadas en la imagen sin girar (giro horario de `rot`).
        let (sx, sy) = match self.rot {
            90 => (ry, self.src_h - 1 - rx),
            180 => (self.src_w - 1 - rx, self.src_h - 1 - ry),
            270 => (self.src_w - 1 - ry, rx),
            _ => (rx, ry),
        };
        Some((sy * self.src_w + sx) * 3)
    }
}

/// JFIF usa rango completo; las cámaras V4L2 se leen como rango limitado.
fn limited_y(y: u8) -> u8 {
    (16 + (y as u32 * 219 / 255)) as u8
}
fn limited_c(c: u8) -> u8 {
    (16 + (c as u32 * 224 / 255)) as u8
}

fn to_i420(img: &Image, rotation: u32, mirror: bool, w: u32, h: u32, out: &mut Vec<u8>) {
    let m = Mapper::new(img, rotation, mirror, w, h);
    let (w, h) = (m.out_w, m.out_h);
    out.clear();
    out.resize(w * h * 3 / 2, 0);
    let (y_plane, uv) = out.split_at_mut(w * h);
    let (u_plane, v_plane) = uv.split_at_mut(w * h / 4);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            match m.source(x, y) {
                Some(s) => {
                    y_plane[i] = limited_y(img.pixels[s]);
                    if x % 2 == 0 && y % 2 == 0 {
                        let c = (y / 2) * (w / 2) + x / 2;
                        u_plane[c] = limited_c(img.pixels[s + 1]);
                        v_plane[c] = limited_c(img.pixels[s + 2]);
                    }
                }
                None => {
                    y_plane[i] = 16;
                    if x % 2 == 0 && y % 2 == 0 {
                        let c = (y / 2) * (w / 2) + x / 2;
                        u_plane[c] = 128;
                        v_plane[c] = 128;
                    }
                }
            }
        }
    }
}

fn to_yuyv(img: &Image, rotation: u32, mirror: bool, w: u32, h: u32, out: &mut Vec<u8>) {
    let m = Mapper::new(img, rotation, mirror, w, h);
    let (w, h) = (m.out_w, m.out_h);
    out.clear();
    out.resize(w * h * 2, 0);
    for y in 0..h {
        for x in (0..w).step_by(2) {
            let o = (y * w + x) * 2;
            let a = m.source(x, y);
            let b = m.source(x + 1, y);
            let px = |s: Option<usize>| s.map_or(16, |s| limited_y(img.pixels[s]));
            out[o] = px(a);
            out[o + 2] = px(b);
            match a.or(b) {
                Some(s) => {
                    out[o + 1] = limited_c(img.pixels[s + 1]);
                    out[o + 3] = limited_c(img.pixels[s + 2]);
                }
                None => {
                    out[o + 1] = 128;
                    out[o + 3] = 128;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4l2_format_has_the_kernel_size() {
        assert_eq!(std::mem::size_of::<Format>(), 208);
        assert_eq!(VIDIOC_G_FMT, 0xc0d0_5604);
        assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
    }

    #[test]
    fn rotation_maps_corners_and_letterboxes() {
        // Imagen 4×2 (apaisada) girada 90°: queda 2×4, centrada en 8×8.
        let pixels: Vec<u8> = (0..4 * 2 * 3).map(|i| i as u8).collect();
        let img = Image {
            pixels: &pixels,
            width: 4,
            height: 2,
        };
        let m = Mapper::new(&img, 90, false, 8, 8);
        assert_eq!(m.scale, 2.0);
        assert_eq!(m.source(0, 0), None); // banda izquierda
        // Arriba a la izquierda de la girada = abajo a la izquierda original.
        assert_eq!(m.source(2, 0), Some(4 * 3));
        let mut out = Vec::new();
        to_i420(&img, 90, false, 8, 8, &mut out);
        assert_eq!(out.len(), 8 * 8 * 3 / 2);
        to_yuyv(&img, 0, true, 8, 8, &mut out);
        assert_eq!(out.len(), 8 * 8 * 2);
    }
}

/// Prueba manual con la cámara virtual real:
/// `KUSHARE_TEST_JPEG=foto.jpg cargo test --bin kushare webcam_manual -- --ignored`
/// (mientras, `ffmpeg -f v4l2 -i /dev/videoN -frames:v 1 captura.png`).
#[cfg(test)]
#[test]
#[ignore]
fn webcam_manual() {
    let jpeg = std::fs::read(std::env::var("KUSHARE_TEST_JPEG").unwrap()).unwrap();
    let device = find_device().expect("sin v4l2loopback");
    let sink = Sink::open(&device).unwrap();
    for _ in 0..80 {
        sink.push(Frame {
            jpeg: jpeg.clone(),
            rotation: 90,
            mirror: false,
        });
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    assert!(sink.frames.load(Ordering::Relaxed) > 10);
}
