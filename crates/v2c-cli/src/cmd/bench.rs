//! `v2c-cli bench --asr breeze|sensevoice --model <path> <dir>`: run every fixture and report the recognised
//! text, expected text, CER, ASR time, and audio duration.
//!
//! Fixtures are `<name>.txt` files; the matching `<name>.wav` is the recording (git-ignored).
//! When `<name>.raw.txt` exists it is the verbatim script and is used for CER instead of
//! `<name>.txt` (which holds the polished target). Missing WAVs are skipped, not errors.
//!
//! With `--llm lmstudio` every transcript is also polished through LM Studio's OpenAI-compatible
//! API (the same prompt the app uses), compared with `<name>.txt`, and timed against the
//! "ASR + polish <= 2.5 s per 10 s of audio" budget. Fixtures without a recording but with a
//! `<name>.raw.txt` get a text-only polish check of the raw script. If LM Studio is not
//! reachable, polish is skipped with a message and the ASR bench still runs.
//!
//! Printing text to stdout is intentional (developer's own recordings); nothing is logged.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Context;
use v2c_core::{
    audio::AudioClip,
    config::LocalConfig,
    llm::{LlmProvider, OpenAiCompatLlm},
    postprocess::{fillers::strip_fillers, zh_tw::to_zh_tw},
    prompts,
};

use super::transcribe::{asr_context, load_engine, load_wav, BreezeArgs, EngineOutput};

/// Request timeout for bench polish. Generous on purpose so slow answers are still shown and
/// judged against the budget; the app itself gives up after `polish.timeout_ms` (1000 ms).
const BENCH_LLM_TIMEOUT_MS: u64 = 10_000;

#[derive(clap::Args)]
pub struct Args {
    /// Directory with `<name>.wav` + `<name>.txt` pairs.
    pub dir: PathBuf,
    /// ASR provider: breeze | sensevoice | <cloud id>.
    #[arg(long, default_value = "breeze")]
    pub asr: String,
    /// Optional LLM for polish: lmstudio.
    #[arg(long)]
    pub llm: Option<String>,
    /// OpenAI-compatible base URL for `--llm lmstudio` (default: `[local] llm_base_url`).
    #[arg(long, default_value_t = LocalConfig::default().llm_base_url)]
    pub llm_url: String,
    /// Model id as shown by LM Studio; omitted = whatever model is loaded.
    #[arg(long)]
    pub llm_model: Option<String>,
    /// Per-request polish timeout in ms.
    #[arg(long, default_value_t = BENCH_LLM_TIMEOUT_MS)]
    pub llm_timeout_ms: u64,
    /// Model file for local providers.
    #[arg(long)]
    pub model: Option<PathBuf>,
    /// Run the whole fixture set this many times (keeps the model loaded longer, for watching
    /// VRAM with nvidia-smi and checking for display stutter).
    #[arg(long, default_value_t = 1)]
    pub repeat: u32,
    /// Vocabulary file (one entry per line) for the ASR initial prompt; compare CER with and
    /// without it.
    #[arg(long)]
    pub vocab: Option<PathBuf>,
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
    polish: Option<PolishRow>,
}

/// Polish result for one fixture.
struct PolishRow {
    llm_ms: u64,
    /// CER of the polished text against `<name>.txt`; `None` when polish failed.
    cer: Option<f64>,
    /// Polish succeeded and ASR + polish fit the budget (always true for text-only checks
    /// that succeeded, which have no audio to scale against).
    ok: bool,
}

/// LM Studio (or another OpenAI-compatible server) used to polish bench transcripts.
struct Polisher {
    llm: OpenAiCompatLlm,
    rt: tokio::runtime::Runtime,
    system: String,
}

