//! Breeze-ASR-25 (a Whisper large-v2 fine-tune) running through whisper.cpp via `whisper-rs`.
//!
//! Language strategy (`tasks/plan.md` section 5): forcing `zh` can make Whisper translate pure
//! English speech into Chinese, so we first run the model's language detection. If it says
//! `en` we decode as `en`; anything else is decoded as `zh`, which keeps embedded English words.
//!
//! Privacy: nothing in this module logs or embeds recognised text in errors. whisper.cpp's own
//! log output is silenced (see [`BreezeAsr::load`]) because its debug paths can print tokens.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use v2c_core::{
    asr::{AsrContext, AsrProvider, Transcript},
    audio::{AudioClip, SAMPLE_RATE},
    error::{ProviderErrorKind, Stage},
    prompts::ASR_BASE_PROMPT,
    Lang, Result, V2cError,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Provider name used in logs, settings, and errors.
pub const PROVIDER_NAME: &str = "breeze";

/// Language the decoder is told to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeLang {
    Zh,
    En,
}

impl DecodeLang {
    /// Whisper language code.
    pub fn code(self) -> &'static str {
        match self {
            DecodeLang::Zh => "zh",
            DecodeLang::En => "en",
        }
    }
}

/// How the decode language is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LanguageMode {
    /// Run language detection first; `en` -> decode as `en`, anything else -> `zh`.
    #[default]
    Detect,
    /// Skip detection and always decode in this language (for comparison in `bench`).
    Fixed(DecodeLang),
}

/// Decode language for a Whisper-detected language code.
pub fn decode_lang_for(detected: &str) -> DecodeLang {
    if detected.eq_ignore_ascii_case("en") {
        DecodeLang::En
    } else {
        DecodeLang::Zh
    }
}

/// `Lang` of a transcript, from the decode language and the script of the output.
///
/// Decoding as `zh` keeps English words, so a `zh` decode containing both CJK characters and
/// Latin letters is `Mixed`. Only character classes are inspected; the text is not stored.
pub fn transcript_lang(decoded: DecodeLang, text: &str) -> Lang {
    let has_cjk = text.chars().any(is_cjk);
    let has_latin = text
        .chars()
        .any(|c| c.is_ascii_alphabetic() || is_fullwidth_latin(c));
    match (has_cjk, has_latin) {
        (true, true) => Lang::Mixed,
        (true, false) => Lang::Zh,
        (false, true) => Lang::En,
        (false, false) => match decoded {
            DecodeLang::Zh => Lang::Zh,
            DecodeLang::En => Lang::En,
        },
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF      // CJK Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
        | 0x20000..=0x2FFFF) // Extensions B and later
}

fn is_fullwidth_latin(c: char) -> bool {
    matches!(c, 'Ａ'..='Ｚ' | 'ａ'..='ｚ')
}

/// Load-time and decode settings.
#[derive(Debug, Clone, PartialEq)]
pub struct BreezeOptions {
    /// Use the GPU. Only has an effect when built with the `cuda` feature.
    pub use_gpu: bool,
    /// whisper.cpp flash attention; lowers VRAM for temporary buffers (`tasks/plan.md` 5.1).
    pub flash_attn: bool,
    /// CPU threads for whisper.cpp.
    pub n_threads: usize,
    pub language: LanguageMode,
    /// Encoder context size override (`audio_ctx`, max 1500 = 30 s). `None` = full window.
    /// Smaller values are faster on short clips but may cost accuracy; for experiments only.
    pub audio_ctx: Option<u32>,
}

impl Default for BreezeOptions {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            use_gpu: true,
            flash_attn: true,
            n_threads: cores.clamp(1, 8),
            language: LanguageMode::Detect,
            audio_ctx: None,
        }
    }
}

/// Detailed result of one recognition, used by `v2c-cli bench`.
#[derive(Debug, Clone, PartialEq)]
pub struct BreezeOutput {
    /// `Debug` of `Transcript` redacts the text.
    pub transcript: Transcript,
    /// Language detected by Whisper and its probability; `None` with `LanguageMode::Fixed`.
    pub detected: Option<(&'static str, f32)>,
    pub decode_lang: DecodeLang,
    pub lang_detect_ms: u64,
    pub decode_ms: u64,
}

impl BreezeOutput {
    pub fn total_ms(&self) -> u64 {
        self.lang_detect_ms + self.decode_ms
    }
}

struct Inner {
    // Kept alive for `state`; whisper-rs states hold an `Arc` to the context internally.
    _ctx: WhisperContext,
    /// One reusable state (GPU buffers are allocated once). Inference is serialized.
    state: Mutex<whisper_rs::WhisperState>,
    opts: BreezeOptions,
}

/// Breeze-ASR-25 provider. Load once at startup and keep it resident.
#[derive(Clone)]
pub struct BreezeAsr {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for BreezeAsr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BreezeAsr")
            .field("opts", &self.inner.opts)
            .finish()
    }
}

