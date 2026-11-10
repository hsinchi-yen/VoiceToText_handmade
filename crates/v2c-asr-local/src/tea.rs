//! TEA-ASR-1.1 on the GPU through transcribe.cpp (crate `transcribe-cpp`), T1.12.
//!
//! TEA-ASR-1.1 is a Qwen3-ASR-1.7B fine-tune for Taiwanese Mandarin and zh/en code-switching
//! (MIT). We load the community GGUF `TEA-ASR-1.1-Q8_0.gguf` (`tasks/plan.md` section 5).
//!
//! - Input is 16 kHz mono. transcribe.cpp caps Qwen3-ASR decoding at 256 tokens, so clips are
//!   cut into pieces of at most [`MAX_CHUNK_SECS`] seconds, at the quietest point of the last
//!   [`SPLIT_SEARCH_SECS`] before the limit ([`chunk_ranges`]), recognised one after the other
//!   and joined ([`join_chunks`]).
//! - Language is forced to `zh`; English inside Chinese speech stays English.
//! - Output: Private Use Area characters (U+E000-U+F8FF) are removed ([`clean_output`]); the GGUF
//!   path does not remove them. The output is already Traditional Chinese with Taiwan wording,
//!   so nothing is converted, and it has almost no punctuation (polishing adds it; with polish
//!   off it is output as is).
//! - `AsrContext::vocabulary` is passed when the model reports `Feature::Vocabulary`; the
//!   prompt is not used.
//! - GPU only: with `use_gpu` the CUDA backend is required (no silent CPU fallback).
//!
//! Cargo feature `tea` (off by default) links transcribe.cpp; pair it with `cuda`. Without it
//! this module still compiles: [`TeaAsr::load`] returns a `Config` error, so callers need no
//! `cfg`.
//!
//! Privacy: nothing here logs or embeds recognised text in errors. transcribe.cpp's own log
//! output is switched off before the first load ([`engine`]), since its debug output may print
//! decoded text.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use v2c_core::{
    asr::{AsrContext, AsrProvider, Transcript},
    audio::{AudioClip, SAMPLE_RATE},
    error::{ProviderErrorKind, Stage},
    Lang, Result, V2cError,
};

use crate::breeze::{transcript_lang, DecodeLang};

/// Provider name used in logs (`asr="tea_asr"`), settings, and errors.
pub const PROVIDER_NAME: &str = "tea_asr";
/// Model file name, as published on Hugging Face (`dorajim15/TEA-ASR-1.1-transcribe-cpp`).
pub const MODEL_FILE: &str = "TEA-ASR-1.1-Q8_0.gguf";
/// SHA-256 of [`MODEL_FILE`].
pub const MODEL_SHA256: &str = "2e4aeb45c9e7f421d99909e29e49a54e204d70ed0e77bebc3f18a135d78298d0";
/// Size of [`MODEL_FILE`] in bytes.
pub const MODEL_SIZE: u64 = 2_184_763_808;
/// Longest piece of audio sent to the model in one run (256-token decode cap).
pub const MAX_CHUNK_SECS: usize = 30;
/// A chunk boundary is placed at the quietest 20 ms frame within this many seconds before
/// [`MAX_CHUNK_SECS`].
pub const SPLIT_SEARCH_SECS: usize = 5;
/// Language passed to the model.
pub const LANGUAGE: &str = "zh";

/// True when this binary was built with the `tea` feature.
pub const fn tea_enabled() -> bool {
    cfg!(feature = "tea")
}

/// Load-time settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeaOptions {
    /// Require the CUDA backend. `false` lets transcribe.cpp pick (CPU in practice); only for
    /// tests and benchmarks, the app always runs TEA-ASR on the GPU.
    pub use_gpu: bool,
}

impl Default for TeaOptions {
    fn default() -> Self {
        Self { use_gpu: true }
    }
}

/// Removes Private Use Area characters (U+E000-U+F8FF) and trims.
pub fn clean_output(raw: &str) -> String {
    raw.chars()
        .filter(|c| !('\u{E000}'..='\u{F8FF}').contains(c))
        .collect::<String>()
        .trim()
        .to_string()
}

