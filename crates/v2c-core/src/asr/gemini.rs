//! Gemini speech recognition via `POST {base_url}/models/{model}:generateContent`.
//!
//! Auth header `x-goog-api-key`. The clip is sent inline as base64 `audio/wav`
//! (`inlineData`), which is fine for requests under Gemini's 20 MB inline limit (a 5-minute
//! clip is ~12.8 MB after base64). The shared request/response helpers here are also used by
//! `llm::gemini`.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};

use super::http::{join_url, CallSite, CloudHttp};
use super::wav::encode_wav_pcm16;
use super::{script_lang, AsrContext, AsrProvider, Transcript};
use crate::audio::AudioClip;
use crate::error::{ProviderErrorKind, Stage};
use crate::secrets::ApiKey;
use crate::Result;

/// Transcription instruction. TODO: move to `prompts.rs` (owned by another task).
pub const TRANSCRIBE_INSTRUCTION: &str = "You are a speech-to-text engine. Transcribe the audio verbatim in the language(s) actually spoken. \
Write Chinese in Traditional Chinese as used in Taiwan (zh-TW), never Simplified. \
Keep English words and names in English exactly as spoken; do not translate. \
Add natural punctuation. Output only the transcript, with no commentary, labels, or quotes. \
If there is no speech, output nothing.";

/// One Gemini `generateContent` call, shared by ASR and LLM.
pub(crate) struct GeminiCall<'a> {
    pub http: &'a CloudHttp,
    pub base_url: &'a str,
    pub api_key: &'a ApiKey,
    pub model: &'a str,
    pub call: &'a CallSite,
}

impl GeminiCall<'_> {
    /// Sends `systemInstruction` + one user turn with `parts`; returns the joined text parts
    /// of the first candidate.
    pub async fn generate(&self, system: &str, parts: Vec<Value>) -> Result<String> {
        let body = json!({
            "systemInstruction": { "parts": [{ "text": system }] },
            "contents": [{ "role": "user", "parts": parts }],
            "generationConfig": { "temperature": 0 },
        });
        let model = path_model(self.model)?;
        let url = join_url(self.base_url, &format!("models/{model}:generateContent"));
        let req = self
            .http
            .client()
            .post(url)
            .header("x-goog-api-key", self.api_key.expose())
            .json(&body);
        let v = self.call.send_json(req).await?;
        candidate_text(&v).ok_or_else(|| self.call.error(ProviderErrorKind::BadResponse))
    }
}

/// Model id safe to put in a URL path segment. Accepts `models/<id>` (the form the models
/// list returns) and rejects ids that could alter the URL.
pub(crate) fn path_model(model: &str) -> Result<&str> {
    let id = model.trim();
    let id = id.strip_prefix("models/").unwrap_or(id);
    let bad = id.is_empty()
        || id
            .chars()
            .any(|c| matches!(c, '/' | '?' | '#' | '%' | '\\') || c.is_whitespace());
    if bad {
        return Err(crate::V2cError::Config("invalid model name".into()));
    }
    Ok(id)
}

/// `candidates[0].content.parts[].text` joined. `None` when there is no usable answer:
/// no candidate (blocked prompt), or a `finishReason` other than `STOP` (e.g. `MAX_TOKENS`
/// = truncated, `SAFETY`), so the caller falls back to local. An empty string is a valid
/// (silent) answer.
pub(crate) fn candidate_text(v: &Value) -> Option<String> {
    let cand = v.get("candidates")?.get(0)?;
    match cand.get("finishReason").and_then(Value::as_str) {
        None | Some("STOP") => {}
        Some(_) => return None,
    }
    let parts = match cand.get("content").and_then(|c| c.get("parts")) {
        Some(p) => p.as_array()?,
        // Finished normally with no parts = empty output.
        None if cand.get("finishReason").and_then(Value::as_str) == Some("STOP") => {
            return Some(String::new())
        }
        None => return None,
    };
    let text: String = parts
        .iter()
        // Skip thought summaries if the model returns them.
        .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    Some(text.trim().to_owned())
}

pub struct GeminiAsr {
    http: CloudHttp,
    base_url: String,
    api_key: ApiKey,
    model: String,
    call: CallSite,
}

impl GeminiAsr {
    /// `base_url` e.g. `https://generativelanguage.googleapis.com/v1beta`
    /// (see `route::default_base_url`); `model` from `CloudService::asr_model`.
    pub fn new(
        http: CloudHttp,
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
                stage: Stage::Asr,
                provider: "gemini".into(),
                timeout,
            },
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.call.provider = name.into();
        self
    }
}

