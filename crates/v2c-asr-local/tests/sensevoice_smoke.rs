//! SenseVoice smoke test on a real model and WAV. Ignored by default; run with
//!
//! ```text
//! $env:V2C_SENSEVOICE_DIR = "$env:LOCALAPPDATA\v2c\models\sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17"
//! $env:V2C_SENSEVOICE_WAV = "tests\fixtures\audio\zh-01.wav"     # 16 kHz mono
//! cargo test -p v2c-asr-local --release --features sensevoice --no-run
//! Copy-Item <CARGO_TARGET_DIR>\release\*.dll <CARGO_TARGET_DIR>\release\deps\
//! cargo test -p v2c-asr-local --release --features sensevoice --test sensevoice_smoke -- --ignored --nocapture
//! ```
//!
//! The DLL copy matters on Windows: the sherpa-onnx build script puts `onnxruntime.dll` next to
//! binaries in `target/<profile>/`, but test executables live in `deps/`, and the loader would
//! otherwise pick Windows' older `System32\onnxruntime.dll` (load then fails with a clear error).
//!
//! Optional `V2C_SENSEVOICE_EXPECT`: a substring the output must contain (compared, never printed).
//! Only lengths, language, and timings are printed.
#![cfg(feature = "sensevoice")]

use std::path::PathBuf;
use std::time::Instant;

use v2c_asr_local::{AsrContext, AsrProvider, SenseVoiceAsr, SenseVoiceOptions};
use v2c_core::audio::{AudioClip, SAMPLE_RATE};

fn read_wav(path: &PathBuf) -> AudioClip {
    let mut r = hound::WavReader::open(path).expect("open wav");
    let spec = r.spec();
    assert_eq!(spec.sample_rate, SAMPLE_RATE, "WAV must be 16 kHz");
    let ch = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(Result::unwrap).collect(),
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.unwrap() as f32 / scale)
                .collect()
        }
    };
    let mono = samples
        .chunks_exact(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    AudioClip::new(mono)
}

#[test]
#[ignore = "needs V2C_SENSEVOICE_DIR and V2C_SENSEVOICE_WAV"]
fn sensevoice_transcribes_a_wav() {
    let dir = PathBuf::from(std::env::var("V2C_SENSEVOICE_DIR").expect("V2C_SENSEVOICE_DIR"));
    let wav = PathBuf::from(std::env::var("V2C_SENSEVOICE_WAV").expect("V2C_SENSEVOICE_WAV"));
    let clip = read_wav(&wav);

    let t = Instant::now();
    let asr = SenseVoiceAsr::load(&dir, SenseVoiceOptions::default()).expect("load");
    eprintln!("load {} ms", t.elapsed().as_millis());

    let out = asr.transcribe_blocking(&clip).expect("transcribe");
    eprintln!(
        "audio {} ms, decode {} ms, {} chars, lang {:?}",
        clip.duration_ms(),
        out.decode_ms,
        out.transcript.text.chars().count(),
        out.transcript.lang
    );
    assert!(!out.transcript.text.is_empty());
    if let Ok(expect) = std::env::var("V2C_SENSEVOICE_EXPECT") {
        assert!(
            out.transcript.text.contains(&expect),
            "output does not contain V2C_SENSEVOICE_EXPECT"
        );
    }

    // The async trait path gives the same transcript.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t2 = rt
        .block_on(asr.transcribe(&clip, &AsrContext::default()))
        .expect("async transcribe");
    assert_eq!(t2, out.transcript);
}
