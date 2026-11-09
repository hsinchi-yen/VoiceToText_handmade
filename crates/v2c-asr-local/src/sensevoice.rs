//! SenseVoice-Small on CPU through the official sherpa-onnx Rust API (crate `sherpa-onnx`).
//!
//! This is the CPU fallback when Breeze cannot use the GPU (`tasks/plan.md` section 5).
//! Language is auto-detected by the model (it knows zh/en/yue/ja/ko); the reported `Lang` is
//! derived from the character classes of the output, as in [`crate::breeze`]. Inverse text
//! normalisation (`use_itn`) is on, which also adds punctuation. Chinese output is Simplified;
//! the pipeline's `zh_tw` step converts it, so nothing is converted here.
//!
//! SenseVoice has no initial prompt, so `AsrContext` (vocabulary, prompt) is not used.
//!
//! Cargo feature `sensevoice` (off by default) links sherpa-onnx. Without it this module still
//! compiles: [`SenseVoiceAsr::load`] returns a `Config` error, so callers need no `cfg`.
//!
//! Privacy: nothing here logs or embeds recognised text in errors; sherpa-onnx debug output
//! (which can print results) stays disabled.

use std::path::{Path, PathBuf};
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

/// Provider name used in logs, settings, and errors.
pub const PROVIDER_NAME: &str = "sensevoice";

/// Quantised model file name in the sherpa-onnx SenseVoice package (preferred).
pub const MODEL_INT8_FILE: &str = "model.int8.onnx";
/// Float32 model file name (used only when the int8 file is absent).
pub const MODEL_F32_FILE: &str = "model.onnx";
/// Token table in the sherpa-onnx SenseVoice package.
pub const TOKENS_FILE: &str = "tokens.txt";

/// True when this binary was built with the `sensevoice` feature.
pub const fn sensevoice_enabled() -> bool {
    cfg!(feature = "sensevoice")
}

/// Language hint passed to SenseVoice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SenseVoiceLanguage {
    /// Let the model detect the language (default).
    #[default]
    Auto,
    Zh,
    En,
}

impl SenseVoiceLanguage {
    /// sherpa-onnx `language` value.
    pub fn code(self) -> &'static str {
        match self {
            SenseVoiceLanguage::Auto => "auto",
            SenseVoiceLanguage::Zh => "zh",
            SenseVoiceLanguage::En => "en",
        }
    }
}

/// Load-time settings.
#[derive(Debug, Clone, PartialEq)]
pub struct SenseVoiceOptions {
    /// onnxruntime intra-op threads.
    pub n_threads: usize,
    pub language: SenseVoiceLanguage,
    /// Inverse text normalisation: digits and punctuation in the output.
    pub use_itn: bool,
}

impl Default for SenseVoiceOptions {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            n_threads: cores.clamp(1, 8),
            language: SenseVoiceLanguage::Auto,
            use_itn: true,
        }
    }
}

/// The two files a SenseVoice model directory must contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenseVoiceModelFiles {
    pub model: PathBuf,
    pub tokens: PathBuf,
}

/// Finds the model files in a sherpa-onnx SenseVoice directory, e.g.
/// `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/`. Prefers `model.int8.onnx`.
pub fn model_files(dir: &Path) -> Result<SenseVoiceModelFiles> {
    if !dir.is_dir() {
        return Err(V2cError::Config(format!(
            "SenseVoice model directory not found: {}",
            dir.display()
        )));
    }
    let model = [MODEL_INT8_FILE, MODEL_F32_FILE]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            V2cError::Config(format!(
                "no {MODEL_INT8_FILE} or {MODEL_F32_FILE} in {}",
                dir.display()
            ))
        })?;
    let tokens = dir.join(TOKENS_FILE);
    if !tokens.is_file() {
        return Err(V2cError::Config(format!(
            "no {TOKENS_FILE} in {}",
            dir.display()
        )));
    }
    Ok(SenseVoiceModelFiles { model, tokens })
}

