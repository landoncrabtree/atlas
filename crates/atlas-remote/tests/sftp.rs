//! Integration tests for the OpenDAL SFTP backend against the
//! paramiko-based mock server in `tools/mock-servers/sftp_server.py`.
//!
//! Run with `cargo test -p atlas-remote --test sftp -- --nocapture`.
//! Set `MOCK_SERVERS_SKIP=1` to skip.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use atlas_core::{BackendKind, Location, RemoteUri};
use atlas_fs::{LocationViewModel, OpenOptions, ViewModelEvent};
use atlas_remote::{backend::open, Credentials, RemoteErrorKind, RemoteLocationViewModel};

use common::MockSftpServer;

/// Wait until `is_loaded()` flips or we hit the deadline.
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
        BackendKind::Sftp,
        creds,
        OpenOptions::default(),
    )?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_anon() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let uri = server.uri("atlas");
    let creds = Credentials::SshKey(server.client_key(), None);

    let vm = open(
        &Location::Remote(uri, BackendKind::Sftp),
        creds,
        OpenOptions::default(),
    )?;
    wait_loaded(&vm, Duration::from_secs(15))?;
    // Empty root; the assertion is just that listing didn't error.
    assert!(vm.is_loaded());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connect_with_pinned_key() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_with_pinned_key("atlas")?;
    let uri = server.uri("atlas");

    // Right key ⇒ success.
    let vm = open_vm(uri.clone(), Credentials::SshKey(server.client_key(), None))?;
    // Force any first-op error by explicitly stat-ing the root.
    vm.stat(".").await.expect("stat . with pinned key");

    // Wrong key ⇒ error, not panic.
    let bad_dir = tempfile::TempDir::new()?;
    let bad_key = common::generate_ssh_keypair(bad_dir.path())?;
    let bad_vm = open_vm(uri, Credentials::SshKey(bad_key, None))?;
    let err = bad_vm
        .stat(".")
        .await
        .expect_err("wrong SSH key must fail SFTP auth");
    // Auth failures surface as PermissionDenied or Unexpected.
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
    let server = MockSftpServer::start_anon()?;

    // Seed the server root with foo/, bar.txt (10 bytes), baz.txt (0 bytes).
    std::fs::create_dir(server.root_dir().join("foo"))?;
    std::fs::write(server.root_dir().join("bar.txt"), b"0123456789")?;
    std::fs::write(server.root_dir().join("baz.txt"), b"")?;

    let vm = open(
        &Location::Remote(server.uri("atlas"), BackendKind::Sftp),
        Credentials::SshKey(server.client_key(), None),
        OpenOptions::default(),
    )?;
    wait_loaded(&vm, Duration::from_secs(15))?;

    let entries = vm.entries();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names.len(), 3, "expected 3 entries, got {names:?}");
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar.txt"));
    assert!(names.contains(&"baz.txt"));

    // Verify metadata for the sized file.
    let bar = entries.iter().find(|e| e.name == "bar.txt").expect("bar");
    assert_eq!(bar.metadata.size, 10);
    let foo = entries.iter().find(|e| e.name == "foo").expect("foo");
    assert!(matches!(foo.kind, atlas_fs::EntryKind::Dir));
    Ok(())
}