/// True when this binary was built with the `cuda` feature.
pub const fn cuda_enabled() -> bool {
    cfg!(feature = "cuda")
}

/// whisper.cpp build/system info (e.g. `CUDA = 1`), for diagnostics. Contains no user text.
pub fn system_info() -> &'static str {
    whisper_rs::print_system_info()
}

fn provider_error(kind: ProviderErrorKind) -> V2cError {
    V2cError::Provider {
        stage: Stage::Asr,
        provider: PROVIDER_NAME.into(),
        kind,
    }
}

impl BreezeAsr {
    /// Loads a GGML model file (e.g. `ggml-breeze-asr-25-q5_0.bin`).
    pub fn load(model: &Path, opts: BreezeOptions) -> Result<Self> {
        if !model.is_file() {
            return Err(V2cError::Config(format!(
                "Breeze model file not found: {}",
                model.display()
            )));
        }
        // Route whisper.cpp / GGML logs into whisper-rs hooks. Without the `log_backend` or
        // `tracing_backend` features they are dropped, which keeps tokens out of any output.
        whisper_rs::install_logging_hooks();

        let mut cparams = WhisperContextParameters::default();
        cparams.use_gpu(opts.use_gpu && cuda_enabled());
        cparams.flash_attn(opts.flash_attn);
        let ctx = WhisperContext::new_with_params(model, cparams).map_err(|e| {
            V2cError::Config(format!("cannot load Breeze model {}: {e}", model.display()))
        })?;
        if !ctx.is_multilingual() {
            return Err(V2cError::Config(
                "Breeze model is not multilingual; wrong file?".into(),
            ));
        }
        let state = ctx
            .create_state()
            .map_err(|_| provider_error(ProviderErrorKind::Server))?;
        Ok(Self {
            inner: Arc::new(Inner {
                _ctx: ctx,
                state: Mutex::new(state),
                opts,
            }),
        })
    }

    pub fn options(&self) -> &BreezeOptions {
        &self.inner.opts
    }

