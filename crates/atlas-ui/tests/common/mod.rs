//! Shared mock-server harness for atlas-ui integration tests.
//!
//! `atlas-remote/tests/common/mock.rs` implements the paramiko/pyftpdlib
//! runner used by every remote backend integration test. Duplicating
//! it would be brittle, so we pull it in via `#[path]` from atlas-ui's
//! integration tests. Only the SFTP path is exercised here.

#![allow(dead_code, unreachable_pub)]

#[path = "../../../atlas-remote/tests/common/mock.rs"]
pub mod mock;

pub use mock::MockSftpServer;
