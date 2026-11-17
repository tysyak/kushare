//! Cámaras del teléfono (webcam y QR), micrófono, tonos DTMF y «encontrar
//! teléfono».

use super::*;

impl Features {
    // --- Cámaras: webcam y QR ------------------------------------------------

    /// Cámaras del teléfono.
    pub async fn cameras(&self, peer_id: &str) -> anyhow::Result<Vec<pb::CameraInfo>> {
        let reply = self
            .request(
                peer_id,
                |request_id| F::CamerasRequest(pb::CamerasRequest { request_id }),
                REQUEST_TIMEOUT,
            )
            .await?;
        match reply {
            F::Cameras(c) if c.error.is_empty() => Ok(c.cameras),
            F::Cameras(c) => anyhow::bail!("{}", phone_error(&c.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    /// Empieza a usar la cámara `camera_id` del teléfono como webcam del PC
    /// (vacía = la trasera por defecto). Devuelve la cámara virtual usada.
    pub async fn webcam_start(
        &self,
        peer_id: &str,
        camera_id: &str,
        backend: Option<crate::link::webcam::Backend>,
    ) -> anyhow::Result<String> {
        use crate::link::webcam::{Backend, Sink};
        if !self.manager.is_connected(peer_id) {
            anyhow::bail!("el dispositivo no está conectado");
        }
        let device = crate::link::webcam::find_device();
        let pipewire = backend != Some(Backend::V4l2)
            && (backend == Some(Backend::PipeWire) || device.is_none())
            && tokio::task::spawn_blocking(crate::link::webcam::pipewire_available)
                .await
                .unwrap_or(false);
        let backend = crate::link::webcam::choose(backend, device.is_some(), pipewire)?;
        // El vídeo solo cabe por Wi-Fi: hay que abrir (o tener) el enlace de datos.
        self.ensure_bulk(peer_id).await;
        if !self.manager.has_bulk(peer_id) {
            anyhow::bail!(
                "la webcam necesita que el teléfono y el PC estén en la misma Wi-Fi \
                 (por Bluetooth no cabe el vídeo)"
            );
        }
        self.webcam_stop();
        let sink = match (backend, &device) {
            (Backend::V4l2, Some(path)) => Sink::open(path)?,
            _ => Sink::open_pipewire(&format!("kushare ({})", self.peer_name(peer_id)))?,
        };
        let label = sink.device.clone();
        // La cámara del teléfono va apaisada: pedir el lado mayor como ancho.
        let (w, h) = (sink.width.max(sink.height), sink.width.min(sink.height));
        self.send(
            peer_id,
            F::WebcamStart(pb::WebcamStart {
                camera_id: camera_id.to_string(),
                width: w,
                height: h,
                fps: WEBCAM_FPS,
                quality: WEBCAM_QUALITY,
            }),
        )?;
        let frames = sink.frames.load(Ordering::Relaxed);
        *self.webcam.lock().expect("mutex envenenado") = Some(WebcamSession {
            peer: peer_id.to_string(),
            camera: camera_id.to_string(),
            sink,
            active: false,
            error: String::new(),
            last_count: (Instant::now(), frames),
        });
        Ok(label)
    }

    /// Para la webcam (si la hay).
    pub fn webcam_stop(&self) {
        if let Some(session) = self.webcam.lock().expect("mutex envenenado").take() {
            let _ = self.send(&session.peer, F::WebcamStop(pb::WebcamStop {}));
        }
    }

    /// Estado: (activa, teléfono, cámara, dispositivo, fps, error).
    pub fn webcam_status(&self) -> (bool, String, String, String, u32, String) {
        let mut webcam = self.webcam.lock().expect("mutex envenenado");
        let Some(s) = webcam.as_mut() else {
            return Default::default();
        };
        let now = Instant::now();
        let count = s.sink.frames.load(Ordering::Relaxed);
        let secs = now.duration_since(s.last_count.0).as_secs_f32();
        let fps = if secs > 0.0 {
            ((count - s.last_count.1) as f32 / secs).round() as u32
        } else {
            0
        };
        if secs >= 2.0 {
            s.last_count = (now, count);
        }
        (
            s.active,
            s.peer.clone(),
            s.camera.clone(),
            s.sink.device.clone(),
            fps,
            s.error.clone(),
        )
    }

    /// Nombre del dispositivo emparejado (o su id).
    pub(super) fn peer_name(&self, peer_id: &str) -> String {
        self.manager
            .peers()
            .into_iter()
            .find(|p| p.device_id == peer_id)
            .map(|p| p.device_name)
            .unwrap_or_else(|| peer_id.to_string())
    }

    // --- Micrófono ---------------------------------------------------------

    /// Empieza a usar el micrófono del teléfono como fuente de audio del PC
    /// («Micrófono de <teléfono>»). Por Wi-Fi va en PCM de 48 kHz; solo con
    /// Bluetooth, en ADPCM de 16 kHz. Devuelve el códec pedido.
    pub async fn mic_start(&self, peer_id: &str, make_default: bool) -> anyhow::Result<String> {
        use crate::link::mic::Codec;
        if !self.manager.is_connected(peer_id) {
            anyhow::bail!("el dispositivo no está conectado");
        }
        self.ensure_bulk(peer_id).await;
        let (codec, wire) = if self.manager.has_bulk(peer_id) {
            (Codec::Pcm16 { rate: 48_000 }, pb::MicCodec::MicPcm16)
        } else {
            (Codec::Adpcm { rate: 16_000 }, pb::MicCodec::MicImaAdpcm)
        };
        self.mic_stop();
        let name = format!("Micrófono de {}", self.peer_name(peer_id));
        let sink = tokio::task::spawn_blocking(move || {
            crate::link::mic::MicSink::open(&name, make_default)
        })
        .await??;
        let rate = match codec {
            Codec::Pcm16 { rate } | Codec::Adpcm { rate } => rate,
        };
        self.send(
            peer_id,
            F::MicStart(pb::MicStart {
                rate,
                codec: wire as i32,
            }),
        )?;
        *self.mic.lock().expect("mutex envenenado") = Some(MicSession {
            peer: peer_id.to_string(),
            sink,
            codec,
            decoder: crate::link::mic::Decoder::new(),
            active: false,
            error: String::new(),
            make_default,
        });
        Ok(codec.label())
    }

    /// Para el micrófono (si lo hay) y quita la fuente virtual.
    pub fn mic_stop(&self) {
        let session = self.mic.lock().expect("mutex envenenado").take();
        if let Some(session) = session {
            let _ = self.send(&session.peer, F::MicStop(pb::MicStop {}));
            // Descargar el módulo de pactl fuera del runtime.
            std::thread::spawn(move || drop(session));
        }
    }

    /// Estado: (activo, teléfono, códec, nivel 0–1000, error, predeterminado).
    pub fn mic_status(&self) -> kushare_dbus::MicRow {
        let mic = self.mic.lock().expect("mutex envenenado");
        let Some(s) = mic.as_ref() else {
            return Default::default();
        };
        (
            s.active,
            s.peer.clone(),
            s.codec.label(),
            s.sink.level.load(Ordering::Relaxed),
            s.error.clone(),
            s.make_default,
        )
    }

    // --- Tonos DTMF ----------------------------------------------------------

    /// Manda tonos DTMF a la llamada en curso del teléfono, por la primera
    /// vía que funcione:
    /// 1. el propio teléfono (si Android deja a kushare entrar en la llamada);
    /// 2. Bluetooth manos libres (`AT+VTS` vía `org.pipewire.Telephony`): el
    ///    teléfono los genera como si se pulsaran en su marcador;
    /// 3. como sonido por el audio de la llamada (si se oye en el PC); los
    ///    menús de voz no siempre los reconocen.
    ///
    /// Devuelve "phone", "bluetooth" o "audio" según por dónde fueron.
    pub async fn dtmf(&self, peer_id: &str, digits: &str) -> anyhow::Result<&'static str> {
        let digits: String = digits.chars().filter(|c| !c.is_whitespace()).collect();
        if !kushare_core::audio::is_dtmf(&digits) {
            anyhow::bail!("solo se pueden mandar 0-9, *, # y A-D");
        }
        let native = self.dtmf_phone(peer_id, &digits).await;
        let native_err = match native {
            Ok(()) => return Ok("phone"),
            Err(err) => err,
        };
        let address = self
            .manager
            .peers()
            .into_iter()
            .find(|p| p.device_id == peer_id)
            .and_then(|p| p.bt_address);
        if let Some(address) = &address {
            match dtmf_hfp(address, &digits).await {
                Ok(()) => return Ok("bluetooth"),
                Err(err) => tracing::info!(%err, "DTMF por Bluetooth HFP no disponible"),
            }
        }
        match address {
            Some(address) => match dtmf_audio(&address, &digits).await {
                Ok(()) => Ok("audio"),
                Err(err) => {
                    tracing::info!(%native_err, %err, "DTMF sin vía");
                    Err(err)
                }
            },
            None => Err(native_err),
        }
    }

    pub(super) async fn dtmf_phone(&self, peer_id: &str, digits: &str) -> anyhow::Result<()> {
        let reply = self
            .request(
                peer_id,
                |request_id| {
                    F::Dtmf(pb::Dtmf {
                        request_id,
                        digits: digits.to_string(),
                    })
                },
                REQUEST_TIMEOUT,
            )
            .await?;
        match reply {
            F::CommandResult(r) if r.ok => Ok(()),
            F::CommandResult(r) => anyhow::bail!("{}", phone_error(&r.error)),
            _ => anyhow::bail!("respuesta inesperada del teléfono"),
        }
    }

    pub(super) fn on_webcam_frame(&self, peer_id: &str, frame: pb::WebcamFrame) {
        let webcam = self.webcam.lock().expect("mutex envenenado");
        let Some(session) = webcam.as_ref().filter(|s| s.peer == peer_id) else {
            // Llegan fotogramas sin haberlos pedido (o tras parar): que pare.
            drop(webcam);
            let _ = self.send(peer_id, F::WebcamStop(pb::WebcamStop {}));
            return;
        };
        session.sink.push(crate::link::webcam::Frame {
            jpeg: frame.jpeg,
            rotation: frame.rotation,
            mirror: frame.mirror,
        });
        drop(webcam);
        // Confirmar para que el teléfono mande el siguiente.
        self.manager.send_feature(
            peer_id,
            pb::Feature {
                kind: Some(F::WebcamAck(pb::WebcamAck { seq: frame.seq })),
            },
        );
    }

    /// Abre el escáner de QR en el teléfono.
    pub fn qr_scan(&self, peer_id: &str) -> anyhow::Result<()> {
        self.send(peer_id, F::QrScan(pb::QrScan {}))
    }

    /// Hace sonar el teléfono para encontrarlo (`ring = false`: lo para).
    pub fn find_phone(&self, peer_id: &str, ring: bool) -> anyhow::Result<()> {
        self.send(peer_id, F::FindPhone(pb::FindPhone { ring }))?;
        // Hasta que el teléfono confirme, se da por hecho.
        self.peers
            .lock()
            .expect("mutex envenenado")
            .entry(peer_id.to_string())
            .or_default()
            .finding = ring;
        Ok(())
    }
}
