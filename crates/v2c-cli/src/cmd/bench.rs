//! `v2c-cli bench --asr breeze|sensevoice --model <path> <dir>`: run every fixture and report the recognised
//! text, expected text, CER, ASR time, and audio duration.
//!
//! Fixtures are `<name>.txt` files; the matching `<name>.wav` is the recording (git-ignored).
//! When `<name>.raw.txt` exists it is the verbatim script and is used for CER instead of
//! `<name>.txt` (which holds the polished target). Missing WAVs are skipped, not errors.
//!
//! Printing text to stdout is intentional (developer's own recordings); nothing is logged.

use std::path::{Path, PathBuf};

use anyhow::Context;
use v2c_core::{asr::AsrContext, audio::AudioClip};

use super::transcribe::{load_engine, load_wav, BreezeArgs, EngineOutput};

#[derive(clap::Args)]
pub struct Args {
    /// Directory with `<name>.wav` + `<name>.txt` pairs.
    pub dir: PathBuf,
    /// ASR provider: breeze | sensevoice | <cloud id>.
    #[arg(long, default_value = "breeze")]
    pub asr: String,
    /// Optional LLM for polish: lmstudio | <cloud id>.
    #[arg(long)]
    pub llm: Option<String>,
    /// Model file for local providers.
    #[arg(long)]
    pub model: Option<PathBuf>,
    /// Run the whole fixture set this many times (keeps the model loaded longer, for watching
    /// VRAM with nvidia-smi and checking for display stutter).
    #[arg(long, default_value_t = 1)]
    pub repeat: u32,
    #[command(flatten)]
    pub breeze: BreezeArgs,
}

/// One fixture: a recording and the text it should produce.
#[derive(Debug, Clone, PartialEq)]
pub struct Fixture {
    pub name: String,
    pub wav: PathBuf,
    /// File the expected text was read from (`.raw.txt` preferred over `.txt`).
    pub expected_file: PathBuf,
}

