//! Encrypted local history of outputs. Implemented in T1.9.
//!
//! Text is encrypted with a platform [`Protector`] (Windows DPAPI, Android Keystore,
//! Linux Secret Service) before it is stored. Audio is never stored.

use crate::{redact::Redacted, Mode, Result};

/// Encrypts data for the current OS user. Implemented in `v2c-platform`.
pub trait Protector: Send + Sync {
    fn protect(&self, plain: &[u8]) -> Result<Vec<u8>>;
    fn unprotect(&self, sealed: &[u8]) -> Result<Vec<u8>>;
}

#[derive(Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    /// Unix time in seconds.
    pub created_at: i64,
    pub mode: Mode,
    /// Foreground app executable when the text was produced.
    pub app: String,
    pub text: String,
}

impl std::fmt::Debug for HistoryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryEntry")
            .field("id", &self.id)
            .field("created_at", &self.created_at)
            .field("mode", &self.mode)
            .field("app", &self.app)
            .field("text", &Redacted(&self.text))
            .finish()
    }
}

/// History store operations (T1.9 provides the SQLite implementation).
pub trait HistoryStore: Send + Sync {
    fn add(&self, mode: Mode, app: &str, text: &str) -> Result<()>;
    fn list(&self, limit: u32) -> Result<Vec<HistoryEntry>>;
    fn delete(&self, id: i64) -> Result<()>;
    fn clear(&self) -> Result<()>;
    /// Deletes entries older than the retention period. Returns how many were removed.
    fn purge_expired(&self, now_unix: i64) -> Result<u64>;
}
