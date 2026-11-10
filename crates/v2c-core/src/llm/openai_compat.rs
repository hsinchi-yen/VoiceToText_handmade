//! OpenAI-compatible chat completions (`POST {base_url}/chat/completions`).
//!
//! Default target is the user's own LM Studio (`http://localhost:1234/v1`, no key, `model`
//! omitted so the loaded model answers). Also works for any self-hosted OpenAI-compatible server.
//!
//! Privacy: request and response bodies carry user text, so they never appear in errors, logs,
//! or `Debug` output. Errors carry only the stage, provider name, and a [`ProviderErrorKind`].
//!
//! The user message is expected to be [`crate::prompts::wrap_transcript`] output; the provider
//! unwraps it to size `max_tokens` and to clean the answer against the raw transcript.
//!
//! LM Studio context length: the model's loaded context (LM Studio "Context Length") must exceed
//! system prompt (~1k tokens with examples) + transcript + `max_tokens` (about 3x the transcript
//! length). 4096 is enough for short phrases; use 8192 for long dictation (a few minutes of
//! hands-free speech), otherwise LM Studio truncates or rejects the request.

use std::{fmt, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::LlmProvider;
use crate::{
    error::{ProviderErrorKind, Stage},
    postprocess::fillers::strip_fillers,
    prompts::{self, TRANSCRIPT_CLOSE, TRANSCRIPT_OPEN},
    Result, V2cError,
};

/// Sampling temperature: low, because polish and translate must stay faithful to the input.
const TEMPERATURE: f32 = 0.1;
/// A transport failure faster than this is treated as a stale pooled connection and retried
/// once (see `send_with_stale_retry`).
const STALE_RETRY_WINDOW: Duration = Duration::from_millis(500);
/// Output token budget: `chars * TOKENS_PER_CHAR + TOKEN_HEADROOM`, clamped to the bounds below.
/// Chinese is about 1-2 tokens per character; English translation of Chinese runs a bit longer.
const TOKENS_PER_CHAR: usize = 3;
const TOKEN_HEADROOM: usize = 64;
const MIN_MAX_TOKENS: usize = 64;
const MAX_MAX_TOKENS: usize = 4096;

/// [`LlmProvider`] for an OpenAI-compatible `/chat/completions` endpoint.
#[derive(Clone)]
pub struct OpenAiCompatLlm {
    client: reqwest::Client,
    name: String,
    /// Without a trailing slash, e.g. `http://localhost:1234/v1`.
    base_url: String,
    api_key: Option<String>,
    model: Option<String>,
    timeout: Duration,
    stage: Stage,
}

impl OpenAiCompatLlm {
    /// `base_url` is the API root including the version, e.g. `http://localhost:1234/v1`.
    /// `api_key` is sent as `Authorization: Bearer` only when present; `model = None` omits the
    /// field (LM Studio then uses the model that is currently loaded). `timeout` covers the whole
    /// request including reading the body. `client` should be shared across providers.
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: Option<String>,
        timeout: Duration,
    ) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Self {
            client,
            name: "lmstudio".into(),
            base_url,
            api_key: api_key.filter(|k| !k.is_empty()),
            model: model.filter(|m| !m.is_empty()),
            timeout,
            stage: Stage::Polish,
        }
    }

    /// Name reported by [`LlmProvider::name`] and in errors (default `lmstudio`), e.g. the
    /// `CloudService::id` for a self-hosted endpoint.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Stage reported in errors (default [`Stage::Polish`]); use [`Stage::Translate`] for the
    /// instance that serves Chinese-to-English translation.
    pub fn with_stage(mut self, stage: Stage) -> Self {
        self.stage = stage;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Cheap availability probe (`GET {base_url}/models`), e.g. "is LM Studio running?".
    pub async fn check(&self) -> Result<()> {
        let mut req = self
            .client
            .get(format!("{}/models", self.base_url))
            .timeout(self.timeout);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.map_err(|e| self.send_error(&e))?;
        match status_error(resp.status()) {
            Some(kind) => Err(self.err(kind)),
            None => Ok(()),
        }
    }

    fn err(&self, kind: ProviderErrorKind) -> V2cError {
        V2cError::Provider {
            stage: self.stage,
            provider: self.name.clone(),
            kind,
        }
    }

    fn timeout_err(&self) -> V2cError {
        V2cError::Timeout {
            stage: self.stage,
            ms: u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
        }
    }

    /// Sends once more on a fresh connection when the first attempt failed at once on the
    /// transport (not a timeout, not a refused connect). Servers such as llama.cpp close idle
    /// keep-alive connections after ~5 s while the client pool keeps them longer, so the first
    /// request after a pause can hit a closed socket and fail in ~0 ms.
    async fn send_with_stale_retry(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let retry = req.try_clone();
        let started = std::time::Instant::now();
        match req.send().await {
            Ok(resp) => Ok(resp),
            Err(e)
                if !e.is_timeout() && !e.is_connect() && started.elapsed() < STALE_RETRY_WINDOW =>
            {
                match retry {
                    Some(again) => again.send().await.map_err(|e| self.send_error(&e)),
                    None => Err(self.send_error(&e)),
                }
            }
            Err(e) => Err(self.send_error(&e)),
        }
    }

    /// Maps a transport error without looking at (or keeping) its message, which may echo the URL.
    fn send_error(&self, e: &reqwest::Error) -> V2cError {
        // Connect first: a connect attempt that hit the deadline still means "not reachable".
        if e.is_connect() {
            self.err(ProviderErrorKind::Unreachable)
        } else if e.is_timeout() {
            self.timeout_err()
        } else if e.is_decode() || e.is_body() {
            self.err(ProviderErrorKind::BadResponse)
        } else {
            // Connection refused, DNS failure, TLS failure, reset: the endpoint is not reachable.
            self.err(ProviderErrorKind::Unreachable)
        }
    }
}

