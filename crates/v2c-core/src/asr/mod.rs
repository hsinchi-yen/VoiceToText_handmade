//! Speech recognition interface. Implementations: local (in `v2c-asr-local`),
//! OpenAI, Gemini, and OpenAI-compatible endpoints (T1.7).

use async_trait::async_trait;

use crate::{audio::AudioClip, redact::Redacted, Lang, Result};

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

#[cfg(test)]
mod tests {
    use super::*;

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
