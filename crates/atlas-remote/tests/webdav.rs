//! Integration tests for the OpenDAL WebDAV backend against the
//! wsgidav-based mock server in `tools/mock-servers/webdav_server.py`.
//!
//! Run with `cargo test -p atlas-remote --test webdav -- --nocapture`.
//! Set `MOCK_SERVERS_SKIP=1` to skip.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use atlas_core::{BackendKind, Location, RemoteUri};
use atlas_fs::{LocationViewModel, OpenOptions};
use atlas_remote::{backend::open, Credentials, RemoteErrorKind, RemoteLocationViewModel};

use common::MockWebDavServer;

fn wait_loaded(vm: &Arc<dyn LocationViewModel>, timeout: Duration) -> Result<()> {
    let sub = vm.subscribe();
    let deadline = Instant::now() + timeout;
    while !vm.is_loaded() {
        if Instant::now() >= deadline {
            bail!("view model never reported Loaded within {:?}", timeout);
        }
        let _ = sub.recv_timeout(Duration::from_millis(100));
    }
    Ok(())
}

fn open_vm(uri: RemoteUri, creds: Credentials) -> Result<Arc<RemoteLocationViewModel>> {
    Ok(RemoteLocationViewModel::open_live(
        uri,
        BackendKind::WebDav,
        creds,
        OpenOptions::default(),
    )?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_anon() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    let vm = open(
        &Location::Remote(server.uri(), BackendKind::WebDav),
        Credentials::Anonymous,
        OpenOptions::default(),
    )?;
    wait_loaded(&vm, Duration::from_secs(15))?;
    assert!(vm.is_loaded());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_with_password() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_auth("atlas-user", "atlas-pass")?;
    std::fs::write(server.root_dir().join("probe.txt"), b"hi")?;

    let vm = open_vm(server.uri(), Credentials::Password("atlas-pass".into()))?;
    // Force a real backend hit to trigger auth (OpenDAL synthesises root DIR).
    let bytes = vm
        .read("probe.txt")
        .await
        .expect("read with valid password");
    assert_eq!(bytes, b"hi".as_slice());

    let bad_vm = open_vm(server.uri(), Credentials::Password("wrong-pass".into()))?;
    let err = bad_vm
        .read("probe.txt")
        .await
        .expect_err("wrong password must fail");
    assert!(
        matches!(
            err.kind(),
            RemoteErrorKind::PermissionDenied | RemoteErrorKind::Unexpected
        ),
        "unexpected error kind: {err:?}",
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_directory_returns_children() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    std::fs::create_dir(server.root_dir().join("foo"))?;
    std::fs::write(server.root_dir().join("bar.txt"), b"0123456789")?;
    std::fs::write(server.root_dir().join("baz.txt"), b"")?;

    let vm = open(
        &Location::Remote(server.uri(), BackendKind::WebDav),
        Credentials::Anonymous,
        OpenOptions::default(),
    )?;
    wait_loaded(&vm, Duration::from_secs(15))?;

    let entries = vm.entries();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"foo"), "names = {names:?}");
    assert!(names.contains(&"bar.txt"), "names = {names:?}");
    assert!(names.contains(&"baz.txt"), "names = {names:?}");
    let bar = entries.iter().find(|e| e.name == "bar.txt").expect("bar");
    assert_eq!(bar.metadata.size, 10);
    Ok(())
}

/// Dotfile handling — the WebDAV backend must return `.`-prefixed
/// entries in the raw listing and mark them as hidden. See the SFTP
/// counterpart in `sftp.rs` for the rationale.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_preserves_dot_entries_and_filter_hides_them() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;

    std::fs::create_dir(server.root_dir().join(".hidden_dot_dir"))?;
    std::fs::write(server.root_dir().join(".dot_file"), b"secret")?;
    std::fs::write(server.root_dir().join("visible_file"), b"public")?;

    let vm = open(
        &Location::Remote(server.uri(), BackendKind::WebDav),
        Credentials::Anonymous,
        OpenOptions::default(),
    )?;
    wait_loaded(&vm, Duration::from_secs(15))?;

    let entries = vm.entries();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&".hidden_dot_dir")
            && names.contains(&".dot_file")
            && names.contains(&"visible_file"),
        "raw list must include all 3 entries; got {names:?}",
    );
    for e in entries.iter() {
        let expected = e.name.starts_with('.');
        assert_eq!(
            e.metadata.is_hidden, expected,
            "entry {:?} must have is_hidden = {expected}",
            e.name,
        );
    }

    let mut filter = vm.filter();
    filter.include_hidden = false;
    vm.set_filter(filter)?;
    let filtered: Vec<String> = vm.entries().iter().map(|e| e.name.clone()).collect();
    assert_eq!(
        filtered.as_slice(),
        &["visible_file"],
        "Filter::include_hidden=false must hide dot entries; got {filtered:?}",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stat_single_entry() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    std::fs::write(server.root_dir().join("target.bin"), vec![0u8; 42])?;

    let vm = open_vm(server.uri(), Credentials::Anonymous)?;
    let meta = vm.stat("target.bin").await?;
    assert!(meta.mode().is_file());
    assert_eq!(meta.content_length(), 42);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_file_returns_bytes() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    let payload = b"the quick brown fox jumps over the lazy dog";
    std::fs::write(server.root_dir().join("payload.txt"), payload)?;

    let vm = open_vm(server.uri(), Credentials::Anonymous)?;
    let bytes = vm.read("payload.txt").await?;
    assert_eq!(bytes, payload);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_file_creates_and_reads_back() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    let vm = open_vm(server.uri(), Credentials::Anonymous)?;
    let payload = b"uploaded via opendal";
    vm.write("uploaded.txt", payload.to_vec()).await?;
    let on_disk = std::fs::read(server.root_dir().join("uploaded.txt"))?;
    assert_eq!(on_disk, payload);
    let back = vm.read("uploaded.txt").await?;
    assert_eq!(back, payload);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mkdir_creates_directory() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    let vm = open_vm(server.uri(), Credentials::Anonymous)?;
    vm.create_dir("newdir").await?;
    assert!(server.root_dir().join("newdir").is_dir());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_moves_file() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    std::fs::write(server.root_dir().join("before.txt"), b"payload")?;
    let vm = open_vm(server.uri(), Credentials::Anonymous)?;
    match vm.rename("before.txt", "after.txt").await {
        Ok(()) => {
            assert!(!server.root_dir().join("before.txt").exists());
            assert!(server.root_dir().join("after.txt").exists());
        }
        Err(e) => {
            assert_eq!(
                e.kind(),
                RemoteErrorKind::Unsupported,
                "expected Unsupported from webdav rename, got: {e:?}",
            );
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_removes_file() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    std::fs::write(server.root_dir().join("victim.txt"), b"..")?;
    let vm = open_vm(server.uri(), Credentials::Anonymous)?;
    vm.delete("victim.txt").await?;
    assert!(!server.root_dir().join("victim.txt").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_cleanup_smoke() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    {
        let vm = open(
            &Location::Remote(server.uri(), BackendKind::WebDav),
            Credentials::Anonymous,
            OpenOptions::default(),
        )?;
        wait_loaded(&vm, Duration::from_secs(15))?;
    }
    Ok(())
}
