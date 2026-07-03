//! SSH host-key TOFU resolver.
//!
//! This module bridges the async `russh` [`Handler::check_server_key`] hook
//! and whatever UI is willing to prompt the user for a Trust decision.
//!
//! Two moving pieces live here:
//!
//! * [`KnownHostsMode`] — a small tri-state enum that selects the trust
//!   strategy for a given SFTP connection: **Strict** (never accept unknown),
//!   **Prompt** (consult [`crate::known_hosts::KnownHosts`], then ask a
//!   resolver on cache miss), and **AutoTrust** (accept every offered key —
//!   test-only opt-in via [`crate::vm::sftp::SftpBackend::with_options`]).
//! * [`HostKeyResolver`] — an owner-agnostic bridge that accepts a
//!   [`HostKeyRequest`] and asynchronously produces a [`HostKeyDecision`].
//!   The owner (the connect controller in `atlas-ui`) constructs one with a
//!   closure that plumbs the request into the connect modal and returns a
//!   `tokio::sync::oneshot::Receiver<HostKeyDecision>` populated by the
//!   modal's Trust / Cancel callbacks.
//!
//! Library-crate discipline: this file **does not** know about Slint,
//! `slint::invoke_from_event_loop`, or `atlas-ui`. The resolver is a
//! generic seam so integration tests and future TUIs can plug in without
//! forking the SFTP handler.

use std::sync::Arc;
use std::time::Duration;

use crate::known_hosts::HostKeyStatus;

/// Trust strategy for the SFTP handshake.
///
/// Default is [`KnownHostsMode::Prompt`], which is the production behaviour
/// for atlas — consult the known-hosts store first, and on a cache miss ask
/// the [`HostKeyResolver`] (falling back to a hard rejection if no resolver
/// was supplied, which is the case for atlas-ops re-open paths where a UI
/// prompt is not appropriate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KnownHostsMode {
    /// Reject any host key not present in the known-hosts store. No user
    /// prompt is issued even if a resolver is attached.
    Strict,
    /// Consult the store, then defer to the resolver on cache miss. If no
    /// resolver is attached the connection is rejected.
    #[default]
    Prompt,
    /// Accept every offered host key unconditionally.
    ///
    /// Intended solely for integration tests against ephemeral mock servers
    /// whose host key rotates on every restart. Never enable in production
    /// code paths.
    AutoTrust,
}

/// One trust decision from the user.
///
/// The resolver returns exactly one of these for every [`HostKeyRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// Accept for the current connection but do not persist. Subsequent
    /// reconnects will re-prompt.
    TrustOnce,
    /// Accept and add to the atlas known-hosts store. Future reconnects
    /// return [`HostKeyStatus::Trusted`] with no prompt.
    TrustAlways,
    /// Reject the connection. The SFTP handshake aborts.
    Cancel,
}

/// A prompt payload the resolver hands to the UI.
///
/// The UI renders `offered_fingerprint` and, when
/// `current_status` is [`HostKeyStatus::Mismatch`], the previously known
/// fingerprint so the user can distinguish "first time seeing this host"
/// from "the server's key changed".
#[derive(Debug, Clone)]
pub struct HostKeyRequest {
    /// Host the client is connecting to (verbatim from the URI).
    pub host: String,
    /// TCP port (22 by default).
    pub port: u16,
    /// SHA-256 fingerprint of the offered key, formatted `SHA256:<base64>`.
    pub offered_fingerprint: String,
    /// The store's current classification of `(host, port)`.
    pub current_status: HostKeyStatus,
}

/// Type-erased handle to the closure that translates a
/// [`HostKeyRequest`] into a future decision.
type Prompter =
    dyn Fn(HostKeyRequest) -> tokio::sync::oneshot::Receiver<HostKeyDecision> + Send + Sync;

