//! Timeout + exponential-backoff retry wrapper for remote operations.
//!
//! # Why?
//!
//! Every network round-trip in [`crate::vm::RemoteLocationViewModel`]
//! and [`crate::stream`] previously surfaced transient failures — a
//! dropped TCP session, a momentary DNS timeout, an intermittent proxy
//! reset — as an immediate error to the user. On flaky office WiFi
//! that made the whole app feel unreliable even when the failure was
//! self-healing.
//!
//! This module gives every remote op a retry envelope that:
//!
//! * Applies a configurable per-op **timeout** so a hung server cannot
//!   deadlock the runtime forever.
//! * Retries only [`RemoteErrorKind::Network`] failures with
//!   **exponential backoff + jitter**. Auth / not-found / already-
//!   exists / unsupported errors propagate on the first attempt — they
//!   are not transient.
//! * Broadcasts progress via an optional [`RetryObserver`] so the ops
//!   panel + status bar can render a "retrying (attempt 2/3)" chip.
//!
//! # Granularity
//!
//! For most ops the retry loop wraps the **entire operation** (a
//! single list/stat/mkdir/rename/delete/read/write round-trip). For
//! large transfers, [`crate::stream::stream_copy`] retries **per
//! chunk** — see the module docs there. This trade-off keeps small
//! ops cheap while avoiding re-hashing multi-GB uploads because the
//! last KiB timed out.
//!
//! # Testing
//!
//! Tests deliberately avoid real network I/O and instead drive the
//! wrapper with in-memory closures that simulate the failure modes.
//! See the `#[cfg(test)]` section at the bottom of this file.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rand::Rng;

use crate::error::{RemoteError, RemoteErrorKind, RemoteResult};

/// Runtime knobs governing the retry envelope. Every field has a
/// sensible default so callers can construct a policy in-line with
/// `RetryPolicy::default()` and only override what they need.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Wall-clock deadline for a single attempt. Exceeding this maps
    /// to [`RemoteErrorKind::Network`] and enters the retry loop.
    pub timeout: Duration,
    /// Maximum number of **retries** after the initial attempt. So
    /// `retries = 3` means at most 4 attempts (1 + 3).
    pub retries: u32,
    /// Initial backoff between attempt 0 and attempt 1.
    pub backoff_initial: Duration,
    /// Hard cap on the backoff after successive multiplication.
    pub backoff_max: Duration,
    /// Exponential growth factor. `1.0` disables growth; `2.0` doubles
    /// each attempt (default).
    pub backoff_multiplier: f32,
}

impl RetryPolicy {
    /// Convenience for tests: retries immediately with a zero backoff
    /// so the loop doesn't slow down the test suite.
    #[must_use]
    pub fn no_backoff(retries: u32) -> Self {
        Self {
            timeout: Duration::from_secs(30),
            retries,
            backoff_initial: Duration::ZERO,
            backoff_max: Duration::ZERO,
            backoff_multiplier: 1.0,
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(15_000),
            retries: 3,
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_millis(5_000),
            backoff_multiplier: 2.0,
        }
    }
}

/// Compute the (deterministic) backoff for the given retry attempt,
/// before jitter is applied. Attempt `0` means "delay before the first
/// retry"; attempt `N` grows by the multiplier.
#[must_use]
pub fn backoff_for(attempt: u32, policy: &RetryPolicy) -> Duration {
    let base = policy.backoff_initial.as_millis() as f64;
    let mult = policy.backoff_multiplier as f64;
    let scaled = base * mult.powi(attempt as i32);
    let capped = scaled.min(policy.backoff_max.as_millis() as f64);
    Duration::from_millis(capped as u64)
}

/// Callback fired at each retry boundary — used by
/// [`crate::vm::RemoteLocationViewModel`] to bubble progress up to the
/// ops panel. Signature: `(op_name, attempt, next_backoff)`.
///
/// The observer is `Arc<dyn Fn>` so it can be shared safely across
/// tasks — retries frequently run inside a `tokio::spawn`.
pub type RetryObserver = Arc<dyn Fn(&str, u32, Duration) + Send + Sync>;

