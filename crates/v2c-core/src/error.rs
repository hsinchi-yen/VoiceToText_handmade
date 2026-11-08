//! Error types. Errors must never embed user text (they end up in logs).

use thiserror::Error;

pub type Result<T, E = V2cError> = std::result::Result<T, E>;

/// Pipeline stage, used in timeout and provider errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Asr,
    Polish,
    Translate,
    Inject,
}

/// Why a provider call failed, in terms the UI can explain to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// Bad or missing API key (HTTP 401/403).
    Unauthorized,
    /// Quota or rate limit (HTTP 429).
    RateLimited,
    /// Could not connect (e.g. LM Studio not running, no network).
    Unreachable,
    /// Endpoint does not support this operation (e.g. no audio transcription).
    Unsupported,
    /// Response could not be parsed or was empty.
    BadResponse,
    /// Any other server-side failure (HTTP 5xx).
    Server,
}

#[derive(Debug, Error)]
pub enum V2cError {
    #[error("audio error: {0}")]
    Audio(String),

    #[error("{stage:?} provider `{provider}` failed: {kind:?}")]
    Provider {
        stage: Stage,
        provider: String,
        kind: ProviderErrorKind,
    },

    #[error("{stage:?} timed out after {ms} ms")]
    Timeout { stage: Stage, ms: u64 },

    /// A required provider for this mode is not configured (e.g. translate without an LLM).
    #[error("not configured: {0}")]
    NotConfigured(&'static str),

    #[error("config error: {0}")]
    Config(String),

    #[error("storage error: {0}")]
    Storage(String),
}