impl Polisher {
    /// `Ok(None)` when no `--llm` was given or the server is not reachable (with a message).
    fn connect(args: &Args) -> anyhow::Result<Option<Self>> {
        let Some(kind) = args.llm.as_deref() else {
            return Ok(None);
        };
        if kind != "lmstudio" {
            anyhow::bail!(
                "--llm {kind}: bench supports only `lmstudio` (cloud providers come with T1.7)"
            );
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("starting the async runtime")?;
        let client = reqwest::Client::builder()
            .build()
            .context("building the HTTP client")?;
        let llm = OpenAiCompatLlm::new(
            client,
            args.llm_url.clone(),
            None,
            args.llm_model.clone(),
            Duration::from_millis(args.llm_timeout_ms),
        );
        if let Err(e) = rt.block_on(llm.check()) {
            println!(
                "SKIP polish: LM Studio is not reachable at {} ({e}). Start it: LM Studio -> \
                 Developer -> load a model -> Start Server (port 1234), or pass --llm-url.",
                llm.base_url()
            );
            return Ok(None);
        }
        // Same vocabulary as the ASR context (`--vocab`), as in the app.
        let system = prompts::polish(&asr_context(args.vocab.as_deref())?.vocabulary);
        let polisher = Self { llm, rt, system };
        // The first request may load the model (LM Studio JIT loading); keep it out of timings.
        let (warm, ms) = polisher.polish("嗯，這是一個測試。");
        match warm {
            Ok(_) => eprintln!("polish warm-up: {ms} ms"),
            Err(e) => eprintln!("polish warm-up failed after {ms} ms: {e}"),
        }
        Ok(Some(polisher))
    }

    /// Polishes `text` the way the pipeline does (zh_tw on the way in and out). Returns the
    /// result and the LLM time in ms.
    fn polish(&self, text: &str) -> (v2c_core::Result<String>, u64) {
        let input = to_zh_tw(text.trim());
        let t = Instant::now();
        let user = prompts::wrap_transcript(&input);
        let out = self.rt.block_on(self.llm.complete(&self.system, &user));
        let ms = t.elapsed().as_millis() as u64;
        (out.map(|s| to_zh_tw(s.trim())), ms)
    }
}

/// `<name>.txt`: the polished target, next to the recording.
fn polished_target(f: &Fixture) -> anyhow::Result<String> {
    let path = f.wav.with_extension("txt");
    let s =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(s.trim().to_string())
}

/// Prints the polish result for one input. `budget` is `Some((asr_ms, audio_ms))` for real
/// recordings, `None` for text-only checks.
fn report_polish(
    polisher: &Polisher,
    f: &Fixture,
    input: &str,
    budget: Option<(u64, u64)>,
) -> anyhow::Result<PolishRow> {
    let target = polished_target(f)?;
    let (out, llm_ms) = polisher.polish(input);
    let row = match out {
        Ok(polished) => {
            let c = cer(&target, &polished);
            println!("  polished: {polished}");
            println!("  target:   {target}");
            print!("  polish CER {:.1}% | llm {llm_ms} ms", c * 100.0);
            if has_simplified(&polished) {
                print!(" | WARN: simplified characters after zh_tw");
            }
            println!();
            PolishRow {
                llm_ms,
                cer: Some(c),
                ok: true,
            }
        }
        Err(e) => {
            println!("  polish FAILED after {llm_ms} ms: {e}");
            println!("  rule-based fallback: {}", strip_fillers(&to_zh_tw(input)));
            println!("  target:   {target}");
            PolishRow {
                llm_ms,
                cer: None,
                ok: false,
            }
        }
    };
    let ok = match budget {
        Some((asr_ms, audio_ms)) => {
            let total = asr_ms + llm_ms;
            let ok = row.ok && within_polish_target(total, audio_ms);
            println!(
                "  {} asr+polish ({} ms per 10 s of audio, target <= {TARGET_WITH_POLISH_MS_PER_10S})",
                if ok { "PASS" } else { "FAIL" },
                per_10s(total, audio_ms)
            );
            ok
        }
        None => row.ok,
    };
    Ok(PolishRow { ok, ..row })
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let fixtures = find_fixtures(&args.dir)?;
    if fixtures.is_empty() {
        anyhow::bail!("no <name>.txt fixtures found in {}", args.dir.display());
    }
    let polisher = Polisher::connect(&args)?;
    let (present, missing): (Vec<_>, Vec<_>) = fixtures.into_iter().partition(|f| f.wav.is_file());
    let mut text_rows = Vec::new();
    for f in &missing {
        println!(
            "SKIP {}: {} not found (record it, see tests/fixtures/audio/README.md)",
            f.name,
            f.wav.display()
        );
        // Without a recording we can still check polish on the verbatim script.
        if let Some(p) = &polisher {
            if f.expected_file.to_string_lossy().ends_with(".raw.txt") {
                let raw = std::fs::read_to_string(&f.expected_file)
                    .with_context(|| format!("reading {}", f.expected_file.display()))?;
                println!("== {} (text-only polish of {}.raw.txt)", f.name, f.name);
                println!("  input:    {}", raw.trim());
                let row = report_polish(p, f, raw.trim(), None)?;
                text_rows.push((f.name.clone(), row));
            }
        }
    }
    if present.is_empty() {
        println!(
            "\nNo WAV recordings found in {}; nothing to measure. Skipped {} fixture(s).",
            args.dir.display(),
            missing.len()
        );
        print_text_summary(&text_rows);
        return Ok(());
    }

    let engine = load_engine(&args.asr, args.model.as_deref(), &args.breeze)?;
    let ctx = asr_context(args.vocab.as_deref())?;

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
            let polish = match &polisher {
                Some(p) => Some(report_polish(
                    p,
                    f,
                    &out.transcript.text,
                    Some((out.total_ms, clip.duration_ms())),
                )?),
                None => None,
            };
            rows.push(Row {
                name: f.name.clone(),
                audio_ms: clip.duration_ms(),
                asr_ms: out.total_ms,
                cer: c,
                speed_ok,
                simplified,
                polish,
            });
        }
    }

    print_summary(&rows, missing.len());
    print_text_summary(&text_rows);
    Ok(())
}

