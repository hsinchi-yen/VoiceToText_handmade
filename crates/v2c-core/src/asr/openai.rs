//! OpenAI speech-to-text: `POST {base_url}/audio/transcriptions` (multipart).
//!
//! Request: `file` (WAV), `model`, optional `prompt` (vocabulary / style hint), and
//! `response_format=json`; response `{"text": "..."}`. Files up to 25 MB are accepted, far
//! above a 5-minute 16 kHz PCM16 clip (~9.6 MB). The language is not forced, so mixed
//! Chinese/English speech is kept as spoken. Also used by [`super::compat::CompatAsr`].

use std::time::Duration;

use async_trait::async_trait;
use reqwest::multipart::{Form, Part};

use super::http::{join_url, CallSite};
use super::wav::encode_wav_pcm16;
use super::{script_lang, AsrContext, AsrProvider, Transcript};
use crate::audio::AudioClip;
use crate::error::{ProviderErrorKind, Stage};
use crate::secrets::ApiKey;
use crate::Result;

pub struct OpenAiAsr {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<ApiKey>,
    model: String,
    call: CallSite,
}

impl OpenAiAsr {
    /// `model` comes from `CloudService::asr_model` (see `route::suggested_models`).
    pub fn new(
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_key: Option<ApiKey>,
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
                provider: "openai".into(),
                timeout,
            },
        }
    }

    /// Overrides the name used in logs and errors (e.g. the `CloudService::id`).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.call.provider = name.into();
        self
    }
}

impl std::fmt::Debug for OpenAiAsr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiAsr")
            .field("name", &self.call.provider)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("has_key", &self.api_key.is_some())
            .finish()
    }
}

#[async_trait]
impl AsrProvider for OpenAiAsr {
    fn name(&self) -> &str {
        &self.call.provider
    }

    async fn transcribe(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<Transcript> {
        let file = Part::bytes(encode_wav_pcm16(clip))
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|_| self.call.error(ProviderErrorKind::BadResponse))?;
        let mut form = Form::new()
            .part("file", file)
            .text("model", self.model.clone())
            .text("response_format", "json");
        if !ctx.prompt.is_empty() {
            form = form.text("prompt", ctx.prompt.clone());
        }
        let mut req = self
            .http
            .post(join_url(&self.base_url, "audio/transcriptions"))
            .multipart(form);
        if let Some(key) = self.api_key.as_ref().filter(|k| !k.is_empty()) {
            req = req.bearer_auth(key.expose());
        }
        let v = self.call.send_json(req).await?;
        let text = v
            .get("text")
            .and_then(|t| t.as_str())
            .ok_or_else(|| self.call.error(ProviderErrorKind::BadResponse))?
            .trim()
            .to_owned();
        let lang = script_lang(&text);
        Ok(Transcript { text, lang })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::test_support::*;
    use crate::{Lang, V2cError};
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn asr(server: &MockServer, key: Option<&str>) -> OpenAiAsr {
        OpenAiAsr::new(
            reqwest::Client::new(),
            format!("{}/v1", server.uri()),
            key.map(ApiKey::new),
            "test-transcribe-model",
            Duration::from_millis(300),
        )
    }

    fn ctx() -> AsrContext {
        AsrContext {
            vocabulary: vec!["v2c".into()],
            prompt: "PROMPT-HINT v2c".into(),
        }
    }

    #[tokio::test]
    async fn success_sends_multipart_with_auth_model_prompt_and_audio() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header("authorization", "Bearer sk-TEST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": " 你好 v2c "})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let t = asr(&server, Some("sk-TEST"))
            .transcribe(&tone(), &ctx())
            .await
            .unwrap();
        assert_eq!(t.text, "你好 v2c");
        assert_eq!(t.lang, Lang::Mixed);

        let req = only_request(&server).await;
        let ct = req.headers.get("content-type").unwrap().to_str().unwrap();
        assert!(ct.starts_with("multipart/form-data"));
        let body = String::from_utf8_lossy(&req.body);
        assert!(body.contains("name=\"model\"\r\n\r\ntest-transcribe-model"));
        assert!(body.contains("name=\"prompt\"\r\n\r\nPROMPT-HINT v2c"));
        assert!(body.contains("name=\"response_format\"\r\n\r\njson"));
        assert!(body.contains("filename=\"audio.wav\""));
        assert!(body.contains("Content-Type: audio/wav"));
        assert!(contains_bytes(&req.body, b"RIFF"), "WAV payload present");
    }

    #[tokio::test]
    async fn empty_prompt_is_omitted_and_missing_key_sends_no_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": ""})))
            .mount(&server)
            .await;
        let t = asr(&server, None)
            .transcribe(&tone(), &AsrContext::default())
            .await
            .unwrap();
        assert_eq!(t.text, "");
        let req = only_request(&server).await;
        assert!(req.headers.get("authorization").is_none());
        assert!(!String::from_utf8_lossy(&req.body).contains("name=\"prompt\""));
    }

    #[tokio::test]
    async fn status_errors_map_to_kinds() {
        for (status, kind) in [
            (401, ProviderErrorKind::Unauthorized),
            (429, ProviderErrorKind::RateLimited),
            (404, ProviderErrorKind::Unsupported),
            (503, ProviderErrorKind::Server),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status).set_body_string("{\"error\":{}}"))
                .mount(&server)
                .await;
            let err = asr(&server, Some("sk-TEST"))
                .transcribe(&tone(), &ctx())
                .await
                .unwrap_err();
            assert_provider_err(&err, Stage::Asr, kind);
            assert_no_secrets(&err, &["sk-TEST", "PROMPT-HINT"]);
        }
    }

    #[tokio::test]
    async fn slow_response_times_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(slow_ok(serde_json::json!({"text": "late"})))
            .mount(&server)
            .await;
        let err = asr(&server, Some("sk-TEST"))
            .transcribe(&tone(), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            V2cError::Timeout {
                stage: Stage::Asr,
                ms: 300
            }
        ));
    }

    #[tokio::test]
    async fn missing_text_is_bad_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        let err = asr(&server, None)
            .transcribe(&tone(), &ctx())
            .await
            .unwrap_err();
        assert_provider_err(&err, Stage::Asr, ProviderErrorKind::BadResponse);
    }

    #[test]
    fn debug_hides_key() {
        let a = OpenAiAsr::new(
            reqwest::Client::new(),
            "https://example.invalid/v1",
            Some(ApiKey::new("sk-SECRET")),
            "m",
            Duration::from_secs(1),
        );
        let s = format!("{a:?}");
        assert!(!s.contains("SECRET"));
        assert!(s.contains("has_key: true"));
    }
}