/// Joins chunk transcripts: no separator, except a space between two ASCII letters / digits
/// (an English word must not be glued to the next one).
pub fn join_chunks<S: AsRef<str>>(parts: &[S]) -> String {
    let mut out = String::new();
    for p in parts
        .iter()
        .map(|p| p.as_ref().trim())
        .filter(|p| !p.is_empty())
    {
        let glue = out
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric())
            && p.chars().next().is_some_and(|c| c.is_ascii_alphanumeric());
        if glue {
            out.push(' ');
        }
        out.push_str(p);
    }
    out
}

/// Splits `samples` (16 kHz) into ranges of at most `max_secs` seconds. Each cut is placed at
/// the 20 ms frame with the lowest energy within the last `search_secs` before the limit, so
/// words are rarely cut in half.
pub fn chunk_ranges(samples: &[f32], max_secs: usize, search_secs: usize) -> Vec<Range<usize>> {
    let rate = SAMPLE_RATE as usize;
    let max = (max_secs * rate).max(1);
    let search = (search_secs * rate).min(max / 2);
    let frame = rate / 50; // 20 ms
    let mut out = Vec::new();
    let mut start = 0;
    while samples.len() - start > max {
        let hi = start + max;
        let lo = hi - search;
        let mut best = (f32::INFINITY, hi);
        let mut f = lo;
        while f + frame <= hi {
            let e: f32 = samples[f..f + frame].iter().map(|s| s * s).sum();
            // `<=`: on ties prefer the later frame (longer chunk, fewer chunks).
            if e <= best.0 {
                best = (e, f + frame / 2);
            }
            f += frame;
        }
        out.push(start..best.1);
        start = best.1;
    }
    if start < samples.len() || out.is_empty() {
        out.push(start..samples.len());
    }
    out
}

/// `Lang` of a TEA-ASR transcript from its character classes; no letters at all -> `Zh`.
pub fn tea_lang(text: &str) -> Lang {
    transcript_lang(DecodeLang::Zh, text)
}

/// Detailed result of one recognition, used by `v2c-cli bench`.
#[derive(Debug, Clone, PartialEq)]
pub struct TeaOutput {
    /// `Debug` of `Transcript` redacts the text.
    pub transcript: Transcript,
    pub decode_ms: u64,
    /// Number of pieces the clip was cut into.
    pub chunks: usize,
}

fn provider_error(kind: ProviderErrorKind) -> V2cError {
    V2cError::Provider {
        stage: Stage::Asr,
        provider: PROVIDER_NAME.into(),
        kind,
    }
}

fn check_clip(clip: &AudioClip) -> Result<()> {
    if clip.sample_rate != SAMPLE_RATE {
        return Err(V2cError::Audio(format!(
            "expected {SAMPLE_RATE} Hz audio, got {} Hz",
            clip.sample_rate
        )));
    }
    if clip.samples.is_empty() {
        return Err(V2cError::Audio("empty clip".into()));
    }
    Ok(())
}

#[cfg(feature = "tea")]
mod engine {
    use std::sync::{Mutex, Once};

    use transcribe_cpp::{Backend, Error, Feature, Model, ModelOptions, RunOptions, Session};

    use super::*;

    pub(super) struct Engine {
        session: Mutex<Session>,
        vocabulary: bool,
    }

    static QUIET: Once = Once::new();

    impl Engine {
        pub(super) fn load(model: &Path, opts: &TeaOptions) -> Result<Self> {
            // The native library logs to stderr by default; its debug output can contain
            // decoded text. Silence it once per process, before any model is loaded.
            QUIET.call_once(transcribe_cpp::disable_logging);
            let options = ModelOptions {
                backend: if opts.use_gpu {
                    Backend::Cuda
                } else {
                    Backend::Auto
                },
                ..ModelOptions::default()
            };
            // Load errors carry paths, backend and format details only.
            let model = Model::load_with(model, &options)
                .map_err(|e| V2cError::Config(format!("cannot load TEA-ASR model: {e}")))?;
            let vocabulary = model.supports(Feature::Vocabulary);
            let session = model
                .session()
                .map_err(|e| V2cError::Config(format!("cannot open TEA-ASR session: {e}")))?;
            Ok(Self {
                session: Mutex::new(session),
                vocabulary,
            })
        }

