//! Shared HTTP helpers for cloud providers (ASR and LLM).
//!
//! Lives under `asr/` only because of T1.7's allowed paths; it should move to a shared
//! `cloud/` module once one exists. Nothing here logs or stores request/response bodies:
//! error bodies are inspected for a fixed error code and then dropped.

use std::time::Duration;

use reqwest::{RequestBuilder, Response, StatusCode};

use crate::error::{ProviderErrorKind, Stage, V2cError};
use crate::Result;

/// The HTTP client every cloud provider must use: redirects are never followed, because
/// reqwest would forward custom key headers (`x-goog-api-key`, `x-api-key`) to whatever
/// host a redirect points at. A 3xx answer is reported as `BadResponse`.
///
/// Providers take `CloudHttp` (not a bare `reqwest::Client`) so this cannot be bypassed.
/// Cheap to clone (shares one connection pool); build one per app.
#[derive(Clone, Debug)]
pub struct CloudHttp(reqwest::Client);

impl CloudHttp {
    /// The underlying client, e.g. for T1.5's `OpenAiCompatLlm`. Keep the no-redirect
    /// policy: do not rebuild it with a different one.
    pub fn client(&self) -> &reqwest::Client {
        &self.0
    }
}

/// Builds the shared [`CloudHttp`] (no redirects, rustls). Fails only if the TLS backend
/// cannot initialise.
pub fn cloud_http_client() -> Result<CloudHttp> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map(CloudHttp)
        .map_err(|_| V2cError::Config("could not initialise the HTTP client".into()))
}

/// Who is calling, for error reporting.
#[derive(Debug, Clone)]
pub(crate) struct CallSite {
    pub stage: Stage,
    pub provider: String,
    pub timeout: Duration,
}

impl CallSite {
    pub fn error(&self, kind: ProviderErrorKind) -> V2cError {
        V2cError::Provider {
            stage: self.stage,
            provider: self.provider.clone(),
            kind,
        }
    }

    fn timeout_error(&self) -> V2cError {
        V2cError::Timeout {
            stage: self.stage,
            ms: self.timeout.as_millis() as u64,
        }
    }

    fn transport_error(&self, e: &reqwest::Error) -> V2cError {
        if e.is_timeout() {
            self.timeout_error()
        } else if e.is_decode() || e.is_body() {
            self.error(ProviderErrorKind::BadResponse)
        } else {
            self.error(ProviderErrorKind::Unreachable)
        }
    }

    /// Sends with the call's timeout (covers connect through the full body) and maps
    /// transport failures and non-2xx statuses to `V2cError`.
    pub async fn send(&self, req: RequestBuilder) -> Result<Response> {
        let resp = req
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|e| self.transport_error(&e))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        Err(self.error(failure_kind(status, &body)))
    }

    /// Sends and parses a JSON body.
    pub async fn send_json(&self, req: RequestBuilder) -> Result<serde_json::Value> {
        let resp = self.send(req).await?;
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| self.transport_error(&e))
    }
}

/// Maps an HTTP failure to a kind the UI can explain.
///
/// Gemini answers a bad key with `400 INVALID_ARGUMENT` + reason `API_KEY_INVALID`, so the
/// body is checked for that marker. 404/405/501 mean the endpoint has no such operation
/// (typical for an OpenAI-compatible server without `/audio/transcriptions`).
pub(crate) fn failure_kind(status: StatusCode, body: &str) -> ProviderErrorKind {
    match status.as_u16() {
        401 | 403 => ProviderErrorKind::Unauthorized,
        400 if body.contains("API_KEY_INVALID") => ProviderErrorKind::Unauthorized,
        429 => ProviderErrorKind::RateLimited,
        404 | 405 | 501 => ProviderErrorKind::Unsupported,
        // Redirects are not followed (see `CloudHttp`); a moved endpoint needs a new base_url.
        300..=399 => ProviderErrorKind::BadResponse,
        500..=599 => ProviderErrorKind::Server,
        _ => ProviderErrorKind::BadResponse,
    }
}

/// `base` + `/path`, tolerating a trailing slash on `base`.
pub(crate) fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_kinds() {
        let k = |s: u16, b: &str| failure_kind(StatusCode::from_u16(s).unwrap(), b);
        assert_eq!(k(401, ""), ProviderErrorKind::Unauthorized);
        assert_eq!(k(403, ""), ProviderErrorKind::Unauthorized);
        assert_eq!(
            k(
                400,
                r#"{"error":{"details":[{"reason":"API_KEY_INVALID"}]}}"#
            ),
            ProviderErrorKind::Unauthorized
        );
        assert_eq!(k(400, "bad"), ProviderErrorKind::BadResponse);
        assert_eq!(k(429, ""), ProviderErrorKind::RateLimited);
        assert_eq!(k(404, ""), ProviderErrorKind::Unsupported);
        assert_eq!(k(500, ""), ProviderErrorKind::Server);
        assert_eq!(k(529, ""), ProviderErrorKind::Server);
        assert_eq!(k(302, ""), ProviderErrorKind::BadResponse);
    }

    #[test]
    fn join_url_handles_slashes() {
        assert_eq!(join_url("http://a/v1/", "/models"), "http://a/v1/models");
        assert_eq!(join_url("http://a/v1", "models"), "http://a/v1/models");
    }
}
