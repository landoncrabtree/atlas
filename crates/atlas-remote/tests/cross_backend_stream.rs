//! Cross-backend streaming copy pipeline test.
//!
//! Exercises the streaming primitive that will back atlas-ops' remote↔
//! remote transfers: 1 MiB pseudorandom payload flows local → sftp →
//! s3 → local, and the final bytes must be byte-identical to the
//! original. Any regression in `stream_copy`, the OpenDAL write API,
//! or backend endpoint wiring surfaces as a failure here.
//!
//! Run with `cargo test -p atlas-remote --test cross_backend_stream --
//! --nocapture`.  Set `MOCK_SERVERS_SKIP=1` to skip.

mod common;

use std::time::Duration;

use anyhow::{Context, Result};
use atlas_core::{BackendKind, RemoteUri};
use atlas_fs::OpenOptions;
use atlas_remote::stream::stream_copy;
use atlas_remote::{Credentials, OpenDalLocationViewModel};
use futures::io::AsyncWriteExt;
use rand::{RngCore, SeedableRng};
use tempfile::TempDir;
use tokio::time::timeout;

use common::{MockS3Server, MockSftpServer};

const PAYLOAD_BYTES: usize = 1024 * 1024; // 1 MiB
const CHUNK_BYTES: usize = 64 * 1024;
const OVERALL_TIMEOUT: Duration = Duration::from_secs(60);

fn open_sftp(server: &MockSftpServer) -> Result<std::sync::Arc<OpenDalLocationViewModel>> {
    Ok(OpenDalLocationViewModel::open_live(
        server.uri("atlas"),
        BackendKind::Sftp,
        Credentials::SshKey(server.client_key(), None),
        OpenOptions::default(),
    )?)
}

fn open_s3(uri: RemoteUri) -> Result<std::sync::Arc<OpenDalLocationViewModel>> {
    Ok(OpenDalLocationViewModel::open_live(
        uri,
        BackendKind::S3,
        Credentials::Iam {
            access_key_id: MockS3Server::ACCESS_KEY.into(),
            secret_key: MockS3Server::SECRET_KEY.into(),
            session_token: None,
        },
        OpenOptions::default(),
    )?)
}

async fn copy_via_stream(
    src_vm: &OpenDalLocationViewModel,
    src_path: &str,
    dst_vm: &OpenDalLocationViewModel,
    dst_path: &str,
    total: u64,
) -> Result<u64> {
    let reader = src_vm
        .operator()
        .reader(src_path)
        .await
        .with_context(|| format!("open reader {src_path}"))?;
    let mut async_reader = reader
        .into_futures_async_read(..total)
        .await
        .context("into_futures_async_read")?;
    let writer = dst_vm
        .operator()
        .writer(dst_path)
        .await
        .with_context(|| format!("open writer {dst_path}"))?;
    let mut async_writer = writer.into_futures_async_write();
    let bytes = stream_copy(
        &mut async_reader,
        &mut async_writer,
        Some(CHUNK_BYTES),
        Some(total),
        None,
    )
    .await
    .context("stream_copy")?;
    // stream_copy already flushed + closed the writer; nothing else needed.
    let _ = async_writer;
    Ok(bytes)
}

async fn copy_local_to_remote(
    local_path: &std::path::Path,
    dst_vm: &OpenDalLocationViewModel,
    dst_path: &str,
    total: u64,
) -> Result<u64> {
    let bytes = tokio::fs::read(local_path).await?;
    let mut reader = futures::io::Cursor::new(bytes);
    let writer = dst_vm.operator().writer(dst_path).await?;
    let mut async_writer = writer.into_futures_async_write();
    let n = stream_copy(
        &mut reader,
        &mut async_writer,
        Some(CHUNK_BYTES),
        Some(total),
        None,
    )
    .await?;
    let _ = async_writer;
    Ok(n)
}

async fn copy_remote_to_local(
    src_vm: &OpenDalLocationViewModel,
    src_path: &str,
    local_path: &std::path::Path,
    total: u64,
) -> Result<u64> {
    let reader = src_vm.operator().reader(src_path).await?;
    let mut async_reader = reader.into_futures_async_read(..total).await?;
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut cursor = futures::io::Cursor::new(&mut buf);
    let n = stream_copy(
        &mut async_reader,
        &mut cursor,
        Some(CHUNK_BYTES),
        Some(total),
        None,
    )
    .await?;
    // stream_copy called close() on the writer, which flushes; but Cursor::close
    // is a no-op so `buf` already has all the bytes.
    cursor.flush().await?;
    tokio::fs::write(local_path, &buf).await?;
    Ok(n)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_to_sftp_to_s3_to_local_roundtrip() -> Result<()> {
    crate::skip_if_no_python!();

    timeout(OVERALL_TIMEOUT, async {
        // ---- Fixtures ----
        let local_dir = TempDir::new()?;
        let original_path = local_dir.path().join("original.bin");
        let roundtripped_path = local_dir.path().join("roundtripped.bin");

        let mut rng = rand::rngs::StdRng::seed_from_u64(0xA71A_55EE_D000);
        let mut payload = vec![0u8; PAYLOAD_BYTES];
        rng.fill_bytes(&mut payload);
        tokio::fs::write(&original_path, &payload).await?;
        let total = payload.len() as u64;

        // ---- Servers ----
        let sftp = MockSftpServer::start_with_pinned_key("atlas")?;
        let s3 = MockS3Server::start("atlas-cross-backend")?;
        let _s3_guard = s3.install_s3_test_env_locked().await;

        let sftp_vm = open_sftp(&sftp)?;
        let s3_vm = open_s3(s3.uri())?;

        // ---- Pipeline ----
        // 1) local → sftp
        let n1 = copy_local_to_remote(&original_path, &sftp_vm, "hop1.bin", total).await?;
        assert_eq!(n1, total, "local→sftp truncated");

        // 2) sftp → s3
        let n2 = copy_via_stream(&sftp_vm, "hop1.bin", &s3_vm, "hop2.bin", total).await?;
        assert_eq!(n2, total, "sftp→s3 truncated");

        // 3) s3 → local
        let n3 = copy_remote_to_local(&s3_vm, "hop2.bin", &roundtripped_path, total).await?;
        assert_eq!(n3, total, "s3→local truncated");

        // ---- Byte equality ----
        let final_bytes = tokio::fs::read(&roundtripped_path).await?;
        assert_eq!(
            final_bytes.len(),
            payload.len(),
            "final length mismatch: {} vs {}",
            final_bytes.len(),
            payload.len()
        );
        assert!(
            final_bytes == payload,
            "cross-backend pipeline lost bytes somewhere",
        );

        anyhow::Ok(())
    })
    .await
    .context("cross-backend pipeline timed out")??;

    Ok(())
}
