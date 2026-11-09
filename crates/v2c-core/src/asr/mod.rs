//! Speech recognition interface. Implementations: local (in `v2c-asr-local`),
//! OpenAI, Gemini, and OpenAI-compatible endpoints (T1.7).

pub mod compat;
pub mod gemini;
pub(crate) mod http;
pub mod openai;
pub mod wav;

use async_trait::async_trait;

use crate::{audio::AudioClip, redact::Redacted, Lang, Result};

/// `Lang` of a cloud transcript from the scripts it contains (CJK and/or Latin letters).
/// Only character classes are inspected. Text with neither defaults to `Zh`.
pub(crate) fn script_lang(text: &str) -> Lang {
    let has_cjk = text.chars().any(|c| {
        matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF)
    });
    let has_latin = text.chars().any(|c| c.is_ascii_alphabetic());
    match (has_cjk, has_latin) {
        (true, true) => Lang::Mixed,
        (false, true) => Lang::En,
        _ => Lang::Zh,
    }
}

/// Extra context sent with each recognition request.
#[derive(Clone, Default)]
pub struct AsrContext {
    /// Personal vocabulary (names, product terms) to bias recognition.
    pub vocabulary: Vec<String>,
    /// Initial prompt, see `prompts::asr_initial_prompt`.
    pub prompt: String,
}

impl std::fmt::Debug for AsrContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsrContext")
            .field("vocabulary_len", &self.vocabulary.len())
            .field("prompt", &Redacted(&self.prompt))
            .finish()
    }
}

/// Raw recognition result, before post-processing.
#[derive(Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub lang: Lang,
}

impl std::fmt::Debug for Transcript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transcript")
            .field("text", &Redacted(&self.text))
            .field("lang", &self.lang)
            .finish()
    }
}

#[async_trait]
pub trait AsrProvider: Send + Sync {
    /// Short stable name for logs and settings, e.g. `breeze`, `openai`.
    fn name(&self) -> &str;

    async fn transcribe(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<Transcript>;
}

/// Helpers shared by the cloud provider tests (ASR and LLM).
#[cfg(test)]
pub(crate) mod test_support {
    use std::time::Duration;

    use crate::audio::AudioClip;
    use crate::error::{ProviderErrorKind, Stage};
    use crate::V2cError;

    /// 1 s test tone.
    pub fn tone() -> AudioClip {
        super::wav::probe_clip(1000)
    }

    /// 200 OK, but only after 2 s (longer than every test timeout).
    pub fn slow_ok(body: serde_json::Value) -> wiremock::ResponseTemplate {
        wiremock::ResponseTemplate::new(200)
            .set_body_json(body)
            .set_delay(Duration::from_secs(2))
    }

    pub async fn only_request(server: &wiremock::MockServer) -> wiremock::Request {
        let mut reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        reqs.remove(0)
    }

    pub fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    pub fn assert_provider_err(err: &V2cError, stage: Stage, kind: ProviderErrorKind) {
        match err {
            V2cError::Provider {
                stage: s, kind: k, ..
            } => assert_eq!((*s, *k), (stage, kind), "{err:?}"),
            other => panic!("expected provider error {kind:?}, got {other:?}"),
        }
    }

    /// Errors end up in logs: neither Display nor Debug may carry keys or user text.
    pub fn assert_no_secrets(err: &V2cError, secrets: &[&str]) {
        let shown = format!("{err} {err:?}");
        for s in secrets {
            assert!(!shown.contains(s), "error leaks {s:?}: {shown}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_lang_classifies() {
        assert_eq!(script_lang("你好"), Lang::Zh);
        assert_eq!(script_lang("hello"), Lang::En);
        assert_eq!(script_lang("開 meeting"), Lang::Mixed);
        assert_eq!(script_lang(""), Lang::Zh);
    }

    #[test]
    fn transcript_debug_hides_text() {
        let t = Transcript {
            text: "秘密的內容".into(),
            lang: Lang::Zh,
        };
        let s = format!("{t:?}");
        assert!(!s.contains("秘密"));
        assert!(s.contains("Zh"));
    }
}
