//! T3.3: end-to-end simulation of one utterance on Linux, without OS input or a microphone:
//! synthetic hotkey [`Event`]s → `trigger::Trigger` commands → synthetic audio clip →
//! `Pipeline` (mock or wiremock `CompatAsr` + wiremock LLM, built from `Config`) →
//! `inject_plan::plan` → a recording [`Injector`].
//!
//! The last test also runs the real `LinuxInjector` (T3.2) against a simulated desktop.
//! Also checks that no transcript / polished / translated text reaches the log
//! (AGENTS.md rule 1), with every log event of this binary captured at TRACE level.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
use v2c_core::audio::AudioClip;
use v2c_core::config::{Config, NewlineKey};
use v2c_core::error::Stage;
use v2c_core::inject_plan::{self, Action, AppProfile};
use v2c_core::llm::{LlmProvider, OpenAiCompatLlm};
use v2c_core::pipeline::{Pipeline, PipelineConfig, Warning};
use v2c_core::postprocess::fillers::strip_fillers;
use v2c_core::route::{self, cloud_http_client, Purpose, Target};
use v2c_core::secrets::{ApiKey, MemorySecretStore, SecretStore};
use v2c_core::trigger::{Command, Event, Key, Trigger, TriggerConfig};
use v2c_core::{prompts, Lang, Mode};
use v2c_platform::{InjectError, Injector};

/// What the (mock) ASR "heard": simplified characters and fillers, as Whisper-style models
/// often return.
const HEARD: &str = "嗯，那个，我们明天早上十点开会";
/// `HEARD` after zh-TW conversion (what the LLM receives).
const HEARD_TW: &str = "嗯，那個，我們明天早上十點開會";
const POLISHED: &str = "我們明天早上十點開會。";
const TRANSLATED: &str = "We have a meeting at ten tomorrow morning.";
const TWO_LINES: &str = "第一項：確認會議室。\n第二項：寄出議程。";

/// Desktop `providers::LOCAL_LLM_TIMEOUT`.
const LOCAL_LLM_TIMEOUT: Duration = Duration::from_secs(8);

// ----------------------------------------------------------------------------------------
// Log capture (global, so events from every test and thread in this binary are seen)
// ----------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn logs() -> &'static LogBuf {
    static LOGS: OnceLock<LogBuf> = OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = LogBuf::default();
        let writer = buf.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_target(true)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("only this binary installs a global subscriber");
        buf
    })
}

// ----------------------------------------------------------------------------------------
// Mocks
// ----------------------------------------------------------------------------------------

struct MockAsr {
    text: &'static str,
    calls: AtomicUsize,
    last_clip_ms: Mutex<Option<u64>>,
}

impl MockAsr {
    fn new(text: &'static str) -> Arc<Self> {
        Arc::new(Self {
            text,
            calls: AtomicUsize::new(0),
            last_clip_ms: Mutex::new(None),
        })
    }
}

#[async_trait]
impl AsrProvider for MockAsr {
    fn name(&self) -> &str {
        "mock-asr"
    }
    async fn transcribe(&self, clip: &AudioClip, _: &AsrContext) -> v2c_core::Result<Transcript> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_clip_ms.lock().unwrap() = Some(clip.duration_ms());
        Ok(Transcript {
            text: self.text.into(),
            lang: Lang::Zh,
        })
    }
}

/// Records every action list it is asked to run.
#[derive(Default)]
struct RecordingInjector {
    runs: Mutex<Vec<Vec<Action>>>,
}

impl Injector for RecordingInjector {
    fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
        self.runs.lock().unwrap().push(actions.to_vec());
        Ok(())
    }
}

// ----------------------------------------------------------------------------------------
// Simulated controller
// ----------------------------------------------------------------------------------------

/// Plays the desktop controller's role: executes trigger commands, records a synthetic clip
/// as long as the key was held, runs the pipeline and injects the plan.
struct Sim {
    trigger: Trigger,
    pipeline: Pipeline,
    profile: AppProfile,
    injector: RecordingInjector,
    recording: Option<(Mode, u64)>,
    commands: Vec<Command>,
    outputs: Vec<v2c_core::pipeline::Output>,
    /// Audio to "record" instead of the synthetic tone (e.g. a WAV for the real-endpoint test).
    clip: Option<AudioClip>,
    /// Pipeline errors (nothing is injected for them).
    errors: Vec<v2c_core::V2cError>,
}