/// Aggregate observer sink. A single caller can wire zero, one, or
/// multiple observers; retries broadcast to all of them.
#[derive(Clone, Default)]
pub struct RetryHooks {
    inner: Arc<Mutex<Vec<RetryObserver>>>,
}

impl RetryHooks {
    /// Construct an empty sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach an observer. All currently-registered observers fire on
    /// each retry attempt.
    pub fn add(&self, observer: RetryObserver) {
        self.inner.lock().push(observer);
    }

    /// Broadcast a retry event.
    pub fn notify(&self, op_name: &str, attempt: u32, next_backoff: Duration) {
        let observers = self.inner.lock().clone();
        for obs in observers {
            obs(op_name, attempt, next_backoff);
        }
    }
}

impl std::fmt::Debug for RetryHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryHooks")
            .field("observers", &self.inner.lock().len())
            .finish()
    }
}

/// True iff the error is safe to retry. Only transport-level failures
/// are retried; permission / not-found / unsupported / already-exists
/// errors indicate a caller mistake or a permanent server state and
/// would just loop.
#[must_use]
pub fn is_retryable(err: &RemoteError) -> bool {
    matches!(err.kind(), RemoteErrorKind::Network)
}

/// Apply `policy.timeout` to a future. On timeout, produces a
/// [`RemoteErrorKind::Network`] error — the retry loop then treats
/// this like any other transient failure.
async fn with_timeout<F, T>(fut: F, timeout: Duration) -> RemoteResult<T>
where
    F: Future<Output = RemoteResult<T>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => Err(RemoteError::new(
            RemoteErrorKind::Network,
            format!("operation timed out after {:?}", timeout),
        )),
    }
}

/// Wrap `f` with the retry envelope. On retryable errors we back off
/// and re-invoke `f`; on non-retryable errors we return the error
/// immediately. `op_name` is included in the log message and passed
/// verbatim to any observer.
///
/// # Errors
///
/// Returns the terminal error from the wrapped closure once the
/// budget is exhausted, or the first non-retryable error encountered.
pub async fn with_retry<F, Fut, T>(
    op_name: &str,
    policy: &RetryPolicy,
    hooks: Option<&RetryHooks>,
    mut f: F,
) -> RemoteResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = RemoteResult<T>>,
{
    let mut attempts_used: u32 = 0;
    loop {
        let attempt_result = with_timeout(f(), policy.timeout).await;
        match attempt_result {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !is_retryable(&e) || attempts_used >= policy.retries {
                    tracing::debug!(
                        target: "atlas_remote::retry",
                        op = op_name,
                        attempts = attempts_used + 1,
                        err = %e,
                        "retry loop terminating",
                    );
                    return Err(e);
                }
                let raw = backoff_for(attempts_used, policy);
                let jittered = jitter(raw);
                tracing::info!(
                    target: "atlas_remote::retry",
                    op = op_name,
                    attempt = attempts_used + 1,
                    backoff_ms = jittered.as_millis() as u64,
                    err = %e,
                    "retrying transient failure",
                );
                if let Some(h) = hooks {
                    h.notify(op_name, attempts_used + 1, jittered);
                }
                if !jittered.is_zero() {
                    tokio::time::sleep(jittered).await;
                }
                attempts_used += 1;
            }
        }
    }
}

/// Apply ±25% jitter to a backoff duration. Zero-length backoffs stay
/// zero (used by tests to keep the loop tight).
fn jitter(base: Duration) -> Duration {
    if base.is_zero() {
        return base;
    }
    let ms = base.as_millis() as i64;
    let range = ms / 4;
    let mut rng = rand::thread_rng();
    let offset: i64 = rng.gen_range(-range..=range);
    let final_ms = (ms + offset).max(0) as u64;
    Duration::from_millis(final_ms)
}