impl std::fmt::Debug for GeminiAsr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiAsr")
            .field("name", &self.call.provider)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl AsrProvider for GeminiAsr {
    fn name(&self) -> &str {
        &self.call.provider
    }

    async fn transcribe(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<Transcript> {
        let audio = base64::engine::general_purpose::STANDARD.encode(encode_wav_pcm16(clip));
        let mut parts = Vec::with_capacity(2);
        if !ctx.prompt.is_empty() {
            parts.push(json!({
                "text": format!("Context and spellings to prefer (do not output this): {}", ctx.prompt)
            }));
        }
        parts.push(json!({ "inlineData": { "mimeType": "audio/wav", "data": audio } }));
        let text = GeminiCall {
            http: &self.http,
            base_url: &self.base_url,
            api_key: &self.api_key,
            model: &self.model,
            call: &self.call,
        }
        .generate(TRANSCRIBE_INSTRUCTION, parts)
        .await?;
        let lang = script_lang(&text);
        Ok(Transcript { text, lang })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::http::cloud_http_client;
    use crate::asr::test_support::*;
    use crate::{Lang, V2cError};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn asr(server: &MockServer) -> GeminiAsr {
        GeminiAsr::new(
            cloud_http_client().unwrap(),
            format!("{}/v1beta", server.uri()),
            ApiKey::new("AIza-TEST"),
            "gemini-test-flash",
            Duration::from_millis(300),
        )
    }

    fn ctx() -> AsrContext {
        AsrContext {
            vocabulary: vec![],
            prompt: "PROMPT-HINT".into(),
        }
    }

    fn ok_body(text: &str) -> Value {
        json!({"candidates": [{"content": {"role": "model", "parts": [{"text": text}]}, "finishReason": "STOP"}]})
    }

    #[tokio::test]
    async fn success_sends_inline_wav_and_instruction() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/gemini-test-flash:generateContent"))
            .and(header("x-goog-api-key", "AIza-TEST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body("今天天氣很好\n")))
            .expect(1)
            .mount(&server)
            .await;
        let t = asr(&server).transcribe(&tone(), &ctx()).await.unwrap();
        assert_eq!(t.text, "今天天氣很好");
        assert_eq!(t.lang, Lang::Zh);

        let req = only_request(&server).await;
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let sys = body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(sys.contains("Taiwan"));
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert!(parts[0]["text"].as_str().unwrap().contains("PROMPT-HINT"));
        let inline = &parts[1]["inlineData"];
        assert_eq!(inline["mimeType"], "audio/wav");
        let wav = base64::engine::general_purpose::STANDARD
            .decode(inline["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(wav.len(), 44 + tone().samples.len() * 2);
        assert!(req.url.query().is_none(), "key must not be in the URL");
    }

    #[tokio::test]
    async fn status_errors_map_to_kinds() {
        for (status, body, kind) in [
            (401, "{}", ProviderErrorKind::Unauthorized),
            (
                400,
                r#"{"error":{"status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#,
                ProviderErrorKind::Unauthorized,
            ),
            (429, "{}", ProviderErrorKind::RateLimited),
            (500, "{}", ProviderErrorKind::Server),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_string(body))
                .mount(&server)
                .await;
            let err = asr(&server).transcribe(&tone(), &ctx()).await.unwrap_err();
            assert_provider_err(&err, Stage::Asr, kind);
            assert_no_secrets(&err, &["AIza-TEST", "PROMPT-HINT"]);
        }
    }

    #[tokio::test]
    async fn slow_response_times_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(slow_ok(ok_body("late")))
            .mount(&server)
            .await;
        let err = asr(&server).transcribe(&tone(), &ctx()).await.unwrap_err();
        assert!(matches!(
            err,
            V2cError::Timeout {
                stage: Stage::Asr,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn blocked_response_is_bad_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"promptFeedback": {"blockReason": "SAFETY"}})),
            )
            .mount(&server)
            .await;
        let err = asr(&server).transcribe(&tone(), &ctx()).await.unwrap_err();
        assert_provider_err(&err, Stage::Asr, ProviderErrorKind::BadResponse);
    }

    #[test]
    fn candidate_text_joins_parts_and_skips_thoughts() {
        let v = json!({"candidates": [{"content": {"parts": [
            {"text": "plan", "thought": true}, {"text": "Hello "}, {"text": "world"}
        ]}}]});
        assert_eq!(candidate_text(&v).unwrap(), "Hello world");
        let empty = json!({"candidates": [{"finishReason": "STOP"}]});
        assert_eq!(candidate_text(&empty).unwrap(), "");
        assert!(candidate_text(&json!({"candidates": []})).is_none());
        for reason in ["MAX_TOKENS", "SAFETY", "RECITATION"] {
            let v = json!({"candidates": [{"content": {"parts": [{"text": "partial"}]},
                "finishReason": reason}]});
            assert!(candidate_text(&v).is_none(), "{reason}");
        }
    }

    #[tokio::test]
    async fn truncated_or_filtered_answer_is_bad_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "half"}]}, "finishReason": "MAX_TOKENS"}]
            })))
            .mount(&server)
            .await;
        let err = asr(&server).transcribe(&tone(), &ctx()).await.unwrap_err();
        assert_provider_err(&err, Stage::Asr, ProviderErrorKind::BadResponse);
    }

    #[test]
    fn path_model_validation() {
        assert_eq!(path_model("gemini-x").unwrap(), "gemini-x");
        assert_eq!(path_model("models/gemini-x").unwrap(), "gemini-x");
        for bad in ["", "a/b", "a?b", "a#b", "a b", "a%2Fb", "../x"] {
            assert!(matches!(path_model(bad), Err(V2cError::Config(_))), "{bad}");
        }
    }

    #[tokio::test]
    async fn invalid_model_sends_nothing() {
        let server = MockServer::start().await;
        let a = GeminiAsr::new(
            cloud_http_client().unwrap(),
            server.uri(),
            ApiKey::new("AIza-TEST"),
            "x?key=1",
            Duration::from_millis(300),
        );
        let err = a.transcribe(&tone(), &ctx()).await.unwrap_err();
        assert!(matches!(err, V2cError::Config(_)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn redirect_to_other_host_does_not_forward_key() {
        let attacker = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body("x")))
            .mount(&attacker)
            .await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/steal", attacker.uri()).as_str()),
            )
            .mount(&server)
            .await;
        let err = asr(&server).transcribe(&tone(), &ctx()).await.unwrap_err();
        assert_provider_err(&err, Stage::Asr, ProviderErrorKind::BadResponse);
        assert!(attacker.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn debug_hides_key() {
        let a = GeminiAsr::new(
            cloud_http_client().unwrap(),
            "https://example.invalid",
            ApiKey::new("AIza-SECRET"),
            "m",
            Duration::from_secs(1),
        );
        assert!(!format!("{a:?}").contains("SECRET"));
    }
}
