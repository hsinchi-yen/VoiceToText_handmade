//! Manual end-to-end smoke test: real Breeze/whisper model + WAV files through `Pipeline`.
//! No keystrokes are injected and no hook is installed.
//!
//! ```text
//! $env:V2C_SMOKE_MODEL = "C:\path\ggml-model.bin"
//! $env:V2C_SMOKE_WAV_DIR = "C:\path\wavs"     # 16 kHz mono WAVs
//! cargo test -p v2c-desktop --release smoke -- --ignored --nocapture
//! ```
//! Prints the recognised text to stdout (test-only; these are synthetic recordings).

use std::path::PathBuf;
use std::sync::Arc;

use v2c_asr_local::{BreezeAsr, BreezeOptions};
use v2c_core::asr::AsrContext;
use v2c_core::audio::AudioClip;
use v2c_core::config::Config;
use v2c_core::pipeline::Pipeline;
use v2c_core::Mode;

fn load_wav(path: &std::path::Path) -> AudioClip {
    let mut r = hound::WavReader::open(path).unwrap();
    let spec = r.spec();
    assert_eq!(spec.sample_rate, 16_000, "{}", path.display());
    let ch = spec.channels.max(1) as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(Result::unwrap).collect(),
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap() as f32 / scale)
                .collect()
        }
    };
    AudioClip::new(
        raw.chunks_exact(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect(),
    )
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs V2C_SMOKE_MODEL and V2C_SMOKE_WAV_DIR"]
async fn smoke_pipeline_on_wavs() {
    let model = PathBuf::from(std::env::var_os("V2C_SMOKE_MODEL").expect("V2C_SMOKE_MODEL"));
    let dir = PathBuf::from(std::env::var_os("V2C_SMOKE_WAV_DIR").expect("V2C_SMOKE_WAV_DIR"));
    let t = std::time::Instant::now();
    let breeze = BreezeAsr::load(&model, BreezeOptions::default()).unwrap();
    println!(
        "loaded in {} ms (cuda feature {})",
        t.elapsed().as_millis(),
        v2c_asr_local::breeze::cuda_enabled()
    );
    let t = std::time::Instant::now();
    let _ = breeze.transcribe_blocking(&AudioClip::new(vec![0.0; 16_000]), &AsrContext::default());
    println!("warm-up {} ms", t.elapsed().as_millis());

    let cfg = Config::with_defaults();
    let pipeline = Pipeline::new(
        Arc::new(breeze),
        None,
        crate::app_state::pipeline_config(&cfg),
    );
    let mut wavs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    wavs.sort();
    assert!(!wavs.is_empty(), "no WAVs in {}", dir.display());
    for wav in wavs {
        let clip = load_wav(&wav);
        let audio_ms = clip.duration_ms();
        let out = pipeline.run(clip, Mode::Dictate).await.unwrap();
        let name = wav.file_name().unwrap().to_string_lossy().to_string();
        match out {
            Some(o) => println!(
                "{name}: audio {audio_ms} ms, asr {} ms, post {} ms, total {} ms, lang {:?} => {}",
                o.timings.asr_ms, o.timings.postprocess_ms, o.timings.total_ms, o.lang, o.text
            ),
            None => println!("{name}: audio {audio_ms} ms => (nothing)"),
        }
    }
}