/// Convenience: unused sink for callers that don't care about retry
/// observers. Handy in tests + one-off call sites.
#[must_use]
pub fn no_hooks() -> Option<&'static RetryHooks> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    async fn run<F, Fut, T>(policy: RetryPolicy, f: F) -> RemoteResult<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = RemoteResult<T>>,
    {
        with_retry("unit-test", &policy, None, f).await
    }

    #[tokio::test]
    async fn succeeds_first_try() {
        let calls = AtomicU32::new(0);
        let res = run(RetryPolicy::no_backoff(3), || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok::<_, RemoteError>(n) }
        })
        .await;
        assert_eq!(res.unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_transient_then_succeeds() {
        let calls = AtomicU32::new(0);
        let res = run(RetryPolicy::no_backoff(3), || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    Err(RemoteError::new(RemoteErrorKind::Network, "flake"))
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(res.unwrap(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn gives_up_after_budget() {
        let calls = AtomicU32::new(0);
        let res: RemoteResult<()> = run(RetryPolicy::no_backoff(2), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Err(RemoteError::new(RemoteErrorKind::Network, "still flake")) }
        })
        .await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), RemoteErrorKind::Network);
        // 1 initial + 2 retries = 3 attempts.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_auth_failure() {
        let calls = AtomicU32::new(0);
        let res: RemoteResult<()> = run(RetryPolicy::no_backoff(5), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Err(RemoteError::permission_denied("bad password")) }
        })
        .await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().kind(), RemoteErrorKind::PermissionDenied);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn does_not_retry_not_found() {
        let calls = AtomicU32::new(0);
        let res: RemoteResult<()> = run(RetryPolicy::no_backoff(5), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Err(RemoteError::not_found("gone")) }
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn observer_fires_on_each_retry() {
        let hooks = RetryHooks::new();
        let counter = Arc::new(AtomicU32::new(0));
        {
            let c = Arc::clone(&counter);
            hooks.add(Arc::new(move |_op, _attempt, _backoff| {
                c.fetch_add(1, Ordering::SeqCst);
            }));
        }
        let calls = AtomicU32::new(0);
        let res: RemoteResult<()> = with_retry(
            "obs-test",
            &RetryPolicy::no_backoff(3),
            Some(&hooks),
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 2 {
                        Err(RemoteError::new(RemoteErrorKind::Network, "flake"))
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;
        assert!(res.is_ok());
        // Observer fires once per *retry* (not per attempt), so
        // 2 retries → 2 notifications.
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn timeout_maps_to_network_kind_and_retries() {
        let calls = AtomicU32::new(0);
        let policy = RetryPolicy {
            timeout: Duration::from_millis(50),
            retries: 2,
            backoff_initial: Duration::ZERO,
            backoff_max: Duration::ZERO,
            backoff_multiplier: 1.0,
        };
        let res: RemoteResult<()> = with_retry("timeout-test", &policy, None, || {
            let attempt = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if attempt < 2 {
                    // Sleep longer than the timeout.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Ok(())
                } else {
                    Ok(())
                }
            }
        })
        .await;
        assert!(res.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let policy = RetryPolicy {
            timeout: Duration::from_secs(30),
            retries: 5,
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_millis(500),
            backoff_multiplier: 2.0,
        };
        assert_eq!(backoff_for(0, &policy), Duration::from_millis(100));
        assert_eq!(backoff_for(1, &policy), Duration::from_millis(200));
        assert_eq!(backoff_for(2, &policy), Duration::from_millis(400));
        // Would be 800 without the cap.
        assert_eq!(backoff_for(3, &policy), Duration::from_millis(500));
        assert_eq!(backoff_for(10, &policy), Duration::from_millis(500));
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let base = Duration::from_millis(200);
        for _ in 0..200 {
            let j = jitter(base);
            // ±25% ⇒ [150, 250].
            assert!(j >= Duration::from_millis(150), "too low: {:?}", j);
            assert!(j <= Duration::from_millis(250), "too high: {:?}", j);
        }
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
    }
}
