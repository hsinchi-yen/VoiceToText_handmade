//! OpenAI-compatible speech-to-text on a user-supplied `base_url` (self-hosted servers,
//! another PC's service). Same wire format as [`super::openai::OpenAiAsr`]; the API key is
//! optional. Servers that only implement chat completions answer 404/405/501 on
//! `/audio/transcriptions`, which surfaces as `ProviderErrorKind::Unsupported`.
//!
//! `model = "auto"` reads the id from the server's `/models` on first use and detects again
//! after a 404 (see [`crate::auto_model`]).

use std::time::Duration;

use async_trait::async_trait;

use super::openai::OpenAiAsr;
use super::{AsrContext, AsrProvider, Transcript};
use crate::asr::http::{failure_kind, CloudHttp};
use crate::audio::AudioClip;
use crate::auto_model::{is_auto, AutoModel, DetectError};
use crate::error::ProviderErrorKind;
use crate::secrets::ApiKey;
use crate::{Result, V2cError};

#[derive(Debug)]
pub struct CompatAsr {
    inner: OpenAiAsr,
    /// `Some` when the configured model is `"auto"`.
    auto: Option<Auto>,
}

/// What detection needs besides the cache (the inner provider keeps its fields private).
#[derive(Debug)]
struct Auto {
    model: AutoModel,
    http: CloudHttp,
    base_url: String,
    api_key: Option<ApiKey>,
    timeout: Duration,
}

impl CompatAsr {
    pub fn new(
        http: CloudHttp,
        base_url: impl Into<String>,
        api_key: Option<ApiKey>,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        let (base_url, model) = (base_url.into(), model.into());
        let auto = is_auto(&model).then(|| Auto {
            model: AutoModel::default(),
            http: http.clone(),
            base_url: base_url.clone(),
            api_key: api_key.clone(),
            timeout,
        });
        Self {
            inner: OpenAiAsr::new(http, base_url, api_key, model, timeout).with_name("compat"),
            auto,
        }
    }

    pub fn with_name(self, name: impl Into<String>) -> Self {
        Self {
            inner: self.inner.with_name(name),
            auto: self.auto,
        }
    }

    fn detect_err(&self, e: DetectError) -> V2cError {
        let call = self.inner.call_site();
        match e {
            DetectError::Status(s) => call.error(failure_kind(s, "")),
            DetectError::Transport(e) => call.transport_error(&e),
            DetectError::NoModel => call.error(ProviderErrorKind::BadResponse),
        }
    }
}

impl Auto {
    fn key(&self) -> Option<&str> {
        self.api_key.as_ref().map(ApiKey::expose)
    }
}

