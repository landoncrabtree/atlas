//! Smoke-test the mock-server harness itself.
//!
//! This is intentionally minimal — the per-backend tests exercise the
//! servers end-to-end. We just verify each mock spins up, prints its
//! sync line, and shuts down without hanging.

mod common;

use anyhow::Result;
use common::{MockFtpServer, MockS3Server, MockSftpServer, MockWebDavServer};

#[test]
fn sftp_mock_boots_and_shuts_down() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockSftpServer::start_anon()?;
    assert!(server.port() > 0);
    assert!(server.root_dir().exists());
    Ok(())
}

#[test]
fn ftp_mock_boots_and_shuts_down() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockFtpServer::start_anon()?;
    assert!(server.port() > 0);
    assert!(server.root_dir().exists());
    Ok(())
}

#[test]
fn webdav_mock_boots_and_shuts_down() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockWebDavServer::start_anon()?;
    assert!(server.port() > 0);
    assert!(server.root_dir().exists());
    Ok(())
}

#[test]
fn s3_mock_boots_and_shuts_down() -> Result<()> {
    crate::skip_if_no_python!();
    let server = MockS3Server::start("atlas-test")?;
    assert!(server.port() > 0);
    assert_eq!(server.bucket(), "atlas-test");
    Ok(())
}

#[test]
fn skip_env_short_circuits() -> Result<()> {
    // Just verify the helper reads the env var correctly by toggling
    // it around a `should_skip()` call. Restore after.
    // SAFETY(env): set_var + remove_var are legal in a test binary as
    // long as we don't touch env from multiple threads concurrently.
    let prior = std::env::var(common::mock::SKIP_ENV).ok();
    std::env::set_var(common::mock::SKIP_ENV, "1");
    assert!(common::mock::should_skip());
    std::env::set_var(common::mock::SKIP_ENV, "0");
    assert!(!common::mock::should_skip());
    std::env::remove_var(common::mock::SKIP_ENV);
    assert!(!common::mock::should_skip());
    if let Some(v) = prior {
        std::env::set_var(common::mock::SKIP_ENV, v);
    }
    Ok(())
}
