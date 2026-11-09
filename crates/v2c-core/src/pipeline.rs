//! The speech-to-text pipeline: ASR -> zh-TW conversion -> optional polish/translate.
//! Flow and latency budget: `tasks/plan.md` section 2. Implemented in T1.1 / T1.5 / T1.6.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::{
    asr::{AsrContext, AsrProvider},
    audio::AudioClip,
    error::{ProviderErrorKind, Stage},
    llm::LlmProvider,
    postprocess::{fillers::strip_fillers, zh_tw::to_zh_tw},
    prompts,
    redact::Redacted,
    Lang, Mode, Result, V2cError,
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
    /// Upper bound for the length-scaled polish timeout; `None` = no scaling.
    polish_cap_ms: Option<u64>,
}

/// Progress reported by [`Pipeline::run_with_progress`] (no text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// The LLM step (polish or translate) is starting.
    LlmStarted(Mode),
}

/// Transcripts up to this many characters (about 10 s of speech) get the plain
/// `polish_timeout_ms`; longer ones get [`POLISH_MS_PER_CHAR`] more per extra character.
pub const POLISH_FREE_CHARS: usize = 60;
/// Extra polish time per character beyond [`POLISH_FREE_CHARS`], ms.
pub const POLISH_MS_PER_CHAR: u64 = 10;

/// Polish timeout for a transcript of `chars` characters: `base_ms` for short utterances, plus
/// [`POLISH_MS_PER_CHAR`] per character beyond [`POLISH_FREE_CHARS`], never above `cap_ms`
/// (and never below `base_ms`). Long hands-free dictation needs more than 1 s to polish.
pub fn scaled_polish_timeout_ms(base_ms: u64, chars: usize, cap_ms: u64) -> u64 {
    let extra = (chars.saturating_sub(POLISH_FREE_CHARS) as u64).saturating_mul(POLISH_MS_PER_CHAR);
    base_ms.saturating_add(extra).min(cap_ms.max(base_ms))
}

/// A clip whose loudest 30 ms window is below this RMS level is treated as silence
/// (about -42 dBFS; normal speech into a headset or laptop mic peaks well above -30 dBFS).
const SILENCE_RMS: f32 = 0.008;
const RMS_WINDOW_MS: u64 = 30;

impl Pipeline {
    pub fn new(
        asr: Arc<dyn AsrProvider>,
        llm: Option<Arc<dyn LlmProvider>>,
        cfg: PipelineConfig,
    ) -> Self {
        Self {
            asr,
            llm,
            cfg,
            polish_cap_ms: None,
        }
    }

    /// Scales the polish timeout with the transcript length (see
    /// [`scaled_polish_timeout_ms`]), up to `cap_ms` (normally the LLM provider's own timeout).
    pub fn with_polish_timeout_cap_ms(mut self, cap_ms: u64) -> Self {
        self.polish_cap_ms = Some(cap_ms);
        self
    }

    /// The polish timeout used for a transcript of `chars` characters.
    pub fn polish_timeout_for(&self, chars: usize) -> u64 {
        match self.polish_cap_ms {
            Some(cap) => scaled_polish_timeout_ms(self.cfg.polish_timeout_ms, chars, cap),
            None => self.cfg.polish_timeout_ms,
        }
    }

    pub fn config(&self) -> &PipelineConfig {
        &self.cfg
    }

    /// Runs one utterance. Returns `Ok(None)` for clips that are too short or silent, and for
    /// clips in which nothing was recognised.
    pub async fn run(&self, clip: AudioClip, mode: Mode) -> Result<Option<Output>> {
        self.run_with_progress(clip, mode, &|_| {}).await
    }