#[async_trait]
impl AsrProvider for CompatAsr {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn transcribe(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<Transcript> {
        let Some(a) = &self.auto else {
            return self.inner.transcribe(clip, ctx).await;
        };
        let client = a.http.client();
        let model = a
            .model
            .get(client, &a.base_url, a.key(), a.timeout)
            .await
            .map_err(|e| self.detect_err(e))?;
        match self.inner.transcribe_as(clip, ctx, &model).await {
            // 404 (reported as Unsupported): the detected model may have been renamed. 405/501
            // map to Unsupported too (no transcription endpoint); re-detecting then costs one
            // extra GET and still ends as Unsupported.
            Err(V2cError::Provider {
                kind: ProviderErrorKind::Unsupported,
                ..
            }) => {
                let fresh = a
                    .model
                    .refresh(&model, client, &a.base_url, a.key(), a.timeout)
                    .await
                    .map_err(|e| self.detect_err(e))?;
                match fresh {
                    Some(m) => self.inner.transcribe_as(clip, ctx, &m).await,
                    None => Err(self.inner.call_site().error(ProviderErrorKind::Unsupported)),
                }
            }
            r => r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::http::cloud_http_client;
    use crate::asr::test_support::*;
    use crate::error::{ProviderErrorKind, Stage};
    use crate::V2cError;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn asr(server: &MockServer, key: Option<&str>) -> CompatAsr {
        CompatAsr::new(
            cloud_http_client().unwrap(),
            format!("{}/v1/", server.uri()),
            key.map(ApiKey::new),
            "whisper-large",
            Duration::from_millis(300),
        )
    }

    fn ctx() -> AsrContext {
        AsrContext {
            vocabulary: vec![],
            prompt: "HINT".into(),
        }
    }

    #[tokio::test]
    async fn success_with_optional_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header("authorization", "Bearer local-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "hello"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let a = asr(&server, Some("local-key"));
        assert_eq!(a.name(), "compat");
        let t = a.transcribe(&tone(), &ctx()).await.unwrap();
        assert_eq!(t.text, "hello");
        let req = only_request(&server).await;
        let body = String::from_utf8_lossy(&req.body);
        assert!(body.contains("name=\"model\"\r\n\r\nwhisper-large"));
        assert!(contains_bytes(&req.body, b"RIFF"));
    }

    #[tokio::test]
    async fn no_key_sends_no_auth_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "x"})),
            )
            .mount(&server)
            .await;
        asr(&server, None)
            .transcribe(&tone(), &ctx())
            .await
            .unwrap();
        assert!(only_request(&server)
            .await
            .headers
            .get("authorization")
            .is_none());
    }

    #[tokio::test]
    async fn endpoint_without_audio_is_unsupported() {
        let server = MockServer::start().await;
        // e.g. a chat-only server: no route for /audio/transcriptions.
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let err = asr(&server, None)
            .transcribe(&tone(), &ctx())
            .await
            .unwrap_err();
        assert_provider_err(&err, Stage::Asr, ProviderErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn status_errors_and_timeout() {
        for (status, kind) in [
            (401, ProviderErrorKind::Unauthorized),
            (429, ProviderErrorKind::RateLimited),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let err = asr(&server, Some("local-key"))
                .transcribe(&tone(), &ctx())
                .await
                .unwrap_err();
            assert_provider_err(&err, Stage::Asr, kind);
            assert_no_secrets(&err, &["local-key", "HINT"]);
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(slow_ok(serde_json::json!({"text": "late"})))
            .mount(&server)
            .await;
        let err = asr(&server, None)
            .transcribe(&tone(), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            V2cError::Timeout {
                stage: Stage::Asr,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn unreachable_server() {
        // Port 9 (discard) on localhost is closed on a normal dev box.
        let a = CompatAsr::new(
            cloud_http_client().unwrap(),
            "http://127.0.0.1:9/v1",
            None,
            "m",
            Duration::from_millis(500),
        );
        let err = a.transcribe(&tone(), &ctx()).await.unwrap_err();
        assert!(
            matches!(
                err,
                V2cError::Provider {
                    kind: ProviderErrorKind::Unreachable,
                    ..
                } | V2cError::Timeout { .. }
            ),
            "{err:?}"
        );
    }

    fn auto_asr(server: &MockServer) -> CompatAsr {
        CompatAsr::new(
            cloud_http_client().unwrap(),
            format!("{}/v1", server.uri()),
            Some(ApiKey::new("local-key")),
            "auto",
            Duration::from_millis(500),
        )
        .with_name("rd-tea-asr")
    }

    fn models(ids: &[&str]) -> ResponseTemplate {
        let data: Vec<_> = ids
            .iter()
            .map(|id| serde_json::json!({ "id": id }))
            .collect();
        ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": data }))
    }

    fn sent_models(reqs: &[wiremock::Request]) -> Vec<String> {
        reqs.iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| {
                let body = String::from_utf8_lossy(&r.body).to_string();
                let rest = body
                    .split("name=\"model\"\r\n\r\n")
                    .nth(1)
                    .unwrap()
                    .to_owned();
                rest.split("\r\n").next().unwrap().to_owned()
            })
            .collect()
    }

    #[tokio::test]
    async fn auto_model_is_detected_with_the_key_and_cached() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer local-key"))
            .respond_with(models(&["tea-asr-1.1"]))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "hi"})),
            )
            .mount(&server)
            .await;
        let a = auto_asr(&server);
        assert_eq!(a.name(), "rd-tea-asr");
        a.transcribe(&tone(), &ctx()).await.unwrap();
        a.transcribe(&tone(), &ctx()).await.unwrap();
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(sent_models(&reqs), vec!["tea-asr-1.1", "tea-asr-1.1"]);
    }

    #[tokio::test]
    async fn auto_model_is_detected_again_after_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(models(&["tea-asr-1"]))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(models(&["tea-asr-1.1"]))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "hi"})),
            )
            .mount(&server)
            .await;
        auto_asr(&server).transcribe(&tone(), &ctx()).await.unwrap();
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(sent_models(&reqs), vec!["tea-asr-1", "tea-asr-1.1"]);
    }

    #[tokio::test]
    async fn auto_model_failures_keep_error_kinds_and_hide_the_key() {
        for (resp, kind) in [
            (ResponseTemplate::new(401), ProviderErrorKind::Unauthorized),
            (models(&[]), ProviderErrorKind::BadResponse),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(resp)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200))
                .expect(0)
                .mount(&server)
                .await;
            let err = auto_asr(&server)
                .transcribe(&tone(), &ctx())
                .await
                .unwrap_err();
            assert_provider_err(&err, Stage::Asr, kind);
            assert_no_secrets(&err, &["local-key", "HINT"]);
        }
        // Same model still listed after a 404: report it, do not resend.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(models(&["m"]))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        let err = auto_asr(&server)
            .transcribe(&tone(), &ctx())
            .await
            .unwrap_err();
        assert_provider_err(&err, Stage::Asr, ProviderErrorKind::Unsupported);
    }
}
