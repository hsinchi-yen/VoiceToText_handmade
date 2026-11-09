//! The coordinator (`tasks/plan.md` section 7): hotkey events -> trigger state machine ->
//! recorder -> pipeline -> injection -> output sinks.
//!
//! One utterance is processed at a time; an utterance finished while another is being
//! processed is queued (at most one). Progress is published as [`StatusEvent`]s on a
//! `tokio::sync::broadcast` channel (the overlay subscribes to it).
//!
//! Providers come from a [`Backend`] (the app's `engine::Engine`): it picks the ASR / LLM for
//! each utterance (local engines, LM Studio, cloud with fallback), so swapping engines never
//! affects an utterance that is already running.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, info, warn};
use v2c_core::audio::AudioClip;
use v2c_core::config::Config;
use v2c_core::inject_plan::{plan, profile_for, Action};
use v2c_core::pipeline::{Output, Pipeline, Progress};
use v2c_core::trigger::{Command, Event, Key, Trigger, TriggerConfig};
use v2c_core::{Mode, V2cError};
use v2c_platform::{Foreground, InjectError, Injector};

use crate::providers::notice_codes;
use crate::recorder::AudioSource;
use crate::sinks::{OutputContext, SinkRegistry};

/// Status code shown after a hands-free recording hit `hotkey.hands_free_max_secs` (the clip
/// is still processed).
pub const HANDS_FREE_LIMIT: &str = "hands_free_limit";
/// Translate pressed but no LLM (LM Studio down and no cloud translate service).
pub const TRANSLATE_UNAVAILABLE: &str = "translate_unavailable";

/// Controller progress, for the overlay and the UI. Carries codes and numbers, never text.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StatusEvent {
    /// Recording; `rms` is the current input level (0.0..=1.0), sent about every 50 ms.
    /// `hands_free`: double-tap mode (key released; a single tap stops).
    Recording {
        rms: f32,
        hands_free: bool,
    },
    Transcribing,
    /// The LLM step (polish, or translation in translate mode) started.
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
    /// An error, or a notice after `Done` (e.g. `polish_fell_back`, `hands_free_limit`).
    Error {
        code: &'static str,
    },
}

/// One utterance's providers, chosen by the [`Backend`].
pub struct Prepared {
    pub pipeline: Pipeline,
    /// Set by the fallback wrappers when a cloud call failed and local was used (or the cloud
    /// provider could not be built).
    pub cloud_fell_back: Arc<AtomicBool>,
    /// Polish runs without LM Studio; show `polish_fell_back` (once per outage).
    pub polish_offline: bool,
    /// Other notice codes to show after `Done`.
    pub notices: Vec<&'static str>,
    /// Held until the utterance is finished (the VRAM guard never unloads while busy).
    pub _busy: Option<Box<dyn Send + Sync>>,
}

/// Provider selection for the controller.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Recording started (e.g. load Breeze on demand). Must return quickly.
    fn on_press(&self, mode: Mode);
    /// Providers for one utterance, or the error code to show.
    async fn prepare(&self, mode: Mode) -> Result<Prepared, &'static str>;
}

/// Everything the controller drives. Platform pieces are traits so tests can fake them.
pub struct Deps {
    pub source: Arc<dyn AudioSource>,
    pub injector: Arc<dyn Injector + Send + Sync>,
    pub foreground: Arc<dyn Foreground + Send + Sync>,
    pub sinks: SinkRegistry,
    pub status: broadcast::Sender<StatusEvent>,
    pub backend: Arc<dyn Backend>,
    /// Live config (hotkey thresholds and app rules are re-read when it changes).
    pub config: watch::Receiver<Arc<Config>>,
}

struct Job {
    clip: AudioClip,
    mode: Mode,
    released_at: Instant,
    /// Hands-free recording stopped at the time limit.
    at_limit: bool,
}

