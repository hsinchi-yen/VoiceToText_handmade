//! Claude text model via the Anthropic Messages API: `POST {base_url}/messages`.
//!
//! Headers `x-api-key` and `anthropic-version: 2023-06-01`; top-level `system`; one user
//! message; the reply is the concatenated `text` content blocks. Claude does not accept audio,
//! so there is no Claude ASR.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::LlmProvider;
use crate::asr::http::{join_url, CallSite};
use crate::error::{ProviderErrorKind, Stage};
use crate::secrets::ApiKey;
use crate::Result;

/// Messages API version header (a protocol version, not a model).
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Output cap. Thinking tokens count toward it, so leave headroom over the expected reply.
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct ClaudeLlm {
    http: reqwest::Client,
    base_url: String,
    api_key: ApiKey,
    model: String,
    max_tokens: u32,
    effort: Option<String>,
    call: CallSite,
}

impl ClaudeLlm {
    /// `base_url` e.g. `https://api.anthropic.com/v1` (see `route::default_base_url`);
    /// `model` from `CloudService::llm_model` (suggested `claude-haiku-5-5`).
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
            max_tokens: DEFAULT_MAX_TOKENS,
            effort: None,
            call: CallSite {
                stage: Stage::Polish,
                provider: "claude".into(),
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

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Sends `output_config.effort` (e.g. `"low"` for latency). Only for models that support
    /// the effort parameter; `None` (default) omits it.
    pub fn with_effort(mut self, effort: Option<String>) -> Self {
        self.effort = effort;
        self
    }
}

impl std::fmt::Debug for ClaudeLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeLlm")
            .field("name", &self.call.provider)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl LlmProvider for ClaudeLlm {
    fn name(&self) -> &str {
        &self.call.provider
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        });
        if let Some(effort) = &self.effort {
            body["output_config"] = json!({ "effort": effort });
        }
        let req = self
            .http
            .post(join_url(&self.base_url, "messages"))
            .header("x-api-key", self.api_key.expose())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body);
        let v = self.call.send_json(req).await?;
        message_text(&v).ok_or_else(|| self.call.error(ProviderErrorKind::BadResponse))
    }
}

/// Joined `text` blocks of a Messages API response; `None` if `content` is missing.
fn message_text(v: &Value) -> Option<String> {
    let blocks = v.get("content")?.as_array()?;
    let text: String = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    Some(text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::test_support::*;
    use crate::V2cError;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn llm(server: &MockServer) -> ClaudeLlm {
        ClaudeLlm::new(
            reqwest::Client::new(),
            format!("{}/v1", server.uri()),
            ApiKey::new("sk-ant-TEST"),
            "claude-test-model",
            Duration::from_millis(300),
        )
    }

    fn ok_body() -> Value {
        json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "x"},
                {"type": "text", "text": "整理好的文字。"}
            ],
            "stop_reason": "end_turn"
        })
    }

    #[tokio::test]
    async fn success_sends_headers_system_and_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-ant-TEST"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;
        let out = llm(&server)
            .complete("SYSTEM-RULES", "USER-TEXT")
            .await
            .unwrap();
        assert_eq!(out, "整理好的文字。");
        let body: Value = serde_json::from_slice(&only_request(&server).await.body).unwrap();
        assert_eq!(body["model"], "claude-test-model");
        assert_eq!(body["system"], "SYSTEM-RULES");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "USER-TEXT");
        assert!(body["max_tokens"].as_u64().unwrap() > 0);
        assert!(body.get("output_config").is_none());
    }

    #[tokio::test]
    async fn effort_is_sent_when_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        llm(&server)
            .with_effort(Some("low".into()))
            .complete("S", "U")
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&only_request(&server).await.body).unwrap();
        assert_eq!(body["output_config"]["effort"], "low");
    }

    #[tokio::test]
    async fn status_errors_map_to_kinds() {
        for (status, kind) in [
            (401, ProviderErrorKind::Unauthorized),
            (429, ProviderErrorKind::RateLimited),
            (529, ProviderErrorKind::Server),
            (400, ProviderErrorKind::BadResponse),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "type": "error", "error": {"type": "x", "message": "m"}
                })))
                .mount(&server)
                .await;
            let err = llm(&server)
                .with_stage(Stage::Translate)
                .complete("SYS", "USER-TEXT")
                .await
                .unwrap_err();
            assert_provider_err(&err, Stage::Translate, kind);
            assert_no_secrets(&err, &["sk-ant-TEST", "USER-TEXT"]);
        }
    }

    #[tokio::test]
    async fn slow_response_times_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(slow_ok(ok_body()))
            .mount(&server)
            .await;
        let err = llm(&server).complete("S", "U").await.unwrap_err();
        assert!(matches!(
            err,
            V2cError::Timeout {
                stage: Stage::Polish,
                ms: 300
            }
        ));
    }

    #[tokio::test]
    async fn missing_content_is_bad_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"type": "message"})))
            .mount(&server)
            .await;
        let err = llm(&server).complete("S", "U").await.unwrap_err();
        assert_provider_err(&err, Stage::Polish, ProviderErrorKind::BadResponse);
    }

    #[test]
    fn debug_hides_key() {
        let l = ClaudeLlm::new(
            reqwest::Client::new(),
            "https://example.invalid/v1",
            ApiKey::new("sk-ant-SECRET"),
            "m",
            Duration::from_secs(1),
        );
        assert!(!format!("{l:?}").contains("SECRET"));
    }
}
