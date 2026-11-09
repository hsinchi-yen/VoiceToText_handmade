//! `v2c-cli transcribe <wav> --asr breeze|sensevoice --model <path>`: recognise one WAV file
//! and print the text.
//!
//! Printing recognised text to stdout is intentional here: this is a developer tool run on the
//! developer's own recordings. Nothing is written to log files.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context};
use v2c_asr_local::{
    sensevoice, BreezeAsr, BreezeOptions, BreezeOutput, DecodeLang, LanguageMode, SenseVoiceAsr,
    SenseVoiceLanguage, SenseVoiceOptions, Transcript,
};
use v2c_core::{asr::AsrContext, audio::AudioClip, audio::SAMPLE_RATE};

#[derive(clap::Args)]
pub struct Args {
    /// 16 kHz mono WAV file.
    pub wav: PathBuf,
    /// ASR provider: breeze | sensevoice | <cloud id>.
    #[arg(long, default_value = "breeze")]
    pub asr: String,
    /// Model for local providers: the GGML file for breeze, the model directory for sensevoice.
    #[arg(long)]
    pub model: Option<PathBuf>,
    /// Vocabulary file (one entry per line) for the ASR initial prompt.
    #[arg(long)]
    pub vocab: Option<PathBuf>,
    #[command(flatten)]
    pub breeze: BreezeArgs,
}

/// Breeze tuning flags shared by `transcribe` and `bench`. SenseVoice uses `--lang`
/// (`detect`/`auto` = model auto-detect) and `--threads`; the other flags are Breeze-only.
#[derive(clap::Args, Clone)]
pub struct BreezeArgs {
    /// Decode language: `detect` (detect first; en -> en, else zh), `zh`, or `en`.
    #[arg(long, default_value = "detect")]
    pub lang: String,
    /// Run on CPU even when built with `--features cuda`.
    #[arg(long)]
    pub cpu: bool,
    /// Disable whisper.cpp flash attention.
    #[arg(long)]
    pub no_flash_attn: bool,
    /// Encoder context override (1..=1500; 1500 = 30 s window). Experimental speed knob.
    #[arg(long)]
    pub audio_ctx: Option<u32>,
    /// CPU threads for whisper.cpp (default: logical cores, max 8).
    #[arg(long)]
    pub threads: Option<usize>,
}

impl BreezeArgs {
    pub fn options(&self) -> anyhow::Result<BreezeOptions> {
        let mut o = BreezeOptions {
            use_gpu: !self.cpu,
            flash_attn: !self.no_flash_attn,
            language: parse_lang(&self.lang)?,
            audio_ctx: self.audio_ctx,
            ..BreezeOptions::default()
        };
        if let Some(t) = self.threads {
            o.n_threads = t.max(1);
        }
        Ok(o)
    }

    pub fn sensevoice_options(&self) -> anyhow::Result<SenseVoiceOptions> {
        let language = match parse_lang(&self.lang)? {
            LanguageMode::Detect => SenseVoiceLanguage::Auto,
            LanguageMode::Fixed(DecodeLang::Zh) => SenseVoiceLanguage::Zh,
            LanguageMode::Fixed(DecodeLang::En) => SenseVoiceLanguage::En,
        };
        let mut o = SenseVoiceOptions {
            language,
            ..SenseVoiceOptions::default()
        };
        if let Some(t) = self.threads {
            o.n_threads = t.max(1);
        }
        Ok(o)
    }
}

/// A loaded local ASR engine.
pub enum Engine {
    Breeze(BreezeAsr),
    SenseVoice(SenseVoiceAsr),
}

/// Result of one recognition with a timing/language summary (the summary holds no text).
pub struct EngineOutput {
    pub transcript: Transcript,
    pub total_ms: u64,
    pub summary: String,
}

impl Engine {
    pub fn run(&self, clip: &AudioClip, ctx: &AsrContext) -> v2c_core::Result<EngineOutput> {
        match self {
            Engine::Breeze(b) => b.transcribe_blocking(clip, ctx).map(|o| EngineOutput {
                summary: describe(&o),
                total_ms: o.total_ms(),
                transcript: o.transcript,
            }),
            Engine::SenseVoice(s) => s.transcribe_blocking(clip).map(|o| EngineOutput {
                summary: format!(
                    "asr {} ms (sensevoice, cpu), lang {:?}",
                    o.decode_ms, o.transcript.lang
                ),
                total_ms: o.decode_ms,
                transcript: o.transcript,
            }),
        }
    }

    pub fn is_sensevoice(&self) -> bool {
        matches!(self, Engine::SenseVoice(_))
    }
}

/// Loads the engine named by `--asr`.
pub fn load_engine(asr: &str, model: Option<&Path>, args: &BreezeArgs) -> anyhow::Result<Engine> {
    match asr {
        "sensevoice" => load_sensevoice(model, args).map(Engine::SenseVoice),
        _ => load_breeze(asr, model, args).map(Engine::Breeze),
    }
}

