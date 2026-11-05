//! Envuelve un `TcpStream` ya conectado: primero conduce el handshake
//! UKEY2 (ver `kushare_crypto::ukey2`) en texto plano sobre el framing de
//! longitud-prefijo, y después expone `send_frame`/`recv_frame` para
//! intercambiar `OfflineFrame`s ya cifrados/autenticados vía
//! `kushare_crypto::SecureChannel`.
//!
//! TODO(android-validation): el orden exacto entre el intercambio en texto
//! plano de `ConnectionRequestFrame`/`ConnectionResponseFrame` y el
//! handshake UKEY2 (aquí: primero UKEY2, después `ConnectionRequestFrame`
//! ya cifrado) sigue sin confirmarse contra una captura real; algunas
//! implementaciones de Nearby Connections intercambian el
//! `ConnectionRequestFrame` en texto plano *antes* del handshake UKEY2.

use kushare_crypto::{SecureChannel, Ukey2Initiator, Ukey2Responder};
use kushare_proto::offline_wire_formats::OfflineFrame;
use prost::Message;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};

use crate::framing;

/// Sesión de transferencia sobre cualquier transporte de bytes ordenado y
/// fiable (TCP, RFCOMM Bluetooth, ...). `S` es genérico para que el mismo
/// handshake UKEY2 y el mismo framing sirvan sin duplicar código sin
/// importar el medio físico.
pub struct TransferSession<S> {
    stream: S,
    channel: SecureChannel,
    /// Secreto de autenticación derivado del handshake UKEY2, idéntico en
    /// ambos extremos. No viaja por la red; sirve para derivar un PIN que
    /// el usuario compara a ojo, o para firmar con una clave a largo plazo
    /// y atar esa firma a esta sesión concreta (ver `kushare-link`).
    auth_string: [u8; 32],
}

impl<S: AsyncRead + AsyncWrite + Unpin> TransferSession<S> {
    /// Conduce el handshake UKEY2 como iniciador (quien abre la conexión)
    /// y devuelve una sesión lista para intercambiar `OfflineFrame`s
    /// cifrados.
    pub async fn handshake_as_initiator(mut stream: S) -> anyhow::Result<Self> {
        let mut initiator = Ukey2Initiator::new();

        let client_init = initiator.build_client_init()?;
        framing::write_frame(&mut stream, &client_init).await?;

        let server_init = framing::read_frame(&mut stream).await?;
        let client_finish = initiator.handle_server_init(&server_init)?;
        framing::write_frame(&mut stream, &client_finish).await?;

        let output = initiator.finish()?;
        Ok(Self {
            stream,
            channel: SecureChannel::new(output.keys),
            auth_string: output.auth_string,
        })
    }

    /// Conduce el handshake UKEY2 como respondedor (quien acepta la
    /// conexión entrante) y devuelve una sesión lista para intercambiar
    /// `OfflineFrame`s cifrados.
    pub async fn handshake_as_responder(mut stream: S) -> anyhow::Result<Self> {
        let mut responder = Ukey2Responder::new();

        let client_init = framing::read_frame(&mut stream).await?;
        let server_init = responder.handle_client_init(&client_init)?;
        framing::write_frame(&mut stream, &server_init).await?;

        let client_finish = framing::read_frame(&mut stream).await?;
        responder.handle_client_finish(&client_finish)?;

        let output = responder.finish()?;
        Ok(Self {
            stream,
            channel: SecureChannel::new(output.keys),
            auth_string: output.auth_string,
        })
    }

    pub fn auth_string(&self) -> &[u8; 32] {
        &self.auth_string
    }

    /// Exige números de secuencia estrictos en los mensajes recibidos (ver
    /// [`SecureChannel::with_strict_sequence`]). Llamar antes del primer
    /// `recv_*`.
    pub fn set_strict_sequence(&mut self, strict: bool) {
        self.channel.set_strict_sequence(strict);
    }

    /// Cifra y envía un payload arbitrario (no un `OfflineFrame`), para
    /// protocolos propios montados sobre este mismo canal seguro.
    pub async fn send_raw(&mut self, payload: &[u8]) -> anyhow::Result<()> {
        let encrypted = self.channel.encrypt_bytes(payload)?;
        framing::write_frame(&mut self.stream, &encrypted).await?;
        Ok(())
    }

    pub async fn recv_raw(&mut self) -> anyhow::Result<Vec<u8>> {
        let raw = framing::read_frame(&mut self.stream).await?;
        Ok(self.channel.decrypt_bytes(&raw)?)
    }

    pub async fn send_frame(&mut self, frame: &OfflineFrame) -> anyhow::Result<()> {
        let encrypted = self.channel.encrypt_frame(frame)?;
        framing::write_frame(&mut self.stream, &encrypted).await?;
        Ok(())
    }

