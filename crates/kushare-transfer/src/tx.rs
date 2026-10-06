//! Envío (TX): conecta con un dispositivo, negocia la conexión, envía los
//! metadatos de introducción y transfiere los bytes del archivo por chunks.
//!
//! TODO(android-validation): el flujo completo de autenticación por pares
//! (`PairedKeyEncryptionFrame`/`PairedKeyResultFrame`) que Quick Share usa
//! para verificar dispositivos ya conocidos se omite aquí; v1 solo soporta
//! el modo "visible para todos" sin cuentas de Google (ver CLAUDE.md).

use kushare_core::TransferEvent;
use kushare_proto::offline_wire_formats::{
    ConnectionRequestFrame, OfflineFrame, PayloadTransferFrame, V1Frame as OfflineV1Frame,
    connection_request_frame::{ConnectionMode, Medium},
    connection_response_frame::ResponseStatus,
    offline_frame::Version as OfflineVersion,
    payload_transfer_frame::{
        PacketType, PayloadChunk, PayloadHeader, payload_header::PayloadType,
    },
    v1_frame::FrameType as OfflineFrameType,
};
use kushare_proto::wire_format::{
    FileMetadata, Frame as WireFrame, IntroductionFrame, V1Frame as WireV1Frame,
    connection_response_frame::Status as WireResponseStatus, file_metadata::Type as FileType,
    frame::Version as WireVersion, v1_frame::FrameType as WireFrameType,
};
use prost::Message;
use std::net::SocketAddr;
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::session::TransferSession;

/// Tamaño de cada chunk de archivo enviado por `PayloadTransferFrame`.
const CHUNK_SIZE: usize = 512 * 1024;

/// Identificador de payload fijo para el canal de control (introducción /
/// respuesta). Los payloads de archivo usan un id distinto por archivo.
///
/// TODO(android-validation): confirmar si Android espera un id concreto o
/// acepta cualquier `int64` estable durante la sesión.
const CONTROL_PAYLOAD_ID: i64 = 1;