    /// Like [`Pipeline::run`], calling `progress` when a stage starts (e.g. to show
    /// "polishing" in the UI). `progress` must return quickly.
    pub async fn run_with_progress(
        &self,
        clip: AudioClip,
        mode: Mode,
        progress: &(dyn Fn(Progress) + Send + Sync),
    ) -> Result<Option<Output>> {
        let started = Instant::now();

        // 1. Too short or silent: output nothing.
        let audio_ms = clip.duration_ms();
        if audio_ms < self.cfg.min_clip_ms {
            debug!(
                audio_ms,
                min_ms = self.cfg.min_clip_ms,
                "clip too short; skipped"
            );
            return Ok(None);
        }
        let peak_rms = peak_window_rms(&clip);
        if peak_rms < SILENCE_RMS {
            debug!(audio_ms, peak_rms, "clip is silent; skipped");
            return Ok(None);
        }
        if mode == Mode::TranslateZhToEn && self.llm.is_none() {
            // Fail before spending time on ASR.
            return Err(V2cError::NotConfigured("translation model"));
        }

        // 2. Speech recognition.
        let ctx = AsrContext {
            vocabulary: self.cfg.vocabulary.clone(),
            // TODO(T1.8): build with `prompts::asr_initial_prompt`; providers fall back to
            // `prompts::ASR_BASE_PROMPT` while this is empty.
            prompt: String::new(),
        };
        let t = Instant::now();
        let transcript = self.asr.transcribe(&clip, &ctx).await?;
        let asr_ms = elapsed_ms(t);

        // 3. Simplified -> Taiwan Traditional.
        let t = Instant::now();
        let text = to_zh_tw(transcript.text.trim());
        let mut postprocess_ms = elapsed_ms(t);
        if text.is_empty() {
            debug!(asr_ms, provider = self.asr.name(), "nothing recognised");
            return Ok(None);
        }

        // 4./5. Optional LLM step; its output goes through zh_tw again.
        let mut warnings = Vec::new();
        let mut llm_ms = None;
        let (text, lang) = match (mode, &self.llm) {
            (Mode::Dictate, Some(llm)) if self.cfg.polish_enabled => {
                progress(Progress::LlmStarted(mode));
                let t = Instant::now();
                let polished = self.polish(llm.as_ref(), &text).await;
                llm_ms = Some(elapsed_ms(t));
                let text = match polished {
                    Some(p) => {
                        let t = Instant::now();
                        let p = to_zh_tw(&p);
                        postprocess_ms += elapsed_ms(t);
                        p
                    }
                    None => {
                        warnings.push(Warning::PolishFellBack);
                        strip_fillers(&text)
                    }
                };
                (text, transcript.lang)
            }
            // Polish on but no LLM available (LM Studio not running, plan §6.1): rule-based
            // filler removal only.
            (Mode::Dictate, None) if self.cfg.polish_enabled => {
                (strip_fillers(&text), transcript.lang)
            }
            (Mode::Dictate, _) => (text, transcript.lang),
            (Mode::TranslateZhToEn, None) => {
                return Err(V2cError::NotConfigured("translation model"))
            }
            (Mode::TranslateZhToEn, Some(llm)) => {
                progress(Progress::LlmStarted(mode));
                let t = Instant::now();
                let translated = llm
                    .complete(
                        &prompts::translate(&self.cfg.vocabulary),
                        &prompts::wrap_transcript(&text),
                    )
                    .await?;
                llm_ms = Some(elapsed_ms(t));
                let t = Instant::now();
                let translated = to_zh_tw(translated.trim());
                postprocess_ms += elapsed_ms(t);
                if translated.is_empty() {
                    return Err(V2cError::Provider {
                        stage: Stage::Translate,
                        provider: llm.name().to_owned(),
                        kind: ProviderErrorKind::BadResponse,
                    });
                }
                (translated, Lang::En)
            }
        };

        let timings = Timings {
            asr_ms,
            postprocess_ms,
            llm_ms,
            total_ms: elapsed_ms(started),
        };
        // Lengths and timings only; never the text (AGENTS.md rule 1).
        info!(
            provider = self.asr.name(),
            ?mode,
            ?lang,
            audio_ms,
            chars = text.chars().count(),
            asr_ms = timings.asr_ms,
            postprocess_ms = timings.postprocess_ms,
            llm_ms = ?timings.llm_ms,
            total_ms = timings.total_ms,
            warnings = warnings.len(),
            "pipeline done"
        );
        // 6. History is written by the caller's output sinks (T1.9).
        Ok(Some(Output {
            text,
            mode,
            lang,
            timings,
            warnings,
        }))
    }

    /// Calls the LLM with the polish timeout. `None` on error, timeout, or empty output.
    async fn polish(&self, llm: &dyn LlmProvider, text: &str) -> Option<String> {
        let system = prompts::polish(&self.cfg.vocabulary);
        let user = prompts::wrap_transcript(text);
        let timeout_ms = self.polish_timeout_for(text.chars().count());
        let limit = Duration::from_millis(timeout_ms);
        match tokio::time::timeout(limit, llm.complete(&system, &user)).await {
            Ok(Ok(out)) if !out.trim().is_empty() => Some(out.trim().to_owned()),
            Ok(Ok(_)) => {
                warn!(
                    provider = llm.name(),
                    "polish returned empty output; falling back"
                );
                None
            }
            Ok(Err(e)) => {
                // `V2cError` never embeds user text.
                warn!(provider = llm.name(), error = %e, "polish failed; falling back");
                None
            }
            Err(_) => {
                warn!(
                    provider = llm.name(),
                    timeout_ms, "polish timed out; falling back"
                );
                None
            }
        }
    }
}