/// Removes any `<|...|>` tags (language, emotion, event) the decoder may leave and trims.
pub fn clean_output(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<|") {
        match rest[start + 2..].find("|>") {
            Some(end) => {
                out.push_str(&rest[..start]);
                rest = &rest[start + 2 + end + 2..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// `Lang` of a SenseVoice transcript from its character classes; no letters at all -> `Zh`.
pub fn sensevoice_lang(text: &str) -> Lang {
    transcript_lang(DecodeLang::Zh, text)
}

/// Detailed result of one recognition, used by `v2c-cli bench`.
#[derive(Debug, Clone, PartialEq)]
pub struct SenseVoiceOutput {
    /// `Debug` of `Transcript` redacts the text.
    pub transcript: Transcript,
    pub decode_ms: u64,
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

#[cfg(feature = "sensevoice")]
mod engine {
    use super::*;
    use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig};

    pub(super) struct Engine {
        recognizer: OfflineRecognizer,
    }

    /// onnxruntime C API version that sherpa-onnx 2.0.1 requests (it bundles onnxruntime 1.28.2).
    /// Bump together with the `sherpa-onnx` version.
    pub(super) const REQUIRED_ORT_API_VERSION: u32 = 28;

    /// `OrtApiBase` from `onnxruntime_c_api.h`.
    #[repr(C)]
    struct OrtApiBase {
        get_api: unsafe extern "system" fn(version: u32) -> *const std::ffi::c_void,
        get_version_string: unsafe extern "system" fn() -> *const std::ffi::c_char,
    }

    // Exported by onnxruntime.dll / libonnxruntime, which sherpa-onnx-sys already links.
    extern "system" {
        fn OrtGetApiBase() -> *const OrtApiBase;
    }

    /// Checks that the onnxruntime the process actually loaded is new enough.
    ///
    /// Windows 11 ships an older `C:\Windows\System32\onnxruntime.dll` (1.17), and the DLL
    /// loader searches System32 before `PATH`. If the bundled `onnxruntime.dll` is not next to
    /// the executable, that one gets loaded and sherpa-onnx crashes with an access violation
    /// when it asks for API version 28. Checking first turns the crash into an error.
    pub(super) fn check_onnxruntime() -> Result<()> {
        // SAFETY: `OrtGetApiBase` has no preconditions and returns a pointer to a static struct
        // (or null). `GetApi` returns null for unsupported versions; `GetVersionString` returns
        // a static NUL-terminated string.
        unsafe {
            let base = OrtGetApiBase();
            if base.is_null() {
                return Err(V2cError::Config("onnxruntime: OrtGetApiBase failed".into()));
            }
            if !((*base).get_api)(REQUIRED_ORT_API_VERSION).is_null() {
                return Ok(());
            }
            let v = ((*base).get_version_string)();
            let version = if v.is_null() {
                "unknown".into()
            } else {
                std::ffi::CStr::from_ptr(v).to_string_lossy().into_owned()
            };
            Err(V2cError::Config(format!(
                "loaded onnxruntime {version} is too old (need C API {REQUIRED_ORT_API_VERSION}); \
                 put the bundled onnxruntime.dll next to the executable"
            )))
        }
    }

    impl Engine {
        pub(super) fn load(files: &SenseVoiceModelFiles, opts: &SenseVoiceOptions) -> Result<Self> {
            check_onnxruntime()?;
            let mut config = OfflineRecognizerConfig::default();
            config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(files.model.to_string_lossy().into_owned()),
                language: Some(opts.language.code().into()),
                use_itn: opts.use_itn,
            };
            config.model_config.tokens = Some(files.tokens.to_string_lossy().into_owned());
            config.model_config.num_threads = opts.n_threads.clamp(1, 64) as i32;
            config.model_config.provider = Some("cpu".into());
            // Debug output can include recognition results; keep it off.
            config.model_config.debug = false;
            let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
                V2cError::Config(format!(
                    "cannot load SenseVoice model {}",
                    files.model.display()
                ))
            })?;
            Ok(Self { recognizer })
        }

        /// Raw decoder text. A fresh stream per call; the recognizer is thread-safe.
        pub(super) fn decode(&self, samples: &[f32]) -> Result<String> {
            let stream = self.recognizer.create_stream();
            stream.accept_waveform(SAMPLE_RATE as i32, samples);
            self.recognizer.decode(&stream);
            stream
                .get_result()
                .map(|r| r.text)
                .ok_or_else(|| provider_error(ProviderErrorKind::BadResponse))
        }
    }
}

#[cfg(not(feature = "sensevoice"))]
mod engine {
    use super::*;

    /// Stand-in when the `sensevoice` feature is off; it can never be constructed.
    pub(super) enum Engine {}

    impl Engine {
        pub(super) fn load(_: &SenseVoiceModelFiles, _: &SenseVoiceOptions) -> Result<Self> {
            Err(V2cError::Config(
                "SenseVoice support not built in (enable the `sensevoice` cargo feature)".into(),
            ))
        }

        pub(super) fn decode(&self, _: &[f32]) -> Result<String> {
            match *self {}
        }
    }
}

struct Inner {
    engine: engine::Engine,
    opts: SenseVoiceOptions,
}

impl Inner {
    fn run(&self, clip: &AudioClip) -> Result<SenseVoiceOutput> {
        check_clip(clip)?;
        let t = Instant::now();
        let raw = self.engine.decode(&clip.samples)?;
        let decode_ms = t.elapsed().as_millis() as u64;
        let text = clean_output(&raw);
        let lang = sensevoice_lang(&text);
        Ok(SenseVoiceOutput {
            transcript: Transcript { text, lang },
            decode_ms,
        })
    }
}

/// SenseVoice-Small provider (CPU). Load once and keep it resident (about 230 MB of RAM).
#[derive(Clone)]
pub struct SenseVoiceAsr {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SenseVoiceAsr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SenseVoiceAsr")
            .field("opts", &self.inner.opts)
            .finish()
    }
}