/// Lists fixtures in `dir`, sorted by name. A fixture exists for every `<name>.txt` that is not
/// itself a `.raw.txt`.
pub fn find_fixtures(dir: &Path) -> anyhow::Result<Vec<Fixture>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        let Some(name) = file.strip_suffix(".txt") else {
            continue;
        };
        if name.ends_with(".raw") {
            continue;
        }
        let raw = dir.join(format!("{name}.raw.txt"));
        out.push(Fixture {
            name: name.to_string(),
            wav: dir.join(format!("{name}.wav")),
            expected_file: if raw.is_file() { raw } else { path.clone() },
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Text used for CER: full-width ASCII folded to half-width, ASCII lower-cased, and every
/// character that is not a letter or digit (whitespace, punctuation, symbols) removed.
/// CJK ideographs count as letters, so each Chinese character and each English letter is one unit.
pub fn normalize_for_cer(s: &str) -> Vec<char> {
    s.chars()
        .map(|c| match c as u32 {
            // Full-width forms U+FF01..U+FF5E -> ASCII U+0021..U+007E.
            0xFF01..=0xFF5E => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            0x3000 => ' ',
            _ => c,
        })
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Levenshtein distance over chars.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Character error rate = edit distance(hyp, ref) / len(ref), both after [`normalize_for_cer`].
/// An empty reference gives 0.0 for an empty hypothesis and 1.0 otherwise.
pub fn cer(reference: &str, hypothesis: &str) -> f64 {
    let r = normalize_for_cer(reference);
    let h = normalize_for_cer(hypothesis);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&r, &h) as f64 / r.len() as f64
}

struct Row {
    name: String,
    audio_ms: u64,
    asr_ms: u64,
    cer: f64,
    speed_ok: bool,
    simplified: bool,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    if let Some(llm) = &args.llm {
        anyhow::bail!("--llm {llm}: polish in bench is not implemented yet (task T1.5)");
    }
    let fixtures = find_fixtures(&args.dir)?;
    if fixtures.is_empty() {
        anyhow::bail!("no <name>.txt fixtures found in {}", args.dir.display());
    }
    let (present, missing): (Vec<_>, Vec<_>) = fixtures.into_iter().partition(|f| f.wav.is_file());
    for f in &missing {
        println!(
            "SKIP {}: {} not found (record it, see tests/fixtures/audio/README.md)",
            f.name,
            f.wav.display()
        );
    }
    if present.is_empty() {
        println!(
            "\nNo WAV recordings found in {}; nothing to measure. Skipped {} fixture(s).",
            args.dir.display(),
            missing.len()
        );
        return Ok(());
    }

    let engine = load_engine(&args.asr, args.model.as_deref(), &args.breeze)?;
    let ctx = AsrContext::default();
    if engine.is_sensevoice() {
        println!(
            "note: SenseVoice outputs Simplified Chinese; the app converts it with zh_tw, this \
             bench does not, so the simplified-output check is expected to fail for zh fixtures."
        );
    }

    // Warm-up so CUDA kernel loading and buffer allocation are not counted in the first fixture.
    let warm = engine.run(&AudioClip::new(vec![0.0; 16_000]), &ctx);
    match warm {
        Ok(o) => eprintln!("warm-up: {} ms", o.total_ms),
        Err(e) => eprintln!("warm-up failed: {e}"),
    }

    let mut rows = Vec::new();
    for round in 1..=args.repeat.max(1) {
        if args.repeat > 1 {
            println!(
                "
## round {round}/{}",
                args.repeat
            );
        }
        for f in &present {
            let expected = std::fs::read_to_string(&f.expected_file)
                .with_context(|| format!("reading {}", f.expected_file.display()))?;
            let expected = expected.trim();
            let clip = match load_wav(&f.wav) {
                Ok(c) => c,
                Err(e) => {
                    println!("SKIP {}: {e:#}", f.name);
                    continue;
                }
            };
            let out: EngineOutput = match engine.run(&clip, &ctx) {
                Ok(o) => o,
                Err(e) => {
                    println!("FAIL {}: {e}", f.name);
                    continue;
                }
            };
            let c = cer(expected, &out.transcript.text);
            println!("== {} ({} ms audio)", f.name, clip.duration_ms());
            println!("  got:      {}", out.transcript.text);
            println!("  expected: {expected}");
            println!("  CER {:.1}% | {}", c * 100.0, out.summary);
            let speed_ok = within_latency_target(out.total_ms, clip.duration_ms());
            let simplified = has_simplified(&out.transcript.text);
            println!(
                "  {} speed ({} ms per 10 s of audio, target <= {TARGET_MS_PER_10S}){}",
                if speed_ok { "PASS" } else { "FAIL" },
                per_10s(out.total_ms, clip.duration_ms()),
                if simplified {
                    " | FAIL: output contains simplified characters"
                } else {
                    ""
                }
            );
            rows.push(Row {
                name: f.name.clone(),
                audio_ms: clip.duration_ms(),
                asr_ms: out.total_ms,
                cer: c,
                speed_ok,
                simplified,
            });
        }
    }

    print_summary(&rows, missing.len());
    Ok(())
}

fn print_summary(rows: &[Row], skipped: usize) {
    println!("\n== Summary");
    println!(
        "{:<12} {:>9} {:>8} {:>7}",
        "fixture", "audio ms", "asr ms", "CER"
    );
    for r in rows {
        println!(
            "{:<12} {:>9} {:>8} {:>6.1}%",
            r.name,
            r.audio_ms,
            r.asr_ms,
            r.cer * 100.0
        );
    }
    if rows.is_empty() {
        println!("no fixtures measured; skipped {skipped}");
        return;
    }
    let n = rows.len() as f64;
    let mean_cer = rows.iter().map(|r| r.cer).sum::<f64>() / n;
    let mean_asr = rows.iter().map(|r| r.asr_ms as f64).sum::<f64>() / n;
    let max = rows
        .iter()
        .max_by_key(|r| r.asr_ms)
        .map(|r| r.asr_ms)
        .unwrap_or(0);
    let audio: u64 = rows.iter().map(|r| r.audio_ms).sum();
    let asr: u64 = rows.iter().map(|r| r.asr_ms).sum();
    println!(
        "measured {}, skipped {skipped}; mean CER {:.1}%; ASR mean {:.0} ms, max {max} ms; \
         real-time factor {:.3}",
        rows.len(),
        mean_cer * 100.0,
        mean_asr,
        if audio == 0 {
            0.0
        } else {
            asr as f64 / audio as f64
        }
    );
    let slow = rows.iter().filter(|r| !r.speed_ok).count();
    let simp = rows.iter().filter(|r| r.simplified).count();
    println!("target: a 10 s clip in <= {TARGET_MS_PER_10S} ms of ASR time");
    println!(
        "verdict: {} (speed failures {slow}, simplified-output failures {simp})",
        if slow == 0 && simp == 0 {
            "PASS"
        } else {
            "FAIL"
        }
    );
}

/// Acceptance target from the T0.1 card: 10 s of audio in at most 1.2 s of ASR.
const TARGET_MS_PER_10S: u64 = 1200;

/// ASR time scaled to 10 s of audio. Short clips scale up their fixed overhead, so this
/// errs on the strict side.
fn per_10s(asr_ms: u64, audio_ms: u64) -> u64 {
    asr_ms * 10_000 / audio_ms.max(1)
}

fn within_latency_target(asr_ms: u64, audio_ms: u64) -> bool {
    per_10s(asr_ms, audio_ms) <= TARGET_MS_PER_10S
}

/// Common characters that only exist in Simplified Chinese (their Traditional forms differ).
/// A heuristic, not a converter: any hit means the zh-TW requirement failed.
const SIMPLIFIED_ONLY: &str =
    "这们说个会时来对开发过还关头问见长车东门实现应该电话国学经机么吗让认识请谢给钟点读书";

fn has_simplified(text: &str) -> bool {
    text.chars().any(|c| SIMPLIFIED_ONLY.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cer_identical_is_zero_and_ignores_punctuation_and_spaces() {
        assert_eq!(cer("今天下午三點要開會。", "今天下午三點要開會"), 0.0);
        assert_eq!(
            cer(
                "這個 meeting 我們 delay 一下，改到下禮拜。",
                "這個meeting我們Delay一下, 改到下禮拜"
            ),
            0.0
        );
    }

    #[test]
    fn cer_counts_substitution_insertion_deletion() {
        // One substitution out of 4.
        assert!((cer("今天開會", "明天開會") - 0.25).abs() < 1e-9);
        // One deletion out of 4.
        assert!((cer("今天開會", "今天會") - 0.25).abs() < 1e-9);
        // Two insertions over 4.
        assert!((cer("今天開會", "今天要去開會") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn cer_simplified_output_is_penalised() {
        // 開會 vs 开会: two of four characters differ.
        assert!((cer("今天開會", "今天开会") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn latency_target_scales_to_ten_seconds() {
        assert!(within_latency_target(1200, 10_000));
        assert!(!within_latency_target(1201, 10_000));
        assert!(within_latency_target(600, 5_000));
        assert!(!within_latency_target(700, 5_000));
        assert_eq!(per_10s(100, 0), 1_000_000);
    }

    #[test]
    fn detects_simplified_output() {
        assert!(has_simplified("这个 meeting 我们 delay 一下"));
        assert!(!has_simplified(
            "這個 meeting 我們 delay 一下，改到下禮拜。"
        ));
        assert!(!has_simplified("Hello world"));
    }

    #[test]
    fn cer_empty_cases() {
        assert_eq!(cer("", ""), 0.0);
        assert_eq!(cer("。", ""), 0.0);
        assert_eq!(cer("", "abc"), 1.0);
        assert_eq!(cer("abc", ""), 1.0);
    }

    #[test]
    fn normalize_folds_full_width_and_case() {
        assert_eq!(normalize_for_cer("ＱＡ　Ok！"), vec!['q', 'a', 'o', 'k']);
    }

    #[test]
    fn finds_fixtures_prefers_raw_and_marks_missing_wavs() {
        let dir = std::env::temp_dir().join(format!("v2c-bench-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("zh-01.txt"), "今天").unwrap();
        std::fs::write(dir.join("filler-01.txt"), "後天見").unwrap();
        std::fs::write(dir.join("filler-01.raw.txt"), "嗯後天見").unwrap();
        std::fs::write(dir.join("README.md"), "x").unwrap();
        std::fs::write(dir.join("zh-01.wav"), b"").unwrap();

        let fx = find_fixtures(&dir).unwrap();
        let names: Vec<_> = fx.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["filler-01", "zh-01"]);
        assert!(fx[0].expected_file.ends_with("filler-01.raw.txt"));
        assert!(!fx[0].wav.is_file());
        assert!(fx[1].expected_file.ends_with("zh-01.txt"));
        assert!(fx[1].wav.is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bench_without_wavs_skips_and_succeeds_without_a_model() {
        let dir = std::env::temp_dir().join(format!("v2c-bench-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("en-01.txt"), "Hello.").unwrap();
        let args = Args {
            dir: dir.clone(),
            asr: "breeze".into(),
            llm: None,
            model: None,
            repeat: 1,
            breeze: BreezeArgs {
                lang: "detect".into(),
                cpu: true,
                no_flash_attn: false,
                audio_ctx: None,
                threads: None,
            },
        };
        run(args).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