impl fmt::Debug for OpenAiCompatLlm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatLlm")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("stage", &self.stage)
            .finish()
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    messages: [Message<'a>; 2],
    temperature: f32,
    max_tokens: usize,
    stream: bool,
    chat_template_kwargs: TemplateKwargs,
}

/// Turns off "thinking" for chat templates that support it (Qwen3 and similar, on llama.cpp
/// `llama-server` and vLLM). A thinking model spends hundreds of tokens before answering, so
/// short polish/translate calls hit the timeout or the `max_tokens` cap; with thinking off the
/// same request measured 0.63 s instead of 3-4 s (2026-11-10, Qwen3 27B on llama.cpp).
/// Servers and templates that do not know the field ignore it (checked with LM Studio).
#[derive(Serialize)]
struct TemplateKwargs {
    enable_thinking: bool,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Option<ResponseMessage>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
}

/// Output token cap relative to the input length, so a runaway model cannot stall the pipeline.
pub(crate) fn max_tokens_for(user: &str) -> usize {
    (user.chars().count() * TOKENS_PER_CHAR + TOKEN_HEADROOM).clamp(MIN_MAX_TOKENS, MAX_MAX_TOKENS)
}

/// `None` for success, otherwise the error kind the UI can explain.
fn status_error(status: reqwest::StatusCode) -> Option<ProviderErrorKind> {
    use ProviderErrorKind as K;
    match status.as_u16() {
        200..=299 => None,
        401 | 403 => Some(K::Unauthorized),
        429 => Some(K::RateLimited),
        404 | 405 | 501 => Some(K::Unsupported),
        500..=599 => Some(K::Server),
        // Other 4xx (e.g. unknown model, no model loaded): we cannot use what came back.
        _ => Some(K::BadResponse),
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatLlm {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let transcript = prompts::unwrap_transcript(user);
        let body = ChatRequest {
            model: self.model.as_deref(),
            messages: [
                Message {
                    role: "system",
                    content: system,
                },
                Message {
                    role: "user",
                    content: user,
                },
            ],
            temperature: TEMPERATURE,
            max_tokens: max_tokens_for(transcript),
            stream: false,
            chat_template_kwargs: TemplateKwargs {
                enable_thinking: false,
            },
        };
        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .timeout(self.timeout)
            .json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = self.send_with_stale_retry(req).await?;
        if let Some(kind) = status_error(resp.status()) {
            return Err(self.err(kind));
        }
        let bytes = resp.bytes().await.map_err(|e| self.send_error(&e))?;
        let parsed: ChatResponse =
            serde_json::from_slice(&bytes).map_err(|_| self.err(ProviderErrorKind::BadResponse))?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| self.err(ProviderErrorKind::BadResponse))?;
        // A truncated answer would silently drop part of what the user said.
        if choice.finish_reason.as_deref() == Some("length") {
            return Err(self.err(ProviderErrorKind::BadResponse));
        }
        let raw = choice
            .message
            .and_then(|m| m.content)
            .ok_or_else(|| self.err(ProviderErrorKind::BadResponse))?;
        let text = clean_output(&raw, transcript);
        if text.is_empty() {
            return Err(self.err(ProviderErrorKind::BadResponse));
        }
        Ok(text)
    }
}

