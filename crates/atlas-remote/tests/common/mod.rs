//! Shared helpers for atlas-remote integration tests.
//!
//! Every per-backend test file (`sftp.rs`, `ftp.rs`, `webdav.rs`,
//! `s3.rs`, `cross_backend_stream.rs`) pulls the mock-server harness
//! in via `mod common;` and reaches for the [`skip_if_no_python`] macro
//! + the `MockXxxServer` types.
#![allow(dead_code, unreachable_pub)]

pub mod mock;

pub use mock::*;

/// Short-circuit the enclosing `#[test]` if `MOCK_SERVERS_SKIP=1` is
/// set or `python3`/`uv` is missing.
///
/// The macro logs the skip reason and returns `Ok(())` from the
/// surrounding `-> anyhow::Result<()>` test. Do not use this in tests
/// that don't return `Result`; add one first.
#[macro_export]
macro_rules! skip_if_no_python {
    () => {{
        if $crate::common::mock::should_skip() {
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
