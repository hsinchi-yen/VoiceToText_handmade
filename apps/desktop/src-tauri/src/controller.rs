//! The coordinator (`tasks/plan.md` section 7): hotkey events -> trigger state machine ->
//! recorder -> pipeline -> injection -> output sinks.
//!
//! One utterance is processed at a time; an utterance finished while another is being
//! processed is queued (at most one). Progress is published as [`StatusEvent`]s on a
//! `tokio::sync::broadcast` channel (the overlay, T1.3, subscribes to it).

use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, warn};
use v2c_core::audio::AudioClip;
use v2c_core::config::AppRule;
use v2c_core::inject_plan::{plan, profile_for};
use v2c_core::pipeline::{Output, Pipeline};
use v2c_core::trigger::{Command, Event, Trigger, TriggerConfig};
use v2c_core::{Mode, V2cError};
use v2c_platform::{Foreground, InjectError, Injector};

use crate::recorder::AudioSource;
use crate::sinks::{OutputContext, SinkRegistry};

/// Controller progress, for the overlay and the UI. Carries codes and numbers, never text.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StatusEvent {
    /// Recording; `rms` is the current input level (0.0..=1.0), sent about every 50 ms.
    Recording {
        rms: f32,
    },
    Transcribing,
    /// Emitted once LLM polish exists (T1.5).
    #[allow(dead_code)]
    Polishing,
    /// Finished. `chars == 0` means there was nothing to output (too short, silent, or
    /// nothing recognised). `latency_ms` runs from hotkey release to the end of injection.
    Done {
        chars: usize,
        latency_ms: u64,
    },
    /// The recording was cancelled (Esc, or a shortcut such as Ctrl+C).
    Cancelled,
    /// The microphone is available again after a `mic_*` error; clears that error.
    MicReady,
    Error {
        code: &'static str,
    },
}

/// Speech recognition availability; the model loads in the background at startup.
#[derive(Clone)]
pub enum AsrState {
    Loading,
    Ready(Arc<Pipeline>),
    /// Error code, e.g. `model_missing`.
    Unavailable(&'static str),
}

/// Everything the controller drives. Platform pieces are traits so tests can fake them.
pub struct Deps {
    pub source: Arc<dyn AudioSource>,
    pub injector: Arc<dyn Injector + Send + Sync>,
    pub foreground: Arc<dyn Foreground + Send + Sync>,
    pub sinks: SinkRegistry,
    pub status: broadcast::Sender<StatusEvent>,
    pub asr: watch::Receiver<AsrState>,
    pub apps: Vec<AppRule>,
    pub trigger: TriggerConfig,
}

struct Job {
    clip: AudioClip,
    mode: Mode,
    released_at: Instant,
}

/// Runs until `events` closes. Feed it hotkey events and periodic `Event::Tick`s.
pub async fn run(deps: Deps, mut events: mpsc::UnboundedReceiver<Event>) {
    let deps = Arc::new(deps);
    let mut trigger = Trigger::new(deps.trigger);
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<()>();
    let mut recording: Option<Mode> = None;
    let mut busy = false;
    let mut queued: Option<Job> = None;

    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                for cmd in trigger.on_event(ev) {
                    debug!(?cmd, "trigger command");
                    match cmd {
                        Command::StartRecording(mode) => {
                            deps.source.begin();
                            recording = Some(mode);
                            emit(&deps.status, StatusEvent::Recording { rms: 0.0 });
                        }
                        Command::StopAndProcess | Command::StopAtLimit => {
                            let Some(mode) = recording.take() else { continue };
                            let released_at = Instant::now();
                            let clip = deps.source.finish();
                            if cmd == Command::StopAtLimit {
                                info!("hands-free recording reached the time limit");
                            }
                            let job = Job { clip, mode, released_at };
                            if !busy {
                                busy = true;
                                spawn_job(Arc::clone(&deps), job, done_tx.clone());
                            } else if queued.is_none() {
                                debug!("utterance queued behind the one being processed");
                                queued = Some(job);
                            } else {
                                warn!("queue full; utterance dropped");
                                emit(&deps.status, StatusEvent::Error { code: "busy" });
                            }
                        }
                        Command::Cancel => {
                            if recording.take().is_some() {
                                deps.source.cancel();
                                emit(&deps.status, StatusEvent::Cancelled);
                            }
                        }
                    }
                }
                if matches!(ev, Event::Tick { .. }) && recording.is_some() {
                    emit(&deps.status, StatusEvent::Recording { rms: deps.source.level() });
                }
            }
            Some(()) = done_rx.recv() => {
                match queued.take() {
                    Some(job) => spawn_job(Arc::clone(&deps), job, done_tx.clone()),
                    None => busy = false,
                }
            }
        }
    }
}