/// Loads SenseVoice from a sherpa-onnx model directory, printing load time.
pub fn load_sensevoice(model: Option<&Path>, args: &BreezeArgs) -> anyhow::Result<SenseVoiceAsr> {
    const HINT: &str = "download and extract \
        https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/\
        sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2";
    if !sensevoice::sensevoice_enabled() {
        bail!("this v2c-cli was built without SenseVoice; rebuild with `--features sensevoice`");
    }
    let Some(dir) = model else {
        bail!("--model <dir> is required for sensevoice (the directory with model.int8.onnx and tokens.txt; {HINT})");
    };
    if !dir.is_dir() {
        bail!("model directory not found: {} ({HINT})", dir.display());
    }
    let opts = args.sensevoice_options()?;
    let t = Instant::now();
    let sv = SenseVoiceAsr::load(dir, opts).context("loading SenseVoice model")?;
    eprintln!(
        "loaded SenseVoice from {} in {} ms (threads: {}, language: {}, itn: {})",
        dir.display(),
        t.elapsed().as_millis(),
        sv.options().n_threads,
        sv.options().language.code(),
        sv.options().use_itn,
    );
    Ok(sv)
}

fn parse_lang(s: &str) -> anyhow::Result<LanguageMode> {
    Ok(match s {
        "detect" | "auto" => LanguageMode::Detect,
        "zh" => LanguageMode::Fixed(DecodeLang::Zh),
        "en" => LanguageMode::Fixed(DecodeLang::En),
        other => bail!("unknown --lang `{other}` (expected detect | zh | en)"),
    })
}

/// Loads Breeze, printing load time and the whisper.cpp backend summary.
pub fn load_breeze(
    asr: &str,
    model: Option<&Path>,
    args: &BreezeArgs,
) -> anyhow::Result<BreezeAsr> {
    if asr != "breeze" {
        bail!("ASR provider `{asr}` is not available in the CLI yet (only `breeze`, `sensevoice`)");
    }
    let Some(model) = model else {
        bail!(
            "--model <path> is required for breeze, e.g. models/ggml-breeze-asr-25-q5_0.bin \
             (create it with tools/convert-breeze/convert-breeze.ps1)"
        );
    };
    if !model.is_file() {
        bail!(
            "model file not found: {} (create it with tools/convert-breeze/convert-breeze.ps1)",
            model.display()
        );
    }
    let opts = args.options()?;
    let cuda = v2c_asr_local::breeze::cuda_enabled();
    if !cuda && opts.use_gpu {
        eprintln!("note: built without `--features cuda`; running on CPU (slow).");
    }
    let t = Instant::now();
    let breeze = BreezeAsr::load(model, opts).context("loading Breeze model")?;
    eprintln!(
        "loaded {} in {} ms (cuda feature: {cuda}, gpu requested: {}, flash_attn: {})",
        model.display(),
        t.elapsed().as_millis(),
        breeze.options().use_gpu,
        breeze.options().flash_attn,
    );
    eprintln!(
        "whisper.cpp: {}",
        v2c_asr_local::breeze::system_info().trim()
    );
    Ok(breeze)
}

/// Reads a WAV as 16 kHz mono f32. Multi-channel audio is down-mixed by averaging;
/// other sample rates are rejected (convert with ffmpeg, see tests/fixtures/audio/README.md).
pub fn load_wav(path: &Path) -> anyhow::Result<AudioClip> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE {
        bail!(
            "{}: sample rate is {} Hz, need {SAMPLE_RATE} Hz. Convert with: \
             ffmpeg -i in.wav -ar 16000 -ac 1 -sample_fmt s16 out.wav",
            path.display(),
            spec.sample_rate
        );
    }
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .with_context(|| format!("reading {}", path.display()))?,
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            if bits == 0 || bits > 32 {
                bail!("{}: unsupported bit depth {bits}", path.display());
            }
            let scale = (1i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()
                .with_context(|| format!("reading {}", path.display()))?
        }
    };
    Ok(AudioClip::new(downmix(&interleaved, spec.channels)))
}

/// Averages interleaved channels into mono.
pub fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    if ch == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

/// One-line timing/language summary for an output (no text).
pub fn describe(out: &BreezeOutput) -> String {
    let detected = match out.detected {
        Some((code, p)) => format!("{code} p={p:.2}"),
        None => "skipped".into(),
    };
    format!(
        "asr {} ms (detect {} ms + decode {} ms), detected {detected}, decoded as {}, lang {:?}",
        out.total_ms(),
        out.lang_detect_ms,
        out.decode_ms,
        out.decode_lang.code(),
        out.transcript.lang
    )
}