fn control_offline_frame(wire_frame: &WireFrame, payload_id: i64) -> OfflineFrame {
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
                    flags: Some(1), // LAST_CHUNK: los mensajes de control caben en un solo chunk.
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

pub struct Sender;

impl Sender {
    pub async fn connect(addr: SocketAddr) -> anyhow::Result<TransferSession> {
        let stream = TcpStream::connect(addr).await?;
        TransferSession::handshake_as_initiator(stream).await
    }

    /// Envía un único archivo a través de una sesión ya conectada. Emite
    /// [`TransferEvent`]s de progreso por `events` (p. ej. para que
    /// `kushare-tray` los traduzca en una notificación de escritorio).
    pub async fn send_file(
        session: &mut TransferSession,
        my_endpoint_id: &str,
        my_device_name: &str,
        path: &Path,
        events: mpsc::Sender<TransferEvent>,
    ) -> anyhow::Result<()> {
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("ruta de archivo sin nombre de archivo"))?
            .to_string_lossy()
            .to_string();
        let metadata = tokio::fs::metadata(path).await?;
        let file_size = metadata.len();

        // 1. Solicitud de conexión (nivel Nearby Connections).
        session
            .send_frame(&OfflineFrame {
                version: Some(OfflineVersion::V1 as i32),
                v1: Some(OfflineV1Frame {
                    r#type: Some(OfflineFrameType::ConnectionRequest as i32),
                    connection_request: Some(ConnectionRequestFrame {
                        endpoint_id: Some(my_endpoint_id.to_string()),
                        endpoint_name: Some(my_device_name.to_string()),
                        mediums: vec![Medium::WifiLan as i32],
                        connection_mode: Some(ConnectionMode::Legacy as i32),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            })
            .await?;

        let response = session.recv_frame().await?;
        let accepted = response
            .v1
            .as_ref()
            .and_then(|v1| v1.connection_response.as_ref())
            .and_then(|r| r.response)
            == Some(ResponseStatus::Accept as i32);
        if !accepted {
            anyhow::bail!("el dispositivo remoto rechazó la solicitud de conexión");
        }

        // 2. Introducción (nivel Quick Share / wire_format).
        let payload_id = CONTROL_PAYLOAD_ID + 1;
        let introduction = WireFrame {
            version: Some(WireVersion::V1 as i32),
            v1: Some(WireV1Frame {
                r#type: Some(WireFrameType::Introduction as i32),
                introduction: Some(IntroductionFrame {
                    file_metadata: vec![FileMetadata {
                        name: Some(file_name.clone()),
                        r#type: Some(FileType::Unknown as i32),
                        payload_id: Some(payload_id),
                        size: Some(file_size as i64),
                        ..Default::default()
                    }],
                    start_transfer: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };
        session
            .send_frame(&control_offline_frame(&introduction, CONTROL_PAYLOAD_ID))
            .await?;

        // 3. Esperar aceptación del usuario en el dispositivo receptor.
        //
        // TODO(android-validation): confirmar el timeout razonable; por
        // ahora esto bloquea indefinidamente hasta que llegue una
        // respuesta o la conexión se cierre.
        let wire_response = decode_wire_frame(&session.recv_frame().await?)?;
        let wire_accepted = wire_response
            .v1
            .as_ref()
            .and_then(|v1| v1.connection_response.as_ref())
            .and_then(|r| r.status)
            == Some(WireResponseStatus::Accept as i32);
        if !wire_accepted {
            let _ = events
                .send(TransferEvent::Failed {
                    file_name,
                    reason: "el destinatario rechazó el archivo".to_string(),
                })
                .await;
            anyhow::bail!("el destinatario rechazó el archivo");
        }

        // 4. Transferencia del archivo por chunks.
        let mut file = tokio::fs::File::open(path).await?;
        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut offset: i64 = 0;
        let mut index: i32 = 0;

        session
            .send_frame(&OfflineFrame {
                version: Some(OfflineVersion::V1 as i32),
                v1: Some(OfflineV1Frame {
                    r#type: Some(OfflineFrameType::PayloadTransfer as i32),
                    payload_transfer: Some(PayloadTransferFrame {
                        packet_type: Some(PacketType::Data as i32),
                        payload_header: Some(PayloadHeader {
                            id: Some(payload_id),
                            r#type: Some(PayloadType::File as i32),
                            total_size: Some(file_size as i64),
                            file_name: Some(file_name.clone()),
                            ..Default::default()
                        }),
                        payload_chunk: None,
                        control_message: None,
                    }),
                    ..Default::default()
                }),
            })
            .await?;

        loop {
            let n = file.read(&mut buf).await?;
            let is_last = n < CHUNK_SIZE || (offset as u64 + n as u64) >= file_size;

            session
                .send_frame(&OfflineFrame {
                    version: Some(OfflineVersion::V1 as i32),
                    v1: Some(OfflineV1Frame {
                        r#type: Some(OfflineFrameType::PayloadTransfer as i32),
                        payload_transfer: Some(PayloadTransferFrame {
                            packet_type: Some(PacketType::Data as i32),
                            payload_header: None,
                            payload_chunk: Some(PayloadChunk {
                                flags: Some(if is_last { 1 } else { 0 }),
                                offset: Some(offset),
                                body: Some(buf[..n].to_vec()),
                                index: Some(index),
                            }),
                            control_message: None,
                        }),
                        ..Default::default()
                    }),
                })
                .await?;

            offset += n as i64;
            index += 1;

            let _ = events
                .send(TransferEvent::Progress {
                    file_name: file_name.clone(),
                    bytes_transferred: offset as u64,
                    total_bytes: file_size,
                })
                .await;

            if is_last || n == 0 {
                break;
            }
        }

        Ok(())
    }
}