/// Dotfile handling — the SFTP backend must return `.` -prefixed
/// entries in the raw listing and mark them as hidden so
/// [`atlas_fs::Filter::include_hidden`] can toggle them at runtime.
///
/// This backs the per-pane `Cmd+.` (macOS) / `Ctrl+H` (Linux/Windows)
/// runtime toggle for remote panes — see
/// `AppShell::toggle_hidden_focused`.
///
/// Before the audit fix, `build_atlas_entry` set `is_hidden = false`
/// unconditionally, so remote panes silently ignored the toggle. This
/// test locks in the fix: name-based hidden detection must survive
/// the OpenDAL Metadata → atlas-fs Entry conversion, and the
/// pane-scoped Filter must reduce the listing when
/// `include_hidden = false`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_preserves_dot_entries_and_filter_hides_them() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;

    // Seed: one hidden dir, one hidden file, one visible file.
    std::fs::create_dir(server.root_dir().join(".hidden_dot_dir"))?;
    std::fs::write(server.root_dir().join(".dot_file"), b"secret")?;
    std::fs::write(server.root_dir().join("visible_file"), b"public")?;

    let vm = open(
        &Location::Remote(server.uri("atlas"), BackendKind::Sftp),
        Credentials::SshKey(server.client_key(), None),
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

    // Every leading-dot entry must be flagged is_hidden so the
    // filter can gate on it.
    for e in entries.iter() {
        let expected = e.name.starts_with('.');
        assert_eq!(
            e.metadata.is_hidden, expected,
            "entry {:?} must have is_hidden = {expected}",
            e.name,
        );
    }

    // Applying `include_hidden = false` at the pane filter layer must
    // reduce the listing to the single visible entry — no re-listing
    // round-trip.
    let mut filter = vm.filter();
    filter.include_hidden = false;
    vm.set_filter(filter)?;
    let filtered: Vec<String> = vm.entries().iter().map(|e| e.name.clone()).collect();
    assert_eq!(
        filtered.as_slice(),
        &["visible_file"],
        "Filter::include_hidden=false must hide dot entries; got {filtered:?}",
    );

    // Flipping back to `include_hidden = true` must restore all 3.
    let mut filter = vm.filter();
    filter.include_hidden = true;
    vm.set_filter(filter)?;
    let restored_len = vm.entries().len();
    assert_eq!(
        restored_len, 3,
        "Filter::include_hidden=true must restore all 3 entries",
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stat_single_entry() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    std::fs::write(server.root_dir().join("target.bin"), vec![0u8; 42])?;

    let vm = open_vm(
        server.uri("atlas"),
        Credentials::SshKey(server.client_key(), None),
    )?;
    let meta = vm.stat("target.bin").await?;
    assert!(meta.mode().is_file());
    assert_eq!(meta.content_length(), 42);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_file_returns_bytes() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let payload = b"the quick brown fox jumps over the lazy dog";
    std::fs::write(server.root_dir().join("payload.txt"), payload)?;

    let vm = open_vm(
        server.uri("atlas"),
        Credentials::SshKey(server.client_key(), None),
    )?;
    let bytes = vm.read("payload.txt").await?;
    assert_eq!(bytes, payload);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_file_creates_and_reads_back() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;

    let vm = open_vm(
        server.uri("atlas"),
        Credentials::SshKey(server.client_key(), None),
    )?;
    let payload = b"uploaded via opendal";
    vm.write("uploaded.txt", payload.to_vec()).await?;

    // Verify on-disk.
    let on_disk = std::fs::read(server.root_dir().join("uploaded.txt"))?;
    assert_eq!(on_disk, payload);
    // Verify via SFTP.
    let back = vm.read("uploaded.txt").await?;
    assert_eq!(back, payload);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mkdir_creates_directory() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;

    let vm = open_vm(
        server.uri("atlas"),
        Credentials::SshKey(server.client_key(), None),
    )?;
    vm.create_dir("newdir").await?;
    assert!(server.root_dir().join("newdir").is_dir());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_moves_file() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    std::fs::write(server.root_dir().join("before.txt"), b"payload")?;

    let vm = open_vm(
        server.uri("atlas"),
        Credentials::SshKey(server.client_key(), None),
    )?;
    vm.rename("before.txt", "after.txt").await?;
    assert!(!server.root_dir().join("before.txt").exists());
    assert!(server.root_dir().join("after.txt").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_removes_file() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    std::fs::write(server.root_dir().join("victim.txt"), b"..")?;

    let vm = open_vm(
        server.uri("atlas"),
        Credentials::SshKey(server.client_key(), None),
    )?;
    vm.delete("victim.txt").await?;
    assert!(!server.root_dir().join("victim.txt").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_cleanup_smoke() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    {
        let vm = open(
            &Location::Remote(server.uri("atlas"), BackendKind::Sftp),
            Credentials::SshKey(server.client_key(), None),
            OpenOptions::default(),
        )?;
        wait_loaded(&vm, Duration::from_secs(15))?;
        // Drop happens at end of scope; no panic expected.
        let _events = vm.subscribe();
    }
    // If we get here without a panic, the Drop path is clean enough.
    Ok(())
}

/// Regression test for the "URI-with-nested-path" case: connecting to
/// `sftp://user@host/atlas` must list the CHILDREN of the `/atlas`
/// directory, not attempt to list `/atlas/atlas` (which would surface
/// a spurious NotFound). See `open_live::from_client` in
/// `crates/atlas-remote/src/vm/mod.rs`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_nested_uri_path_lists_children() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;

    // Seed <root>/atlas/{alpha.txt,beta.txt}.
    let sub = server.root_dir().join("atlas");
    std::fs::create_dir(&sub)?;
    std::fs::write(sub.join("alpha.txt"), b"a")?;
    std::fs::write(sub.join("beta.txt"), b"bb")?;

    let mut uri = server.uri("atlas");
    uri.path = "/atlas".into();
    let vm = open(
        &Location::Remote(uri, BackendKind::Sftp),
        Credentials::SshKey(server.client_key(), None),
        OpenOptions::default(),
    )?;
    wait_loaded(&vm, Duration::from_secs(15))?;

    let entries = vm.entries();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names.len(), 2, "expected 2 entries, got {names:?}");
    assert!(names.contains(&"alpha.txt"));
    assert!(names.contains(&"beta.txt"));
    Ok(())
}

/// Symlink-resolution regression: a symlink pointing at a directory
/// on the SFTP server should surface as `EntryKind::Dir` (so
/// `fs::View` transparently navigates into the target); a symlink
/// pointing at a file should surface as `EntryKind::File`. A broken
/// symlink surfaces as `EntryKind::Symlink { broken: true, .. }`.
/// See `SftpBackend::list` in `crates/atlas-remote/src/vm/sftp.rs`.
///
/// Skipped on Windows: `std::os::unix::fs::symlink` creates the seed
/// symlinks on the mock server's data dir, and there is no Windows
/// equivalent that produces a symlink readable back over SFTP without
/// admin privileges. The SFTP backend's symlink-resolution logic is
/// exercised via the Unix runners in CI; Windows users driving Atlas
/// against a Unix SFTP server see the same behaviour at runtime — this
/// test's Windows gap is a seed-fixture limitation, not a runtime one.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_resolves_symlink_target_kinds() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;

    // Seed:
    //   real_dir/           (dir)
    //   real_file.txt       (file, 5 bytes)
    //   link_to_dir → real_dir
    //   link_to_file → real_file.txt
    //   dangling → ./nowhere
    let real_dir = server.root_dir().join("real_dir");
    std::fs::create_dir(&real_dir)?;
    std::fs::write(real_dir.join("inside.txt"), b"x")?;
    std::fs::write(server.root_dir().join("real_file.txt"), b"hello")?;
    std::os::unix::fs::symlink("real_dir", server.root_dir().join("link_to_dir"))?;
    std::os::unix::fs::symlink("real_file.txt", server.root_dir().join("link_to_file"))?;
    std::os::unix::fs::symlink("nowhere", server.root_dir().join("dangling"))?;

    let vm = open(
        &Location::Remote(server.uri("atlas"), BackendKind::Sftp),
        Credentials::SshKey(server.client_key(), None),
        OpenOptions::default(),
    )?;
    wait_loaded(&vm, Duration::from_secs(15))?;

    let entries = vm.entries();
    let by_name = |n: &str| entries.iter().find(|e| e.name == n).cloned();

    let link_dir = by_name("link_to_dir").expect("link_to_dir listed");
    assert!(
        matches!(link_dir.kind, atlas_fs::EntryKind::Dir),
        "link_to_dir should surface as Dir (target follows), got {:?}",
        link_dir.kind,
    );

    let link_file = by_name("link_to_file").expect("link_to_file listed");
    assert!(
        matches!(link_file.kind, atlas_fs::EntryKind::File),
        "link_to_file should surface as File, got {:?}",
        link_file.kind,
    );
    assert_eq!(link_file.metadata.size, 5, "target size propagated");

    let dangling = by_name("dangling").expect("dangling listed");
    match &dangling.kind {
        atlas_fs::EntryKind::Symlink { broken, target } => {
            assert!(*broken, "dangling symlink should be broken");
            assert_eq!(
                target.as_ref().and_then(|p| p.to_str()),
                Some("nowhere"),
                "raw target string preserved",
            );
        }
        other => panic!("dangling should be Symlink{{broken:true}}, got {other:?}"),
    }
    Ok(())
}