impl Sim {
    fn new(cfg: &Config, pipeline: Pipeline, profile: AppProfile) -> Self {
        Self {
            trigger: Trigger::new(TriggerConfig::from(&cfg.hotkey)),
            pipeline,
            profile,
            injector: RecordingInjector::default(),
            recording: None,
            commands: Vec::new(),
            outputs: Vec::new(),
            clip: None,
            errors: Vec::new(),
        }
    }

    async fn feed(&mut self, events: &[Event]) {
        for &ev in events {
            let at = match ev {
                Event::KeyDown { at_ms, .. }
                | Event::KeyUp { at_ms, .. }
                | Event::Tick { at_ms } => at_ms,
            };
            for cmd in self.trigger.on_event(ev) {
                self.commands.push(cmd);
                match cmd {
                    Command::StartRecording(mode) => self.recording = Some((mode, at)),
                    Command::Cancel => self.recording = None,
                    Command::StopAndProcess | Command::StopAtLimit => {
                        let (mode, started) = self.recording.take().expect("was recording");
                        let clip = self
                            .clip
                            .clone()
                            .unwrap_or_else(|| speech_like_clip(at - started));
                        match self.pipeline.run(clip, mode).await {
                            Ok(out) => {
                                let out = out.expect("some text");
                                let actions = inject_plan::plan(&out.text, &self.profile);
                                self.injector.run(&actions).unwrap();
                                self.outputs.push(out);
                            }
                            Err(e) => self.errors.push(e),
                        }
                    }
                }
            }
        }
    }

    fn runs(&self) -> Vec<Vec<Action>> {
        self.injector.runs.lock().unwrap().clone()
    }
}

fn down(key: Key, at_ms: u64) -> Event {
    Event::KeyDown { key, at_ms }
}
fn up(key: Key, at_ms: u64) -> Event {
    Event::KeyUp { key, at_ms }
}
fn tick(at_ms: u64) -> Event {
    Event::Tick { at_ms }
}

/// Non-silent synthetic audio (a 220 Hz tone, not speech) of `ms` milliseconds.
fn speech_like_clip(ms: u64) -> AudioClip {
    let n = (16 * ms) as usize;
    AudioClip::new(
        (0..n)
            .map(|i| 0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / 16_000.0).sin())
            .collect(),
    )
}

fn local_config(llm_base_url: &str) -> Config {
    Config::from_toml_str(&format!(
        "[local]\nllm_base_url = \"{llm_base_url}\"\nllm_model = \"Qwen3.8-27B-UD-Q6_K_L\"\n\n[gpu]\nenabled = false\n"
    ))
    .unwrap()
}

/// As desktop `Providers::build` builds the local LLM.
fn local_llm(cfg: &Config, stage: Stage) -> Arc<dyn LlmProvider> {
    Arc::new(
        OpenAiCompatLlm::new(
            cloud_http_client().unwrap().client().clone(),
            cfg.local.llm_base_url.clone(),
            None,
            cfg.local.llm_model.clone(),
            LOCAL_LLM_TIMEOUT,
        )
        .with_stage(stage),
    )
}

fn pipeline(cfg: &Config, asr: Arc<dyn AsrProvider>, llm: Arc<dyn LlmProvider>) -> Pipeline {
    Pipeline::new(
        asr,
        Some(llm),
        PipelineConfig {
            polish_enabled: cfg.polish.enabled,
            polish_timeout_ms: cfg.polish.timeout_ms,
            min_clip_ms: cfg.audio.min_clip_ms,
            vocabulary: Vec::new(),
        },
    )
    .with_polish_timeout_cap_ms(LOCAL_LLM_TIMEOUT.as_millis() as u64)
}

/// llama.cpp-style chat response (separate `reasoning_content`).
fn llm_reply(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "reasoning_content": "Remove the fillers.",
                "content": content
            }
        }]
    }))
}