/// Runs until `events` closes. Feed it hotkey events and periodic `Event::Tick`s.
pub async fn run(deps: Deps, mut events: mpsc::UnboundedReceiver<Event>) {
    let deps = Arc::new(deps);
    let mut config = deps.config.clone();
    let mut trigger_cfg = TriggerConfig::from(&config.borrow_and_update().hotkey);
    let mut trigger = Trigger::new(trigger_cfg);
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<()>();
    let mut recording: Option<Mode> = None;
    let mut hands_free = false;
    let mut trigger_down = false;
    let mut busy = false;
    let mut queued: Option<Job> = None;

    loop {
        // Config reload: new thresholds apply from the next recording.
        if recording.is_none() && config.has_changed().unwrap_or(false) {
            let new = TriggerConfig::from(&config.borrow_and_update().hotkey);
            if new != trigger_cfg {
                info!("hotkey thresholds reloaded");
                trigger_cfg = new;
                trigger = Trigger::new(new);
            }
        }
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                let cmds = trigger.on_event(ev);
                if let Event::KeyDown { key: Key::Trigger, .. } = ev {
                    // A fresh press during a recording that produced no command is the second
                    // tap of a double tap: hands-free mode.
                    if !trigger_down && cmds.is_empty() && recording.is_some() && !hands_free {
                        hands_free = true;
                        info!("hands-free recording");
                        emit(
                            &deps.status,
                            StatusEvent::Recording { rms: deps.source.level(), hands_free },
                        );
                    }
                }
                match ev {
                    Event::KeyDown { key: Key::Trigger, .. } => trigger_down = true,
                    Event::KeyUp { key: Key::Trigger, .. } => trigger_down = false,
                    _ => {}
                }
                for cmd in cmds {
                    debug!(?cmd, "trigger command");
                    match cmd {
                        Command::StartRecording(mode) => {
                            deps.source.begin();
                            deps.backend.on_press(mode);
                            recording = Some(mode);
                            hands_free = false;
                            emit(
                                &deps.status,
                                StatusEvent::Recording { rms: 0.0, hands_free },
                            );
                        }
                        Command::StopAndProcess | Command::StopAtLimit => {
                            let Some(mode) = recording.take() else { continue };
                            hands_free = false;
                            let released_at = Instant::now();
                            let clip = deps.source.finish();
                            let at_limit = cmd == Command::StopAtLimit;
                            if at_limit {
                                info!("hands-free recording reached the time limit");
                            }
                            let job = Job { clip, mode, released_at, at_limit };
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
                                hands_free = false;
                                deps.source.cancel();
                                emit(&deps.status, StatusEvent::Cancelled);
                            }
                        }
                    }
                }
                if matches!(ev, Event::Tick { .. }) && recording.is_some() {
                    emit(
                        &deps.status,
                        StatusEvent::Recording { rms: deps.source.level(), hands_free },
                    );
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
    let limit_notice = job.at_limit.then_some(HANDS_FREE_LIMIT);
    let prepared = match deps.backend.prepare(job.mode).await {
        Ok(p) => p,
        Err(code) => {
            warn!(code, mode = ?job.mode, "no provider for this utterance");
            emit(&deps.status, StatusEvent::Error { code });
            return;
        }
    };
    emit(&deps.status, StatusEvent::Transcribing);

    let status = deps.status.clone();
    let progress = move |p: Progress| match p {
        Progress::LlmStarted(_) => emit(&status, StatusEvent::Polishing),
    };
    let result = prepared
        .pipeline
        .run_with_progress(job.clip, job.mode, &progress)
        .await;
    let output = match result {
        Ok(Some(out)) => out,
        Ok(None) => {
            emit(
                &deps.status,
                StatusEvent::Done {
                    chars: 0,
                    latency_ms: ms_since(job.released_at),
                },
            );
            if let Some(code) = limit_notice {
                emit(&deps.status, StatusEvent::Error { code });
            }
            return;
        }
        Err(e) => {
            let code = pipeline_error_code(&e, job.mode);
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
            let mut codes = notice_codes(
                &output.warnings,
                prepared.cloud_fell_back.load(Ordering::SeqCst),
                prepared.polish_offline,
            );
            codes.extend(prepared.notices.iter().copied());
            codes.extend(limit_notice);
            for code in codes {
                info!(code, "notice");
                emit(&deps.status, StatusEvent::Error { code });
            }
            true
        }
        Err(e) => {
            let code = inject_error_code(&e);
            warn!(code, "injection failed");
            emit(&deps.status, StatusEvent::Error { code });
            false
        }
    };
    drop(prepared);
    deps.sinks
        .notify(&output, &OutputContext { app_exe, injected });
}

/// An injection error after the paste fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectFailure {
    pub error: InjectError,
    /// Part of the text was already typed when the error happened.
    pub after_partial_typing: bool,
}