impl SenseVoiceAsr {
    /// Loads the model from a sherpa-onnx SenseVoice directory (see [`model_files`]).
    pub fn load(model_dir: &Path, opts: SenseVoiceOptions) -> Result<Self> {
        let files = model_files(model_dir)?;
        let engine = engine::Engine::load(&files, &opts)?;
        Ok(Self {
            inner: Arc::new(Inner { engine, opts }),
        })
    }

    pub fn options(&self) -> &SenseVoiceOptions {
        &self.inner.opts
    }

    /// Runs recognition on the calling thread. Blocks for the whole inference.
    pub fn transcribe_blocking(&self, clip: &AudioClip) -> Result<SenseVoiceOutput> {
        self.inner.run(clip)
    }
}

#[async_trait]
impl AsrProvider for SenseVoiceAsr {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// CPU-bound: inside a Tokio runtime it runs on the blocking pool, otherwise inline.
    /// `ctx` is ignored (SenseVoice takes no prompt).
    async fn transcribe(&self, clip: &AudioClip, _ctx: &AsrContext) -> Result<Transcript> {
        if tokio::runtime::Handle::try_current().is_ok() {
            let inner = Arc::clone(&self.inner);
            let clip = clip.clone();
            tokio::task::spawn_blocking(move || inner.run(&clip))
                .await
                .map_err(|_| provider_error(ProviderErrorKind::Server))?
                .map(|o| o.transcript)
        } else {
            self.inner.run(clip).map(|o| o.transcript)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("v2c-sv-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn clean_output_strips_tags_and_trims() {
        assert_eq!(
            clean_output("<|zh|><|NEUTRAL|><|Speech|><|withitn|>开会。 "),
            "开会。"
        );
        assert_eq!(clean_output("  hello <|en|>world "), "hello world");
        assert_eq!(clean_output("a <|unterminated"), "a <|unterminated");
        assert_eq!(clean_output(""), "");
    }

    #[test]
    fn lang_from_characters() {
        assert_eq!(sensevoice_lang("今天下午三点开会。"), Lang::Zh);
        assert_eq!(sensevoice_lang("Please send me the slides."), Lang::En);
        assert_eq!(sensevoice_lang("这个 meeting 我们 delay 一下"), Lang::Mixed);
        assert_eq!(sensevoice_lang(""), Lang::Zh);
    }

    #[test]
    fn language_codes() {
        assert_eq!(SenseVoiceLanguage::default().code(), "auto");
        assert_eq!(SenseVoiceLanguage::Zh.code(), "zh");
        assert_eq!(SenseVoiceLanguage::En.code(), "en");
    }

    #[test]
    fn default_options() {
        let o = SenseVoiceOptions::default();
        assert!(o.use_itn);
        assert_eq!(o.language, SenseVoiceLanguage::Auto);
        assert!(o.n_threads >= 1 && o.n_threads <= 8);
    }

    #[test]
    fn model_files_prefers_int8_and_requires_tokens() {
        let d = temp_dir("files");
        assert!(matches!(model_files(&d), Err(V2cError::Config(_))));
        std::fs::write(d.join(MODEL_F32_FILE), b"x").unwrap();
        assert!(matches!(model_files(&d), Err(V2cError::Config(_)))); // no tokens
        std::fs::write(d.join(TOKENS_FILE), b"x").unwrap();
        assert_eq!(model_files(&d).unwrap().model, d.join(MODEL_F32_FILE));
        std::fs::write(d.join(MODEL_INT8_FILE), b"x").unwrap();
        let f = model_files(&d).unwrap();
        assert_eq!(f.model, d.join(MODEL_INT8_FILE));
        assert_eq!(f.tokens, d.join(TOKENS_FILE));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_dir_is_config_error() {
        let err = SenseVoiceAsr::load(
            Path::new("definitely/not/here/sensevoice"),
            SenseVoiceOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, V2cError::Config(_)));
    }

    #[cfg(not(feature = "sensevoice"))]
    #[test]
    fn without_feature_load_reports_feature() {
        let d = temp_dir("nofeature");
        std::fs::write(d.join(MODEL_INT8_FILE), b"x").unwrap();
        std::fs::write(d.join(TOKENS_FILE), b"x").unwrap();
        let err = SenseVoiceAsr::load(&d, SenseVoiceOptions::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("sensevoice"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn errors_do_not_contain_text() {
        let e = provider_error(ProviderErrorKind::BadResponse).to_string();
        assert!(e.contains("sensevoice"));
        let e = check_clip(&AudioClip::new(vec![])).unwrap_err();
        assert!(matches!(e, V2cError::Audio(_)));
        let mut clip = AudioClip::new(vec![0.0; 10]);
        clip.sample_rate = 8000;
        assert!(matches!(check_clip(&clip), Err(V2cError::Audio(_))));
    }
}