fn emit(status: &broadcast::Sender<StatusEvent>, ev: StatusEvent) {
    // No subscribers is fine.
    let _ = status.send(ev);
}

fn spawn_job(deps: Arc<Deps>, job: Job, done: mpsc::UnboundedSender<()>) {
    tokio::spawn(async move {
        process(&deps, job).await;
        let _ = done.send(());
    });
}

async fn process(deps: &Deps, job: Job) {
    let pipeline = match &*deps.asr.borrow() {
        AsrState::Ready(p) => Arc::clone(p),
        AsrState::Loading => {
            emit(
                &deps.status,
                StatusEvent::Error {
                    code: "asr_loading",
                },
            );
            return;
        }
        AsrState::Unavailable(code) => {
            emit(&deps.status, StatusEvent::Error { code });
            return;
        }
    };
    emit(&deps.status, StatusEvent::Transcribing);

    let output = match pipeline.run(job.clip, job.mode).await {
        Ok(Some(out)) => out,
        Ok(None) => {
            emit(
                &deps.status,
                StatusEvent::Done {
                    chars: 0,
                    latency_ms: ms_since(job.released_at),
                },
            );
            return;
        }
        Err(e) => {
            let code = pipeline_error_code(&e);
            warn!(code, error = %e, "pipeline failed");
            emit(&deps.status, StatusEvent::Error { code });
            return;
        }
    };

    let (app_exe, inject_result, inject_ms) = inject(deps, &output).await;
    let latency_ms = ms_since(job.released_at);
    let chars = output.text.chars().count();
    // Timings, lengths, and codes only (AGENTS.md rule 1).
    info!(
        mode = ?output.mode,
        chars,
        asr_ms = output.timings.asr_ms,
        postprocess_ms = output.timings.postprocess_ms,
        llm_ms = ?output.timings.llm_ms,
        pipeline_ms = output.timings.total_ms,
        inject_ms,
        latency_ms,
        app = app_exe.as_deref().unwrap_or("?"),
        warnings = output.warnings.len(),
        "Timings"
    );
    let injected = match inject_result {
        Ok(()) => {
            emit(&deps.status, StatusEvent::Done { chars, latency_ms });
            true
        }
        Err(e) => {
            let code = inject_error_code(&e);
            warn!(code, "injection failed");
            emit(&deps.status, StatusEvent::Error { code });
            false
        }
    };
    deps.sinks
        .notify(&output, &OutputContext { app_exe, injected });
}

/// Plans and runs the injection for the current foreground app.
async fn inject(deps: &Deps, output: &Output) -> (Option<String>, Result<(), InjectError>, u64) {
    let t = Instant::now();
    let app = deps.foreground.current_app();
    let app_exe = app.map(|a| a.exe);
    let profile = profile_for(app_exe.as_deref().unwrap_or(""), &deps.apps);
    let actions = plan(&output.text, &profile);
    let injector = Arc::clone(&deps.injector);
    // SendInput waits for modifier keys to be released, so keep it off the async workers.
    let result = tokio::task::spawn_blocking(move || injector.run(&actions))
        .await
        .unwrap_or_else(|_| Err(InjectError::Os("inject task panicked".into())));
    (app_exe, result, ms_since(t))
}

fn ms_since(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

fn inject_error_code(e: &InjectError) -> &'static str {
    match e {
        InjectError::Elevated => "inject_elevated",
        InjectError::Partial { .. } => "inject_partial",
        InjectError::Os(msg) if msg.contains("paste not implemented") => "inject_paste_unsupported",
        InjectError::Os(msg) if msg.contains("modifier") => "inject_modifiers_held",
        InjectError::Os(_) => "inject_failed",
    }
}

