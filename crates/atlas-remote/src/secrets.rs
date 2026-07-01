//! Thin wrapper over the [`keyring`] crate for storing remote credentials.
//!
//! Secrets are keyed by an opaque `credential_ref` string that Atlas stores
//! alongside a [`atlas_core::RemoteUri`]. The `credential_ref` is deliberately
//! *not* the secret material — retrieving the secret always requires a call to
//! [`retrieve`], which delegates to the OS keychain.
//!
//! The `namespace` argument prevents test runs and staging binaries from
//! trampling the real user keychain. Production code uses [`ATLAS_NAMESPACE`];
//! tests use a randomised prefix so parallel tests don't collide.

use std::fmt;

use thiserror::Error;

/// Default namespace used by shipping builds. Test-only code should pass its
/// own namespace to keep the user keychain clean.
pub const ATLAS_NAMESPACE: &str = "com.atlas.credentials";

/// Errors returned by the secret store.
#[derive(Debug, Error)]
pub enum SecretError {
    /// The underlying keychain returned an error.
    #[error("keychain error: {0}")]
    Keyring(#[from] keyring::Error),
    /// The requested credential ref was not found.
    #[error("credential not found: {0}")]
    NotFound(String),
}

/// Opaque credential handle used by callers to refer to a stored secret.
///
/// The handle round-trips through serde as a plain string so it can live on a
/// [`atlas_core::RemoteUri`] and survive workspace-state (de)serialization.
#[derive(Debug, Clone)]
pub struct CredentialRef(String);

impl CredentialRef {
    /// Build a credential ref from an already-known string (e.g. read from
    /// persisted workspace state).
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw handle as a `&str`; suitable for passing to [`retrieve`].
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the underlying handle string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for CredentialRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<CredentialRef> for String {
    fn from(value: CredentialRef) -> Self {
        value.0
    }
}

fn entry_for(namespace: &str, account: &str) -> Result<keyring::Entry, SecretError> {
    Ok(keyring::Entry::new(namespace, account)?)
}

/// Store `secret` in the OS keychain under `namespace` + `account`, returning
/// a [`CredentialRef`] that can later be passed to [`retrieve`].
///
/// The returned handle is `"{namespace}::{account}"`; callers should not rely
/// on that exact format — treat it as opaque.
///
/// # Errors
///
/// Returns [`SecretError::Keyring`] if the OS keychain rejects the write.
pub fn store(namespace: &str, account: &str, secret: &str) -> Result<CredentialRef, SecretError> {
    let entry = entry_for(namespace, account)?;
    entry.set_password(secret)?;
    Ok(CredentialRef(format!("{namespace}::{account}")))
}

/// Retrieve a secret previously stored via [`store`].
///
/// # Errors
///
/// Returns [`SecretError::NotFound`] if the credential ref is malformed or the
/// keychain does not contain a matching entry, or [`SecretError::Keyring`]
/// for any other OS error.
pub fn retrieve(credential_ref: &str) -> Result<String, SecretError> {
    let (namespace, account) = split(credential_ref)?;
    let entry = entry_for(namespace, account)?;
    match entry.get_password() {
        Ok(secret) => Ok(secret),
        Err(keyring::Error::NoEntry) => Err(SecretError::NotFound(credential_ref.to_owned())),
        Err(e) => Err(SecretError::Keyring(e)),
    }
}

/// Delete a secret previously stored via [`store`].
///
/// Absent entries are silently treated as success — the post-condition (the
/// credential no longer exists) is what callers care about.
///
/// # Errors
///
/// Returns [`SecretError::Keyring`] for any non-`NoEntry` OS error, or
/// [`SecretError::NotFound`] if the handle is malformed.
pub fn delete(credential_ref: &str) -> Result<(), SecretError> {
    let (namespace, account) = split(credential_ref)?;
    let entry = entry_for(namespace, account)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(SecretError::Keyring(e)),
    }
}

fn split(credential_ref: &str) -> Result<(&str, &str), SecretError> {
    credential_ref
        .split_once("::")
        .ok_or_else(|| SecretError::NotFound(credential_ref.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_namespace() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        format!("com.atlas.tests.{}.{}", std::process::id(), n)
    }

    #[test]
    fn credential_ref_round_trips_through_string() {
        let handle = CredentialRef::new("ns::acct");
        assert_eq!(handle.as_str(), "ns::acct");
        assert_eq!(String::from(handle.clone()), "ns::acct".to_owned());
        assert_eq!(format!("{handle}"), "ns::acct");
    }

    #[test]
    fn retrieve_rejects_malformed_ref() {
        let err = retrieve("no-double-colon").unwrap_err();
        assert!(matches!(err, SecretError::NotFound(_)));
    }

    #[test]
    fn split_extracts_namespace_and_account() {
        let (ns, acct) = split("com.atlas.creds::alice@host").expect("split");
        assert_eq!(ns, "com.atlas.creds");
        assert_eq!(acct, "alice@host");
    }

    // The real store/retrieve/delete round-trip requires an actual OS keychain.
    // On CI runners without a keyring available (headless Linux without
    // dbus + secret-service, sandboxed macOS), this test is skipped rather
    // than failing the whole suite. The split() tests above cover the pure
    // logic side.
    #[test]
    fn store_retrieve_delete_round_trip_when_keychain_available() {
        let namespace = test_namespace();
        let account = "atlas-test-account";
        let secret = "s3cr3t-passphrase";

        let handle = match store(&namespace, account, secret) {
            Ok(h) => h,
            Err(SecretError::Keyring(_)) => {
                // No usable keychain in this environment (common on CI).
                // Skip the round-trip half of the test.
                return;
            }
            Err(other) => panic!("unexpected error storing secret: {other}"),
        };

        let got = retrieve(handle.as_str()).expect("retrieve stored secret");
        assert_eq!(got, secret);

        delete(handle.as_str()).expect("delete stored secret");

        let err = retrieve(handle.as_str()).expect_err("stored secret should be gone");
        assert!(matches!(err, SecretError::NotFound(_)));
    }
}