/// ASR context for `--vocab <file>`: the vocabulary and `prompts::asr_initial_prompt` built
/// from it (Whisper limit). Without a file: the default context (providers use the base prompt).
/// Only the entry count is printed, never the entries.
pub fn asr_context(vocab: Option<&Path>) -> anyhow::Result<AsrContext> {
    let Some(path) = vocab else {
        return Ok(AsrContext::default());
    };
    let words = v2c_core::vocab::Vocab::load(path)
        .with_context(|| format!("reading vocabulary {}", path.display()))?
        .words;
    eprintln!(
        "vocabulary: {} entries from {}",
        words.len(),
        path.display()
    );
    let prompt =
        v2c_core::prompts::asr_initial_prompt(&words, v2c_core::prompts::WHISPER_PROMPT_MAX_CHARS);
    Ok(AsrContext {
        vocabulary: words,
        prompt,
    })
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let clip = load_wav(&args.wav)?;
    let engine = load_engine(&args.asr, args.model.as_deref(), &args.breeze)?;
    let ctx = asr_context(args.vocab.as_deref())?;
    let out = engine.run(&clip, &ctx).context("transcribing")?;
    eprintln!("audio {} ms, {}", clip.duration_ms(), out.summary);
    if engine.is_sensevoice() {
        eprintln!("note: SenseVoice output is Simplified; the app converts it with zh_tw.");
    }
    println!("{}", out.transcript.text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asr_context_from_vocab_file() {
        assert!(asr_context(None).unwrap().prompt.is_empty());
        let path = std::env::temp_dir().join(format!("v2c-cli-vocab-{}.txt", std::process::id()));
        std::fs::write(
            &path,
            "台積電
# comment
LM Studio
",
        )
        .unwrap();
        let ctx = asr_context(Some(&path)).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(ctx.vocabulary, ["台積電", "LM Studio"]);
        assert!(ctx.prompt.ends_with("詞彙：台積電、LM Studio。"));
    }

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix(&[0.2, 0.4, -1.0, 1.0], 2), vec![0.3, 0.0]);
        assert_eq!(downmix(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn parse_lang_values() {
        assert_eq!(parse_lang("detect").unwrap(), LanguageMode::Detect);
        assert_eq!(
            parse_lang("zh").unwrap(),
            LanguageMode::Fixed(DecodeLang::Zh)
        );
        assert_eq!(
            parse_lang("en").unwrap(),
            LanguageMode::Fixed(DecodeLang::En)
        );
        assert!(parse_lang("fr").is_err());
    }

    #[test]
    fn load_wav_reads_16k_stereo_and_rejects_other_rates() {
        let dir = std::env::temp_dir().join(format!("v2c-cli-wav-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ok = dir.join("stereo16k.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&ok, spec).unwrap();
        for _ in 0..1600 {
            w.write_sample(i16::MAX / 2).unwrap();
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let clip = load_wav(&ok).unwrap();
        assert_eq!(clip.samples.len(), 1600);
        assert_eq!(clip.duration_ms(), 100);
        assert!((clip.samples[0] - 0.25).abs() < 1e-3);

        let bad = dir.join("mono44k.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            ..spec
        };
        let mut w = hound::WavWriter::create(&bad, spec).unwrap();
        w.write_sample(0i16).unwrap();
        w.finalize().unwrap();
        let err = load_wav(&bad).unwrap_err().to_string();
        assert!(err.contains("44100"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_model_is_a_clear_error() {
        let args = BreezeArgs {
            lang: "detect".into(),
            cpu: true,
            no_flash_attn: false,
            audio_ctx: None,
            threads: None,
        };
        let err = load_breeze("breeze", Some(Path::new("no/such/model.bin")), &args)
            .unwrap_err()
            .to_string();
        assert!(err.contains("model file not found"), "{err}");
        let err = load_breeze("breeze", None, &args).unwrap_err().to_string();
        assert!(err.contains("--model"), "{err}");
        assert!(load_breeze("sensevoice", None, &args).is_err());
    }

    #[test]
    fn sensevoice_load_errors_are_clear() {
        let args = BreezeArgs {
            lang: "detect".into(),
            cpu: false,
            no_flash_attn: false,
            audio_ctx: None,
            threads: Some(2),
        };
        let err = load_engine("sensevoice", None, &args)
            .err()
            .unwrap()
            .to_string();
        if sensevoice::sensevoice_enabled() {
            assert!(err.contains("--model"), "{err}");
            let err = load_engine("sensevoice", Some(Path::new("no/such/dir")), &args)
                .err()
                .unwrap()
                .to_string();
            assert!(err.contains("not found"), "{err}");
        } else {
            assert!(err.contains("--features sensevoice"), "{err}");
        }
    }

    #[test]
    fn sensevoice_options_from_flags() {
        let mut args = BreezeArgs {
            lang: "auto".into(),
            cpu: false,
            no_flash_attn: false,
            audio_ctx: None,
            threads: Some(3),
        };
        let o = args.sensevoice_options().unwrap();
        assert_eq!(o.language, SenseVoiceLanguage::Auto);
        assert_eq!(o.n_threads, 3);
        assert!(o.use_itn);
        args.lang = "en".into();
        assert_eq!(
            args.sensevoice_options().unwrap().language,
            SenseVoiceLanguage::En
        );
        args.lang = "zh".into();
        assert_eq!(
            args.sensevoice_options().unwrap().language,
            SenseVoiceLanguage::Zh
        );
    }
}
