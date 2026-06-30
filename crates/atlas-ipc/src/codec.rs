//! Length-prefixed bincode framing.
//!
//! Each frame is: `[u32 LE length][bincode payload]`.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{IpcError, Result};
use crate::protocol::Envelope;

/// Maximum frame body size (16 MiB).
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Read one [`Envelope`] from an async reader.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Envelope> {
    let mut len_buf = [0_u8; 4];
    reader.read_exact(&mut len_buf).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            IpcError::Closed
        } else {
            IpcError::Io(error)
        }
    })?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge(len));
    }

    let mut buf = vec![0_u8; len];
    reader.read_exact(&mut buf).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            IpcError::Closed
        } else {
            IpcError::Io(error)
        }
    })?;

    let (env, _) =
        bincode::serde::decode_from_slice::<Envelope, _>(&buf, bincode::config::standard())
            .map_err(|error| IpcError::Codec(error.to_string()))?;
    Ok(env)
}

/// Write one [`Envelope`] to an async writer.
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, env: &Envelope) -> Result<()> {
    let buf = bincode::serde::encode_to_vec(env, bincode::config::standard())
        .map_err(|error| IpcError::Codec(error.to_string()))?;
    let len = buf.len();
    if len > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge(len));
    }

    let len_bytes = (len as u32).to_le_bytes();
    writer.write_all(&len_bytes).await?;
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}
