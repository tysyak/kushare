//! Framing TCP de Nearby Connections: cada mensaje va precedido por su
//! longitud en 4 bytes big-endian. Documentado públicamente por
//! `grishka/NearDrop` (PROTOCOL.md) y confirmado por `Martichou/rquickshare`.
//! No depende de ningún tipo de mensaje concreto: se usa tanto para los
//! `Ukey2Message` en texto plano del handshake como para los
//! `SecureMessage` cifrados posteriores.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Límite defensivo para el tamaño de un único mensaje (16 MiB). Los
/// mensajes de control son pequeños; los chunks de payload tienen su propio
/// tamaño acotado en `tx.rs`/`rx.rs`. Esto solo evita que un peer malicioso
/// o un bug hagan que se intente reservar una cantidad de memoria absurda.
const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(payload.len()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "frame demasiado grande")
    })?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame de {len} bytes excede el límite de {MAX_FRAME_LEN}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read_roundtrips() {
        let (mut client, mut server) = tokio::io::duplex(1024);

        let payload = b"hola desde kushare".to_vec();
        write_frame(&mut client, &payload).await.unwrap();

        let received = read_frame(&mut server).await.unwrap();
        assert_eq!(received, payload);
    }
}
