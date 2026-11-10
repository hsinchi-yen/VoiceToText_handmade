//! T3.3: end-to-end simulation of one utterance on Linux, without OS input or a microphone:
//! synthetic hotkey [`Event`]s → `trigger::Trigger` commands → synthetic audio clip →
//! `Pipeline` (mock or wiremock `CompatAsr` + wiremock LLM, built from `Config`) →
//! `inject_plan::plan` → a recording [`Injector`].
//!
//! The real `LinuxInjector` is wired in after T3.2; this test only needs the trait.
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
use v2c_core::secrets::MemorySecretStore;
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
                        let clip = speech_like_clip(at - started);
                        let out = self
                            .pipeline
                            .run(clip, mode)
                            .await
                            .expect("pipeline ok")
                            .expect("some text");
                        let actions = inject_plan::plan(&out.text, &self.profile);
                        self.injector.run(&actions).unwrap();
                        self.outputs.push(out);
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
