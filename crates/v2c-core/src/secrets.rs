//! API key storage, looked up by `CloudService::id`.
//!
//! v2c-core stays OS-agnostic, so this module only defines the [`SecretStore`] interface plus:
//! - [`MemorySecretStore`] for tests and for callers that keep keys elsewhere;
//! - [`KeyringSecretStore`] (cargo feature `keyring`), built on `keyring-core`. `keyring-core`
//!   itself has no platform code: the binary installs the platform store once at startup, e.g.
//!   on Windows `keyring_core::set_default_store(windows_native_keyring_store::Store::new()?)`
//!   (Credential Manager). That call belongs in `v2c-platform` / the desktop app.
//!
//! Keys never appear in `Debug` output or in errors.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::{error::V2cError, Result};

/// An API key. `Debug` is redacted and there is no `Display`; use [`ApiKey::expose`] only to
/// build the request header.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The raw key, for the HTTP auth header only. Never log it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The last 4 characters, for the settings UI ("…abcd"). Short keys show nothing.
    pub fn last4(&self) -> String {
        let n = self.0.chars().count();
        if n < 12 {
            return String::new();
        }
        self.0.chars().skip(n - 4).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Where API keys live. Keyed by `CloudService::id`.
pub trait SecretStore: Send + Sync {
    /// `Ok(None)` when no key is stored for this service.
    fn get(&self, service_id: &str) -> Result<Option<ApiKey>>;
    fn set(&self, service_id: &str, key: &ApiKey) -> Result<()>;
    /// Deleting a missing key is not an error.
    fn delete(&self, service_id: &str) -> Result<()>;
}

/// In-memory store (tests, or callers that hold keys themselves, e.g. Android before the
/// Keystore bridge exists).
#[derive(Default)]
pub struct MemorySecretStore {
    keys: Mutex<HashMap<String, ApiKey>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.keys.lock().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("MemorySecretStore")
            .field("entries", &n)
            .finish()
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, service_id: &str) -> Result<Option<ApiKey>> {
        Ok(self.lock()?.get(service_id).cloned())
    }

    fn set(&self, service_id: &str, key: &ApiKey) -> Result<()> {
        self.lock()?.insert(service_id.to_owned(), key.clone());
        Ok(())
    }

    fn delete(&self, service_id: &str) -> Result<()> {
        self.lock()?.remove(service_id);
        Ok(())
    }
}

impl MemorySecretStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, ApiKey>>> {
        self.keys
            .lock()
            .map_err(|_| V2cError::Storage("secret store lock poisoned".into()))
    }
}

/// Keyring entry "service" name; the entry "user" is the `CloudService::id`.
#[cfg(feature = "keyring")]
pub const KEYRING_SERVICE: &str = "v2c";

/// OS keyring via `keyring-core`'s default store (installed by the app at startup).
#[cfg(feature = "keyring")]
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

#[cfg(feature = "keyring")]
impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self {
            service: KEYRING_SERVICE.into(),
        }
    }
}

#[cfg(feature = "keyring")]
impl KeyringSecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a different keyring service name (e.g. to isolate tests).
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, service_id: &str) -> Result<keyring_core::Entry> {
        keyring_core::Entry::new(&self.service, service_id).map_err(keyring_error)
    }
}

/// Maps keyring errors to fixed messages: some variants (e.g. `BadEncoding`) carry the
/// secret's bytes, so the original error is never formatted.
#[cfg(feature = "keyring")]
fn keyring_error(e: keyring_core::Error) -> V2cError {
    use keyring_core::Error as E;
    let what = match e {
        E::NoDefaultStore => "keyring: no credential store installed",
        E::NoStorageAccess(_) => "keyring: storage locked or access denied",
        E::PlatformFailure(_) => "keyring: platform failure",
        E::BadEncoding(_) | E::BadDataFormat(..) => "keyring: stored key is not valid text",
        E::TooLong(..) | E::Invalid(..) => "keyring: invalid entry",
        E::Ambiguous(_) => "keyring: ambiguous entry",
        E::NotSupportedByStore(_) => "keyring: not supported by store",
        _ => "keyring: error",
    };
    V2cError::Storage(what.into())
}

#[cfg(feature = "keyring")]
impl SecretStore for KeyringSecretStore {
    fn get(&self, service_id: &str) -> Result<Option<ApiKey>> {
        match self.entry(service_id)?.get_password() {
            Ok(k) => Ok(Some(ApiKey::new(k))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(keyring_error(e)),
        }
    }

    fn set(&self, service_id: &str, key: &ApiKey) -> Result<()> {
        self.entry(service_id)?
            .set_password(key.expose())
            .map_err(keyring_error)
    }

    fn delete(&self, service_id: &str) -> Result<()> {
        match self.entry(service_id)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(keyring_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-test-SECRETVALUE-1234";

    #[test]
    fn api_key_debug_is_redacted() {
        let k = ApiKey::new(KEY);
        let s = format!("{k:?}");
        assert!(!s.contains("SECRET"));
        assert!(!s.contains("1234"));
        assert_eq!(k.expose(), KEY);
    }

    #[test]
    fn last4_only_for_long_keys() {
        assert_eq!(ApiKey::new(KEY).last4(), "1234");
        assert_eq!(ApiKey::new("short").last4(), "");
    }

    #[test]
    fn memory_store_roundtrip_and_debug() {
        let s = MemorySecretStore::new();
        assert_eq!(s.get("openai").unwrap(), None);
        s.set("openai", &ApiKey::new(KEY)).unwrap();
        assert_eq!(s.get("openai").unwrap().unwrap().expose(), KEY);
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("SECRET"));
        assert!(dbg.contains("entries: 1"));
        s.delete("openai").unwrap();
        s.delete("openai").unwrap();
        assert_eq!(s.get("openai").unwrap(), None);
    }

    #[cfg(feature = "keyring")]
    #[test]
    fn keyring_store_roundtrip_with_sample_store() {
        keyring_core::set_default_store(keyring_core::sample::Store::new().unwrap());
        let s = KeyringSecretStore::with_service("v2c-test");
        assert_eq!(s.get("gemini").unwrap(), None);
        s.set("gemini", &ApiKey::new(KEY)).unwrap();
        assert_eq!(s.get("gemini").unwrap().unwrap().expose(), KEY);
        s.delete("gemini").unwrap();
        s.delete("gemini").unwrap();
        assert_eq!(s.get("gemini").unwrap(), None);
        assert!(!format!("{s:?}").contains("SECRET"));
    }
}