    pub async fn recv_frame(&mut self) -> anyhow::Result<OfflineFrame> {
        let raw = framing::read_frame(&mut self.stream).await?;
        Ok(self.channel.decrypt_frame(&raw)?)
    }

    /// Parte la sesión en una mitad de escritura y otra de lectura que
    /// pueden vivir en tareas distintas (necesario para un enlace
    /// persistente donde se reciben mensajes mientras se envían otros).
    /// Ambas comparten el mismo [`SecureChannel`]: cifrar y descifrar son
    /// operaciones solo de CPU y muy cortas, así que un `Mutex` síncrono
    /// basta. Cada mitad debe tener un único dueño (una sola tarea
    /// escritora), para que el orden de los números de secuencia coincida
    /// con el orden en que los mensajes salen por el stream.
    pub fn into_split(self) -> (SessionWriter<WriteHalf<S>>, SessionReader<ReadHalf<S>>) {
        let (read, write) = tokio::io::split(self.stream);
        let channel = Arc::new(Mutex::new(self.channel));
        (
            SessionWriter {
                stream: write,
                channel: channel.clone(),
            },
            SessionReader {
                stream: read,
                channel,
            },
        )
    }

    /// Codifica y envía cualquier mensaje protobuf como un `OfflineFrame`
    /// ya construido por el llamador (usado por `rx`/`tx` para envolver
    /// `wire_format::Frame` dentro de un `PayloadTransferFrame`).
    pub fn encode<M: Message>(msg: &M) -> Vec<u8> {
        msg.encode_to_vec()
    }
}

/// Mitad de escritura de una [`TransferSession`] partida con
/// [`TransferSession::into_split`].
pub struct SessionWriter<W> {
    stream: W,
    channel: Arc<Mutex<SecureChannel>>,
}

impl<W: AsyncWrite + Unpin> SessionWriter<W> {
    pub async fn send_raw(&mut self, payload: &[u8]) -> anyhow::Result<()> {
        let encrypted = self
            .channel
            .lock()
            .expect("mutex del canal envenenado")
            .encrypt_bytes(payload)?;
        framing::write_frame(&mut self.stream, &encrypted).await?;
        Ok(())
    }
}

/// Mitad de lectura de una [`TransferSession`] partida con
/// [`TransferSession::into_split`].
pub struct SessionReader<R> {
    stream: R,
    channel: Arc<Mutex<SecureChannel>>,
}

impl<R: AsyncRead + Unpin> SessionReader<R> {
    pub async fn recv_raw(&mut self) -> anyhow::Result<Vec<u8>> {
        let raw = framing::read_frame(&mut self.stream).await?;
        Ok(self
            .channel
            .lock()
            .expect("mutex del canal envenenado")
            .decrypt_bytes(&raw)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kushare_proto::offline_wire_formats::{
        V1Frame, offline_frame::Version, v1_frame::FrameType,
    };
    use tokio::net::{TcpListener, TcpStream};

    #[tokio::test]
    async fn handshake_and_frame_exchange_over_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut session = TransferSession::handshake_as_responder(stream)
                .await
                .unwrap();
            let frame = session.recv_frame().await.unwrap();
            session.send_frame(&frame).await.unwrap();
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut session = TransferSession::handshake_as_initiator(stream)
            .await
            .unwrap();

        let frame = OfflineFrame {
            version: Some(Version::V1 as i32),
            v1: Some(V1Frame {
                r#type: Some(FrameType::KeepAlive as i32),
                ..Default::default()
            }),
        };
        session.send_frame(&frame).await.unwrap();
        let echoed = session.recv_frame().await.unwrap();
        assert_eq!(echoed, frame);

        server.await.unwrap();
    }

    #[tokio::test]
    async fn split_halves_exchange_raw_payloads_concurrently() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let (initiator, responder) = tokio::join!(
            TransferSession::handshake_as_initiator(a),
            TransferSession::handshake_as_responder(b),
        );
        let (mut initiator, responder) = (initiator.unwrap(), responder.unwrap());
        assert_eq!(initiator.auth_string(), responder.auth_string());

        initiator.set_strict_sequence(true);
        let (mut writer, mut reader) = responder.into_split();

        // El respondedor escribe y lee a la vez desde tareas distintas.
        let echo = tokio::spawn(async move {
            for _ in 0..3 {
                let msg = reader.recv_raw().await.unwrap();
                writer.send_raw(&msg).await.unwrap();
            }
        });
        for msg in [&b"uno"[..], b"dos", b"tres"] {
            initiator.send_raw(msg).await.unwrap();
            assert_eq!(initiator.recv_raw().await.unwrap(), msg);
        }
        echo.await.unwrap();
    }
}