/// Bridge between the SFTP handshake and the connect-modal Trust prompt.
///
/// Construct with [`HostKeyResolver::new`]. The closure is invoked from the
/// tokio runtime that drives the SSH handshake; it must return promptly
/// (channel-only work) and hand back a `oneshot::Receiver` that the UI will
/// complete when the user clicks Trust / Cancel.
///
/// A single resolver is cheap to clone — internally it wraps an [`Arc`].
#[derive(Clone)]
pub struct HostKeyResolver {
    prompter: Arc<Prompter>,
    /// How long we wait for a decision before defaulting to Cancel. 60 s
    /// long enough for a real human to read the
    /// banner, short enough that a forgotten-about prompt eventually times
    /// out and frees the connection.
    timeout: Duration,
}

impl HostKeyResolver {
    /// Default per-prompt timeout — 60 s.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

    /// Construct a resolver from a closure. `prompter` is called from the
    /// SFTP handshake thread; it must not block (channel dispatch only)
    /// and must return a receiver that will eventually be completed by
    /// the UI or dropped on timeout / cancellation.
    pub fn new<F>(prompter: F) -> Self
    where
        F: Fn(HostKeyRequest) -> tokio::sync::oneshot::Receiver<HostKeyDecision>
            + Send
            + Sync
            + 'static,
    {
        Self {
            prompter: Arc::new(prompter),
            timeout: Self::DEFAULT_TIMEOUT,
        }
    }

    /// Override the per-prompt timeout. Tests use short timeouts; the
    /// default is [`Self::DEFAULT_TIMEOUT`].
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Dispatch `request` to the UI and await the user's decision.
    ///
    /// Returns [`HostKeyDecision::Cancel`] if the receiver is dropped
    /// (sender panicked or was garbage-collected) or the timeout fires
    /// before the user replies. The SFTP handler treats Cancel as a
    /// rejection, so a timed-out prompt safely fails closed.
    pub async fn resolve(&self, request: HostKeyRequest) -> HostKeyDecision {
        tracing::debug!(
            host = %request.host,
            port = request.port,
            fp = %request.offered_fingerprint,
            "host_key: prompting user",
        );
        let rx = (self.prompter)(request);
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_recv_err)) => {
                tracing::warn!("host_key: prompt receiver dropped without reply");
                HostKeyDecision::Cancel
            }
            Err(_elapsed) => {
                tracing::warn!("host_key: prompt timed out after {:?}", self.timeout);
                HostKeyDecision::Cancel
            }
        }
    }
}

impl std::fmt::Debug for HostKeyResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostKeyResolver")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn dummy_request() -> HostKeyRequest {
        HostKeyRequest {
            host: "example.com".into(),
            port: 22,
            offered_fingerprint: "SHA256:test".into(),
            current_status: HostKeyStatus::Unknown,
        }
    }

    #[tokio::test]
    async fn resolver_returns_ui_decision() {
        let resolver = HostKeyResolver::new(|_req| {
            let (tx, rx) = oneshot::channel();
            tx.send(HostKeyDecision::TrustAlways).unwrap();
            rx
        });
        let d = resolver.resolve(dummy_request()).await;
        assert_eq!(d, HostKeyDecision::TrustAlways);
    }

    #[tokio::test]
    async fn resolver_treats_dropped_sender_as_cancel() {
        let resolver = HostKeyResolver::new(|_req| {
            let (_tx, rx) = oneshot::channel::<HostKeyDecision>();
            // Drop `_tx` immediately by going out of scope after the return.
            rx
        });
        let d = resolver.resolve(dummy_request()).await;
        assert_eq!(d, HostKeyDecision::Cancel);
    }

    #[tokio::test]
    async fn resolver_times_out_when_ui_never_replies() {
        let resolver = HostKeyResolver::new(|_req| {
            // Leak the sender: the receiver never completes.
            let (tx, rx) = oneshot::channel();
            std::mem::forget(tx);
            rx
        })
        .with_timeout(Duration::from_millis(20));
        let d = resolver.resolve(dummy_request()).await;
        assert_eq!(d, HostKeyDecision::Cancel);
    }

    #[test]
    fn default_mode_is_prompt() {
        assert_eq!(KnownHostsMode::default(), KnownHostsMode::Prompt);
    }
}
