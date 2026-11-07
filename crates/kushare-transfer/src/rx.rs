//! Recepción (RX): acepta conexiones entrantes, responde a la solicitud de
//! conexión y a la introducción, y escribe los bytes de archivo recibidos a
//! disco de forma asíncrona (streaming, sin cargar el archivo completo en
//! memoria).
//!
//! Sin [`Consent`] se aceptan los archivos sin preguntar (pruebas, y el
//! receptor Bluetooth, que solo atiende a dispositivos vinculados). El
//! daemon pone un [`Consent`] al receptor TCP: cualquiera en la red puede
//! conectarse a él, así que pregunta al usuario antes de guardar nada.

use kushare_core::TransferEvent;
use kushare_proto::offline_wire_formats::{
    ConnectionResponseFrame as OfflineConnectionResponseFrame, OfflineFrame, PayloadTransferFrame,
    V1Frame as OfflineV1Frame,
    connection_response_frame::ResponseStatus,
    offline_frame::Version as OfflineVersion,
    payload_transfer_frame::{PacketType, PayloadHeader, payload_header::PayloadType},
    v1_frame::FrameType as OfflineFrameType,
};
use kushare_proto::wire_format::{
    ConnectionResponseFrame as WireConnectionResponseFrame, Frame as WireFrame,
    V1Frame as WireV1Frame, connection_response_frame::Status as WireResponseStatus,
    frame::Version as WireVersion, v1_frame::FrameType as WireFrameType,
};
use prost::Message;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::session::TransferSession;

const CONTROL_PAYLOAD_ID: i64 = 1;

/// Pregunta si aceptar los archivos `(nombre, tamaño)` que quiere enviar
/// `device_name`; `false` los rechaza sin escribir nada.
pub type Consent = std::sync::Arc<
    dyn Fn(
            String,
            Vec<(String, u64)>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send
        + Sync,
>;

pub struct Receiver {
    listener: TcpListener,
    download_dir: PathBuf,
    events: mpsc::Sender<TransferEvent>,
    consent: Option<Consent>,
}

impl Receiver {
    pub async fn bind(
        port: u16,
        download_dir: PathBuf,
        events: mpsc::Sender<TransferEvent>,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port)).await?;
        tokio::fs::create_dir_all(&download_dir).await?;
        Ok(Self {
            listener,
            download_dir,
            events,
            consent: None,
        })
    }

    /// Preguntar antes de aceptar cada envío (ver [`Consent`]).
    pub fn with_consent(mut self, consent: Consent) -> Self {
        self.consent = Some(consent);
        self
    }

    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Bucle de aceptación: cada conexión entrante se atiende en su propia
    /// tarea de tokio para no bloquear a los demás peers.
    pub async fn run(self) -> anyhow::Result<()> {
        loop {
            let (stream, addr) = self.listener.accept().await?;
            tracing::info!(%addr, "conexión TCP entrante");
            let download_dir = self.download_dir.clone();
            let events = self.events.clone();
            let consent = self.consent.clone();
            tokio::spawn(async move {
                if let Err(err) =
                    handle_connection_with_consent(stream, download_dir, events.clone(), consent)
                        .await
                {
                    tracing::warn!(%addr, ?err, "conexión de recepción terminó con error");
                }
            });
        }
    }
}

fn decode_wire_frame(offline: &OfflineFrame) -> anyhow::Result<WireFrame> {
    let body = offline
        .v1
        .as_ref()
        .and_then(|v1| v1.payload_transfer.as_ref())
        .and_then(|pt| pt.payload_chunk.as_ref())
        .and_then(|chunk| chunk.body.as_ref())
        .ok_or_else(|| {
            anyhow::anyhow!("se esperaba un PayloadTransferFrame con chunk de control")
        })?;
    Ok(WireFrame::decode(body.as_slice())?)
}

fn control_offline_frame(wire_frame: &WireFrame, payload_id: i64) -> OfflineFrame {
    use kushare_proto::offline_wire_formats::payload_transfer_frame::PayloadChunk;
    let body = wire_frame.encode_to_vec();
    OfflineFrame {
        version: Some(OfflineVersion::V1 as i32),
        v1: Some(OfflineV1Frame {
            r#type: Some(OfflineFrameType::PayloadTransfer as i32),
            payload_transfer: Some(PayloadTransferFrame {
                packet_type: Some(PacketType::Data as i32),
                payload_header: Some(PayloadHeader {
                    id: Some(payload_id),
                    r#type: Some(PayloadType::Bytes as i32),
                    total_size: Some(body.len() as i64),
                    ..Default::default()
                }),
                payload_chunk: Some(PayloadChunk {
                    flags: Some(1),
                    offset: Some(0),
                    body: Some(body),
                    index: Some(0),
                }),
                control_message: None,
            }),
            ..Default::default()
        }),
    }
}

/// Quita cualquier componente de directorio del nombre de archivo recibido
/// por red, para que un peer malicioso no pueda escribir fuera de
/// `download_dir` (p. ej. con un `file_name` como `../../.bashrc`).
fn sanitize_file_name(name: &str) -> String {
    Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "archivo_recibido".to_string())
}

