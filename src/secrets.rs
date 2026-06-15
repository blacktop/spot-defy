//! Keychain-backed storage for the rotating OAuth refresh token.
//!
//! Uses `keyring-core` with the `apple-native-keyring-store` macOS login
//! Keychain store (the `keychain` credential, never `protected`, to avoid the
//! `-34018` entitlement error on un-codesigned binaries). The refresh token is
//! the only secret persisted; access tokens stay in memory as
//! [`SecretString`] and are never written to disk.

use crate::error::AuthError;
use apple_native_keyring_store::keychain::Store;
use keyring_core::{Entry, Error as KeyringError};
use secrecy::{ExposeSecret as _, SecretString};

/// Keychain service name (the application identifier).
const SERVICE: &str = "spot-defy";

/// Which OAuth flow a refresh token belongs to. The streaming (keymaster) and
/// Web API flows use separate client ids, so their rotating refresh tokens are
/// stored under distinct Keychain accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// The librespot streaming-session (keymaster) refresh token.
    Streaming,
    /// The Web API (extended-quota client) refresh token.
    WebApi,
}

impl TokenKind {
    /// The Keychain account name for this token kind.
    fn account(self) -> &'static str {
        match self {
            TokenKind::Streaming => "oauth-refresh-streaming",
            TokenKind::WebApi => "oauth-refresh-webapi",
        }
    }
}

/// Install the macOS Keychain store as the process-wide default `keyring-core`
/// store.
///
/// Must be called once at startup before any [`keyring_core::Entry`] is created.
///
/// # Errors
///
/// Returns an error if the Keychain store cannot be constructed or registered.
pub fn install_default_store() -> anyhow::Result<()> {
    let store = Store::new()
        .map_err(|err| anyhow::anyhow!("failed to construct macOS keychain store: {err}"))?;
    keyring_core::set_default_store(store);
    Ok(())
}

/// Load the stored refresh token for `kind` from the Keychain.
///
/// # Returns
///
/// `Ok(Some(token))` when a refresh token is present, `Ok(None)` when no entry
/// exists (first launch / after logout).
///
/// # Errors
///
/// Returns [`AuthError::Keychain`] if the Keychain cannot be read.
pub fn load_refresh_token(kind: TokenKind) -> Result<Option<SecretString>, AuthError> {
    load_from(SERVICE, kind.account())
}

/// Persist the rotating refresh token for `kind` to the Keychain.
///
/// PKCE refresh tokens are single-use; the caller must store the new token
/// returned by every refresh before discarding the prior one.
///
/// # Errors
///
/// Returns [`AuthError::Keychain`] if the Keychain cannot be written.
pub fn store_refresh_token(kind: TokenKind, token: &SecretString) -> Result<(), AuthError> {
    store_into(SERVICE, kind.account(), token)
}

/// Remove the stored refresh token for `kind` from the Keychain (logout).
///
/// Deleting a non-existent entry is treated as success so logout is idempotent.
///
/// # Errors
///
/// Returns [`AuthError::Keychain`] if the Keychain entry cannot be deleted.
pub fn clear_refresh_token(kind: TokenKind) -> Result<(), AuthError> {
    clear_from(SERVICE, kind.account())
}

/// Read a refresh token from the given Keychain `service`/`account` pair.
///
/// A missing entry maps to `Ok(None)`; any other Keychain failure maps to
/// [`AuthError::Keychain`].
fn load_from(service: &str, account: &str) -> Result<Option<SecretString>, AuthError> {
    let entry = entry_for(service, account)?;
    match entry.get_password() {
        Ok(password) => Ok(Some(SecretString::from(password))),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(err) => Err(AuthError::Keychain(format!(
            "reading refresh token failed: {err}"
        ))),
    }
}

/// Write a refresh token into the given Keychain `service`/`account` pair.
fn store_into(service: &str, account: &str, token: &SecretString) -> Result<(), AuthError> {
    let entry = entry_for(service, account)?;
    entry
        .set_password(token.expose_secret())
        .map_err(|err| AuthError::Keychain(format!("writing refresh token failed: {err}")))
}

