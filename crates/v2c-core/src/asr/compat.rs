//! OpenAI-compatible speech-to-text on a user-supplied `base_url` (self-hosted servers,
//! another PC's service). Same wire format as [`super::openai::OpenAiAsr`]; the API key is
//! optional. Servers that only implement chat completions answer 404/405/501 on
//! `/audio/transcriptions`, which surfaces as `ProviderErrorKind::Unsupported`.

use std::time::Duration;

use async_trait::async_trait;

use super::openai::OpenAiAsr;
use super::{AsrContext, AsrProvider, Transcript};
use crate::asr::http::CloudHttp;
use crate::audio::AudioClip;
use crate::secrets::ApiKey;
use crate::Result;

#[derive(Debug)]
pub struct CompatAsr(OpenAiAsr);

impl CompatAsr {
    pub fn new(
        http: CloudHttp,
        base_url: impl Into<String>,
        api_key: Option<ApiKey>,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self(OpenAiAsr::new(http, base_url, api_key, model, timeout).with_name("compat"))
    }

    pub fn with_name(self, name: impl Into<String>) -> Self {
        Self(self.0.with_name(name))
    }
}

#[async_trait]
impl AsrProvider for CompatAsr {
    fn name(&self) -> &str {
        self.0.name()
    }

    async fn transcribe(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<Transcript> {
        self.0.transcribe(clip, ctx).await
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
}
