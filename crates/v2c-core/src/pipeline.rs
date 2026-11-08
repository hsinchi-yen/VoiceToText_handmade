//! The speech-to-text pipeline: ASR -> zh-TW conversion -> optional polish/translate.
//! Flow and latency budget: `tasks/plan.md` section 2. Implemented in T1.1 / T1.5 / T1.6.

use std::sync::Arc;

use crate::{
    asr::AsrProvider, audio::AudioClip, llm::LlmProvider, redact::Redacted, Lang, Mode, Result,
};

/// Milliseconds spent in each stage. Logged and used by `v2c-cli bench`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timings {
    pub asr_ms: u64,
    pub postprocess_ms: u64,
    /// `None` when no LLM step ran.
    pub llm_ms: Option<u64>,
    pub total_ms: u64,
}

/// Something went wrong but the pipeline still produced usable text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// LLM polish failed or timed out; rule-based filler removal was used instead.
    PolishFellBack,
    /// A cloud provider failed and the local provider was used instead.
    CloudFellBackToLocal,
}

#[derive(Clone, PartialEq)]
pub struct Output {
    pub text: String,
    pub mode: Mode,
    pub lang: Lang,
    pub timings: Timings,
    pub warnings: Vec<Warning>,
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("text", &Redacted(&self.text))
            .field("mode", &self.mode)
            .field("lang", &self.lang)
            .field("timings", &self.timings)
            .field("warnings", &self.warnings)
            .finish()
    }
}

/// Pipeline settings derived from `Config`.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineConfig {
    pub polish_enabled: bool,
    pub polish_timeout_ms: u64,
    pub min_clip_ms: u64,
    pub vocabulary: Vec<String>,
}

pub struct Pipeline {
    asr: Arc<dyn AsrProvider>,
    /// Used for polish and translate; `None` if no LLM is available.
    llm: Option<Arc<dyn LlmProvider>>,
    cfg: PipelineConfig,
}

impl Pipeline {
    pub fn new(
        asr: Arc<dyn AsrProvider>,
        llm: Option<Arc<dyn LlmProvider>>,
        cfg: PipelineConfig,
    ) -> Self {
        Self { asr, llm, cfg }
    }

    pub fn config(&self) -> &PipelineConfig {
        &self.cfg
    }

    /// Runs one utterance. Returns `Ok(None)` for clips that are too short or silent.
    pub async fn run(&self, clip: AudioClip, mode: Mode) -> Result<Option<Output>> {
        let _ = (&self.asr, &self.llm, clip, mode);
        todo!("T1.1: implement per tasks/plan.md section 2")
    }
}