async fn llm_server(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

const EDITOR: AppProfile = AppProfile {
    newline: NewlineKey::Enter,
    always_paste: false,
};

// ----------------------------------------------------------------------------------------
// Scenarios
// ----------------------------------------------------------------------------------------

async fn scenario_hold_to_talk_dictate_is_polished_and_typed() {
    logs();
    let server = llm_server(llm_reply(POLISHED)).await;
    let cfg = local_config(&format!("{}/v1", server.uri()));
    let asr = MockAsr::new(HEARD);
    let mut sim = Sim::new(
        &cfg,
        pipeline(&cfg, asr.clone(), local_llm(&cfg, Stage::Polish)),
        EDITOR,
    );
    sim.feed(&[
        down(Key::Trigger, 1_000),
        tick(1_400),
        up(Key::Trigger, 2_500),
    ])
    .await;

    assert_eq!(
        sim.commands,
        vec![
            Command::StartRecording(Mode::Dictate),
            Command::StopAndProcess
        ]
    );
    assert_eq!(*asr.last_clip_ms.lock().unwrap(), Some(1_500));
    assert_eq!(sim.runs(), vec![vec![Action::Text(POLISHED.into())]]);
    assert!(sim.outputs[0].warnings.is_empty());
    // The LLM got the zh-TW converted transcript, wrapped as data.
    let reqs = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(
        body["messages"][1]["content"],
        json!(prompts::wrap_transcript(HEARD_TW))
    );
}

async fn scenario_shift_hold_translates_to_english() {
    logs();
    let server = llm_server(llm_reply(TRANSLATED)).await;
    let cfg = local_config(&format!("{}/v1", server.uri()));
    let mut sim = Sim::new(
        &cfg,
        pipeline(&cfg, MockAsr::new(HEARD), local_llm(&cfg, Stage::Translate)),
        EDITOR,
    );
    sim.feed(&[
        down(Key::Shift, 0),
        down(Key::Trigger, 50),
        up(Key::Shift, 400),
        up(Key::Trigger, 1_200),
    ])
    .await;
    assert_eq!(
        sim.commands,
        vec![
            Command::StartRecording(Mode::TranslateZhToEn),
            Command::StopAndProcess
        ]
    );
    assert_eq!(sim.runs(), vec![vec![Action::Text(TRANSLATED.into())]]);
    assert_eq!(sim.outputs[0].lang, Lang::En);
}

#[tokio::test]
async fn hands_free_double_tap_types_lines_with_chat_newline() {
    logs();
    let server = llm_server(llm_reply(TWO_LINES)).await;
    let cfg = local_config(&format!("{}/v1", server.uri()));
    let chat = inject_plan::profile_for("slack.exe", &cfg.apps);
    assert_eq!(chat.newline, NewlineKey::ShiftEnter);
    let mut sim = Sim::new(
        &cfg,
        pipeline(&cfg, MockAsr::new(HEARD), local_llm(&cfg, Stage::Polish)),
        chat,
    );
    sim.feed(&[
        down(Key::Trigger, 0),
        up(Key::Trigger, 100),
        down(Key::Trigger, 250), // second tap: hands-free
        up(Key::Trigger, 320),
        tick(1_000),
        tick(2_000),
        down(Key::Trigger, 4_000), // tap again to stop
        up(Key::Trigger, 4_080),
    ])
    .await;
    assert_eq!(
        sim.commands,
        vec![
            Command::StartRecording(Mode::Dictate),
            Command::StopAndProcess
        ]
    );
    assert_eq!(
        sim.runs(),
        vec![vec![
            Action::Text("第一項：確認會議室。".into()),
            Action::NewLine(NewlineKey::ShiftEnter),
            Action::Text("第二項：寄出議程。".into()),
        ]]
    );
}

#[tokio::test]
async fn shortcut_and_single_tap_cancel_without_asr_or_injection() {
    logs();
    let server = llm_server(llm_reply(POLISHED)).await;
    let cfg = local_config(&format!("{}/v1", server.uri()));
    let asr = MockAsr::new(HEARD);
    let mut sim = Sim::new(
        &cfg,
        pipeline(&cfg, asr.clone(), local_llm(&cfg, Stage::Polish)),
        EDITOR,
    );
    sim.feed(&[
        // Ctrl+C with the trigger key.
        down(Key::Trigger, 0),
        down(Key::Other, 120),
        up(Key::Other, 180),
        up(Key::Trigger, 200),
        // A single short tap that times out.
        down(Key::Trigger, 2_000),
        up(Key::Trigger, 2_100),
        tick(2_600),
    ])
    .await;
    assert_eq!(
        sim.commands,
        vec![
            Command::StartRecording(Mode::Dictate),
            Command::Cancel,
            Command::StartRecording(Mode::Dictate),
            Command::Cancel,
        ]
    );
    assert_eq!(asr.calls.load(Ordering::SeqCst), 0);
    assert!(sim.runs().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

async fn scenario_llm_failure_types_filler_free_transcript() {
    logs();
    let server = llm_server(ResponseTemplate::new(500)).await;
    let cfg = local_config(&format!("{}/v1", server.uri()));
    let mut sim = Sim::new(
        &cfg,
        pipeline(&cfg, MockAsr::new(HEARD), local_llm(&cfg, Stage::Polish)),
        EDITOR,
    );
    sim.feed(&[down(Key::Trigger, 0), up(Key::Trigger, 900)])
        .await;
    let expected = strip_fillers(HEARD_TW);
    assert!(!expected.starts_with('嗯'), "{expected}");
    assert_eq!(sim.runs(), vec![vec![Action::Text(expected)]]);
    assert_eq!(sim.outputs[0].warnings, vec![Warning::PolishFellBack]);
}

/// ASR through `CompatAsr` (a `[[cloud]]` open_ai_compat service on wiremock) instead of the
/// mock provider, LLM on another wiremock server, paste-only target (the Linux path after T3.2).
async fn scenario_compat_asr_and_llm_servers_end_to_end() {
    logs();
    let asr_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "text": HEARD })))
        .expect(1)
        .mount(&asr_server)
        .await;
    let llm = llm_server(llm_reply(POLISHED)).await;
    let cfg = Config::from_toml_str(&format!(
        r#"
[route]
asr = "lan-asr"

[local]
llm_base_url = "{}/v1"

[gpu]
enabled = false

[[cloud]]
id = "lan-asr"
kind = "open_ai_compat"
base_url = "{}/v1"
asr_model = "breeze-asr-25"
"#,
        llm.uri(),
        asr_server.uri()
    ))
    .unwrap();
    let plan = route::resolve(&cfg.route, &cfg.cloud);
    let Target::Cloud { service_id, .. } = &plan.get(Purpose::Asr).target else {
        panic!("ASR not routed: {:?}", plan.issues);
    };
    let asr: Arc<dyn AsrProvider> = Arc::from(
        route::build_cloud_asr(
            cfg.cloud_service(service_id).unwrap(),
            &MemorySecretStore::new(),
            &cloud_http_client().unwrap(),
            Duration::from_millis(cfg.timeouts.asr_cloud_ms),
        )
        .unwrap()
        .unwrap(),
    );
    let paste = AppProfile {
        newline: NewlineKey::Enter,
        always_paste: true,
    };
    let mut sim = Sim::new(
        &cfg,
        pipeline(&cfg, asr, local_llm(&cfg, Stage::Polish)),
        paste,
    );
    sim.feed(&[down(Key::Trigger, 0), up(Key::Trigger, 1_000)])
        .await;

    assert_eq!(sim.runs(), vec![vec![Action::Paste(POLISHED.into())]]);
    let asr_reqs = asr_server.received_requests().await.unwrap();
    assert!(
        String::from_utf8_lossy(&asr_reqs[0].body).contains("name=\"model\"\r\n\r\nbreeze-asr-25")
    );
    let llm_reqs = llm.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&llm_reqs[0].body).unwrap();
    assert_eq!(
        body["messages"][1]["content"],
        json!(prompts::wrap_transcript(HEARD_TW))
    );
}