/// `follow_symlink` on the VM resolves relative-and-absolute link
/// targets and returns a fully-formed [`Location::Remote`] pointing
/// at the resolved path. See `RemoteLocationViewModel::follow_symlink`.
///
/// Skipped on Windows for the same reason as
/// `list_resolves_symlink_target_kinds` above — the fixture uses
/// `std::os::unix::fs::symlink` to seed a symlink on the mock
/// server's data dir.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_symlink_returns_target_location() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;

    std::fs::write(server.root_dir().join("real.txt"), b"payload")?;
    // Relative link at root.
    std::os::unix::fs::symlink("real.txt", server.root_dir().join("link.txt"))?;

    let vm = open_vm(
        server.uri("atlas"),
        Credentials::SshKey(server.client_key(), None),
    )?;

    let loc = vm.follow_symlink("link.txt").await?;
    match loc {
        Location::Remote(uri, kind) => {
            assert_eq!(kind, BackendKind::Sftp);
            // Relative link at root resolves against parent "/".
            assert_eq!(uri.path, "/real.txt");
        }
        Location::Local(p) => panic!("expected Remote, got Local({p:?})"),
    }
    Ok(())
}

/// Regression for the "Trust always doesn't stick on deeper
/// nav" bug: with an `AutoTrust` default installed at process level
/// (mirroring what the connect controller does when the user picks
/// "Trust always"), `SftpBackend::new` — the code path invoked by
/// `mount_remote_navigation` on pool-miss — must consult
/// `default_known_hosts_mode()` instead of hard-coding `Strict`.
/// This test flows a *fresh* URL (subdir) that would otherwise
/// require a host-key prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mount_deeper_path_honours_autotrust_default() -> Result<()> {
    crate::skip_if_no_python!();
    // The mock installs `AutoTrust` via `install_sftp_test_env`.
    let server = MockSftpServer::start_anon()?;

    // Seed <root>/sub/file.txt.
    std::fs::create_dir(server.root_dir().join("sub"))?;
    std::fs::write(server.root_dir().join("sub").join("file.txt"), b"x")?;

    // Simulate a deeper mount: open at `/sub` (as
    // `mount_remote_navigation` does on the pool-miss code path).
    // With `Strict` this would fail because the mock's ephemeral
    // host key is never persisted; with `AutoTrust` it succeeds.
    let mut uri = server.uri("atlas");
    uri.path = "/sub".into();
    let vm = open(
        &Location::Remote(uri, BackendKind::Sftp),
        Credentials::SshKey(server.client_key(), None),
        OpenOptions::default(),
    )?;
    wait_loaded(&vm, Duration::from_secs(15))?;
    let names: Vec<String> = vm.entries().iter().map(|e| e.name.clone()).collect();
    assert!(names.iter().any(|n| n == "file.txt"), "found: {names:?}");
    Ok(())
}

// Import unused ViewModelEvent from atlas_fs to give the compiler a
// visible reference (used indirectly through subscribe() in the smoke
// test above).
const _: fn(&ViewModelEvent) = |_e| {};