/// RMS of the loudest `RMS_WINDOW_MS` window. Unlike whole-clip RMS this is not diluted when a
/// short word is surrounded by a long pause (or by the pre-roll buffer).
fn peak_window_rms(clip: &AudioClip) -> f32 {
    let window = (u64::from(clip.sample_rate) * RMS_WINDOW_MS / 1000).max(1) as usize;
    clip.samples
        .chunks(window)
        .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt())
        .fold(0.0, f32::max)
}

fn elapsed_ms(since: Instant) -> u64 {
    since.elapsed().as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::asr::Transcript;

    struct FakeAsr {
        text: &'static str,
        lang: Lang,
        calls: AtomicUsize,
    }

    impl FakeAsr {
        fn new(text: &'static str, lang: Lang) -> Arc<Self> {
            Arc::new(Self {
                text,
                lang,
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl AsrProvider for FakeAsr {
        fn name(&self) -> &str {
            "fake-asr"
        }
        async fn transcribe(&self, _clip: &AudioClip, _ctx: &AsrContext) -> Result<Transcript> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Transcript {
                text: self.text.into(),
                lang: self.lang,
            })
        }
    }

    enum LlmBehavior {
        Reply(&'static str),
        Fail,
        Sleep(u64),
    }

    struct FakeLlm {
        behavior: LlmBehavior,
        last_user: std::sync::Mutex<Option<String>>,
    }

    impl FakeLlm {
        fn new(behavior: LlmBehavior) -> Arc<Self> {
            Arc::new(Self {
                behavior,
                last_user: std::sync::Mutex::new(None),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for FakeLlm {
        fn name(&self) -> &str {
            "fake-llm"
        }
        async fn complete(&self, _system: &str, user: &str) -> Result<String> {
            *self.last_user.lock().unwrap() = Some(user.to_owned());
            match self.behavior {
                LlmBehavior::Reply(s) => Ok(s.into()),
                LlmBehavior::Fail => Err(V2cError::Provider {
                    stage: Stage::Polish,
                    provider: "fake-llm".into(),
                    kind: ProviderErrorKind::Unreachable,
                }),
                LlmBehavior::Sleep(ms) => {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    Ok("too late".into())
                }
            }
        }
    }

    fn cfg(polish_enabled: bool) -> PipelineConfig {
        PipelineConfig {
            polish_enabled,
            polish_timeout_ms: 50,
            min_clip_ms: 300,
            vocabulary: vec![],
        }
    }

    /// A clip of `ms` milliseconds with a 220 Hz tone at the given amplitude.
    fn tone(ms: u64, amplitude: f32) -> AudioClip {
        let n = (16 * ms) as usize;
        AudioClip::new(
            (0..n)
                .map(|i| amplitude * (i as f32 * 220.0 * std::f32::consts::TAU / 16_000.0).sin())
                .collect(),
        )
    }

    fn pipeline(asr: Arc<FakeAsr>, llm: Option<Arc<FakeLlm>>, polish: bool) -> Pipeline {
        Pipeline::new(asr, llm.map(|l| l as Arc<dyn LlmProvider>), cfg(polish))
    }

    #[tokio::test]
    async fn short_clip_gives_none_without_calling_asr() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let p = pipeline(asr.clone(), None, false);
        assert_eq!(p.run(tone(299, 0.3), Mode::Dictate).await.unwrap(), None);
        assert_eq!(asr.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn silent_clip_gives_none_without_calling_asr() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let p = pipeline(asr.clone(), None, false);
        assert_eq!(p.run(tone(2000, 0.002), Mode::Dictate).await.unwrap(), None);
        assert_eq!(
            p.run(AudioClip::new(vec![0.0; 32_000]), Mode::Dictate)
                .await
                .unwrap(),
            None
        );
        assert_eq!(asr.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn short_word_inside_long_silence_is_not_silent() {
        let mut samples = vec![0.0; 16_000];
        samples.extend(tone(100, 0.2).samples);
        samples.extend(vec![0.0; 16_000]);
        let asr = FakeAsr::new("好", Lang::Zh);
        let p = pipeline(asr.clone(), None, false);
        let out = p.run(AudioClip::new(samples), Mode::Dictate).await.unwrap();
        assert_eq!(out.unwrap().text, "好");
    }

    #[tokio::test]
    async fn dictate_converts_to_taiwan_traditional() {
        let asr = FakeAsr::new("  这个软件的视频功能 ", Lang::Zh);
        let p = pipeline(asr, None, true);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "這個軟體的影片功能");
        assert_eq!(out.mode, Mode::Dictate);
        assert_eq!(out.lang, Lang::Zh);
        assert_eq!(out.timings.llm_ms, None);
        assert!(out.timings.total_ms >= out.timings.asr_ms);
        assert!(out.warnings.is_empty());
    }

    #[tokio::test]
    async fn english_passes_through_unchanged() {
        let asr = FakeAsr::new("I think we should test this first.", Lang::En);
        let p = pipeline(asr, None, false);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "I think we should test this first.");
        assert_eq!(out.lang, Lang::En);
    }

    #[tokio::test]
    async fn empty_transcript_gives_none() {
        let asr = FakeAsr::new("   ", Lang::Zh);
        let p = pipeline(asr, None, false);
        assert_eq!(p.run(tone(1000, 0.3), Mode::Dictate).await.unwrap(), None);
    }

    #[tokio::test]
    async fn polish_output_is_used_and_converted() {
        let asr = FakeAsr::new("嗯 这个软件", Lang::Zh);
        let llm = FakeLlm::new(LlmBehavior::Reply("这个软件"));
        let p = pipeline(asr, Some(llm.clone()), true);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "這個軟體");
        assert!(out.timings.llm_ms.is_some());
        assert!(out.warnings.is_empty());
        // The LLM receives the already-converted ASR text, wrapped as data.
        let user = llm.last_user.lock().unwrap().clone().unwrap();
        assert_eq!(user, prompts::wrap_transcript("嗯 這個軟體"));
        assert_eq!(prompts::unwrap_transcript(&user), "嗯 這個軟體");
    }

    #[tokio::test]
    async fn polish_disabled_skips_llm() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let llm = FakeLlm::new(LlmBehavior::Reply("不該出現"));
        let p = pipeline(asr, Some(llm.clone()), false);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "你好");
        assert_eq!(out.timings.llm_ms, None);
        assert!(llm.last_user.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn polish_failure_falls_back_with_warning() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let p = pipeline(asr, Some(FakeLlm::new(LlmBehavior::Fail)), true);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "你好");
        assert_eq!(out.warnings, vec![Warning::PolishFellBack]);
    }

    #[tokio::test]
    async fn polish_failure_strips_fillers() {
        let asr = FakeAsr::new("嗯，我們明天見。", Lang::Zh);
        let p = pipeline(asr, Some(FakeLlm::new(LlmBehavior::Fail)), true);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, strip_fillers("嗯，我們明天見。"));
        assert!(!out.text.starts_with('嗯'), "{}", out.text);
        assert_eq!(out.warnings, vec![Warning::PolishFellBack]);
    }

    #[tokio::test]
    async fn polish_without_llm_strips_fillers() {
        let asr = FakeAsr::new("嗯，我們明天見。", Lang::Zh);
        let p = pipeline(asr, None, true);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert!(!out.text.starts_with('嗯'), "{}", out.text);
        assert_eq!(out.timings.llm_ms, None);
    }

    #[tokio::test]
    async fn polish_disabled_keeps_text_verbatim() {
        let asr = FakeAsr::new("嗯，我們明天見。", Lang::Zh);
        let p = pipeline(asr, None, false);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "嗯，我們明天見。");
    }

    #[tokio::test]
    async fn polish_empty_output_falls_back_with_warning() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let p = pipeline(asr, Some(FakeLlm::new(LlmBehavior::Reply("  "))), true);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "你好");
        assert_eq!(out.warnings, vec![Warning::PolishFellBack]);
    }

    #[tokio::test]
    async fn polish_timeout_falls_back_with_warning() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let p = pipeline(asr, Some(FakeLlm::new(LlmBehavior::Sleep(2_000))), true);
        let started = Instant::now();
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(1_500));
        assert_eq!(out.text, "你好");
        assert_eq!(out.warnings, vec![Warning::PolishFellBack]);
        assert!(out.timings.llm_ms.unwrap() >= 50);
    }

    #[tokio::test]
    async fn translate_without_llm_is_not_configured() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let p = pipeline(asr.clone(), None, true);
        let err = p
            .run(tone(1000, 0.3), Mode::TranslateZhToEn)
            .await
            .unwrap_err();
        assert!(matches!(err, V2cError::NotConfigured(_)), "{err:?}");
        assert_eq!(asr.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn translate_with_llm_outputs_english() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let llm = FakeLlm::new(LlmBehavior::Reply(" Hello. "));
        let p = pipeline(asr, Some(llm), false);
        let out = p
            .run(tone(1000, 0.3), Mode::TranslateZhToEn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "Hello.");
        assert_eq!(out.lang, Lang::En);
        assert_eq!(out.mode, Mode::TranslateZhToEn);
        assert!(out.timings.llm_ms.is_some());
    }

    #[tokio::test]
    async fn translate_errors_propagate() {
        let asr = FakeAsr::new("你好", Lang::Zh);
        let p = pipeline(asr, Some(FakeLlm::new(LlmBehavior::Fail)), false);
        let err = p
            .run(tone(1000, 0.3), Mode::TranslateZhToEn)
            .await
            .unwrap_err();
        assert!(matches!(err, V2cError::Provider { .. }), "{err:?}");
    }

    #[test]
    fn polish_timeout_scales_with_length_and_is_capped() {
        // Short utterances (about 10 s of speech) keep the 1.0 s budget.
        assert_eq!(scaled_polish_timeout_ms(1000, 0, 3000), 1000);
        assert_eq!(
            scaled_polish_timeout_ms(1000, POLISH_FREE_CHARS, 3000),
            1000
        );
        // Longer ones get 10 ms per extra character.
        assert_eq!(
            scaled_polish_timeout_ms(1000, POLISH_FREE_CHARS + 100, 3000),
            2000
        );
        // Capped at the provider timeout, but never below the configured base.
        assert_eq!(scaled_polish_timeout_ms(1000, 10_000, 3000), 3000);
        assert_eq!(scaled_polish_timeout_ms(1000, 10_000, 500), 1000);
        assert_eq!(
            scaled_polish_timeout_ms(1000, usize::MAX, u64::MAX),
            u64::MAX
        );
    }

    #[test]
    fn polish_timeout_scaling_is_opt_in() {
        let asr = FakeAsr::new("x", Lang::Zh);
        let p = pipeline(asr.clone(), None, true);
        assert_eq!(p.polish_timeout_for(1_000), 50);
        let p = pipeline(asr, None, true).with_polish_timeout_cap_ms(5_000);
        assert_eq!(p.polish_timeout_for(10), 50);
        assert_eq!(p.polish_timeout_for(POLISH_FREE_CHARS + 20), 250);
    }

    #[tokio::test]
    async fn long_transcript_gets_a_longer_polish_budget() {
        // 260 chars: budget = 50 + 200 * 10 = 2050 ms; the LLM takes 300 ms.
        let long: &'static str = Box::leak("好".repeat(260).into_boxed_str());
        let asr = FakeAsr::new(long, Lang::Zh);
        let llm = FakeLlm::new(LlmBehavior::Sleep(300));
        let p = pipeline(asr, Some(llm), true).with_polish_timeout_cap_ms(3_000);
        let out = p
            .run(tone(1000, 0.3), Mode::Dictate)
            .await
            .unwrap()
            .unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.text, "too late");
    }

    #[tokio::test]
    async fn progress_reports_llm_start_only_when_an_llm_runs() {
        let seen = std::sync::Mutex::new(Vec::new());
        let record = |p: Progress| seen.lock().unwrap().push(p);
        let asr = FakeAsr::new("你好", Lang::Zh);
        let p = pipeline(asr.clone(), None, true);
        p.run_with_progress(tone(1000, 0.3), Mode::Dictate, &record)
            .await
            .unwrap();
        assert!(seen.lock().unwrap().is_empty());
        let llm = FakeLlm::new(LlmBehavior::Reply("Hello."));
        let p = pipeline(asr, Some(llm), true);
        p.run_with_progress(tone(1000, 0.3), Mode::Dictate, &record)
            .await
            .unwrap();
        p.run_with_progress(tone(1000, 0.3), Mode::TranslateZhToEn, &record)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                Progress::LlmStarted(Mode::Dictate),
                Progress::LlmStarted(Mode::TranslateZhToEn)
            ]
        );
    }

    #[test]
    fn peak_window_rms_finds_loud_part() {
        let mut samples = vec![0.0; 16_000];
        samples.extend(vec![0.5; 480]);
        let rms = peak_window_rms(&AudioClip::new(samples));
        assert!((rms - 0.5).abs() < 1e-3, "{rms}");
        assert_eq!(peak_window_rms(&AudioClip::new(vec![])), 0.0);
    }
}