/// Atiende una conexión entrante ya establecida, sin importar el
/// transporte (TCP, RFCOMM Bluetooth, un puente a `BluetoothSocket` de
/// Android, ...): conduce el handshake UKEY2 como respondedor, acepta la
/// solicitud y la introducción, y guarda a disco cada archivo recibido.
/// Pública porque los consumidores de transportes externos a este crate
/// (p. ej. el puente UniFFI de `kushare-android` para Bluetooth clásico
/// vía `BluetoothSocket`) también la necesitan, no solo [`Receiver`] y
/// `kushare_transfer::bt::BtService`.
pub async fn handle_connection<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    download_dir: PathBuf,
    events: mpsc::Sender<TransferEvent>,
) -> anyhow::Result<()> {
    handle_connection_with_consent(stream, download_dir, events, None).await
}

/// Como [`handle_connection`], preguntando a `consent` (si hay) antes de
/// aceptar los archivos.
pub async fn handle_connection_with_consent<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    download_dir: PathBuf,
    events: mpsc::Sender<TransferEvent>,
    consent: Option<Consent>,
) -> anyhow::Result<()> {
    let mut session = TransferSession::handshake_as_responder(stream).await?;

    // 1. Solicitud de conexión: v1 la acepta siempre (modo "visible para
    // todos", sin verificación de identidad).
    let request = session.recv_frame().await?;
    let device_name = request
        .v1
        .as_ref()
        .and_then(|v1| v1.connection_request.as_ref())
        .and_then(|r| r.endpoint_name.clone())
        .unwrap_or_else(|| "dispositivo desconocido".to_string());

    session
        .send_frame(&OfflineFrame {
            version: Some(OfflineVersion::V1 as i32),
            v1: Some(OfflineV1Frame {
                r#type: Some(OfflineFrameType::ConnectionResponse as i32),
                connection_response: Some(OfflineConnectionResponseFrame {
                    response: Some(ResponseStatus::Accept as i32),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        })
        .await?;

    // 2. Introducción: leer metadatos de archivo(s) propuestos.
    let introduction = decode_wire_frame(&session.recv_frame().await?)?;
    let file_metadata = introduction
        .v1
        .as_ref()
        .and_then(|v1| v1.introduction.as_ref())
        .map(|intro| intro.file_metadata.clone())
        .unwrap_or_default();

    for meta in &file_metadata {
        let _ = events
            .send(TransferEvent::IncomingRequest {
                device_name: device_name.clone(),
                file_name: meta.name.clone().unwrap_or_default(),
                file_size: meta.size.unwrap_or(0) as u64,
            })
            .await;
    }

    let accepted = match &consent {
        Some(ask) => {
            let files = file_metadata
                .iter()
                .map(|m| {
                    (
                        sanitize_file_name(m.name.as_deref().unwrap_or("archivo_recibido")),
                        m.size.unwrap_or(0) as u64,
                    )
                })
                .collect();
            ask(device_name.clone(), files).await
        }
        None => true,
    };
    let status = if accepted {
        WireResponseStatus::Accept
    } else {
        WireResponseStatus::Reject
    };

    session
        .send_frame(&control_offline_frame(
            &WireFrame {
                version: Some(WireVersion::V1 as i32),
                v1: Some(WireV1Frame {
                    r#type: Some(WireFrameType::Response as i32),
                    connection_response: Some(WireConnectionResponseFrame {
                        status: Some(status as i32),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            },
            CONTROL_PAYLOAD_ID,
        ))
        .await?;

    if !accepted {
        tracing::info!(%device_name, "envío rechazado por el usuario");
        return Ok(());
    }

    // 3. Recibir cada payload de archivo esperado, identificado por su
    // payload_id (anunciado en cada FileMetadata).
    let mut expected: HashMap<i64, (String, u64)> = file_metadata
        .iter()
        .filter_map(|m| {
            let id = m.payload_id?;
            Some((
                id,
                (
                    sanitize_file_name(m.name.as_deref().unwrap_or("archivo_recibido")),
                    m.size.unwrap_or(0) as u64,
                ),
            ))
        })
        .collect();

    let mut open_files: HashMap<i64, tokio::fs::File> = HashMap::new();

    while !expected.is_empty() {
        let frame = session.recv_frame().await?;
        let Some(pt) = frame
            .v1
            .as_ref()
            .and_then(|v1| v1.payload_transfer.as_ref())
        else {
            continue;
        };

        let payload_id = pt.payload_header.as_ref().and_then(|h| h.id).or_else(|| {
            // Los chunks posteriores al primero no repiten el header; el
            // id se infiere del único archivo en curso si solo hay uno.
            open_files.keys().next().copied()
        });
        let Some(payload_id) = payload_id else {
            continue;
        };

        if let Some(header) = &pt.payload_header
            && header.r#type == Some(PayloadType::File as i32)
            && let Some((file_name, _size)) = expected.get(&payload_id)
        {
            let dest = download_dir.join(file_name);
            let file = tokio::fs::File::create(&dest).await?;
            open_files.insert(payload_id, file);
        }

        if let Some(chunk) = &pt.payload_chunk {
            let Some(file) = open_files.get_mut(&payload_id) else {
                continue;
            };
            if let Some(body) = &chunk.body {
                if let Some(offset) = chunk.offset {
                    file.seek(std::io::SeekFrom::Start(offset as u64)).await?;
                }
                file.write_all(body).await?;
            }

            let is_last = chunk.flags.unwrap_or(0) & 0x1 != 0;
            if is_last {
                file.flush().await?;
                open_files.remove(&payload_id);
                if let Some((file_name, _)) = expected.remove(&payload_id) {
                    let saved_to = download_dir.join(&file_name);
                    let _ = events
                        .send(TransferEvent::Completed {
                            device_name: device_name.clone(),
                            file_name,
                            saved_to,
                        })
                        .await;
                }
            }
        }
    }

    Ok(())
}