    /// Runs recognition on the calling thread. Blocks for the whole inference.
    pub fn transcribe_blocking(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<BreezeOutput> {
        self.inner.run(clip, ctx)
    }
}

impl Inner {
    fn run(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<BreezeOutput> {
        if clip.sample_rate != SAMPLE_RATE {
            return Err(V2cError::Audio(format!(
                "expected {SAMPLE_RATE} Hz audio, got {} Hz",
                clip.sample_rate
            )));
        }
        if clip.samples.is_empty() {
            return Err(V2cError::Audio("empty clip".into()));
        }
        let threads = self.opts.n_threads.max(1);
        let mut state = self
            .state
            .lock()
            .map_err(|_| provider_error(ProviderErrorKind::Server))?;

        let t0 = Instant::now();
        let (decode_lang, detected) = match self.opts.language {
            LanguageMode::Fixed(l) => (l, None),
            LanguageMode::Detect => {
                state
                    .pcm_to_mel(&clip.samples, threads)
                    .map_err(|_| provider_error(ProviderErrorKind::Server))?;
                let (id, probs) = state
                    .lang_detect(0, threads)
                    .map_err(|_| provider_error(ProviderErrorKind::Server))?;
                let code = whisper_rs::get_lang_str(id).unwrap_or("zh");
                let prob = probs.get(id as usize).copied().unwrap_or(0.0);
                (decode_lang_for(code), Some((code, prob)))
            }
        };
        let lang_detect_ms = t0.elapsed().as_millis() as u64;

        let prompt = initial_prompt(ctx);
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(threads as i32);
        params.set_language(Some(decode_lang.code()));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_single_segment(true);
        params.set_no_timestamps(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(n) = self.opts.audio_ctx {
            params.set_audio_ctx(n.min(1500) as i32);
        }
        if !prompt.is_empty() {
            params.set_initial_prompt(&prompt);
        }

        let t1 = Instant::now();
        state
            .full(params, &clip.samples)
            .map_err(|_| provider_error(ProviderErrorKind::Server))?;
        let mut text = String::new();
        for seg in state.as_iter() {
            let s = seg
                .to_str_lossy()
                .map_err(|_| provider_error(ProviderErrorKind::BadResponse))?;
            text.push_str(&s);
        }
        let decode_ms = t1.elapsed().as_millis() as u64;

        let text = text.trim().to_string();
        let lang = transcript_lang(decode_lang, &text);
        Ok(BreezeOutput {
            transcript: Transcript { text, lang },
            detected,
            decode_lang,
            lang_detect_ms,
            decode_ms,
        })
    }
}

/// `ctx.prompt` if set, otherwise the base prompt. NUL bytes are removed (whisper-rs panics on them).
///
/// TODO(T1.8): callers will build `ctx.prompt` with `prompts::asr_initial_prompt` (vocabulary).
fn initial_prompt(ctx: &AsrContext) -> String {
    let p = if ctx.prompt.trim().is_empty() {
        ASR_BASE_PROMPT
    } else {
        ctx.prompt.as_str()
    };
    p.replace('\0', "")
}

#[async_trait]
impl AsrProvider for BreezeAsr {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// Inference is CPU/GPU-bound, so inside a Tokio runtime it runs on the blocking pool;
    /// without a runtime it runs on the calling thread.
    async fn transcribe(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<Transcript> {
        if tokio::runtime::Handle::try_current().is_ok() {
            let inner = Arc::clone(&self.inner);
            let clip = clip.clone();
            let ctx = ctx.clone();
            tokio::task::spawn_blocking(move || inner.run(&clip, &ctx))
                .await
                .map_err(|_| provider_error(ProviderErrorKind::Server))?
                .map(|o| o.transcript)
        } else {
            self.inner.run(clip, ctx).map(|o| o.transcript)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_detection_decodes_as_english() {
        assert_eq!(decode_lang_for("en"), DecodeLang::En);
        assert_eq!(decode_lang_for("EN"), DecodeLang::En);
    }

    #[test]
    fn everything_else_decodes_as_chinese() {
        for code in ["zh", "ja", "ko", "yue", "de", ""] {
            assert_eq!(decode_lang_for(code), DecodeLang::Zh, "code {code:?}");
        }
    }

    #[test]
    fn decode_lang_codes() {
        assert_eq!(DecodeLang::Zh.code(), "zh");
        assert_eq!(DecodeLang::En.code(), "en");
    }

    #[test]
    fn transcript_lang_from_script() {
        assert_eq!(
            transcript_lang(DecodeLang::Zh, "今天下午三點要開會。"),
            Lang::Zh
        );
        assert_eq!(
            transcript_lang(DecodeLang::Zh, "這個 meeting 我們 delay 一下"),
            Lang::Mixed
        );
        assert_eq!(
            transcript_lang(DecodeLang::En, "Please send me the slides."),
            Lang::En
        );
        // A zh decode that produced only English is still English.
        assert_eq!(transcript_lang(DecodeLang::Zh, "OK fine"), Lang::En);
        // Full-width Latin counts as Latin.
        assert_eq!(transcript_lang(DecodeLang::Zh, "這個ＱＡ"), Lang::Mixed);
    }

    #[test]
    fn transcript_lang_without_letters_falls_back_to_decode_lang() {
        assert_eq!(transcript_lang(DecodeLang::Zh, ""), Lang::Zh);
        assert_eq!(transcript_lang(DecodeLang::En, "123。"), Lang::En);
    }

    #[test]
    fn initial_prompt_defaults_to_base_and_strips_nul() {
        let ctx = AsrContext::default();
        assert_eq!(initial_prompt(&ctx), ASR_BASE_PROMPT);
        let ctx = AsrContext {
            vocabulary: vec![],
            prompt: "a\0b".into(),
        };
        assert_eq!(initial_prompt(&ctx), "ab");
    }

    #[test]
    fn missing_model_is_config_error() {
        let err = BreezeAsr::load(
            Path::new("definitely/not/here/ggml-breeze.bin"),
            BreezeOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, V2cError::Config(_)));
    }

    #[test]
    fn default_options_enable_flash_attn_and_detection() {
        let o = BreezeOptions::default();
        assert!(o.flash_attn);
        assert_eq!(o.language, LanguageMode::Detect);
        assert!(o.n_threads >= 1 && o.n_threads <= 8);
    }
}