/// Labels small models like to put before the answer. Matched at the very start, followed by a
/// full- or half-width colon. Only unambiguous wrappers: generic words such as 結果、輸出、Result
/// can start a real sentence and are never stripped.
const LEADING_LABELS: &[&str] = &[
    "潤飾後的文字",
    "潤飾後文字",
    "潤飾結果",
    "潤飾後",
    "润饰后",
    "整理後的文字",
    "整理後",
    "譯文",
    "译文",
    "英文翻譯",
    "英文譯文",
    "Polished text",
    "Polished",
    "English translation",
    "Translation",
];

/// Quote pairs a model may wrap the whole answer in.
const QUOTE_PAIRS: &[(char, char)] = &[
    ('"', '"'),
    ('\'', '\''),
    ('“', '”'),
    ('「', '」'),
    ('『', '』'),
    ('`', '`'),
];

/// Removes model artefacts around the answer: reasoning blocks (`<think>…</think>`), Markdown
/// code fences, a leading "潤飾後：" style label, and quotes around the whole answer (only when
/// the user's text was not itself quoted), plus echoed `<逐字稿>` tags. `user` is the raw
/// transcript (not the wrapped message). Returns the trimmed text; empty means "no answer".
pub(crate) fn clean_output(raw: &str, user: &str) -> String {
    let user = user.trim();
    let mut s = strip_think(raw).trim().to_string();
    s = strip_code_fence(&s).trim().to_string();
    s = s.replace(TRANSCRIPT_OPEN, "").replace(TRANSCRIPT_CLOSE, "");
    s = strip_leading_label(s.trim(), user).trim().to_string();
    for &(open, close) in QUOTE_PAIRS {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() >= 2
            && chars[0] == open
            && chars[chars.len() - 1] == close
            && !user.starts_with(open)
            // Only a single pair around everything, e.g. not `"a" and "b"`.
            && !chars[1..chars.len() - 1].contains(&close)
            && (open == close || !chars[1..chars.len() - 1].contains(&open))
        {
            s = chars[1..chars.len() - 1].iter().collect::<String>();
            s = s.trim().to_string();
            break;
        }
    }
    s
}