/// Runs the actions; if the OS accepted only part of the typing, pastes the rest
/// (`v2c_platform::windows::inject::run_with_fallback`).
fn run_injection(
    injector: &(dyn Injector + Send + Sync),
    actions: &[Action],
) -> Result<(), InjectFailure> {
    #[cfg(windows)]
    {
        v2c_platform::windows::inject::run_with_fallback(injector, actions).map_err(|e| {
            InjectFailure {
                error: e.error,
                after_partial_typing: e.after_partial_typing,
            }
        })
    }
    #[cfg(not(windows))]
    {
        injector.run(actions).map_err(|error| InjectFailure {
            error,
            after_partial_typing: false,
        })
    }
}

/// Plans and runs the injection for the current foreground app.
async fn inject(deps: &Deps, output: &Output) -> (Option<String>, Result<(), InjectFailure>, u64) {
    let t = Instant::now();
    let app = deps.foreground.current_app();
    let app_exe = app.map(|a| a.exe);
    let profile = {
        let cfg = deps.config.borrow();
        profile_for(app_exe.as_deref().unwrap_or(""), &cfg.apps)
    };
    let actions = plan(&output.text, &profile);
    let injector = Arc::clone(&deps.injector);
    // SendInput waits for modifier keys to be released, so keep it off the async workers.
    let result = tokio::task::spawn_blocking(move || run_injection(injector.as_ref(), &actions))
        .await
        .unwrap_or_else(|_| {
            Err(InjectFailure {
                error: InjectError::Os("inject task panicked".into()),
                after_partial_typing: false,
            })
        });
    (app_exe, result, ms_since(t))
}

fn ms_since(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// Overlay code for an injection failure. Elevated: the whole text is on the clipboard; after
/// partial typing only the rest is (or the paste of the rest failed).
pub fn inject_error_code(f: &InjectFailure) -> &'static str {
    match (&f.error, f.after_partial_typing) {
        (InjectError::Partial { .. }, _) | (_, true) => "inject_partial",
        (InjectError::Elevated, false) => "inject_elevated",
        (InjectError::ModifiersHeld, false) => "inject_modifiers_held",
        (InjectError::Os(_), false) => "inject_failed",
    }
}