/// Delete the refresh token at the given Keychain `service`/`account` pair.
///
/// A missing entry is treated as success so logout never errors on a fresh
/// install.
fn clear_from(service: &str, account: &str) -> Result<(), AuthError> {
    let entry = entry_for(service, account)?;
    match entry.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(err) => Err(AuthError::Keychain(format!(
            "deleting refresh token failed: {err}"
        ))),
    }
}

/// Build a Keychain [`Entry`] for the given `service`/`account` pair.
fn entry_for(service: &str, account: &str) -> Result<Entry, AuthError> {
    Entry::new(service, account)
        .map_err(|err| AuthError::Keychain(format!("opening keychain entry failed: {err}")))
}

#[cfg(test)]
mod tests {
    use crate::secrets::{clear_from, load_from, store_into};
    use keyring_core::mock::Store as MockStore;
    use keyring_core::{Entry, Error as KeyringError};
    use secrecy::{ExposeSecret as _, SecretString};
    use std::sync::Once;
    use std::sync::atomic::{AtomicU64, Ordering};

    static INIT: Once = Once::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Install the in-memory mock store once per test binary so the wrapper
    /// logic is exercised without touching the real macOS Keychain.
    fn install_mock_store() {
        INIT.call_once(|| {
            let store = MockStore::new().expect("mock store builds");
            keyring_core::set_default_store(store);
        });
    }

    /// Unique service/account pair so concurrent tests never share an entry in
    /// the process-global mock store.
    fn unique_keys(tag: &str) -> (String, String) {
        let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
        (
            format!("spot-defy-test-{tag}-{nonce}"),
            format!("acct-{nonce}"),
        )
    }

    #[test]
    fn load_returns_none_when_no_entry_exists() {
        install_mock_store();
        let (service, account) = unique_keys("missing");
        let loaded = load_from(&service, &account).expect("load succeeds");
        assert!(loaded.is_none(), "expected no token for a fresh entry");
    }

    #[test]
    fn store_then_load_round_trips_the_secret() {
        install_mock_store();
        let (service, account) = unique_keys("roundtrip");
        let token = SecretString::from("refresh-rotating-123");
        store_into(&service, &account, &token).expect("store succeeds");
        let loaded = load_from(&service, &account)
            .expect("load succeeds")
            .expect("token present after store");
        assert_eq!(loaded.expose_secret(), "refresh-rotating-123");
    }

    #[test]
    fn store_overwrites_a_prior_token() {
        install_mock_store();
        let (service, account) = unique_keys("overwrite");
        store_into(&service, &account, &SecretString::from("old")).expect("first store");
        store_into(&service, &account, &SecretString::from("new")).expect("second store");
        let loaded = load_from(&service, &account)
            .expect("load succeeds")
            .expect("token present");
        assert_eq!(loaded.expose_secret(), "new");
    }

    #[test]
    fn clear_removes_a_stored_token() {
        install_mock_store();
        let (service, account) = unique_keys("clear");
        store_into(&service, &account, &SecretString::from("doomed")).expect("store succeeds");
        clear_from(&service, &account).expect("clear succeeds");
        let loaded = load_from(&service, &account).expect("load succeeds");
        assert!(loaded.is_none(), "token should be gone after clear");
    }

    #[test]
    fn clear_is_idempotent_on_missing_entry() {
        install_mock_store();
        let (service, account) = unique_keys("idempotent");
        clear_from(&service, &account).expect("clearing a missing entry succeeds");
    }

    #[test]
    fn load_surfaces_non_missing_keychain_errors() {
        install_mock_store();
        let (service, account) = unique_keys("error");
        // Create the entry, then arm the mock to fail the next read with a
        // platform-style error that is NOT NoEntry.
        store_into(&service, &account, &SecretString::from("present")).expect("store succeeds");
        let entry = Entry::new(&service, &account).expect("entry opens");
        let mock: &keyring_core::mock::Cred =
            entry.as_any().downcast_ref().expect("mock credential");
        mock.set_error(KeyringError::Invalid(
            "mock".to_string(),
            "forced read failure".to_string(),
        ));
        let result = load_from(&service, &account);
        assert!(result.is_err(), "non-NoEntry errors must propagate");
    }
}
