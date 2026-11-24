//! `model = "auto"` for OpenAI-compatible servers: the model id is read from the server's
//! `GET {base_url}/models` instead of being written into the config.
//!
//! Self-hosted servers rename models (llama.cpp → vLLM changed `Qwen3.8-27B-UD-Q6_K_L` to
//! `Qwen3.8-27B-FP8`, TEA `tea-asr-1` to `tea-asr-1.1`) and keep the old name only as a
//! courtesy alias; once the alias goes, a hard-coded name makes every request fail. With
//! `auto` the provider asks the server on first use, takes the first listed id (vLLM lists its
//! primary served name first), and caches it. When a later request is answered 404 (the model
//! is gone), the cache is dropped and the id is detected once more.
//!
//! Model ids are not user data, so they may appear in `Debug` output; nothing here touches
//! request bodies.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// Config value meaning "ask the server which model to use".
pub const AUTO_MODEL: &str = "auto";

/// A transport failure faster than this is treated as a stale pooled connection and retried
/// once (same rule as the providers' own requests).
const STALE_RETRY_WINDOW: Duration = Duration::from_millis(500);
/// Upper bound for one `/models` request: the list is tiny, and detection runs inside the
/// first polish or transcription, so it must not eat that call's whole budget.
const DETECT_TIMEOUT_CAP: Duration = Duration::from_millis(2000);

/// Whether a configured model id means [`AUTO_MODEL`] (case-insensitive, surrounding spaces
/// ignored).
pub fn is_auto(model: &str) -> bool {
    model.trim().eq_ignore_ascii_case(AUTO_MODEL)
}

/// Why detection failed; each provider maps it to its own error type.
#[derive(Debug)]
pub(crate) enum DetectError {
    /// `/models` answered with a non-2xx status.
    Status(reqwest::StatusCode),
    /// The request did not complete (timeout, refused, reset, unreadable body).
    Transport(reqwest::Error),
    /// The list was empty or not in the OpenAI format.
    NoModel,
}

#[derive(Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    #[serde(default)]
    id: String,
}

/// The detected model id, shared by clones of one provider.
#[derive(Debug, Clone, Default)]
pub(crate) struct AutoModel(Arc<Mutex<Option<String>>>);

impl AutoModel {
    /// The cached id, or a fresh detection when none is cached.
    pub async fn get(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        api_key: Option<&str>,
        timeout: Duration,
    ) -> Result<String, DetectError> {
        if let Some(id) = self.cached() {
            return Ok(id);
        }
        let id = detect(client, base_url, api_key, timeout).await?;
        *self.lock() = Some(id.clone());
        Ok(id)
    }

    /// Drops the cached id and detects again. `Ok(Some(id))` only when the server now names a
    /// different model than `stale`, i.e. when retrying the request can help.
    pub async fn refresh(
        &self,
        stale: &str,
        client: &reqwest::Client,
        base_url: &str,
        api_key: Option<&str>,
        timeout: Duration,
    ) -> Result<Option<String>, DetectError> {
        *self.lock() = None;
        let id = self.get(client, base_url, api_key, timeout).await?;
        Ok((id != stale).then_some(id))
    }

    pub fn cached(&self) -> Option<String> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        // A poisoned lock only means another thread panicked mid-assignment of a String.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `GET {base_url}/models` → the first non-empty `data[].id`.
async fn detect(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    timeout: Duration,
) -> Result<String, DetectError> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let timeout = timeout.min(DETECT_TIMEOUT_CAP);
    let build = || {
        let req = client.get(&url).timeout(timeout);
        match api_key.filter(|k| !k.is_empty()) {
            Some(k) => req.bearer_auth(k),
            None => req,
        }
    };
    let started = Instant::now();
    let resp = match build().send().await {
        Ok(r) => r,
        Err(e) if !e.is_timeout() && !e.is_connect() && started.elapsed() < STALE_RETRY_WINDOW => {
            build().send().await.map_err(DetectError::Transport)?
        }
        Err(e) => return Err(DetectError::Transport(e)),
    };
    if !resp.status().is_success() {
        return Err(DetectError::Status(resp.status()));
    }
    let bytes = resp.bytes().await.map_err(DetectError::Transport)?;
    let list: ModelList = serde_json::from_slice(&bytes).map_err(|_| DetectError::NoModel)?;
    list.data
        .into_iter()
        .map(|m| m.id.trim().to_owned())
        .find(|id| !id.is_empty())
        .ok_or(DetectError::NoModel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn list(ids: &[&str]) -> ResponseTemplate {
        let data: Vec<_> = ids
            .iter()
            .map(|id| json!({"id": id, "object": "model"}))
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({"object": "list", "data": data}))
    }

    const T: Duration = Duration::from_secs(2);

    #[test]
    fn auto_spelling() {
        assert!(is_auto("auto"));
        assert!(is_auto(" AUTO "));
        assert!(!is_auto("auto-model"));
        assert!(!is_auto(""));
    }

    #[tokio::test]
    async fn takes_first_id_sends_key_and_caches() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer k"))
            .respond_with(list(&["", "Qwen3.8-27B-FP8", "Qwen3.8-27B-UD-Q6_K_L"]))
            .expect(1)
            .mount(&server)
            .await;
        let a = AutoModel::default();
        let c = reqwest::Client::new();
        let base = format!("{}/v1/", server.uri());
        assert_eq!(
            a.get(&c, &base, Some("k"), T).await.unwrap(),
            "Qwen3.8-27B-FP8"
        );
        // Second call is served from the cache (the mock expects exactly one request).
        assert_eq!(
            a.get(&c, &base, Some("k"), T).await.unwrap(),
            "Qwen3.8-27B-FP8"
        );
        assert_eq!(a.clone().cached().as_deref(), Some("Qwen3.8-27B-FP8"));
    }

    #[tokio::test]
    async fn refresh_reports_only_a_changed_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(list(&["m1"]))
            .mount(&server)
            .await;
        let a = AutoModel::default();
        let c = reqwest::Client::new();
        let base = server.uri();
        assert_eq!(a.refresh("m1", &c, &base, None, T).await.unwrap(), None);
        assert_eq!(
            a.refresh("old", &c, &base, None, T)
                .await
                .unwrap()
                .as_deref(),
            Some("m1")
        );
    }

    #[tokio::test]
    async fn failures() {
        let c = reqwest::Client::new();
        for (resp, want) in [
            (ResponseTemplate::new(401), "status"),
            (list(&[]), "none"),
            (
                ResponseTemplate::new(200).set_body_string("not json"),
                "none",
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(resp)
                .mount(&server)
                .await;
            let e = AutoModel::default()
                .get(&c, &server.uri(), None, T)
                .await
                .unwrap_err();
            match (want, &e) {
                ("status", DetectError::Status(s)) => assert_eq!(s.as_u16(), 401),
                ("none", DetectError::NoModel) => {}
                _ => panic!("{want}: {e:?}"),
            }
        }
        let e = AutoModel::default()
            .get(&c, "http://127.0.0.1:9/v1", None, T)
            .await
            .unwrap_err();
        assert!(matches!(e, DetectError::Transport(_)), "{e:?}");
    }
}