fn pipeline_error_code(e: &V2cError, mode: Mode) -> &'static str {
    use v2c_core::error::Stage;
    match e {
        V2cError::Audio(_) => "audio_invalid",
        V2cError::Provider { stage, .. } | V2cError::Timeout { stage, .. } => match stage {
            Stage::Asr => "asr_failed",
            Stage::Polish => "polish_failed",
            Stage::Translate => "translate_failed",
            Stage::Inject => "inject_failed",
        },
        // Translate without any LLM (the backend normally catches this first).
        V2cError::NotConfigured(_) if mode == Mode::TranslateZhToEn => TRANSLATE_UNAVAILABLE,
        V2cError::NotConfigured(_) => "not_configured",
        V2cError::Config(_) => "config_invalid",
        V2cError::Storage(_) => "storage_failed",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
    use v2c_core::llm::LlmProvider;
    use v2c_core::pipeline::PipelineConfig;
    use v2c_core::Lang;
    use v2c_platform::AppInfo;

    use super::*;

    const SENTENCE: &str = "這是一句不可以出現在日誌裡的秘密句子 secret";
    const POLISHED: &str = "這是潤飾後也不可以出現在日誌裡的秘密句子 secret";

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

    struct FakeLlm;

    #[async_trait]
    impl LlmProvider for FakeLlm {
        fn name(&self) -> &str {
            "fake-llm"
        }
        async fn complete(&self, _system: &str, _user: &str) -> v2c_core::Result<String> {
            Ok(POLISHED.into())
        }
    }

    /// Backend fake: a fixed ASR, optional LLM, or a fixed error code.
    struct FakeBackend {
        result: Result<(), &'static str>,
        llm: bool,
        polish_offline: bool,
        cloud_fell_back: bool,
        presses: Mutex<Vec<Mode>>,
    }

    impl FakeBackend {
        fn ok() -> Self {
            Self {
                result: Ok(()),
                llm: false,
                polish_offline: false,
                cloud_fell_back: false,
                presses: Mutex::new(vec![]),
            }
        }
        fn err(code: &'static str) -> Self {
            Self {
                result: Err(code),
                ..Self::ok()
            }
        }
    }

    #[async_trait]
    impl Backend for FakeBackend {
        fn on_press(&self, mode: Mode) {
            self.presses.lock().unwrap().push(mode);
        }
        async fn prepare(&self, _mode: Mode) -> Result<Prepared, &'static str> {
            self.result?;
            let llm = self.llm.then(|| Arc::new(FakeLlm) as Arc<dyn LlmProvider>);
            Ok(Prepared {
                pipeline: Pipeline::new(
                    Arc::new(FakeAsr),
                    llm,
                    PipelineConfig {
                        polish_enabled: true,
                        polish_timeout_ms: 1000,
                        min_clip_ms: 300,
                        vocabulary: vec![],
                    },
                ),
                cloud_fell_back: Arc::new(AtomicBool::new(self.cloud_fell_back)),
                polish_offline: self.polish_offline,
                notices: vec![],
                _busy: None,
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
        error: Option<InjectError>,
        runs: Mutex<Vec<Vec<Action>>>,
    }

    impl Injector for FakeInjector {
        fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
            self.runs.lock().unwrap().push(actions.to_vec());
            match &self.error {
                Some(e) => Err(e.clone()),
                None => Ok(()),
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
        backend: Arc<FakeBackend>,
        config: watch::Sender<Arc<Config>>,
        task: tokio::task::JoinHandle<()>,
    }

    fn harness_with(backend: FakeBackend, error: Option<InjectError>, cfg: Config) -> Harness {
        let source = Arc::new(FakeSource::default());
        let injector = Arc::new(FakeInjector {
            error,
            runs: Mutex::new(vec![]),
        });
        let backend = Arc::new(backend);
        let (status_tx, status) = broadcast::channel(256);
        let (config, config_rx) = watch::channel(Arc::new(cfg));
        let deps = Deps {
            source: source.clone(),
            injector: injector.clone(),
            foreground: Arc::new(FakeForeground),
            sinks: SinkRegistry::with_defaults(),
            status: status_tx,
            backend: backend.clone(),
            config: config_rx,
        };
        let (events, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(deps, rx));
        Harness {
            events,
            status,
            injector,
            source,
            backend,
            config,
            task,
        }
    }

    fn harness(backend: FakeBackend, error: Option<InjectError>) -> Harness {
        harness_with(backend, error, Config::with_defaults())
    }

    fn down(e: &mpsc::UnboundedSender<Event>, key: Key, at_ms: u64) {
        e.send(Event::KeyDown { key, at_ms }).unwrap();
    }

    fn up(e: &mpsc::UnboundedSender<Event>, key: Key, at_ms: u64) {
        e.send(Event::KeyUp { key, at_ms }).unwrap();
    }

    fn ticks(e: &mpsc::UnboundedSender<Event>, from: u64, to: u64) {
        for t in (from..to).step_by(50) {
            e.send(Event::Tick { at_ms: t }).unwrap();
        }
    }

    /// Holds the trigger for 1 s (push-to-talk) with ticks in between.
    fn push_to_talk(events: &mpsc::UnboundedSender<Event>) {
        down(events, Key::Trigger, 1_000);
        ticks(events, 1_050, 2_000);
        up(events, Key::Trigger, 2_000);
    }

    /// Next status event (skipping over lag from the 50 ms level updates).
    async fn recv(status: &mut broadcast::Receiver<StatusEvent>) -> StatusEvent {
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), status.recv())
                .await
                .expect("status timeout")
            {
                Ok(ev) => return ev,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(e) => panic!("status channel: {e}"),
            }
        }
    }

    /// Next event that is not a progress event.
    async fn next_final(status: &mut broadcast::Receiver<StatusEvent>) -> StatusEvent {
        loop {
            let ev = recv(status).await;
            if !matches!(
                ev,
                StatusEvent::Recording { .. } | StatusEvent::Transcribing | StatusEvent::Polishing
            ) {
                return ev;
            }
        }
    }

    /// Every event until (and including) `Done` or an error, then `extra` more.
    async fn collect(
        status: &mut broadcast::Receiver<StatusEvent>,
        extra: usize,
    ) -> Vec<StatusEvent> {
        let mut out = Vec::new();
        loop {
            let ev = recv(status).await;
            let last = matches!(ev, StatusEvent::Done { .. } | StatusEvent::Error { .. });
            out.push(ev);
            if last {
                break;
            }
        }
        for _ in 0..extra {
            out.push(recv(status).await);
        }
        out
    }

    #[tokio::test]
    async fn push_to_talk_records_transcribes_and_injects() {
        let mut h = harness(FakeBackend::ok(), None);
        push_to_talk(&h.events);
        let first = h.status.recv().await.unwrap();
        assert_eq!(
            first,
            StatusEvent::Recording {
                rms: 0.0,
                hands_free: false
            }
        );
        let second = h.status.recv().await.unwrap();
        assert_eq!(
            second,
            StatusEvent::Recording {
                rms: 0.25,
                hands_free: false
            }
        );
        match next_final(&mut h.status).await {
            StatusEvent::Done { chars, .. } => assert_eq!(chars, SENTENCE.chars().count()),
            other => panic!("unexpected {other:?}"),
        }
        let runs = h.injector.runs.lock().unwrap().clone();
        assert_eq!(runs, vec![vec![Action::Text(SENTENCE.into())]]);
        assert_eq!(*h.source.calls.lock().unwrap(), vec!["begin", "finish"]);
        assert_eq!(*h.backend.presses.lock().unwrap(), vec![Mode::Dictate]);
        h.task.abort();
    }

    #[tokio::test]
    async fn polish_emits_polishing_and_injects_polished_text() {
        let mut h = harness(
            FakeBackend {
                llm: true,
                ..FakeBackend::ok()
            },
            None,
        );
        push_to_talk(&h.events);
        let evs = collect(&mut h.status, 0).await;
        let i_t = evs
            .iter()
            .position(|e| *e == StatusEvent::Transcribing)
            .unwrap();
        let i_p = evs
            .iter()
            .position(|e| *e == StatusEvent::Polishing)
            .unwrap();
        assert!(i_t < i_p, "{evs:?}");
        assert!(matches!(evs.last(), Some(StatusEvent::Done { .. })));
        let runs = h.injector.runs.lock().unwrap().clone();
        assert_eq!(runs, vec![vec![Action::Text(POLISHED.into())]]);
        h.task.abort();
    }

    #[tokio::test]
    async fn notices_follow_done() {
        let mut h = harness(
            FakeBackend {
                polish_offline: true,
                cloud_fell_back: true,
                ..FakeBackend::ok()
            },
            None,
        );
        push_to_talk(&h.events);
        let evs = collect(&mut h.status, 2).await;
        let n = evs.len();
        assert!(matches!(evs[n - 3], StatusEvent::Done { .. }), "{evs:?}");
        assert_eq!(
            evs[n - 2..],
            [
                StatusEvent::Error {
                    code: "cloud_fell_back"
                },
                StatusEvent::Error {
                    code: "polish_fell_back"
                }
            ]
        );
        h.task.abort();
    }

    #[tokio::test]
    async fn escape_cancels_without_injecting() {
        let mut h = harness(FakeBackend::ok(), None);
        down(&h.events, Key::Trigger, 1_000);
        down(&h.events, Key::Escape, 1_500);
        assert_eq!(next_final(&mut h.status).await, StatusEvent::Cancelled);
        assert!(h.injector.runs.lock().unwrap().is_empty());
        assert_eq!(*h.source.calls.lock().unwrap(), vec!["begin", "cancel"]);
        h.task.abort();
    }

    #[tokio::test]
    async fn double_tap_enters_hands_free_and_single_tap_stops() {
        let mut h = harness(FakeBackend::ok(), None);
        let e = &h.events;
        down(e, Key::Trigger, 1_000);
        up(e, Key::Trigger, 1_100);
        down(e, Key::Trigger, 1_250);
        up(e, Key::Trigger, 1_300);
        // Long dictation with the key released.
        ticks(e, 1_350, 60_000);
        loop {
            if let StatusEvent::Recording {
                hands_free: true, ..
            } = recv(&mut h.status).await
            {
                break;
            }
        }
        down(e, Key::Trigger, 60_000);
        up(e, Key::Trigger, 60_050);
        assert!(matches!(
            next_final(&mut h.status).await,
            StatusEvent::Done { .. }
        ));
        assert_eq!(h.injector.runs.lock().unwrap().len(), 1);
        assert_eq!(*h.source.calls.lock().unwrap(), vec!["begin", "finish"]);
        h.task.abort();
    }

    #[tokio::test]
    async fn auto_repeat_while_holding_is_not_hands_free() {
        let mut h = harness(FakeBackend::ok(), None);
        let e = &h.events;
        down(e, Key::Trigger, 1_000);
        down(e, Key::Trigger, 1_030);
        down(e, Key::Trigger, 1_060);
        ticks(e, 1_100, 1_500);
        up(e, Key::Trigger, 1_500);
        loop {
            match recv(&mut h.status).await {
                StatusEvent::Recording { hands_free, .. } => assert!(!hands_free),
                StatusEvent::Done { .. } => break,
                _ => {}
            }
        }
        h.task.abort();
    }

    #[tokio::test]
    async fn hands_free_escape_cancels_without_output() {
        let mut h = harness(FakeBackend::ok(), None);
        let e = &h.events;
        down(e, Key::Trigger, 1_000);
        up(e, Key::Trigger, 1_100);
        down(e, Key::Trigger, 1_250);
        up(e, Key::Trigger, 1_300);
        ticks(e, 1_350, 5_000);
        down(e, Key::Escape, 5_000);
        assert_eq!(next_final(&mut h.status).await, StatusEvent::Cancelled);
        assert!(h.injector.runs.lock().unwrap().is_empty());
        h.task.abort();
    }

    #[tokio::test]
    async fn hands_free_limit_processes_and_then_tells_the_user() {
        let mut cfg = Config::with_defaults();
        cfg.hotkey.hands_free_max_secs = 2;
        let mut h = harness_with(FakeBackend::ok(), None, cfg);
        let e = &h.events;
        down(e, Key::Trigger, 1_000);
        up(e, Key::Trigger, 1_100);
        down(e, Key::Trigger, 1_250);
        up(e, Key::Trigger, 1_300);
        ticks(e, 1_350, 3_100);
        let evs = collect(&mut h.status, 1).await;
        let n = evs.len();
        assert!(matches!(evs[n - 2], StatusEvent::Done { .. }), "{evs:?}");
        assert_eq!(
            evs[n - 1],
            StatusEvent::Error {
                code: HANDS_FREE_LIMIT
            }
        );
        assert_eq!(h.injector.runs.lock().unwrap().len(), 1);
        h.task.abort();
    }

    #[tokio::test]
    async fn translate_mode_flows_through_and_reports_missing_llm() {
        let mut h = harness(FakeBackend::err(TRANSLATE_UNAVAILABLE), None);
        let e = &h.events;
        down(e, Key::Shift, 900);
        down(e, Key::Trigger, 1_000);
        ticks(e, 1_050, 2_000);
        up(e, Key::Trigger, 2_000);
        assert_eq!(
            next_final(&mut h.status).await,
            StatusEvent::Error {
                code: TRANSLATE_UNAVAILABLE
            }
        );
        assert_eq!(
            *h.backend.presses.lock().unwrap(),
            vec![Mode::TranslateZhToEn]
        );
        assert!(h.injector.runs.lock().unwrap().is_empty());
        h.task.abort();
    }

    #[tokio::test]
    async fn translate_with_llm_injects_english() {
        let mut h = harness(
            FakeBackend {
                llm: true,
                ..FakeBackend::ok()
            },
            None,
        );
        let e = &h.events;
        down(e, Key::Shift, 900);
        down(e, Key::Trigger, 1_000);
        ticks(e, 1_050, 2_000);
        up(e, Key::Trigger, 2_000);
        assert!(matches!(
            next_final(&mut h.status).await,
            StatusEvent::Done { .. }
        ));
        // The fake LLM's reply is what gets typed.
        assert_eq!(
            h.injector.runs.lock().unwrap()[0],
            vec![Action::Text(POLISHED.into())]
        );
        h.task.abort();
    }

    #[tokio::test]
    async fn backend_error_code_is_reported() {
        let mut h = harness(FakeBackend::err("model_missing"), None);
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
        let mut h = harness(FakeBackend::ok(), Some(InjectError::Elevated));
        push_to_talk(&h.events);
        assert_eq!(
            next_final(&mut h.status).await,
            StatusEvent::Error {
                code: "inject_elevated"
            }
        );
        h.task.abort();
    }

    #[tokio::test]
    async fn config_reload_changes_hold_threshold() {
        let mut h = harness(FakeBackend::ok(), None);
        let mut cfg = Config::with_defaults();
        cfg.hotkey.hold_threshold_ms = 2_000;
        h.config.send(Arc::new(cfg)).unwrap();
        // Let the controller see the change before the next press.
        h.events.send(Event::Tick { at_ms: 10 }).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // A 1 s hold is now a tap: nothing is processed; the tap expires and cancels.
        push_to_talk(&h.events);
        ticks(&h.events, 2_050, 3_000);
        assert_eq!(next_final(&mut h.status).await, StatusEvent::Cancelled);
        assert!(h.injector.runs.lock().unwrap().is_empty());
        h.task.abort();
    }

    #[test]
    fn inject_error_codes() {
        let f = |error, after_partial_typing| InjectFailure {
            error,
            after_partial_typing,
        };
        assert_eq!(
            inject_error_code(&f(InjectError::Elevated, false)),
            "inject_elevated"
        );
        assert_eq!(
            inject_error_code(&f(InjectError::Elevated, true)),
            "inject_partial"
        );
        assert_eq!(
            inject_error_code(&f(InjectError::Partial { sent: 1, total: 2 }, false)),
            "inject_partial"
        );
        assert_eq!(
            inject_error_code(&f(InjectError::ModifiersHeld, false)),
            "inject_modifiers_held"
        );
        assert_eq!(
            inject_error_code(&f(InjectError::Os("x".into()), false)),
            "inject_failed"
        );
        assert_eq!(
            inject_error_code(&f(InjectError::Os("x".into()), true)),
            "inject_partial"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn partial_typing_pastes_the_rest() {
        /// Accepts 2 of the typed key events the first time, then everything.
        struct PartialOnce(Mutex<Vec<Vec<Action>>>);
        impl Injector for PartialOnce {
            fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
                let mut runs = self.0.lock().unwrap();
                runs.push(actions.to_vec());
                if runs.len() == 1 {
                    Err(InjectError::Partial { sent: 2, total: 6 })
                } else {
                    Ok(())
                }
            }
        }
        let inj = PartialOnce(Mutex::new(vec![]));
        assert_eq!(run_injection(&inj, &[Action::Text("abc".into())]), Ok(()));
        assert_eq!(inj.0.lock().unwrap()[1], vec![Action::Paste("bc".into())]);
    }

    #[test]
    fn pipeline_error_codes() {
        assert_eq!(
            pipeline_error_code(&V2cError::NotConfigured("translation model"), Mode::Dictate),
            "not_configured"
        );
        assert_eq!(
            pipeline_error_code(
                &V2cError::NotConfigured("translation model"),
                Mode::TranslateZhToEn
            ),
            TRANSLATE_UNAVAILABLE
        );
    }

    #[test]
    fn status_event_serializes_with_kind_tag() {
        let v = serde_json::to_value(StatusEvent::Recording {
            rms: 0.5,
            hands_free: true,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({ "kind": "recording", "rms": 0.5, "hands_free": true })
        );
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
    /// pipeline (ASR and LLM polish) with the real log formatter at TRACE level; the log must
    /// not contain it.
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
        // tests in this binary then log here as well; they use the same sentences.
        tracing::subscriber::set_global_default(subscriber)
            .expect("only this test installs a global subscriber");

        // Polish path (LLM output), plus fallback notices.
        let mut h = harness(
            FakeBackend {
                llm: true,
                polish_offline: true,
                ..FakeBackend::ok()
            },
            None,
        );
        push_to_talk(&h.events);
        assert!(matches!(
            next_final(&mut h.status).await,
            StatusEvent::Done { .. }
        ));
        h.task.abort();

        // Cloud fallback wrappers around failing / working providers.
        {
            use crate::providers::{FallbackAsr, FallbackLlm};
            struct Failing;
            #[async_trait]
            impl LlmProvider for Failing {
                fn name(&self) -> &str {
                    "failing"
                }
                async fn complete(&self, _: &str, _: &str) -> v2c_core::Result<String> {
                    Err(V2cError::Timeout {
                        stage: v2c_core::error::Stage::Polish,
                        ms: 1,
                    })
                }
            }
            #[async_trait]
            impl AsrProvider for Failing {
                fn name(&self) -> &str {
                    "failing"
                }
                async fn transcribe(
                    &self,
                    _: &AudioClip,
                    _: &AsrContext,
                ) -> v2c_core::Result<Transcript> {
                    Err(V2cError::Timeout {
                        stage: v2c_core::error::Stage::Asr,
                        ms: 1,
                    })
                }
            }
            let flag = Arc::new(AtomicBool::new(false));
            let llm = FallbackLlm {
                primary: Arc::new(Failing),
                fallback: Arc::new(FakeLlm),
                fell_back: flag.clone(),
            };
            assert_eq!(llm.complete("sys", SENTENCE).await.unwrap(), POLISHED);
            let asr = FallbackAsr {
                primary: Arc::new(Failing),
                fallback: Arc::new(FakeAsr),
                fell_back: flag.clone(),
            };
            let p = Pipeline::new(
                Arc::new(asr),
                Some(Arc::new(llm)),
                crate::app_state::pipeline_config(&Config::with_defaults()),
            );
            let out = p
                .run(AudioClip::new(vec![0.2; 16_000]), Mode::Dictate)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(out.text, POLISHED);
            assert!(flag.load(Ordering::SeqCst));
        }

        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(
            log.contains("pipeline done"),
            "pipeline log missing:\n{log}"
        );
        assert!(log.contains("Timings"), "controller log missing:\n{log}");
        assert!(
            log.contains("cloud LLM failed"),
            "fallback log missing:\n{log}"
        );
        for s in [SENTENCE, POLISHED] {
            assert!(!log.contains(s), "sentence leaked:\n{log}");
        }
        for word in ["秘密", "secret", "日誌裡", "潤飾後"] {
            assert!(!log.contains(word), "`{word}` leaked:\n{log}");
        }
    }
}
