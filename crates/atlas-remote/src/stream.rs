//! Streaming copy pipeline for cross-backend transfers.
//!
//! [`stream_copy`] pulls bytes off any `AsyncRead` and pushes them onto
//! any `AsyncWrite`, in fixed-size chunks (default 4 MiB — matches the
//! plan's `ops.stream_chunk_bytes` config knob that will land in a
//! later phase). An optional progress channel receives per-chunk byte
//! counts so the caller can drive a progress bar without polling.
//!
//! # Why here (and not in atlas-ops)
//!
//! The upcoming `atlas-ops` adapter will call this exact function for
//! remote↔remote and local↔remote transfers. Landing it in
//! `atlas-remote` now — where the OpenDAL types already live — avoids
//! duplicating the read/write plumbing and lets the cross-backend
//! integration test drive it before atlas-ops learns about `Location`.
//!
//! # Backpressure
//!
//! The optional progress sender is a `crossbeam_channel::Sender<u64>`.
//! When it's full or dropped we silently degrade — the copy always
//! completes as long as the underlying reader and writer are healthy.

use std::io;

use crossbeam_channel::Sender;
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Default chunk size for [`stream_copy`], in bytes.
pub const DEFAULT_CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Progress event emitted by [`stream_copy`].
///
/// `bytes_transferred` is *cumulative* across the whole transfer; a
/// consumer redrawing a progress bar can use it verbatim.
#[derive(Debug, Clone, Copy)]
pub struct StreamProgress {
    /// Cumulative bytes read + written since the copy began.
    pub bytes_transferred: u64,
    /// Total size in bytes, if known ahead of time. `None` for streams
    /// whose length isn't discoverable (chunked HTTP, live pipes, …).
    pub total_bytes: Option<u64>,
}

/// Chunked pump from `reader` to `writer` with optional progress.
///
/// Returns the total number of bytes copied.
///
/// * `chunk_bytes` — chunk size in bytes; falls back to
///   [`DEFAULT_CHUNK_BYTES`] when `None`.
/// * `total_bytes` — optional size hint used to populate
///   `StreamProgress::total_bytes` on each event. Purely informational —
///   this function does not validate that the reader actually delivers
///   that many bytes.
/// * `progress_tx` — optional channel that receives one
///   [`StreamProgress`] per completed chunk. `send` failures are
///   silently ignored so an abandoned progress consumer never stalls
///   the copy.
///
/// # Errors
///
/// Propagates any [`std::io::Error`] surfaced by either the reader or
/// the writer.
pub async fn stream_copy<R, W>(
    reader: &mut R,
    writer: &mut W,
    chunk_bytes: Option<usize>,
    total_bytes: Option<u64>,
    progress_tx: Option<&Sender<StreamProgress>>,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let chunk = chunk_bytes.unwrap_or(DEFAULT_CHUNK_BYTES).max(1);
    let mut buf = vec![0_u8; chunk];
    let mut transferred: u64 = 0;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await?;
        transferred = transferred.saturating_add(n as u64);
        if let Some(tx) = progress_tx {
            // Best-effort — never block the copy on progress consumers.
            let _ = tx.try_send(StreamProgress {
                bytes_transferred: transferred,
                total_bytes,
            });
        }
    }
    writer.flush().await?;
    writer.close().await?;
    Ok(transferred)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use futures::io::Cursor;

    #[tokio::test]
    async fn stream_copy_moves_bytes_end_to_end() {
        let src = b"hello world, this is a stream_copy test".to_vec();
        let mut reader = Cursor::new(src.clone());
        let mut writer: Vec<u8> = Vec::new();
        // Use a very small chunk so we exercise the loop.
        let n = stream_copy(
            &mut reader,
            &mut Cursor::new(&mut writer),
            Some(7),
            None,
            None,
        )
        .await
        .expect("stream_copy");
        assert_eq!(n as usize, src.len());
        assert_eq!(writer, src);
    }

    #[tokio::test]
    async fn stream_copy_emits_progress_events() {
        let src: Vec<u8> = (0..1024_u32).flat_map(u32::to_le_bytes).collect();
        let mut reader = Cursor::new(src.clone());
        let mut sink: Vec<u8> = Vec::new();
        let (tx, rx) = unbounded::<StreamProgress>();
        let total = src.len() as u64;
        let n = stream_copy(
            &mut reader,
            &mut Cursor::new(&mut sink),
            Some(256),
            Some(total),
            Some(&tx),
        )
        .await
        .expect("stream_copy");
        drop(tx);
        assert_eq!(n, total);
        let events: Vec<StreamProgress> = rx.into_iter().collect();
        assert!(!events.is_empty(), "expected at least one progress event");
        // Cumulative counter must be monotonically increasing.
        let mut last = 0;
        for ev in &events {
            assert!(ev.bytes_transferred >= last);
            last = ev.bytes_transferred;
            assert_eq!(ev.total_bytes, Some(total));
        }
        // Final event equals the total.
        assert_eq!(events.last().expect("has final").bytes_transferred, total);
    }

    #[tokio::test]
    async fn stream_copy_default_chunk_size_covers_large_input() {
        // 5 MiB — bigger than one default chunk, smaller than two.
        let src = vec![0xAB_u8; 5 * 1024 * 1024];
        let mut reader = Cursor::new(src.clone());
        let mut sink: Vec<u8> = Vec::new();
        let n = stream_copy(&mut reader, &mut Cursor::new(&mut sink), None, None, None)
            .await
            .expect("stream_copy");
        assert_eq!(n as usize, src.len());
        assert_eq!(sink.len(), src.len());
        assert!(sink.iter().all(|&b| b == 0xAB));
    }
}