#[tokio::test]
async fn hold_to_talk_dictate_is_polished_and_typed() {
    scenario_hold_to_talk_dictate_is_polished_and_typed().await;
}

#[tokio::test]
async fn shift_hold_translates_to_english() {
    scenario_shift_hold_translates_to_english().await;
}

#[tokio::test]
async fn llm_failure_types_filler_free_transcript() {
    scenario_llm_failure_types_filler_free_transcript().await;
}

#[tokio::test]
async fn compat_asr_and_llm_servers_end_to_end() {
    scenario_compat_asr_and_llm_servers_end_to_end().await;
}

/// AGENTS.md rule 1: run the scenarios above (and fallbacks) with every log event captured at
/// TRACE; no transcript, polished or translated text may appear.
#[tokio::test]
async fn logs_never_contain_the_sentence() {
    let buf = logs();
    // Runs here too so the check does not depend on test order.
    scenario_hold_to_talk_dictate_is_polished_and_typed().await;
    scenario_shift_hold_translates_to_english().await;
    scenario_llm_failure_types_filler_free_transcript().await;
    scenario_compat_asr_and_llm_servers_end_to_end().await;

    let text = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    assert!(
        text.contains("pipeline done"),
        "log capture is not working: {} bytes",
        text.len()
    );
    assert!(text.contains("polish failed; falling back"));
    let fillerless = strip_fillers(HEARD_TW);
    for secret in [
        HEARD,
        HEARD_TW,
        POLISHED,
        TRANSLATED,
        fillerless.as_str(),
        "明天早上",
        "tomorrow morning",
        "第一項",
    ] {
        assert!(
            !text.contains(secret),
            "log contains user text ({} chars)",
            secret.chars().count()
        );
    }
}

