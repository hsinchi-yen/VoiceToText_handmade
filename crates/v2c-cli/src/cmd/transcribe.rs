//! `v2c-cli transcribe <wav> --model <path>`: recognise one WAV file and print the text.
//!
//! Printing recognised text to stdout is intentional here: this is a developer tool run on the
//! developer's own recordings. Nothing is written to log files.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context};
use v2c_asr_local::{BreezeAsr, BreezeOptions, BreezeOutput, DecodeLang, LanguageMode};
use v2c_core::{asr::AsrContext, audio::AudioClip, audio::SAMPLE_RATE};

#[derive(clap::Args)]
pub struct Args {
    /// 16 kHz mono WAV file.
    pub wav: PathBuf,
    /// ASR provider: breeze | sensevoice | <cloud id>.
    #[arg(long, default_value = "breeze")]
    pub asr: String,
    /// Model file for local providers.
    #[arg(long)]
    pub model: Option<PathBuf>,
    #[command(flatten)]
    pub breeze: BreezeArgs,
}

/// Breeze tuning flags shared by `transcribe` and `bench`.
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
        bail!("ASR provider `{asr}` is not available in the CLI yet (only `breeze`, task T0.1)");
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

pub fn run(args: Args) -> anyhow::Result<()> {
    let clip = load_wav(&args.wav)?;
    let breeze = load_breeze(&args.asr, args.model.as_deref(), &args.breeze)?;
    let out = breeze
        .transcribe_blocking(&clip, &AsrContext::default())
        .context("transcribing")?;
    eprintln!("audio {} ms, {}", clip.duration_ms(), describe(&out));
    println!("{}", out.transcript.text);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