        /// Raw text of one chunk. Truncated or repeating output keeps its partial transcript.
        pub(super) fn decode(&self, samples: &[f32], ctx: &AsrContext) -> Result<String> {
            let options = RunOptions {
                language: Some(LANGUAGE.into()),
                vocabulary: if self.vocabulary {
                    ctx.vocabulary
                        .iter()
                        .map(|v| v.replace('\0', ""))
                        .filter(|v| !v.trim().is_empty())
                        .collect()
                } else {
                    Vec::new()
                },
                ..RunOptions::default()
            };
            let mut session = self.session.lock().unwrap_or_else(|p| p.into_inner());
            match session.run(samples, &options) {
                Ok(t) => Ok(t.text),
                Err(Error::OutputTruncated {
                    partial: Some(t), ..
                })
                | Err(Error::OutputRepetition {
                    partial: Some(t), ..
                }) => Ok(t.text),
                // The error text is not forwarded: it is not needed and stays free of
                // anything decoded.
                Err(Error::OutOfMemory(_)) => Err(provider_error(ProviderErrorKind::Server)),
                Err(_) => Err(provider_error(ProviderErrorKind::BadResponse)),
            }
        }
    }
}

#[cfg(not(feature = "tea"))]
mod engine {
    use super::*;

    /// Stand-in when the `tea` feature is off; it can never be constructed.
    pub(super) enum Engine {}

    impl Engine {
        pub(super) fn load(_: &Path, _: &TeaOptions) -> Result<Self> {
            Err(V2cError::Config(
                "TEA-ASR support not built in (enable the `tea` cargo feature)".into(),
            ))
        }

        pub(super) fn decode(&self, _: &[f32], _: &AsrContext) -> Result<String> {
            match *self {}
        }
    }
}

struct Inner {
    engine: engine::Engine,
    opts: TeaOptions,
}

impl Inner {
    fn run(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<TeaOutput> {
        check_clip(clip)?;
        let t = Instant::now();
        let ranges = chunk_ranges(&clip.samples, MAX_CHUNK_SECS, SPLIT_SEARCH_SECS);
        let mut parts = Vec::with_capacity(ranges.len());
        for r in &ranges {
            parts.push(clean_output(
                &self.engine.decode(&clip.samples[r.clone()], ctx)?,
            ));
        }
        let decode_ms = t.elapsed().as_millis() as u64;
        let text = join_chunks(&parts);
        let lang = tea_lang(&text);
        Ok(TeaOutput {
            transcript: Transcript { text, lang },
            decode_ms,
            chunks: ranges.len(),
        })
    }
}

/// TEA-ASR-1.1 provider (GPU). Load once and keep it resident; VRAM is freed when the last
/// clone is dropped.
#[derive(Clone)]
pub struct TeaAsr {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for TeaAsr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeaAsr")
            .field("opts", &self.inner.opts)
            .finish()
    }
}

impl TeaAsr {
    /// Loads the GGUF model file.
    pub fn load(model: &Path, opts: TeaOptions) -> Result<Self> {
        if !model.is_file() {
            return Err(V2cError::Config(format!(
                "TEA-ASR model not found: {}",
                model.display()
            )));
        }
        let engine = engine::Engine::load(model, &opts)?;
        Ok(Self {
            inner: Arc::new(Inner { engine, opts }),
        })
    }

    pub fn options(&self) -> &TeaOptions {
        &self.inner.opts
    }

    /// Runs recognition on the calling thread. Blocks for the whole inference.
    pub fn transcribe_blocking(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<TeaOutput> {
        self.inner.run(clip, ctx)
    }
}

#[async_trait]
impl AsrProvider for TeaAsr {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// GPU-bound: inside a Tokio runtime it runs on the blocking pool, otherwise inline.
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

    const RATE: usize = SAMPLE_RATE as usize;

    #[test]
    fn clean_output_strips_private_use_and_trims() {
        assert_eq!(clean_output(" 今天\u{E000}開會\u{F8FF} "), "今天開會");
        assert_eq!(clean_output("\u{E123}"), "");
        // Just outside the range is kept.
        assert_eq!(clean_output("a\u{D7FF}\u{F900}b"), "a\u{D7FF}\u{F900}b");
        assert_eq!(clean_output("hello world"), "hello world");
    }