// ----------------------------------------------------------------------------------------
// Real endpoints (opt-in)
// ----------------------------------------------------------------------------------------

/// Minimal PCM16 WAV reader (any channel count, mixed down to mono). Test-only; same as in
/// `linux_asr.rs`.
fn read_wav(bytes: &[u8]) -> Option<AudioClip> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let (mut channels, mut rate, mut bits) = (0u16, 0u32, 0u16);
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().ok()?) as usize;
        let data = bytes.get(i + 8..(i + 8 + len).min(bytes.len()))?;
        if id == b"fmt " && data.len() >= 16 {
            if u16::from_le_bytes([data[0], data[1]]) != 1 {
                return None;
            }
            channels = u16::from_le_bytes([data[2], data[3]]);
            rate = u32::from_le_bytes(data[4..8].try_into().ok()?);
            bits = u16::from_le_bytes([data[14], data[15]]);
        } else if id == b"data" {
            if bits != 16 || channels == 0 {
                return None;
            }
            let ch = channels as usize;
            let samples = data
                .chunks_exact(2 * ch)
                .map(|frame| {
                    frame
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|s| f32::from(i16::from_le_bytes(*s)) / 32768.0)
                        .sum::<f32>()
                        / ch as f32
                })
                .collect();
            return Some(AudioClip {
                samples,
                sample_rate: rate,
            });
        }
        i += 8 + len + (len & 1);
    }
    None
}

