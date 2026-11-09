//! Gemini text model via `generateContent`: `systemInstruction` + one user turn.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use super::LlmProvider;
use crate::asr::gemini::GeminiCall;
use crate::asr::http::CallSite;
use crate::error::Stage;
use crate::secrets::ApiKey;
use crate::Result;

pub struct GeminiLlm {
    http: reqwest::Client,
    base_url: String,
    api_key: ApiKey,
    model: String,
    call: CallSite,
}

impl GeminiLlm {
    /// `model` from `CloudService::llm_model`. Errors report `Stage::Polish` unless changed
    /// with [`GeminiLlm::with_stage`].
    pub fn new(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: ApiKey,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            api_key,
            model: model.into(),
            call: CallSite {
                stage: Stage::Polish,
                provider: "gemini".into(),
                timeout,
            },
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.call.provider = name.into();
        self
    }

    /// Stage reported in errors (`Polish` or `Translate`).
    pub fn with_stage(mut self, stage: Stage) -> Self {
        self.call.stage = stage;
        self
    }
}

impl std::fmt::Debug for GeminiLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiLlm")
            .field("name", &self.call.provider)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl LlmProvider for GeminiLlm {
    fn name(&self) -> &str {
        &self.call.provider
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        GeminiCall {
            http: &self.http,
            base_url: &self.base_url,
            api_key: &self.api_key,
            model: &self.model,
            call: &self.call,
        }
        .generate(system, vec![json!({ "text": user })])
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::test_support::*;
    use crate::error::ProviderErrorKind;
    use crate::V2cError;
    use serde_json::Value;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn llm(server: &MockServer) -> GeminiLlm {
        GeminiLlm::new(
            reqwest::Client::new(),
            format!("{}/v1beta/", server.uri()),
            ApiKey::new("AIza-TEST"),
            "gemini-test-flash",
            Duration::from_millis(300),
        )
        .with_stage(Stage::Translate)
    }

    #[tokio::test]
    async fn success_sends_system_and_user() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-test-flash:generateContent"))
            .and(header("x-goog-api-key", "AIza-TEST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "Hello there."}]}}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let out = llm(&server)
            .complete("SYSTEM-RULES", "USER-TEXT 你好")
            .await
            .unwrap();
        assert_eq!(out, "Hello there.");
        let body: Value = serde_json::from_slice(&only_request(&server).await.body).unwrap();
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "SYSTEM-RULES"
        );
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "USER-TEXT 你好");
    }

    #[tokio::test]
    async fn status_errors_map_to_kinds() {
        for (status, kind) in [
            (401, ProviderErrorKind::Unauthorized),
            (429, ProviderErrorKind::RateLimited),
            (503, ProviderErrorKind::Server),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let err = llm(&server).complete("SYS", "USER-TEXT").await.unwrap_err();
            assert_provider_err(&err, Stage::Translate, kind);
            assert_no_secrets(&err, &["AIza-TEST", "USER-TEXT"]);
        }
    }

    #[tokio::test]
    async fn slow_response_times_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(slow_ok(json!({"candidates": []})))
            .mount(&server)
            .await;
        let err = llm(&server).complete("SYS", "USER").await.unwrap_err();
        assert!(matches!(
            err,
            V2cError::Timeout {
                stage: Stage::Translate,
                ms: 300
            }
        ));
    }
}