fn pipeline_error_code(e: &V2cError) -> &'static str {
    use v2c_core::error::Stage;
    match e {
        V2cError::Audio(_) => "audio_invalid",
        V2cError::Provider { stage, .. } | V2cError::Timeout { stage, .. } => match stage {
            Stage::Asr => "asr_failed",
            Stage::Polish => "polish_failed",
            Stage::Translate => "translate_failed",
            Stage::Inject => "inject_failed",
        },
        V2cError::NotConfigured(_) => "not_configured",
        V2cError::Config(_) => "config_invalid",
        V2cError::Storage(_) => "storage_failed",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
    use v2c_core::config::Config;
    use v2c_core::inject_plan::Action;
    use v2c_core::pipeline::PipelineConfig;
    use v2c_core::trigger::Key;
    use v2c_core::Lang;
    use v2c_platform::AppInfo;

    use super::*;

    const SENTENCE: &str = "這是一句不可以出現在日誌裡的秘密句子 secret";

    struct FakeAsr;

    #[async_trait]
    impl AsrProvider for FakeAsr {
        fn name(&self) -> &str {
            "fake"
        }
        async fn transcribe(
            &self,
            _clip: &AudioClip,
            _ctx: &AsrContext,
        ) -> v2c_core::Result<Transcript> {
            Ok(Transcript {
                text: SENTENCE.into(),
                lang: Lang::Mixed,
            })
        }
    }

    #[derive(Default)]
    struct FakeSource {
        calls: Mutex<Vec<&'static str>>,
    }

    impl AudioSource for FakeSource {
        fn begin(&self) {
            self.calls.lock().unwrap().push("begin");
        }
        fn finish(&self) -> AudioClip {
            self.calls.lock().unwrap().push("finish");
            AudioClip::new(vec![0.2; 16_000])
        }
        fn cancel(&self) {
            self.calls.lock().unwrap().push("cancel");
        }
        fn level(&self) -> f32 {
            0.25
        }
    }

    struct FakeInjector {
        elevated: bool,
        runs: Mutex<Vec<Vec<Action>>>,
    }

    impl Injector for FakeInjector {
        fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
            self.runs.lock().unwrap().push(actions.to_vec());
            if self.elevated {
                Err(InjectError::Elevated)
            } else {
                Ok(())
            }
        }
    }

    struct FakeForeground;

    impl Foreground for FakeForeground {
        fn current_app(&self) -> Option<AppInfo> {
            Some(AppInfo {
                exe: "notepad.exe".into(),
                elevated: false,
                fullscreen: false,
            })
        }
    }

    struct Harness {
        events: mpsc::UnboundedSender<Event>,
        status: broadcast::Receiver<StatusEvent>,
        injector: Arc<FakeInjector>,
        source: Arc<FakeSource>,
        task: tokio::task::JoinHandle<()>,
    }

    fn harness(asr: AsrState, elevated: bool) -> Harness {
        let source = Arc::new(FakeSource::default());
        let injector = Arc::new(FakeInjector {
            elevated,
            runs: Mutex::new(vec![]),
        });
        let (status_tx, status) = broadcast::channel(64);
        let (_asr_tx, asr_rx) = watch::channel(asr);
        let cfg = Config::with_defaults();
        let deps = Deps {
            source: source.clone(),
            injector: injector.clone(),
            foreground: Arc::new(FakeForeground),
            sinks: SinkRegistry::with_defaults(),
            status: status_tx,
            asr: asr_rx,
            apps: cfg.apps.clone(),
            trigger: TriggerConfig::from(&cfg.hotkey),
        };
        let (events, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(deps, rx));
        Harness {
            events,
            status,
            injector,
            source,
            task,
        }
    }

    fn ready_pipeline() -> AsrState {
        AsrState::Ready(Arc::new(Pipeline::new(
            Arc::new(FakeAsr),
            None,
            PipelineConfig {
                polish_enabled: true,
                polish_timeout_ms: 1000,
                min_clip_ms: 300,
                vocabulary: vec![],
            },
        )))
    }

    /// Holds the trigger for 1 s (push-to-talk) with ticks in between.
    fn push_to_talk(events: &mpsc::UnboundedSender<Event>) {
        events
            .send(Event::KeyDown {
                key: Key::Trigger,
                at_ms: 1_000,
            })
            .unwrap();
        for t in (1_050..2_000).step_by(50) {
            events.send(Event::Tick { at_ms: t }).unwrap();
        }
        events
            .send(Event::KeyUp {
                key: Key::Trigger,
                at_ms: 2_000,
            })
            .unwrap();
    }

    async fn next_final(status: &mut broadcast::Receiver<StatusEvent>) -> StatusEvent {
        loop {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), status.recv())
                .await
                .expect("status timeout")
                .expect("status channel");
            if !matches!(
                ev,
                StatusEvent::Recording { .. } | StatusEvent::Transcribing
            ) {
                return ev;
            }
        }
    }

    #[tokio::test]
    async fn push_to_talk_records_transcribes_and_injects() {
        let mut h = harness(ready_pipeline(), false);
        push_to_talk(&h.events);
        let first = h.status.recv().await.unwrap();
        assert_eq!(first, StatusEvent::Recording { rms: 0.0 });
        let second = h.status.recv().await.unwrap();
        assert_eq!(second, StatusEvent::Recording { rms: 0.25 });
        match next_final(&mut h.status).await {
            StatusEvent::Done { chars, .. } => assert_eq!(chars, SENTENCE.chars().count()),
            other => panic!("unexpected {other:?}"),
        }
        let runs = h.injector.runs.lock().unwrap().clone();
        assert_eq!(runs, vec![vec![Action::Text(SENTENCE.into())]]);
        assert_eq!(*h.source.calls.lock().unwrap(), vec!["begin", "finish"]);
        h.task.abort();
    }

    #[tokio::test]
    async fn escape_cancels_without_injecting() {
        let mut h = harness(ready_pipeline(), false);
        let e = &h.events;
        e.send(Event::KeyDown {
            key: Key::Trigger,
            at_ms: 1_000,
        })
        .unwrap();
        e.send(Event::KeyDown {
            key: Key::Escape,
            at_ms: 1_500,
        })
        .unwrap();
        assert_eq!(next_final(&mut h.status).await, StatusEvent::Cancelled);
        assert!(h.injector.runs.lock().unwrap().is_empty());
        assert_eq!(*h.source.calls.lock().unwrap(), vec!["begin", "cancel"]);
        h.task.abort();
    }

    #[tokio::test]
    async fn missing_model_reports_error_code() {
        let mut h = harness(AsrState::Unavailable("model_missing"), false);
        push_to_talk(&h.events);
        assert_eq!(
            next_final(&mut h.status).await,
            StatusEvent::Error {
                code: "model_missing"
            }
        );
        assert!(h.injector.runs.lock().unwrap().is_empty());
        h.task.abort();
    }

    #[tokio::test]
    async fn inject_failure_reports_code() {
        let mut h = harness(ready_pipeline(), true);
        push_to_talk(&h.events);
        assert_eq!(
            next_final(&mut h.status).await,
            StatusEvent::Error {
                code: "inject_elevated"
            }
        );
        h.task.abort();
    }

    #[test]
    fn error_codes() {
        assert_eq!(
            inject_error_code(&InjectError::Partial { sent: 1, total: 2 }),
            "inject_partial"
        );
        assert_eq!(
            inject_error_code(&InjectError::Os("paste not implemented (T1.2)".into())),
            "inject_paste_unsupported"
        );
        assert_eq!(
            pipeline_error_code(&V2cError::NotConfigured("translation model")),
            "not_configured"
        );
    }

    #[test]
    fn status_event_serializes_with_kind_tag() {
        let v = serde_json::to_value(StatusEvent::Recording { rms: 0.5 }).unwrap();
        assert_eq!(v, serde_json::json!({ "kind": "recording", "rms": 0.5 }));
        let v = serde_json::to_value(StatusEvent::Error {
            code: "model_missing",
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "kind": "error", "code": "model_missing" })
        );
    }

    /// AGENTS.md rule 1 / plan section 7: run a known sentence through the controller and the
    /// pipeline with the real log formatter at TRACE level; the log must not contain it.
    #[tokio::test]
    async fn logs_never_contain_the_sentence() {
        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Buf::default();
        let writer = buf.clone();
        let subscriber = crate::logging::subscriber(
            move || writer.clone(),
            tracing_subscriber::EnvFilter::new("trace"),
        );
        // Global (not thread-scoped) so callsite-interest caching by tests running in parallel
        // cannot hide events, and logs from spawned/blocking threads are captured too. Other
        // tests in this binary then log here as well; they use the same sentence.
        tracing::subscriber::set_global_default(subscriber)
            .expect("only this test installs a global subscriber");

        let mut h = harness(ready_pipeline(), false);
        push_to_talk(&h.events);
        assert!(matches!(
            next_final(&mut h.status).await,
            StatusEvent::Done { .. }
        ));
        h.task.abort();

        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("pipeline done"),
            "pipeline log missing:\n{log}"
        );
        assert!(log.contains("Timings"), "controller log missing:\n{log}");
        assert!(!log.contains(SENTENCE), "sentence leaked:\n{log}");
        for word in ["秘密", "secret", "日誌裡"] {
            assert!(!log.contains(word), "`{word}` leaked:\n{log}");
        }
    }
}