/// Character error rate ignoring case, whitespace and punctuation. Same as in `linux_asr.rs`.
fn cer(expected: &str, got: &str) -> f64 {
    let norm = |s: &str| -> Vec<char> {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (a, b) = (norm(expected), norm(got));
    if a.is_empty() {
        return if b.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()] as f64 / a.len() as f64
}

/// Real ASR (`CompatAsr`) → pipeline → real LLM polish → plan → recording injector, with the
/// config defaults (`[timeouts] asr_cloud_ms`, `[polish] timeout_ms`) unless overridden.
///
/// Env: `V2C_TEST_ASR_BASE_URL`, `V2C_TEST_ASR_WAV` and `V2C_TEST_LLM_BASE_URL` (required);
/// `V2C_TEST_ASR_MODEL`, `V2C_TEST_ASR_KEY_FILE` (never printed), `V2C_TEST_ASR_EXPECT`,
/// `V2C_TEST_ASR_TIMEOUT_MS`, `V2C_TEST_LLM_MODEL`. Prints only outcomes, lengths, timings
/// and match/CER; never the text.
#[tokio::test]
#[ignore = "needs V2C_TEST_ASR_BASE_URL, V2C_TEST_ASR_WAV and V2C_TEST_LLM_BASE_URL"]
async fn e2e_real_asr_llm_polish_inject() {
    let var = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
    let (Some(asr_url), Some(wav), Some(llm_url)) = (
        var("V2C_TEST_ASR_BASE_URL"),
        var("V2C_TEST_ASR_WAV"),
        var("V2C_TEST_LLM_BASE_URL"),
    ) else {
        eprintln!("e2e_real: ASR/WAV/LLM env not set; skipped");
        return;
    };
    let buf = logs();
    let mut cfg = Config::with_defaults();
    cfg.gpu.enabled = false;
    cfg.route.asr = "lan-asr".into();
    cfg.local.llm_base_url = llm_url;
    cfg.local.llm_model = var("V2C_TEST_LLM_MODEL");
    if let Some(ms) = var("V2C_TEST_ASR_TIMEOUT_MS").and_then(|s| s.parse().ok()) {
        cfg.timeouts.asr_cloud_ms = ms;
    }
    cfg.cloud.push(v2c_core::config::CloudService {
        id: "lan-asr".into(),
        name: None,
        kind: v2c_core::config::CloudKind::OpenAiCompat,
        base_url: Some(asr_url),
        asr_model: Some(var("V2C_TEST_ASR_MODEL").unwrap_or_else(|| "whisper-1".into())),
        llm_model: None,
    });
    let secrets = MemorySecretStore::new();
    if let Some(p) = var("V2C_TEST_ASR_KEY_FILE") {
        let key = std::fs::read_to_string(p.trim()).expect("key file readable");
        if !key.trim().is_empty() {
            secrets.set("lan-asr", &ApiKey::new(key.trim())).unwrap();
        }
    }
    let clip = read_wav(&std::fs::read(wav.trim()).expect("WAV readable")).expect("PCM16 WAV");
    let clip_ms = clip.duration_ms();

    let plan = route::resolve(&cfg.route, &cfg.cloud);
    assert!(plan.issues.is_empty(), "{:?}", plan.issues);
    let asr: Arc<dyn AsrProvider> = Arc::from(
        route::build_cloud_asr(
            cfg.cloud_service("lan-asr").unwrap(),
            &secrets,
            &cloud_http_client().unwrap(),
            Duration::from_millis(cfg.timeouts.asr_cloud_ms),
        )
        .unwrap()
        .unwrap(),
    );
    let p = pipeline(&cfg, asr, local_llm(&cfg, Stage::Polish));
    let mut sim = Sim::new(&cfg, p, EDITOR);
    sim.clip = Some(clip);
    println!(
        "e2e_real: audio {clip_ms} ms, asr timeout {} ms, polish budget {} ms",
        cfg.timeouts.asr_cloud_ms, cfg.polish.timeout_ms
    );
    let started = std::time::Instant::now();
    sim.feed(&[down(Key::Trigger, 0), up(Key::Trigger, clip_ms)])
        .await;
    let wall_ms = started.elapsed().as_millis();
    assert_eq!(
        sim.commands,
        vec![
            Command::StartRecording(Mode::Dictate),
            Command::StopAndProcess
        ]
    );
    if let Some(e) = sim.errors.first() {
        println!("e2e_real: pipeline error {e}, {wall_ms} ms");
        panic!("pipeline failed: {e}");
    }
    let out = &sim.outputs[0];
    let fell_back = out.warnings.contains(&Warning::PolishFellBack);
    let score = var("V2C_TEST_ASR_EXPECT").map(|e| cer(&e, &out.text));
    println!(
        "e2e_real: ok, {} chars, asr_ms {}, llm_ms {:?}, total_ms {}, polish fell_back {fell_back}, match {:?}, CER {:?}",
        out.text.chars().count(),
        out.timings.asr_ms,
        out.timings.llm_ms,
        out.timings.total_ms,
        score.map(|c| c == 0.0),
        score.map(|c| format!("{c:.3}"))
    );
    let runs = sim.runs();
    assert_eq!(runs.len(), 1);
    let typed: String = runs[0]
        .iter()
        .map(|a| match a {
            Action::Text(t) => t.clone(),
            Action::NewLine(_) => "\n".into(),
            Action::Paste(t) => t.clone(),
        })
        .collect();
    assert_eq!(typed, out.text);
    assert!(!out.text.trim().is_empty());
    if let Some(c) = score {
        assert!(c <= 0.15, "CER {c:.3} above 0.15");
    }
    let log = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    assert!(!log.contains(out.text.as_str()), "log contains the output");
    if let Some(e) = var("V2C_TEST_ASR_EXPECT") {
        assert!(!log.contains(e.as_str()), "log contains the transcript");
    }
}

// ----------------------------------------------------------------------------------------
// The real Linux injector (T3.2) on a simulated desktop
// ----------------------------------------------------------------------------------------

use std::cell::RefCell;
use v2c_platform::linux::clipboard::{ClipContent, ClipboardBackend};
use v2c_platform::linux::inject::{InjectConfig, LinuxInjector, RestoreOutcome};
use v2c_platform::linux::keys::{
    KeyBackend, ModifierState, KEY_ENTER, KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V,
};

/// A focused text field plus the system clipboard, driven only by key events, the way a
/// GNOME app sees the RemoteDesktop portal's virtual keyboard.
#[derive(Default)]
struct FakeDesktop {
    clipboard: RefCell<ClipContent>,
    /// The clipboard content v2c set last (for `still_ours`).
    ours: RefCell<Option<ClipContent>>,
    held: RefCell<Vec<u16>>,
    document: RefCell<String>,
    /// Shift+Enter in a chat box inserts a line break; plain Enter would "send".
    sent_messages: RefCell<usize>,
}

fn text_clip(s: &str) -> ClipContent {
    ClipContent(vec![(
        "text/plain;charset=utf-8".into(),
        s.as_bytes().to_vec(),
    )])
}

struct FakeClipboard<'a>(&'a FakeDesktop);
struct FakeKeys<'a>(&'a FakeDesktop);