fn print_text_summary(rows: &[(String, PolishRow)]) {
    if rows.is_empty() {
        return;
    }
    println!("\n== Text-only polish (no recording)");
    for (name, r) in rows {
        match r.cer {
            Some(c) => println!("{name:<12} llm {:>6} ms  CER {:>5.1}%", r.llm_ms, c * 100.0),
            None => println!("{name:<12} llm {:>6} ms  FAILED", r.llm_ms),
        }
    }
    let failed = rows.iter().filter(|(_, r)| !r.ok).count();
    println!(
        "text-only polish: {} ok, {failed} failed (check by eye that the meaning is unchanged)",
        rows.len() - failed
    );
}

fn print_summary(rows: &[Row], skipped: usize) {
    println!("\n== Summary");
    println!(
        "{:<12} {:>9} {:>8} {:>7} {:>8} {:>11}",
        "fixture", "audio ms", "asr ms", "CER", "llm ms", "polish CER"
    );
    for r in rows {
        let (llm, pcer) = match &r.polish {
            Some(p) => (
                p.llm_ms.to_string(),
                p.cer
                    .map(|c| format!("{:.1}%", c * 100.0))
                    .unwrap_or_else(|| "FAILED".into()),
            ),
            None => ("-".into(), "-".into()),
        };
        println!(
            "{:<12} {:>9} {:>8} {:>6.1}% {:>8} {:>11}",
            r.name,
            r.audio_ms,
            r.asr_ms,
            r.cer * 100.0,
            llm,
            pcer
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
    let polished: Vec<&PolishRow> = rows.iter().filter_map(|r| r.polish.as_ref()).collect();
    if !polished.is_empty() {
        let failed = polished.iter().filter(|p| !p.ok).count();
        let mean_llm =
            polished.iter().map(|p| p.llm_ms as f64).sum::<f64>() / polished.len() as f64;
        println!(
            "polish: mean llm {mean_llm:.0} ms; target: ASR + polish <= \
             {TARGET_WITH_POLISH_MS_PER_10S} ms per 10 s of audio"
        );
        println!(
            "polish verdict: {} ({failed} of {} over budget or failed)",
            if failed == 0 { "PASS" } else { "FAIL" },
            polished.len()
        );
    }
}

/// Acceptance target from the T0.1 card: 10 s of audio in at most 1.2 s of ASR.
const TARGET_MS_PER_10S: u64 = 1200;
/// T1.5 / plan section 2 budget: ASR + polish for 10 s of audio in at most 2.5 s.
const TARGET_WITH_POLISH_MS_PER_10S: u64 = 2500;

fn within_polish_target(total_ms: u64, audio_ms: u64) -> bool {
    per_10s(total_ms, audio_ms) <= TARGET_WITH_POLISH_MS_PER_10S
}

/// ASR time normalised to 10 s of audio. The target is "a sentence of up to 10 s finishes
/// within the budget", and Whisper's cost is nearly fixed (it always encodes a 30 s window),
/// so clips up to 10 s are judged on their absolute time; longer clips are scaled down.
fn per_10s(asr_ms: u64, audio_ms: u64) -> u64 {
    asr_ms * 10_000 / audio_ms.max(10_000)
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
        // Up to 10 s of audio: absolute time against the target.
        assert!(within_latency_target(1200, 5_000));
        assert!(!within_latency_target(1201, 5_000));
        assert_eq!(per_10s(100, 0), 100);
        // Longer clips are scaled to 10 s.
        assert!(within_latency_target(2400, 20_000));
        assert!(!within_latency_target(2402, 20_000));
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
    fn simplified_asr_output_is_scored_after_zh_tw() {
        // SenseVoice-style Simplified output: the bench scores it the way the app shows it.
        let out = EngineOutput {
            transcript: v2c_core::asr::Transcript {
                text: "这个 meeting 我们 delay 一下".into(),
                lang: v2c_core::Lang::Mixed,
            },
            total_ms: 10,
            summary: "asr 10 ms".into(),
        }
        .with_zh_tw();
        assert!(!has_simplified(&out.transcript.text));
        assert_eq!(
            cer("這個 meeting 我們 delay 一下", &out.transcript.text),
            0.0
        );
        assert!(out.summary.ends_with("zh_tw applied"));
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
        run(args_for(&dir, None, LocalConfig::default().llm_base_url)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn args_for(dir: &Path, llm: Option<&str>, llm_url: String) -> Args {
        Args {
            dir: dir.to_path_buf(),
            asr: "breeze".into(),
            llm: llm.map(Into::into),
            llm_url,
            llm_model: None,
            llm_timeout_ms: 2000,
            model: None,
            repeat: 1,
            vocab: None,
            breeze: BreezeArgs {
                lang: "detect".into(),
                cpu: true,
                no_flash_attn: false,
                audio_ctx: None,
                threads: None,
            },
        }
    }

    #[test]
    fn polish_budget_scales_to_ten_seconds() {
        assert!(within_polish_target(2500, 10_000));
        assert!(!within_polish_target(2501, 10_000));
        assert!(within_polish_target(1250, 5_000));
    }

    #[test]
    fn unknown_llm_is_an_error() {
        let dir = std::env::temp_dir().join(format!("v2c-bench-llm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("en-01.txt"), "Hello.").unwrap();
        let err = run(args_for(&dir, Some("gpt"), "http://127.0.0.1:9/v1".into()))
            .err()
            .unwrap();
        assert!(err.to_string().contains("lmstudio"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreachable_lmstudio_skips_polish() {
        let dir = std::env::temp_dir().join(format!("v2c-bench-down-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("filler-01.txt"), "後天見").unwrap();
        std::fs::write(dir.join("filler-01.raw.txt"), "嗯後天見").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let args = args_for(
            &dir,
            Some("lmstudio"),
            format!("http://127.0.0.1:{port}/v1"),
        );
        assert!(Polisher::connect(&args).unwrap().is_none());
        run(args).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn text_only_polish_against_mock_server() {
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/v1/models"))
                .respond_with(ResponseTemplate::new(200).set_body_string("{\"data\":[]}"))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{ "message": { "role": "assistant",
                                               "content": "我们后天见。" },
                                  "finish_reason": "stop" }]
                })))
                .mount(&server)
                .await;
            server
        });
        let dir = std::env::temp_dir().join(format!("v2c-bench-mock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("filler-01.txt"), "我們後天見。").unwrap();
        std::fs::write(dir.join("filler-01.raw.txt"), "嗯，我們後天見。").unwrap();
        let args = args_for(&dir, Some("lmstudio"), format!("{}/v1", server.uri()));

        let p = Polisher::connect(&args)
            .unwrap()
            .expect("mock server is reachable");
        let fx = find_fixtures(&dir).unwrap();
        let row = report_polish(&p, &fx[0], "嗯，我們後天見。", None).unwrap();
        assert!(row.ok);
        // The simplified model output was converted by zh_tw, so it matches the target.
        assert_eq!(row.cer, Some(0.0));
        let slow = report_polish(&p, &fx[0], "嗯，我們後天見。", Some((3000, 10_000))).unwrap();
        assert!(!slow.ok, "3 s of ASR alone is over the 2.5 s budget");
        drop(p);
        run(args).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        drop(server);
    }
}