/// Drops every `<think>…</think>` block. If a closing tag appears without an opening one (some
/// chat templates put `<think>` in the prompt), everything before the last `</think>` goes. An
/// unclosed `<think>` means the answer never started, so the result is empty.
fn strip_think(s: &str) -> String {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut s = s.to_string();
    if let Some(close) = s.rfind(CLOSE) {
        if !s[..close].contains(OPEN) {
            s = s[close + CLOSE.len()..].to_string();
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// ```` ```text\n…\n``` ```` → `…` when the fence wraps the whole answer.
fn strip_code_fence(s: &str) -> &str {
    let Some(inner) = s.strip_prefix("```") else {
        return s;
    };
    let Some(inner) = inner.strip_suffix("```") else {
        return s;
    };
    // Drop an info string such as `text` on the opening line.
    match inner.find('\n') {
        Some(nl) if !inner[..nl].trim().contains(' ') => &inner[nl + 1..],
        _ => inner,
    }
}

/// True when `text` starts with `label` (ASCII case-insensitive).
fn starts_with_label(text: &str, label: &str) -> bool {
    text.get(..label.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(label))
}

/// Strips a leading label unless the speaker actually began with that word (checked on the raw
/// transcript and on it with leading fillers removed).
fn strip_leading_label<'a>(s: &'a str, user: &str) -> &'a str {
    // Markdown bold around the label: `**Translation:** …`.
    let t = s.trim_start_matches('*');
    let spoken = strip_fillers(user);
    for label in LEADING_LABELS {
        if !starts_with_label(t, label) {
            continue;
        }
        if starts_with_label(user, label) || starts_with_label(&spoken, label) {
            return s;
        }
        let rest = t[label.len()..].trim_start_matches([' ', '*']);
        for colon in ['：', ':'] {
            if let Some(after) = rest.strip_prefix(colon) {
                return after.trim_start_matches(['*', ' ']);
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_keeps_plain_answer() {
        assert_eq!(
            clean_output("  我們後天早上十點見面。\n", "x"),
            "我們後天早上十點見面。"
        );
    }

    #[test]
    fn clean_strips_think_blocks() {
        assert_eq!(
            clean_output(
                "<think>\nThe user wants...\n</think>\n\n我們後天見面。",
                "x"
            ),
            "我們後天見面。"
        );
        assert_eq!(
            clean_output("<think>a</think>第一句。<think>b</think>第二句。", "x"),
            "第一句。第二句。"
        );
        // Opening tag was part of the prompt template.
        assert_eq!(clean_output("reasoning...</think>\n好的。", "x"), "好的。");
        // Truncated reasoning: no answer at all.
        assert_eq!(clean_output("<think>still thinking", "x"), "");
    }

    #[test]
    fn clean_strips_labels() {
        assert_eq!(
            clean_output("潤飾後：我們後天見面。", "x"),
            "我們後天見面。"
        );
        assert_eq!(
            clean_output("潤飾結果: 我們後天見面。", "x"),
            "我們後天見面。"
        );
        assert_eq!(
            clean_output("**Translation:** See you the day after tomorrow.", "x"),
            "See you the day after tomorrow."
        );
        assert_eq!(clean_output("translation: Hi.", "x"), "Hi.");
        assert_eq!(clean_output("譯文：Hi.", "你好"), "Hi.");
        // A label-like word that is real content (no colon) stays.
        assert_eq!(clean_output("結果很好。", "x"), "結果很好。");
        assert_eq!(clean_output("Output is fine.", "x"), "Output is fine.");
        // Generic words are never treated as labels.
        assert_eq!(clean_output("結果：我們贏了。", "x"), "結果：我們贏了。");
        assert_eq!(clean_output("Result: we won.", "x"), "Result: we won.");
        assert_eq!(clean_output("English: 很難。", "x"), "English: 很難。");
    }

    #[test]
    fn clean_keeps_label_the_speaker_said() {
        assert_eq!(
            clean_output("結果：我們贏了。", "結果我們贏了"),
            "結果：我們贏了。"
        );
        assert_eq!(
            clean_output("譯文：要再校一次。", "譯文要再校一次"),
            "譯文：要再校一次。"
        );
        // Leading fillers before the word do not hide it.
        assert_eq!(
            clean_output("Translation: 我們下週交。", "嗯，translation 我們下週交"),
            "Translation: 我們下週交。"
        );
    }

    #[test]
    fn clean_strips_echoed_transcript_tags() {
        assert_eq!(
            clean_output("<逐字稿>\n我們後天見面。\n</逐字稿>", "我們後天見面"),
            "我們後天見面。"
        );
    }

    #[test]
    fn clean_strips_surrounding_quotes_unless_user_quoted() {
        assert_eq!(
            clean_output("「我們後天見面。」", "我們後天見面"),
            "我們後天見面。"
        );
        assert_eq!(clean_output("\"Hello there.\"", "你好"), "Hello there.");
        assert_eq!(clean_output("“Hello.”", "你好"), "Hello.");
        assert_eq!(clean_output("潤飾後：「好。」", "好"), "好。");
        // The user really said a quotation.
        assert_eq!(clean_output("「我愛你」", "「我愛你」"), "「我愛你」");
        // Quotes that are not one pair around everything stay.
        assert_eq!(
            clean_output("「早安」和「晚安」", "早安和晚安"),
            "「早安」和「晚安」"
        );
        assert_eq!(clean_output("\"a\" and \"b\"", "x"), "\"a\" and \"b\"");
    }

    #[test]
    fn clean_strips_code_fence() {
        assert_eq!(clean_output("```\n我們見面。\n```", "x"), "我們見面。");
        assert_eq!(clean_output("```text\n我們見面。\n```", "x"), "我們見面。");
    }

    #[test]
    fn max_tokens_scales_and_is_bounded() {
        assert_eq!(max_tokens_for(""), MIN_MAX_TOKENS);
        assert_eq!(max_tokens_for("我們後天見面"), 6 * 3 + 64);
        assert_eq!(max_tokens_for(&"字".repeat(100_000)), MAX_MAX_TOKENS);
    }

    #[test]
    fn status_mapping() {
        use reqwest::StatusCode as S;
        use ProviderErrorKind as K;
        assert_eq!(status_error(S::OK), None);
        assert_eq!(status_error(S::UNAUTHORIZED), Some(K::Unauthorized));
        assert_eq!(status_error(S::FORBIDDEN), Some(K::Unauthorized));
        assert_eq!(status_error(S::TOO_MANY_REQUESTS), Some(K::RateLimited));
        assert_eq!(status_error(S::INTERNAL_SERVER_ERROR), Some(K::Server));
        assert_eq!(status_error(S::SERVICE_UNAVAILABLE), Some(K::Server));
        assert_eq!(status_error(S::NOT_FOUND), Some(K::Unsupported));
        assert_eq!(status_error(S::BAD_REQUEST), Some(K::BadResponse));
    }

    #[test]
    fn debug_redacts_api_key() {
        let llm = OpenAiCompatLlm::new(
            reqwest::Client::new(),
            "http://localhost:1234/v1/",
            Some("sk-secret".into()),
            None,
            Duration::from_secs(1),
        );
        let dbg = format!("{llm:?}");
        assert!(!dbg.contains("sk-secret"));
        assert!(dbg.contains("<redacted>"));
        assert_eq!(llm.base_url(), "http://localhost:1234/v1");
    }
}

/// HTTP-level tests against a local mock server.
#[cfg(test)]
mod wire_tests {
    use serde_json::{json, Value};
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, Request, ResponseTemplate,
    };

    use super::*;

    const SYSTEM: &str = "system prompt";
    const USER: &str = "嗯，我們明天見";

    fn llm(server: &MockServer, key: Option<&str>, model: Option<&str>) -> OpenAiCompatLlm {
        OpenAiCompatLlm::new(
            reqwest::Client::new(),
            format!("{}/v1", server.uri()),
            key.map(Into::into),
            model.map(Into::into),
            Duration::from_millis(500),
        )
    }

    /// A server that drops the first connection without answering (like a keep-alive socket
    /// the server already closed), then answers the second one normally.
    #[tokio::test]
    async fn connection_dropped_at_once_is_retried_on_a_fresh_one() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (first, _) = listener.accept().await.unwrap();
            drop(first);
            let (mut second, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64 * 1024];
            let mut got = Vec::new();
            // Read headers + body (Content-Length) before answering.
            loop {
                let n = second.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let body =
                r#"{"choices":[{"message":{"content":"我們明天見。"},"finish_reason":"stop"}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            second.write_all(resp.as_bytes()).await.unwrap();
        });
        let llm = OpenAiCompatLlm::new(
            reqwest::Client::new(),
            format!("http://{addr}/v1"),
            None,
            None,
            Duration::from_secs(2),
        );
        let out = llm
            .complete(SYSTEM, &prompts::wrap_transcript(USER))
            .await
            .unwrap();
        assert_eq!(out, "我們明天見。");
    }

    fn reply(content: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        }))
    }

    fn kind(e: V2cError) -> ProviderErrorKind {
        match e {
            V2cError::Provider {
                kind,
                stage,
                provider,
            } => {
                assert_eq!(stage, Stage::Polish);
                assert_eq!(provider, "lmstudio");
                kind
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    async fn status(code: u16) -> V2cError {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(code).set_body_string("{\"error\":\"x\"}"))
            .mount(&server)
            .await;
        llm(&server, None, None)
            .complete(SYSTEM, USER)
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn request_format_without_key_or_model() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("content-type", "application/json"))
            .respond_with(reply("我們明天見。"))
            .expect(1)
            .mount(&server)
            .await;

        let out = llm(&server, None, None)
            .complete(SYSTEM, USER)
            .await
            .unwrap();
        assert_eq!(out, "我們明天見。");

        let reqs: Vec<Request> = server.received_requests().await.unwrap();
        let req = &reqs[0];
        assert!(req.headers.get("authorization").is_none());
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert!(body.get("model").is_none(), "model omitted for LM Studio");
        assert_eq!(body["stream"], json!(false));
        // Thinking off, so reasoning models answer within the polish budget.
        assert_eq!(
            body["chat_template_kwargs"],
            json!({ "enable_thinking": false })
        );
        let t = body["temperature"].as_f64().unwrap();
        assert!((0.0..=0.2).contains(&t));
        assert_eq!(body["max_tokens"], json!(max_tokens_for(USER)));
        assert_eq!(
            body["messages"],
            json!([
                { "role": "system", "content": SYSTEM },
                { "role": "user", "content": USER }
            ])
        );
    }

    #[tokio::test]
    async fn request_sends_bearer_key_and_model_when_configured() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(reply("ok"))
            .expect(1)
            .mount(&server)
            .await;
        let out = llm(&server, Some("sk-test"), Some("qwen2.5-3b-instruct"))
            .complete(SYSTEM, USER)
            .await
            .unwrap();
        assert_eq!(out, "ok");
        let reqs = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["model"], json!("qwen2.5-3b-instruct"));
    }

    #[tokio::test]
    async fn wrapped_transcript_is_sent_as_is_and_sized_by_raw_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(reply("<逐字稿>\n「我們明天見。」\n</逐字稿>"))
            .mount(&server)
            .await;
        let wrapped = prompts::wrap_transcript(USER);
        let out = llm(&server, None, None)
            .complete(SYSTEM, &wrapped)
            .await
            .unwrap();
        assert_eq!(out, "我們明天見。");
        let reqs = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["messages"][1]["content"], json!(wrapped));
        assert_eq!(body["max_tokens"], json!(max_tokens_for(USER)));
    }

    #[tokio::test]
    async fn success_output_is_cleaned() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(reply("<think>hmm</think>\n潤飾後：「我們後天見面。」"))
            .mount(&server)
            .await;
        let out = llm(&server, None, None)
            .complete(SYSTEM, USER)
            .await
            .unwrap();
        assert_eq!(out, "我們後天見面。");
    }

    #[tokio::test]
    async fn simplified_output_is_returned_unchanged() {
        // zh-TW conversion is the pipeline's job (step 5), not the provider's.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(reply("我们后天见面。"))
            .mount(&server)
            .await;
        let out = llm(&server, None, None)
            .complete(SYSTEM, USER)
            .await
            .unwrap();
        assert_eq!(out, "我们后天见面。");
    }

    #[tokio::test]
    async fn http_errors_map_to_kinds() {
        assert_eq!(kind(status(401).await), ProviderErrorKind::Unauthorized);
        assert_eq!(kind(status(429).await), ProviderErrorKind::RateLimited);
        assert_eq!(kind(status(500).await), ProviderErrorKind::Server);
        assert_eq!(kind(status(400).await), ProviderErrorKind::BadResponse);
        // llama.cpp answers 501 for operations it does not implement.
        assert_eq!(kind(status(501).await), ProviderErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn llama_cpp_reasoning_content_field_is_ignored() {
        // llama.cpp (default `--reasoning-format`) returns the reasoning in its own field.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "index": 0,
                    "finish_reason": "stop",
                    "message": {
                        "role": "assistant",
                        "reasoning_content": "The user said 嗯; drop it.",
                        "content": "我們明天見。"
                    }
                }]
            })))
            .mount(&server)
            .await;
        let out = llm(&server, None, None)
            .complete(SYSTEM, USER)
            .await
            .unwrap();
        assert_eq!(out, "我們明天見。");
    }

    #[tokio::test]
    async fn bad_json_and_empty_answers_are_bad_response() {
        for template in [
            ResponseTemplate::new(200).set_body_string("not json"),
            ResponseTemplate::new(200).set_body_json(json!({ "choices": [] })),
            reply("   "),
            reply("<think>never finished"),
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{ "message": { "role": "assistant", "content": "我們" },
                              "finish_reason": "length" }]
            })),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(template)
                .mount(&server)
                .await;
            let e = llm(&server, None, None)
                .complete(SYSTEM, USER)
                .await
                .unwrap_err();
            assert_eq!(kind(e), ProviderErrorKind::BadResponse);
        }
    }

    #[tokio::test]
    async fn slow_server_times_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(reply("late").set_delay(Duration::from_millis(1500)))
            .mount(&server)
            .await;
        let llm = OpenAiCompatLlm::new(
            reqwest::Client::new(),
            format!("{}/v1", server.uri()),
            None,
            None,
            Duration::from_millis(100),
        );
        let started = std::time::Instant::now();
        let e = llm.complete(SYSTEM, USER).await.unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(1000));
        assert!(
            matches!(
                e,
                V2cError::Timeout {
                    stage: Stage::Polish,
                    ms: 100
                }
            ),
            "{e:?}"
        );
    }

    #[tokio::test]
    async fn nothing_listening_is_unreachable() {
        // Bind and drop a listener to get a local port that refuses connections. Windows
        // retries a refused localhost connect for about 2 s, hence the long timeout.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let llm = OpenAiCompatLlm::new(
            reqwest::Client::new(),
            format!("http://127.0.0.1:{port}/v1"),
            None,
            None,
            Duration::from_secs(10),
        );
        let e = llm.complete(SYSTEM, USER).await.unwrap_err();
        assert_eq!(kind(e), ProviderErrorKind::Unreachable);
        let e = llm.check().await.unwrap_err();
        assert_eq!(kind(e), ProviderErrorKind::Unreachable);
    }

    #[tokio::test]
    async fn check_hits_models_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
            .expect(1)
            .mount(&server)
            .await;
        llm(&server, None, None).check().await.unwrap();
    }

    #[tokio::test]
    async fn errors_do_not_contain_user_text() {
        let e = status(500).await;
        let msg = e.to_string();
        assert!(!msg.contains("明天"), "{msg}");
        let stage_named = llm(&MockServer::start().await, None, None)
            .with_stage(Stage::Translate)
            .with_name("my-server");
        assert_eq!(stage_named.name(), "my-server");
    }
}