impl ClipboardBackend for FakeClipboard<'_> {
    fn backup(&self) -> Result<ClipContent, InjectError> {
        Ok(self.0.clipboard.borrow().clone())
    }
    fn set_text(&self, text: &str) -> Result<(), InjectError> {
        *self.0.clipboard.borrow_mut() = text_clip(text);
        *self.0.ours.borrow_mut() = Some(text_clip(text));
        Ok(())
    }
    fn still_ours(&self) -> Result<bool, InjectError> {
        Ok(self.0.ours.borrow().as_ref() == Some(&*self.0.clipboard.borrow()))
    }
    fn restore(&self, content: &ClipContent) -> Result<(), InjectError> {
        *self.0.clipboard.borrow_mut() = content.clone();
        Ok(())
    }
}

impl KeyBackend for FakeKeys<'_> {
    fn modifiers(&self) -> Result<ModifierState, InjectError> {
        Ok(ModifierState::Unknown)
    }
    fn key(&self, code: u16, pressed: bool) -> Result<(), InjectError> {
        let d = self.0;
        if !pressed {
            d.held.borrow_mut().retain(|&k| k != code);
            return Ok(());
        }
        let held = d.held.borrow().clone();
        match code {
            KEY_V if held.contains(&KEY_LEFTCTRL) => {
                let clip = d.clipboard.borrow();
                let text = clip
                    .0
                    .iter()
                    .find(|(m, _)| m.starts_with("text/plain"))
                    .map(|(_, b)| String::from_utf8(b.clone()).unwrap())
                    .unwrap_or_default();
                d.document.borrow_mut().push_str(&text);
            }
            KEY_ENTER if held.contains(&KEY_LEFTSHIFT) => d.document.borrow_mut().push('\n'),
            KEY_ENTER => *d.sent_messages.borrow_mut() += 1,
            _ => {}
        }
        d.held.borrow_mut().push(code);
        Ok(())
    }
}

/// The hands-free scenario's output, run through the real `LinuxInjector`: both lines land in
/// the chat box, the line break does not send the message, no key stays held, and the user's
/// own clipboard is back afterwards.
#[tokio::test]
async fn linux_injector_pastes_the_pipeline_output_into_a_simulated_app() {
    logs();
    let server = llm_server(llm_reply(TWO_LINES)).await;
    let cfg = local_config(&format!("{}/v1", server.uri()));
    let chat = inject_plan::profile_for("slack.exe", &cfg.apps);
    let mut sim = Sim::new(
        &cfg,
        pipeline(&cfg, MockAsr::new(HEARD), local_llm(&cfg, Stage::Polish)),
        chat,
    );
    sim.feed(&[down(Key::Trigger, 0), up(Key::Trigger, 1_200)])
        .await;
    let runs = sim.runs();
    assert_eq!(runs.len(), 1);

    let desktop = FakeDesktop::default();
    *desktop.clipboard.borrow_mut() = text_clip("user's own copy");
    let config = InjectConfig {
        settle: Duration::from_millis(1),
        ..InjectConfig::default()
    };
    let inj = LinuxInjector::new(FakeClipboard(&desktop), FakeKeys(&desktop), config);
    inj.run(&runs[0]).unwrap();

    assert_eq!(*desktop.document.borrow(), TWO_LINES);
    assert_eq!(
        *desktop.sent_messages.borrow(),
        0,
        "a line break sent the message"
    );
    assert!(
        desktop.held.borrow().is_empty(),
        "keys left held: {:?}",
        desktop.held.borrow()
    );
    assert_eq!(*desktop.clipboard.borrow(), text_clip("user's own copy"));
    assert_eq!(inj.last_restore_outcome(), Some(RestoreOutcome::Restored));
}
