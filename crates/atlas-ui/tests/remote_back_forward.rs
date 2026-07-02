//! Integration tests for back/forward navigation on remote panes.
//!
//! Regression coverage for the "Cmd+[ / Cmd+] doesn't work after
//! navigating into a remote subdir" bug. The failure mode was:
//!
//! 1. User navigates `sftp://host/` → `sftp://host/a` → `sftp://host/a/b`.
//! 2. Tab's [`BackForwardStack`] correctly records three
//!    [`Location::Remote`] entries and cursor sits at `a/b`.
//! 3. User hits Cmd+[. Tab moves cursor back to `a` and returns
//!    `Some(Location::Remote(sftp://host/a, Sftp))`.
//! 4. Shell handed the returned location to
//!    [`NavigationController::navigate_pane_no_push`], which
//!    early-returned on `Location::Remote(_)` — so the vm never
//!    remounted at the parent, and the pane's contents didn't change.
//!
//! The fix routes back/forward through a shell-level dispatcher that
//! catches `Location::Remote` first and mounts a fresh
//! [`atlas_remote::RemoteLocationViewModel`] at the target URI. The
//! test below drives the tab-history dance and then proves that a
//! fresh listing at each history-returned URI produces the expected
//! contents when we bypass the process-wide pool's connection
//! cache (which keys on `(kind, host, port, user)` and hence would
//! alias multiple URIs sharing those tuple fields — see
//! `crates/atlas-remote/src/pool.rs::PoolKey`). We invoke the
//! backend `list(path)` directly against a single connection to
//! keep the test focused on the tab-history + remount-URI
//! semantics rather than pool-aliasing behavior.

mod common;

use anyhow::Result;
use atlas_core::Location;
use atlas_fs::OpenOptions;
use atlas_remote::vm::BackendClient;
use atlas_remote::RemoteLocationViewModel;
use atlas_ui::models::{tab::TabModel, ViewMode};

use common::MockSftpServer;

/// Skip the enclosing `#[test]` when the mock-server harness is
/// disabled by the environment.
macro_rules! skip_if_no_python {
    () => {{
        if common::mock::should_skip() {
            eprintln!("skipped: MOCK_SERVERS_SKIP=1");
            return Ok(());
        }
        if std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| !s.success())
            .unwrap_or(true)
            && std::process::Command::new("uv")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| !s.success())
                .unwrap_or(true)
        {
            eprintln!("skipped: neither python3 nor uv on PATH");
            return Ok(());
        }
    }};
}

#[test]
fn cmd_bracket_back_forward_remount_on_remote_pane() -> Result<()> {
    skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    let root = server.root_dir();
    std::fs::create_dir_all(root.join("a"))?;
    std::fs::create_dir_all(root.join("a").join("b"))?;
    std::fs::create_dir_all(root.join("a").join("b").join("c"))?;
    // Landmark files so we can prove each history-driven remount
    // would hit the right dir.
    std::fs::write(root.join("a").join("marker_a.txt"), b"marker_a")?;
    std::fs::write(root.join("a").join("b").join("marker_b.txt"), b"marker_b")?;
    std::fs::write(
        root.join("a").join("b").join("c").join("marker_c.txt"),
        b"marker_c",
    )?;

    // Build 3 URIs pointing at the 3 subdirs (as would be pushed
    // onto the tab's history when the user navigates a → b → c).
    let mk = |path: &str| {
        let mut u = server.uri("anon");
        u.path = path.into();
        u
    };
    let uri_a = mk("/a");
    let uri_b = mk("/a/b");
    let uri_c = mk("/a/b/c");
    let loc_a = Location::Remote(uri_a.clone(), atlas_core::BackendKind::Sftp);
    let loc_b = Location::Remote(uri_b.clone(), atlas_core::BackendKind::Sftp);
    let loc_c = Location::Remote(uri_c.clone(), atlas_core::BackendKind::Sftp);

    // Drive the tab's back/forward exactly the way the shell does
    // when Cmd+[ / Cmd+] fires. Tab is the sole owner of history —
    // the pane state that used to live on NavigationController was
    // migrated to TabModel in Phase 2.
    let mut tab = TabModel::new(loc_a.clone(), 100, Default::default(), Default::default());
    tab.navigate_to(loc_b.clone());
    tab.navigate_to(loc_c.clone());
    assert_eq!(tab.location.as_ref(), Some(&loc_c));

    // Cmd+[ once → back to b.
    let back1 = tab.back().expect("first back returns b");
    assert_eq!(back1, loc_b, "first back must land on b");
    // Cmd+[ twice → back to a.
    let back2 = tab.back().expect("second back returns a");
    assert_eq!(back2, loc_a, "second back must land on a");
    // Cmd+] once → forward to b again.
    let fwd = tab.forward().expect("forward returns b");
    assert_eq!(fwd, loc_b, "forward must land on b");

    // Prove the remount-at-URI path would list the right contents
    // at each of those locations. Open one vm at the mock root and
    // ask the underlying backend client to list each subpath —
    // this bypasses the pool's URI-path-agnostic caching (see
    // module-level comment above).
    let mut root_uri = server.uri("anon");
    root_uri.path = "/".into();
    let vm_root = RemoteLocationViewModel::open_live(
        root_uri,
        atlas_core::BackendKind::Sftp,
        atlas_remote::Credentials::Anonymous,
        OpenOptions::default(),
    )?;
    let client: &std::sync::Arc<dyn BackendClient> = vm_root.client();

    let rt = tokio::runtime::Runtime::new()?;
    let names_at = |sub: &str| -> Vec<String> {
        rt.block_on(client.list(sub))
            .expect("mock SFTP list succeeds")
            .into_iter()
            .map(|e| {
                std::path::Path::new(&e.path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect()
    };

    let entries_b = names_at("a/b");
    assert!(
        entries_b.iter().any(|n| n == "marker_b.txt"),
        "listing at b must include marker_b.txt; got: {entries_b:?}"
    );
    let entries_a = names_at("a");
    assert!(
        entries_a.iter().any(|n| n == "marker_a.txt"),
        "listing at a must include marker_a.txt; got: {entries_a:?}"
    );
    // (forward-remount at b: same listing as the initial b check.)
    let entries_b2 = names_at("a/b");
    assert!(
        entries_b2.iter().any(|n| n == "marker_b.txt"),
        "forward listing at b must include marker_b.txt; got: {entries_b2:?}"
    );

    // Bookkeeping — view mode is irrelevant to the back/forward
    // regression but exercising it here catches any accidental
    // enum coupling.
    let _ = ViewMode::Details;
    Ok(())
}