    #[test]
    fn join_chunks_spaces_only_between_ascii_words() {
        assert_eq!(join_chunks(&["今天", "開會"]), "今天開會");
        assert_eq!(join_chunks(&["send the", "slides"]), "send the slides");
        assert_eq!(join_chunks(&["這個 meeting", "我們"]), "這個 meeting我們");
        assert_eq!(join_chunks(&["", " 好 ", ""]), "好");
        assert_eq!(join_chunks::<&str>(&[]), "");
    }

    #[test]
    fn short_clip_is_one_chunk() {
        let s = vec![0.1; 10 * RATE];
        assert_eq!(chunk_ranges(&s, 30, 5), vec![0..s.len()]);
        let exact = vec![0.1; 30 * RATE];
        assert_eq!(chunk_ranges(&exact, 30, 5), vec![0..exact.len()]);
        assert_eq!(chunk_ranges(&[], 30, 5), vec![0..0]);
    }

    #[test]
    fn long_clip_is_cut_at_the_quiet_spot_and_covers_everything() {
        // 70 s of "speech" with a silent 100 ms gap at 27.0 s and at 55.0 s.
        let mut s = vec![0.5f32; 70 * RATE];
        for gap in [27 * RATE, 55 * RATE] {
            s[gap..gap + RATE / 10].iter_mut().for_each(|x| *x = 0.0);
        }
        let r = chunk_ranges(&s, 30, 5);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].start, 0);
        assert_eq!(r.last().unwrap().end, s.len());
        for w in r.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
        for c in &r {
            assert!(c.len() <= 30 * RATE, "{c:?}");
        }
        // Cuts land inside the silent gaps.
        assert!(
            (27 * RATE..27 * RATE + RATE / 10).contains(&r[0].end),
            "{r:?}"
        );
        assert!(
            (55 * RATE..55 * RATE + RATE / 10).contains(&r[1].end),
            "{r:?}"
        );
    }

    #[test]
    fn uniform_audio_is_cut_at_the_limit() {
        let s = vec![0.2f32; 65 * RATE];
        let r = chunk_ranges(&s, 30, 5);
        assert!(r.iter().all(|c| c.len() <= 30 * RATE));
        assert_eq!(r.last().unwrap().end, s.len());
        // Ties prefer the latest frame, so the first chunk is close to 30 s.
        assert!(r[0].end > 29 * RATE);
    }

    #[test]
    fn lang_from_characters() {
        assert_eq!(tea_lang("今天下午三點開會"), Lang::Zh);
        assert_eq!(tea_lang("Please send me the slides"), Lang::En);
        assert_eq!(tea_lang("這個 meeting 我們 delay 一下"), Lang::Mixed);
        assert_eq!(tea_lang(""), Lang::Zh);
    }

    #[test]
    fn missing_model_is_config_error() {
        let err =
            TeaAsr::load(Path::new("definitely/not/here.gguf"), TeaOptions::default()).unwrap_err();
        assert!(matches!(err, V2cError::Config(_)));
    }

    #[cfg(not(feature = "tea"))]
    #[test]
    fn without_feature_load_reports_feature() {
        let dir = std::env::temp_dir().join(format!("v2c-tea-nofeature-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(MODEL_FILE);
        std::fs::write(&f, b"x").unwrap();
        let err = TeaAsr::load(&f, TeaOptions::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("`tea`"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_do_not_contain_text() {
        let e = provider_error(ProviderErrorKind::BadResponse).to_string();
        assert!(e.contains("tea_asr"));
        assert!(matches!(
            check_clip(&AudioClip::new(vec![])),
            Err(V2cError::Audio(_))
        ));
        let mut clip = AudioClip::new(vec![0.0; 10]);
        clip.sample_rate = 8000;
        assert!(matches!(check_clip(&clip), Err(V2cError::Audio(_))));
    }

    #[test]
    fn default_options_use_the_gpu() {
        assert!(TeaOptions::default().use_gpu);
    }
}
