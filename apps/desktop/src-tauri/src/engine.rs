//! Runtime state behind the controller: live config, local engines, VRAM guard, providers,
//! and LM Studio liveness. Implements [`controller::Backend`] through [`EngineHandle`].
//!
//! Threads / tasks (started by `main.rs`):
//! - `v2c-asr` thread: [`Engine::startup_blocking`] (select + load the local engines), then the
//!   VRAM guard poll every `gpu_guard::POLL_INTERVAL` ([`Engine::guard_loop`]).
//! - async task [`Engine::lm_check_loop`]: LM Studio `GET /models` every
//!   [`providers::LM_CHECK_INTERVAL`] (and right after a config reload).
//!
//! Config reload ([`Engine::reload`]) swaps the config, rebuilds providers, and asks the guard
//! to poll right away, so ASR changes take effect within a second; a GPU model whose file
//! changed (Breeze quantisation, or Breeze <-> TEA-ASR) is unloaded first.
//!
//! Only one local ASR model is in memory at a time (`local_asr.rs`, plan section 5).
//!
//! "GPU engine" is Breeze-ASR-25, or TEA-ASR-1.1 when `[local] asr = "tea_asr"` (T1.12). Both
//! follow the same selection and VRAM rules (`v2c_asr_local::{select, gpu_guard}`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{broadcast, watch, Notify};
use tracing::{debug, info, warn};
use v2c_asr_local::gpu_guard::POLL_INTERVAL;
use v2c_asr_local::{
    gpu_engine, gpu_vram_estimate_mb, pick_with_reason, ForegroundInfo, GpuGuard, GpuInfo,
    GpuMonitor, GpuSnapshot, GuardAction, GuardInput, GuardReason, LocalAsrKind,
};
use v2c_core::asr::wav::probe_clip;
use v2c_core::asr::{AsrContext, AsrProvider};
use v2c_core::config::{Config, KeepLoaded, LocalAsr};
use v2c_core::llm::LlmProvider;
use v2c_core::pipeline::Pipeline;
use v2c_core::route::{CloudHttp, Purpose, Target};
use v2c_core::secrets::SecretStore;
use v2c_core::vocab::Vocab;
use v2c_core::Mode;

use crate::app_state::pipeline_config;
use crate::controller::{Backend, Prepared, StatusEvent};
use crate::gpu_task::{self, NoticeLatch, Target as SlotTarget};
use crate::local_asr::{self, LocalEngines, Startup};
use crate::paths::Paths;
use crate::providers::{self, select, wrap_asr, wrap_llm, LmHealth, Providers};

/// How long the settings test waits for startup to finish.
const ASR_LOAD_WAIT: Duration = Duration::from_secs(30);
/// Upper bound for loading one model (TEA-ASR takes about 10 s, Breeze about 3 s).
const MODEL_LOAD_WAIT: Duration = Duration::from_secs(60);
/// How long an utterance waits for the local model: a switch first waits up to
/// `local_asr::RELEASE_WAIT` for the old model to be released, then loads the new one.
const ASR_SWITCH_WAIT: Duration = local_asr::RELEASE_WAIT.saturating_add(MODEL_LOAD_WAIT);
/// After a failed GPU engine load, the guard does not retry for this long.
const GPU_RETRY_AFTER: Duration = Duration::from_secs(300);

/// A GPU load whose VRAM use is not recorded yet: the background warm-up is still running, and
/// its first inference grows the CUDA buffers, so the "after" snapshot waits for it.
struct PendingLoad {
    estimate: u64,
    before: Option<GpuSnapshot>,
    why: &'static str,
    asr: Option<&'static str>,
    started: Instant,
    loaded_at: Instant,
    warmup: Option<local_asr::WarmupDone>,
}

/// Waits for the warm-up (if any), then takes the "after" snapshot and records the load.
/// Returns the VRAM need now assumed for `estimate`.
fn record_after_warmup(
    guard: &mut GpuGuard,
    estimate: u64,
    before: Option<GpuSnapshot>,
    warmup: Option<local_asr::WarmupDone>,
    snapshot: impl FnOnce() -> Option<GpuSnapshot>,
    now: Instant,
) -> u64 {
    if let Some(done) = warmup {
        // Ends on the signal, or when the warm-up thread is gone.
        let _ = done.recv();
    }
    guard.record_load(estimate, before, snapshot(), now);
    guard.breeze_need_mb(estimate)
}

pub struct Engine {
    pending_load: Mutex<Option<PendingLoad>>,
    paths: Paths,
    config: RwLock<Arc<Config>>,
    config_tx: watch::Sender<Arc<Config>>,
    status: broadcast::Sender<StatusEvent>,
    local: LocalEngines,
    monitor: GpuMonitor,
    guard: Mutex<GpuGuard>,
    notices: Mutex<NoticeLatch>,
    providers: RwLock<Arc<Providers>>,
    lm: LmHealth,
    lm_wake: Notify,
    secrets: Arc<dyn SecretStore>,
    http: Option<CloudHttp>,
    busy: Arc<AtomicUsize>,
    last_use: Mutex<Instant>,
    gpu_failed_at: Mutex<Option<Instant>>,
    /// Personal vocabulary (`vocab.txt`), for the ASR prompt and the LLM prompts.
    vocabulary: RwLock<Arc<Vec<String>>>,
    /// Set by `reload`: poll the guard now instead of waiting for the next interval.
    poll_now: AtomicBool,
    /// Set by `reload` when the GPU model file changed: a loaded GPU model is stale.
    gpu_stale: AtomicBool,
    /// A hotkey-triggered load is about to start or running (`on_press`).
    press_loading: AtomicBool,
    /// Why the configured GPU model is not the loaded one (last decision); `None` once it is.
    cpu_reason: Mutex<Option<CpuReason>>,
    /// The settings page's 「測試」 is running.
    testing: AtomicBool,
    stop: AtomicBool,
}

/// Why the configured GPU model (Breeze or TEA-ASR) is not the loaded one: SenseVoice stands
/// in, or (idle) nothing is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuReason {
    /// The selection rules or the VRAM guard (config, no CUDA, low VRAM, heavy app, idle...).
    Guard(GuardReason),
    /// The GPU model was allowed but failed to load.
    LoadFailed,
}

/// The one local model as it is right now (settings page 「目前使用中」).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalStatus {
    /// The loaded model, which the next utterance uses; `None` when none is loaded.
    pub active: Option<LocalAsrKind>,
    /// The loaded model runs on the GPU.
    pub on_gpu: bool,
    /// The config's GPU engine (`select::gpu_engine`), for wording.
    pub gpu_engine: LocalAsrKind,
    /// A model switch (unload + load) is running; nothing is loaded meanwhile.
    pub switching: bool,
    /// Startup selection / loading has not finished.
    pub starting: bool,
    /// Nothing loaded on purpose (idle or on-demand unload); the hotkey loads it again.
    pub unloaded: bool,
    /// Error code when no engine is loaded after startup.
    pub error: Option<&'static str>,
    pub cpu_reason: Option<CpuReason>,
}

/// What actually serves a purpose, from the route and whether the cloud provider could be
/// built (mirrors [`providers::select`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PurposeUse {
    Local,
    Cloud {
        service_id: String,
    },
    /// Routed to a cloud service that cannot be built (key, URL, model); local is used.
    LocalInsteadOfCloud {
        service_id: String,
    },
    /// Routed to a cloud service that cannot be built, without fallback: nothing works.
    Unavailable {
        service_id: String,
    },
}

/// Pure: see [`PurposeUse`].
pub fn purpose_use(target: &Target, fallback_to_local: bool, cloud_built: bool) -> PurposeUse {
    match target {
        Target::Local => PurposeUse::Local,
        Target::Cloud { service_id, .. } => {
            let service_id = service_id.clone();
            if cloud_built {
                PurposeUse::Cloud { service_id }
            } else if fallback_to_local {
                PurposeUse::LocalInsteadOfCloud { service_id }
            } else {
                PurposeUse::Unavailable { service_id }
            }
        }
    }
}

/// Live routing state for the settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteStatus {
    pub asr: PurposeUse,
    pub polish: PurposeUse,
    pub translate: PurposeUse,
    /// LM Studio answered the last liveness check.
    pub lm_up: bool,
}

/// Result of the settings page's local engine test. Never holds the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTestRun {
    pub engine: LocalAsrKind,
    pub on_gpu: bool,
    /// Why SenseVoice was used instead of the GPU model (if it was).
    pub cpu_reason: Option<CpuReason>,
    /// The model was loaded by this test, taking this long.
    pub load_ms: Option<u64>,
    /// Transcription time of the test clip, or the error code.
    pub transcribe: Result<u64, &'static str>,
    /// VRAM the GPU model uses (measured at load, else the estimate), when it ran.
    pub vram_mb: Option<u64>,
}

/// Reads `vocab.txt`; a missing file is empty, a broken one is logged and treated as empty.
fn load_vocab(paths: &Paths) -> Vec<String> {
    match Vocab::load(&paths.vocab_file()) {
        Ok(v) => {
            info!(count = v.len(), "vocabulary loaded");
            v.words().to_vec()
        }
        Err(e) => {
            // The error carries the path only, never entries.
            warn!(error = %e, "vocabulary could not be read; using none");
            Vec::new()
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Engine {
    pub fn new(
        paths: Paths,
        config: Config,
        status: broadcast::Sender<StatusEvent>,
        secrets: Arc<dyn SecretStore>,
    ) -> Arc<Self> {
        let http = match v2c_core::route::cloud_http_client() {
            Ok(h) => Some(h),
            Err(e) => {
                warn!(error = %e, "HTTP client could not be created");
                None
            }
        };
        let providers = Providers::build(&config, secrets.as_ref(), http.as_ref());
        let config = Arc::new(config);
        let (config_tx, _) = watch::channel(Arc::clone(&config));
        let vocabulary = load_vocab(&paths);
        Arc::new(Self {
            paths,
            config: RwLock::new(config),
            config_tx,
            status,
            local: LocalEngines::default(),
            monitor: GpuMonitor::new(),
            guard: Mutex::new(GpuGuard::default()),
            notices: Mutex::new(NoticeLatch::default()),
            providers: RwLock::new(Arc::new(providers)),
            lm: LmHealth::default(),
            lm_wake: Notify::new(),
            secrets,
            http,
            busy: Arc::new(AtomicUsize::new(0)),
            last_use: Mutex::new(Instant::now()),
            pending_load: Mutex::new(None),
            gpu_failed_at: Mutex::new(None),
            vocabulary: RwLock::new(Arc::new(vocabulary)),
            poll_now: AtomicBool::new(false),
            gpu_stale: AtomicBool::new(false),
            press_loading: AtomicBool::new(false),
            cpu_reason: Mutex::new(None),
            testing: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read().unwrap_or_else(|p| p.into_inner()))
    }

    pub fn config_watch(&self) -> watch::Receiver<Arc<Config>> {
        self.config_tx.subscribe()
    }

    /// Re-reads `vocab.txt` (after the vocabulary page saved it, or a config reload).
    pub fn reload_vocab(&self) {
        let v = load_vocab(&self.paths);
        *self.vocabulary.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(v);
    }

    fn vocabulary(&self) -> Arc<Vec<String>> {
        Arc::clone(&self.vocabulary.read().unwrap_or_else(|p| p.into_inner()))
    }

    fn providers(&self) -> Arc<Providers> {
        Arc::clone(&self.providers.read().unwrap_or_else(|p| p.into_inner()))
    }

    fn emit(&self, code: &'static str) {
        let _ = self.status.send(StatusEvent::Error { code });
    }

    /// A CUDA device is usable for the config's GPU engine: built with `cuda` (and `tea` for
    /// TEA-ASR) and NVML found an NVIDIA GPU.
    fn cuda_available(&self, cfg: &Config) -> bool {
        let built = match gpu_engine(cfg) {
            LocalAsrKind::TeaAsr => v2c_asr_local::tea::tea_enabled(),
            _ => true,
        };
        built && v2c_asr_local::breeze::cuda_enabled() && self.monitor.available()
    }

    /// Stops the background loops (at app exit).
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.lm_wake.notify_one();
    }

    // ---- local engines ---------------------------------------------------------------
    //
    // One model in memory at a time (`local_asr.rs`). Startup loads the picked model; the
    // guard poll (`gpu_task::target`) switches between the GPU model and the SenseVoice
    // stand-in; a hotkey press loads a model when the slot is empty (idle / on demand).

    /// Picks and loads the one local model (blocking).
    pub fn startup_blocking(&self) {
        let cfg = self.config();
        let gpu_model = local_asr::gpu_model(&cfg, &self.paths);
        let sensevoice = local_asr::sensevoice_model(&self.paths);
        let snap = self.monitor.snapshot();
        let gpu = GpuInfo {
            cuda_available: self.cuda_available(&cfg),
            free_vram_mb: snap.map_or(0, |s| s.free_mb),
            total_vram_mb: snap.map_or(0, |s| s.total_mb),
            breeze_model_present: gpu_model.is_some(),
            breeze_vram_mb: gpu_vram_estimate_mb(&cfg),
        };
        let pick = pick_with_reason(&cfg, &gpu);
        info!(
            engine = ?pick.kind,
            asr = pick.kind.provider_name(),
            reason = ?pick.reason,
            cuda_feature = v2c_asr_local::breeze::cuda_enabled(),
            tea_feature = v2c_asr_local::tea::tea_enabled(),
            nvml = self.monitor.available(),
            free_vram_mb = gpu.free_vram_mb,
            total_vram_mb = gpu.total_vram_mb,
            gpu_engine = gpu_engine(&cfg).provider_name(),
            gpu_model = gpu.breeze_model_present,
            sensevoice_model = sensevoice.is_some(),
            "local ASR selected"
        );
        if pick.kind == LocalAsrKind::SenseVoice {
            *lock(&self.cpu_reason) = Some(CpuReason::Guard(GuardReason::Engine(pick.reason)));
        }
        if let Some(code) = gpu_task::reason_code(GuardReason::Engine(pick.reason)) {
            if let Some(code) = lock(&self.notices).offer(code) {
                self.emit(code);
            }
        }

        let mut error = None;
        if pick.kind != LocalAsrKind::SenseVoice {
            if let Err(code) = self.switch_to(Some(pick.kind), "startup") {
                error = Some(code);
            }
        }
        if self.local.current().is_none() && sensevoice.is_some() {
            // Picked SenseVoice, or the GPU model failed: SenseVoice is the one model.
            if let Err(code) = self.switch_to(Some(LocalAsrKind::SenseVoice), "startup") {
                error = Some(code);
            }
        } else if sensevoice.is_none() && pick.kind == LocalAsrKind::SenseVoice {
            info!("SenseVoice model not found");
        }
        let ready = self.local.current().is_some();
        let code = startup_code(ready, gpu_model.is_some() || sensevoice.is_some(), error);
        self.local.set_startup(Startup::Done(code));
        match code {
            None => info!(
                asr = self.local.current().map(|k| k.provider_name()),
                "speech recognition ready"
            ),
            Some(code) => {
                warn!(code, "local speech recognition unavailable");
                // With a cloud ASR route this is not fatal; the error shows when used.
                if self.providers().purpose(Purpose::Asr).target == Target::Local {
                    self.emit(code);
                }
            }
        }
        // After "ready": the background warm-up is still running; record its VRAM use.
        self.settle_load();
    }

    /// Finishes the VRAM accounting of the last GPU load (blocking until its warm-up ends;
    /// call it off the hot path). Does nothing when nothing is pending.
    fn settle_load(&self) {
        let Some(p) = lock(&self.pending_load).take() else {
            return;
        };
        let mut guard = lock(&self.guard);
        let need = record_after_warmup(
            &mut guard,
            p.estimate,
            p.before,
            p.warmup,
            || self.monitor.snapshot(),
            p.loaded_at,
        );
        info!(
            why = p.why,
            asr = p.asr,
            load_ms = p.started.elapsed().as_millis() as u64,
            vram_estimate_mb = p.estimate,
            vram_need_mb = need,
            "GPU ASR VRAM recorded"
        );
    }

    /// Switches the one local model to `kind` (`None` = unload), blocking: the old model is
    /// released first (see `LocalEngines::switch_model`). For a GPU model, the VRAM drop is
    /// recorded for the guard; a model whose config changed while loading is discarded.
    fn switch_to(&self, kind: Option<LocalAsrKind>, why: &'static str) -> Result<(), &'static str> {
        let cfg = self.config();
        let from = self.local.current();
        let started = Instant::now();
        let gpu = kind.filter(|k| *k != LocalAsrKind::SenseVoice);
        let estimate = gpu_vram_estimate_mb(&cfg);
        let mut before = None;
        let mut warmup = None;
        let on_gpu = kind.is_some_and(|k| local_asr::runs_on_gpu(k, &cfg));
        let result = self
            .local
            .switch_model(kind, on_gpu, local_asr::RELEASE_WAIT, || {
                let kind = kind.expect("load is only called with a model");
                before = gpu.and_then(|_| self.monitor.snapshot());
                let (asr, warm) = local_asr::load_model(kind, &cfg, &self.paths)?;
                warmup = warm;
                let now = self.config();
                if gpu.is_some()
                    && (gpu_engine(&now) != kind || !gpu_model_still_current(&cfg, &now))
                {
                    // The config switched the GPU model while loading: this one is stale. The
                    // guard loads the right one on its next poll.
                    return Err("model_stale");
                }
                Ok(asr)
            });
        let asr = kind.map(|k| k.provider_name());
        let from = from.map(|k| k.provider_name());
        match result {
            Ok(()) if gpu.is_some() => {
                // Record the guard's VRAM measurement only after the warm-up (`settle_load`).
                // A previous pending record is settled first so none is lost.
                self.settle_load();
                *lock(&self.pending_load) = Some(PendingLoad {
                    estimate,
                    before,
                    why,
                    asr,
                    started,
                    loaded_at: Instant::now(),
                    warmup,
                });
                // The load time counts at once (restore delay, back-off); the VRAM
                // measurement follows in `settle_load`.
                lock(&self.guard).record_load(estimate, None, None, Instant::now());
                info!(
                    why,
                    asr,
                    from,
                    load_ms = started.elapsed().as_millis() as u64,
                    vram_estimate_mb = estimate,
                    "GPU ASR active"
                );
                *lock(&self.gpu_failed_at) = None;
                *lock(&self.cpu_reason) = None;
                if lock(&self.notices).reset() {
                    // Back on the GPU after a yield the user was told about.
                    self.emit(gpu_task::GPU_RESTORED);
                }
                Ok(())
            }
            Ok(()) => {
                info!(
                    why,
                    asr,
                    from,
                    load_ms = started.elapsed().as_millis() as u64,
                    "local ASR model switched"
                );
                Ok(())
            }
            Err(code @ ("switch_running" | "model_busy" | "model_stale")) => {
                info!(why, asr, from, code, "local ASR switch skipped");
                Err(code)
            }
            Err(code) => {
                warn!(why, asr, from, code, "local ASR model unavailable");
                if gpu.is_some() {
                    *lock(&self.gpu_failed_at) = Some(Instant::now());
                    *lock(&self.cpu_reason) = Some(CpuReason::LoadFailed);
                }
                Err(code)
            }
        }
    }

    fn foreground() -> Option<(String, bool)> {
        #[cfg(windows)]
        {
            use v2c_platform::windows::{foreground, fullscreen};
            let app = foreground::current_app();
            let fs = fullscreen::foreground_fullscreen(app.as_ref());
            app.map(|a| (a.exe, fs))
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// One guard poll and its consequences.
    fn poll_guard(&self) {
        if self.local.switching() || self.local.startup() == Startup::Loading {
            return;
        }
        let cfg = self.config();
        let gpu = gpu_engine(&cfg);
        if self.gpu_stale.swap(false, Ordering::SeqCst)
            && self
                .local
                .current()
                .is_some_and(|k| k != LocalAsrKind::SenseVoice)
        {
            info!("GPU model file changed; unloading the loaded one");
            let _ = self.switch_to(None, "config");
        }
        let current = self.local.current();
        let fg = Self::foreground();
        let idle = lock(&self.last_use).elapsed();
        let input = GuardInput {
            gpu: self.monitor.snapshot(),
            cuda_available: self.cuda_available(&cfg),
            breeze_model_present: local_asr::gpu_model(&cfg, &self.paths).is_some(),
            breeze_vram_mb: gpu_vram_estimate_mb(&cfg),
            foreground: fg.as_ref().map(|(exe, fullscreen)| ForegroundInfo {
                exe,
                fullscreen: *fullscreen,
            }),
            breeze_loaded: current == Some(gpu),
            busy: self.busy.load(Ordering::SeqCst) > 0,
            idle,
            now: Instant::now(),
        };
        let action = lock(&self.guard).decide(&cfg, &input);
        if let Some(code) = lock(&self.notices).update(action) {
            info!(code, "GPU yield notice");
            self.emit(code);
        }
        let gpu_blocked = lock(&self.gpu_failed_at).is_some_and(|t| t.elapsed() < GPU_RETRY_AFTER);
        let has_cpu = local_asr::sensevoice_model(&self.paths).is_some();
        if let GuardAction::UseCpu(reason) | GuardAction::Unload(reason) = action {
            if reason != GuardReason::RestorePending || lock(&self.cpu_reason).is_none() {
                *lock(&self.cpu_reason) = Some(CpuReason::Guard(reason));
            }
        }
        match gpu_task::target(action, current, gpu, has_cpu, gpu_blocked) {
            SlotTarget::Keep => {}
            SlotTarget::Load(kind) => {
                info!(
                    ?action,
                    asr = kind.provider_name(),
                    "guard switches the local model"
                );
                if self.switch_to(Some(kind), "guard").is_err()
                    && kind != LocalAsrKind::SenseVoice
                    && has_cpu
                    && self.local.current().is_none()
                {
                    // The GPU model failed to load: SenseVoice stands in.
                    let _ = self.switch_to(Some(LocalAsrKind::SenseVoice), "guard");
                }
            }
            SlotTarget::Unload => {
                info!(?action, "guard unloads the local model");
                let _ = self.switch_to(None, "guard");
            }
        }
        self.settle_load();
    }

    /// Guard polling until shutdown (blocking; runs on the `v2c-asr` thread).
    pub fn guard_loop(&self) {
        info!(
            interval_s = POLL_INTERVAL.as_secs(),
            nvml = self.monitor.available(),
            "VRAM guard started"
        );
        while !self.stop.load(Ordering::SeqCst) {
            let until = Instant::now() + POLL_INTERVAL;
            while Instant::now() < until {
                if self.stop.load(Ordering::SeqCst) {
                    return;
                }
                if self.poll_now.swap(false, Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            self.poll_guard();
        }
    }

    /// Hotkey pressed: when the slot is empty (idle / on demand), or holds SenseVoice while the
    /// configured GPU model could run and `keep_loaded = "on_demand"` (the guard never restores
    /// there), load in the background. The utterance waits for it (`local_asr`).
    fn on_press(self: &Arc<Self>) {
        *lock(&self.last_use) = Instant::now();
        if self.local.switching() || self.local.startup() == Startup::Loading {
            return;
        }
        let cfg = self.config();
        let on_demand = cfg.gpu.keep_loaded == KeepLoaded::OnDemand;
        let stand_in = self.local.current() == Some(LocalAsrKind::SenseVoice)
            && cfg.local.asr != LocalAsr::SenseVoice;
        if self.local.current().is_some() && !(on_demand && stand_in) {
            return;
        }
        let p = self.providers();
        if p.purpose(Purpose::Asr).target != Target::Local && p.cloud_asr.is_some() {
            return;
        }
        self.spawn_ensure("hotkey", on_demand);
    }

    /// Runs [`Engine::ensure_model`] on a background thread (one at a time).
    fn spawn_ensure(self: &Arc<Self>, why: &'static str, upgrade: bool) {
        if self.press_loading.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("v2c-asr-load".into())
            .spawn(move || {
                me.ensure_model(why, upgrade);
                me.settle_load();
                me.press_loading.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.press_loading.store(false, Ordering::SeqCst);
        }
    }

    /// Makes sure a model is loaded, outside the guard's poll (hotkey, settings change, settings
    /// test), by [`gpu_task::press_target`]: fills an empty slot with the GPU model when allowed
    /// now, otherwise SenseVoice; with `upgrade`, also replaces a SenseVoice stand-in by the GPU
    /// model when allowed. Returns the load time (if something was loaded) and why the GPU
    /// model is not used (if it is not). Blocking.
    fn ensure_model(&self, why: &'static str, upgrade: bool) -> (Option<u64>, Option<CpuReason>) {
        let cfg = self.config();
        let gpu = gpu_engine(&cfg);
        let current = self.local.current();
        if current == Some(gpu) {
            return (None, None);
        }
        let has_cpu = local_asr::sensevoice_model(&self.paths).is_some();
        let fg = Self::foreground();
        let input = GuardInput {
            gpu: self.monitor.snapshot(),
            cuda_available: self.cuda_available(&cfg),
            breeze_model_present: local_asr::gpu_model(&cfg, &self.paths).is_some(),
            breeze_vram_mb: gpu_vram_estimate_mb(&cfg),
            foreground: fg.as_ref().map(|(exe, fullscreen)| ForegroundInfo {
                exe,
                fullscreen: *fullscreen,
            }),
            breeze_loaded: false,
            busy: false,
            idle: Duration::ZERO,
            now: Instant::now(),
        };
        let gpu_blocked = lock(&self.gpu_failed_at).is_some_and(|t| t.elapsed() < GPU_RETRY_AFTER);
        let allowed = lock(&self.guard).can_load(&cfg, &input);
        let mut reason = match allowed {
            Ok(()) if gpu_blocked => Some(CpuReason::LoadFailed),
            Ok(()) => None,
            Err(r) => Some(CpuReason::Guard(r)),
        };
        let target =
            gpu_task::press_target(current, gpu, upgrade, allowed.is_ok(), gpu_blocked, has_cpu);
        let t = Instant::now();
        let ms = || Some(t.elapsed().as_millis() as u64);
        match target {
            SlotTarget::Keep => {
                if current.is_none() {
                    debug!(?reason, "no local model to load");
                }
                if reason.is_some() {
                    *lock(&self.cpu_reason) = reason;
                }
                return (None, reason.or(*lock(&self.cpu_reason)));
            }
            SlotTarget::Unload => {
                *lock(&self.cpu_reason) = reason;
                let _ = self.switch_to(None, why);
                return (None, reason);
            }
            SlotTarget::Load(kind) if kind == gpu => match self.switch_to(Some(gpu), why) {
                Ok(()) => return (ms(), None),
                // Another switch is running, or the config changed: that path decides.
                Err("switch_running" | "model_busy" | "model_stale") => {
                    return (None, *lock(&self.cpu_reason));
                }
                // `switch_to` recorded `LoadFailed`; SenseVoice stands in below.
                Err(_) => reason = Some(CpuReason::LoadFailed),
            },
            SlotTarget::Load(_) => *lock(&self.cpu_reason) = reason,
        }
        if !has_cpu || self.local.current().is_some() {
            return (None, reason);
        }
        let t = Instant::now();
        match self.switch_to(Some(LocalAsrKind::SenseVoice), why) {
            Ok(()) => (Some(t.elapsed().as_millis() as u64), reason),
            Err(_) => (None, reason),
        }
    }

    /// The local engine, waiting while it is still loading or switching. Tells the user once
    /// per utterance when it waits for a switch ("切換模型中").
    async fn local_asr(&self, wait: bool) -> Result<Arc<dyn AsrProvider>, &'static str> {
        let deadline = Instant::now() + ASR_SWITCH_WAIT;
        let mut told = false;
        loop {
            match self.local.active() {
                // Loading or switching, or the hotkey-triggered load has not started yet.
                Err(code)
                    if wait
                        && Instant::now() < deadline
                        && (code == "asr_loading" || self.press_loading.load(Ordering::SeqCst)) =>
                {
                    if !told && self.local.switching() {
                        told = true;
                        self.emit(gpu_task::ASR_SWITCHING);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                r => return r,
            }
        }
    }

    // ---- settings page: live status and engine test ------------------------------------------

    /// The one local model right now.
    pub fn local_status(&self) -> LocalStatus {
        let cfg = self.config();
        let loaded = self.local.loaded();
        let active = loaded.as_ref().map(|l| l.kind);
        let gpu = gpu_engine(&cfg);
        let startup = self.local.startup();
        let switching = self.local.switching() || self.press_loading.load(Ordering::SeqCst);
        let reason = *lock(&self.cpu_reason);
        let idle = matches!(reason, Some(CpuReason::Guard(GuardReason::Idle)))
            || cfg.gpu.keep_loaded == v2c_core::config::KeepLoaded::OnDemand;
        let unloaded =
            active.is_none() && !switching && matches!(startup, Startup::Done(None)) && idle;
        LocalStatus {
            active,
            on_gpu: loaded.as_ref().is_some_and(|l| l.on_gpu),
            gpu_engine: gpu,
            switching,
            starting: startup == Startup::Loading,
            unloaded,
            error: match startup {
                Startup::Done(code) if active.is_none() && !switching && !unloaded => {
                    code.or(Some("model_missing"))
                }
                _ => None,
            },
            cpu_reason: if active == Some(gpu) { None } else { reason },
        }
    }

    /// What serves each purpose right now (same rules as [`Engine::prepare`]).
    pub fn route_status(&self) -> RouteStatus {
        let p = self.providers();
        let usage = |purpose, built: bool| {
            let plan = p.purpose(purpose);
            purpose_use(&plan.target, plan.fallback_to_local, built)
        };
        RouteStatus {
            asr: usage(Purpose::Asr, p.cloud_asr.is_some()),
            polish: usage(Purpose::Polish, p.cloud_polish.is_some()),
            translate: usage(Purpose::Translate, p.cloud_translate.is_some()),
            lm_up: self.lm.is_up(),
        }
    }

    /// Current VRAM reading (`None` without NVIDIA / NVML).
    pub fn vram(&self) -> Option<GpuSnapshot> {
        self.monitor.snapshot()
    }

    /// VRAM the config's GPU model (Breeze or TEA-ASR) needs (measured at the last load, else
    /// estimated).
    pub fn gpu_need_mb(&self) -> u64 {
        let estimate = gpu_vram_estimate_mb(&self.config());
        lock(&self.guard).breeze_need_mb(estimate)
    }

    /// Runs the built-in 3 s test clip through the one local model the next utterance would use.
    /// When nothing is loaded (idle / on demand) it loads one first, by the same rules as a
    /// hotkey press; it never loads a second model. The transcript is dropped.
    /// Errors: `busy` (an utterance is running), `already_running`, `asr_switching` (a model
    /// switch is running), or the local engine error (`model_missing`, `asr_loading`, ...).
    pub async fn test_local_asr(self: &Arc<Self>) -> Result<LocalTestRun, &'static str> {
        // A second press reports the running test, not "dictating".
        if self.testing.swap(true, Ordering::SeqCst) {
            return Err("already_running");
        }
        struct Done<'a>(&'a AtomicBool);
        impl Drop for Done<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _done = Done(&self.testing);
        if self.busy.load(Ordering::SeqCst) > 0 {
            return Err("busy");
        }
        // Keeps the guard from unloading the model in the middle of the test.
        let _busy = BusyToken::new(&self.busy);

        // Startup still selecting / loading: wait like an utterance would.
        let deadline = Instant::now() + ASR_LOAD_WAIT;
        while self.local.startup() == Startup::Loading && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if self.local.switching() || self.press_loading.load(Ordering::SeqCst) {
            return Err(gpu_task::ASR_SWITCHING);
        }

        let me = Arc::clone(self);
        let (load_ms, reason) =
            tauri::async_runtime::spawn_blocking(move || me.ensure_model("test", false))
                .await
                .map_err(|_| "internal_error")?;
        if self.local.switching() {
            return Err(gpu_task::ASR_SWITCHING);
        }
        let Some(loaded) = self.local.loaded() else {
            return Err(self.local.active().err().unwrap_or("model_missing"));
        };
        let gpu = gpu_engine(&self.config());
        let cpu_reason = if loaded.kind == gpu {
            None
        } else {
            reason.or(*lock(&self.cpu_reason))
        };
        let t = Instant::now();
        let transcribe = match loaded
            .asr
            .transcribe(
                &probe_clip(v2c_core::route::PROBE_CLIP_MS),
                &AsrContext::default(),
            )
            .await
        {
            Ok(_) => Ok(t.elapsed().as_millis() as u64),
            Err(e) => {
                warn!(error = %e, engine = loaded.asr.name(), "local ASR test failed");
                Err("asr_failed")
            }
        };
        *lock(&self.last_use) = Instant::now();
        let is_gpu = loaded.kind != LocalAsrKind::SenseVoice;
        let run = LocalTestRun {
            engine: loaded.kind,
            on_gpu: loaded.on_gpu,
            cpu_reason,
            load_ms,
            vram_mb: is_gpu.then(|| self.gpu_need_mb()),
            transcribe,
        };
        info!(
            asr = loaded.kind.provider_name(),
            on_gpu = run.on_gpu,
            reason = ?run.cpu_reason,
            load_ms = ?run.load_ms,
            transcribe_ms = ?run.transcribe.ok(),
            vram_mb = ?run.vram_mb,
            "local ASR test"
        );
        Ok(run)
    }

    // ---- LM Studio ---------------------------------------------------------------------

    /// Checks LM Studio now and every `LM_CHECK_INTERVAL` until shutdown.
    pub async fn lm_check_loop(self: Arc<Self>) {
        let mut first = true;
        while !self.stop.load(Ordering::SeqCst) {
            let probe = self.providers().lm_probe.clone();
            let up = match &probe {
                Some(p) => p.check().await.is_ok(),
                None => false,
            };
            let changed = self.lm.set(up);
            if first || changed.is_some() {
                info!(
                    up,
                    base_url = probe.as_ref().map(|p| p.base_url()).unwrap_or("-"),
                    "LM Studio status"
                );
                first = false;
            }
            tokio::select! {
                () = tokio::time::sleep(providers::LM_CHECK_INTERVAL) => {}
                () = self.lm_wake.notified() => {}
            }
        }
    }

    // ---- per utterance -------------------------------------------------------------------

    async fn prepare(self: &Arc<Self>, mode: Mode) -> Result<Prepared, &'static str> {
        let cfg = self.config();
        let p = self.providers();
        let fell_back = Arc::new(AtomicBool::new(false));
        let mut notices = Vec::new();

        // LLM first: translate without any LLM fails before ASR runs.
        let lm_up = self.lm.is_up();
        let (purpose, local_llm, cloud_llm) = match mode {
            Mode::Dictate => (Purpose::Polish, &p.local_polish, &p.cloud_polish),
            Mode::TranslateZhToEn => (Purpose::Translate, &p.local_translate, &p.cloud_translate),
        };
        let llm_plan = p.purpose(purpose);
        let llm_needed = mode == Mode::TranslateZhToEn || cfg.polish.enabled;
        let llm_sel = select::<dyn LlmProvider>(
            &llm_plan,
            cloud_llm.clone(),
            local_llm.clone().filter(|_| lm_up),
        );
        let llm = if llm_needed {
            wrap_llm(&llm_sel, &fell_back)
        } else {
            None
        };
        let mut polish_offline = false;
        if llm_needed {
            if llm_sel.fell_back_upfront {
                fell_back.store(true, Ordering::SeqCst);
            }
            if llm.is_none() {
                if mode == Mode::TranslateZhToEn {
                    return Err(if llm_sel.cloud_missing {
                        providers::CLOUD_NOT_CONFIGURED
                    } else {
                        crate::controller::TRANSLATE_UNAVAILABLE
                    });
                }
                if llm_sel.cloud_missing {
                    notices.push(providers::CLOUD_NOT_CONFIGURED);
                } else {
                    polish_offline = self.lm.take_offline_notice();
                }
            }
        }
        // Polish budget: local -> `polish.timeout_ms` (1 s), scaled with length up to the LM
        // Studio timeout. Cloud -> the cloud LLM timeout (`timeouts.llm_cloud_ms`): a network
        // round trip does not fit in the local 1 s budget, and the provider's own timeout
        // bounds it anyway.
        let cloud_llm = matches!(llm_plan.target, Target::Cloud { .. })
            && llm_sel.primary.is_some()
            && !llm_sel.fell_back_upfront;
        let mut pcfg = pipeline_config(&cfg);
        pcfg.vocabulary = self.vocabulary().as_ref().clone();
        let cap_ms = if cloud_llm {
            pcfg.polish_timeout_ms = p.cloud_llm_timeout_ms;
            p.cloud_llm_timeout_ms
        } else {
            providers::LOCAL_LLM_TIMEOUT.as_millis() as u64
        };

        // ASR.
        let asr_plan = p.purpose(Purpose::Asr);
        let local_is_primary = asr_plan.target == Target::Local
            || (p.cloud_asr.is_none() && asr_plan.fallback_to_local);
        let local = self.local_asr(local_is_primary).await;
        let local_err = local.as_ref().err().copied();
        // Cloud first with local fallback, but no local model loaded right now (idle / on
        // demand unload): the fallback loads it only if the cloud call fails.
        let local = match local {
            Err(_)
                if !local_is_primary
                    && asr_plan.fallback_to_local
                    && self.local.startup() != Startup::Loading =>
            {
                Some(Arc::new(LazyLocalAsr {
                    engine: Arc::clone(self),
                    name: gpu_engine(&cfg).provider_name(),
                }) as Arc<dyn AsrProvider>)
            }
            other => other.ok(),
        };
        let asr_sel = select(&asr_plan, p.cloud_asr.clone(), local);
        let Some(asr) = wrap_asr(&asr_sel, &fell_back) else {
            return Err(if asr_sel.cloud_missing {
                providers::CLOUD_NOT_CONFIGURED
            } else {
                local_err.unwrap_or("model_missing")
            });
        };
        if asr_sel.fell_back_upfront {
            fell_back.store(true, Ordering::SeqCst);
        }

        // Provider names only.
        info!(
            ?mode,
            asr = asr.name(),
            llm = llm.as_ref().map(|l| l.name()).unwrap_or("-"),
            "providers for utterance"
        );
        let pipeline = Pipeline::new(asr, llm, pcfg).with_polish_timeout_cap_ms(cap_ms);
        Ok(Prepared {
            pipeline,
            cloud_fell_back: fell_back,
            polish_offline,
            notices,
            _busy: Some(Box::new(BusyToken::new(&self.busy))),
        })
    }

    // ---- config reload ---------------------------------------------------------------------

    /// Applies a new config (from `config.toml` after the tray or settings saved it).
    pub fn reload(self: &Arc<Self>, new: Config) {
        let old = self.config();
        let diff = ConfigDiff::between(&old, &new);
        if diff.is_empty() {
            debug!("config unchanged");
            return;
        }
        let new = Arc::new(new);
        *self.config.write().unwrap_or_else(|p| p.into_inner()) = Arc::clone(&new);
        self.config_tx.send_replace(Arc::clone(&new));
        if diff.providers {
            let built = Providers::build(&new, self.secrets.as_ref(), self.http.as_ref());
            *self.providers.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(built);
            self.lm_wake.notify_one();
        }
        self.reload_vocab();
        if diff.breeze_model {
            self.gpu_stale.store(true, Ordering::SeqCst);
        }
        // ASR choice, GPU settings, or a model that was just downloaded: decide now.
        self.poll_now.store(true, Ordering::SeqCst);
        // `on_demand` polls never load the GPU model: apply SenseVoice -> GPU engine here.
        if diff.local_asr
            && new.gpu.keep_loaded == KeepLoaded::OnDemand
            && new.local.asr != LocalAsr::SenseVoice
            && self.local.current() == Some(LocalAsrKind::SenseVoice)
            && self.local.startup() != Startup::Loading
        {
            self.spawn_ensure("config", true);
        }
        if diff.audio {
            warn!("audio settings change: restart required");
        }
        if diff.history {
            warn!("history settings change: restart required");
        }
        info!(?diff, "config reloaded");
    }
}

/// The GPU model file chosen by `started` is still the one `now` wants.
fn gpu_model_still_current(started: &Config, now: &Config) -> bool {
    local_asr::gpu_model_key(started) == local_asr::gpu_model_key(now)
}

/// Error code after startup, or `None` when an engine is ready.
fn startup_code(
    ready: bool,
    any_model: bool,
    load_error: Option<&'static str>,
) -> Option<&'static str> {
    if ready {
        None
    } else if !any_model {
        Some("model_missing")
    } else {
        Some(load_error.unwrap_or("model_load_failed"))
    }
}

/// Counts running utterances while alive.
struct BusyToken(Arc<AtomicUsize>);

impl BusyToken {
    fn new(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(counter))
    }
}

impl Drop for BusyToken {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Which parts of the app a config change touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConfigDiff {
    /// `[hotkey]`: the controller rebuilds the trigger state machine.
    pub hotkey: bool,
    /// `[route]`, `[[cloud]]`, `[timeouts]`, LM Studio address / model: rebuild providers.
    pub providers: bool,
    /// `[local] asr`, `[gpu]`: handled by the VRAM guard on its next poll.
    pub local_asr: bool,
    /// The GPU engine's model file changed (`[local] breeze_quant`, or `asr` switched between
    /// TEA-ASR and Breeze / auto): a loaded GPU engine is the wrong one and is dropped.
    pub breeze_model: bool,
    /// `[audio]`: needs a restart (the recorder keeps its stream).
    pub audio: bool,
    /// `[history]`: needs a restart (the history store keeps the settings it opened with).
    pub history: bool,
    /// `[polish]`, `[[apps]]`: read from the live config per utterance (and by history
    /// re-output).
    pub other: bool,
}

impl ConfigDiff {
    pub fn between(a: &Config, b: &Config) -> Self {
        let providers = a.route != b.route
            || a.cloud != b.cloud
            || a.timeouts != b.timeouts
            || a.local.llm_base_url != b.local.llm_base_url
            || a.local.llm_model != b.local.llm_model;
        Self {
            hotkey: a.hotkey != b.hotkey,
            providers,
            local_asr: a.local.asr != b.local.asr || a.gpu != b.gpu,
            breeze_model: local_asr::gpu_model_key(a) != local_asr::gpu_model_key(b),
            audio: a.audio != b.audio,
            history: a.history != b.history,
            other: a.polish != b.polish || a.apps != b.apps,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Local fallback for a cloud ASR route while no local model is loaded: loads one (same rules
/// as a hotkey press) only when it is actually needed, and waits for it.
struct LazyLocalAsr {
    engine: Arc<Engine>,
    name: &'static str,
}

#[async_trait]
impl AsrProvider for LazyLocalAsr {
    fn name(&self) -> &str {
        self.name
    }

    async fn transcribe(
        &self,
        clip: &v2c_core::audio::AudioClip,
        ctx: &AsrContext,
    ) -> v2c_core::Result<v2c_core::asr::Transcript> {
        if self.engine.local.current().is_none() {
            self.engine.spawn_ensure("fallback", false);
        }
        let asr = self.engine.local_asr(true).await.map_err(|code| {
            warn!(code, "local fallback ASR unavailable");
            v2c_core::V2cError::Provider {
                stage: v2c_core::error::Stage::Asr,
                provider: self.name.into(),
                kind: v2c_core::error::ProviderErrorKind::Unreachable,
            }
        })?;
        asr.transcribe(clip, ctx).await
    }
}

/// The [`Backend`] the controller uses.
pub struct EngineHandle(pub Arc<Engine>);

#[async_trait]
impl Backend for EngineHandle {
    fn on_press(&self, _mode: Mode) {
        self.0.on_press();
    }

    async fn prepare(&self, mode: Mode) -> Result<Prepared, &'static str> {
        self.0.prepare(mode).await
    }
}

#[cfg(test)]
mod tests {
    use v2c_core::config::{BreezeQuant, CloudKind, CloudService, GpuProfile, LocalAsr};
    use v2c_core::secrets::MemorySecretStore;

    use super::*;

    fn snap(free_mb: u64) -> Option<GpuSnapshot> {
        Some(GpuSnapshot {
            free_mb,
            total_mb: 8000,
            used_by_us_mb: None,
        })
    }

    #[test]
    fn vram_is_recorded_from_the_snapshot_taken_after_the_warmup() {
        use std::sync::atomic::AtomicBool;
        const ESTIMATE: u64 = 1500;
        let mut guard = GpuGuard::default();
        let warmed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let flag = Arc::clone(&warmed);
        let warm = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            // The first inference grows the buffers: 4000 free -> 2200 free.
            flag.store(true, Ordering::SeqCst);
            tx.send(()).unwrap();
        });
        let flag = Arc::clone(&warmed);
        let need = record_after_warmup(
            &mut guard,
            ESTIMATE,
            snap(4000),
            Some(rx),
            // The snapshot source reports the post-warm-up reading only once the warm-up ran.
            move || {
                snap(if flag.load(Ordering::SeqCst) {
                    2200
                } else {
                    3400
                })
            },
            Instant::now(),
        );
        warm.join().unwrap();
        assert_eq!(need, 1800, "the need includes the warm-up's buffers");
        assert_eq!(guard.breeze_need_mb(ESTIMATE), 1800);
        // No warm-up (SenseVoice, TEA): the snapshot is taken at once.
        let mut other = GpuGuard::default();
        let need = record_after_warmup(
            &mut other,
            ESTIMATE,
            snap(4000),
            None,
            || snap(3000),
            Instant::now(),
        );
        assert_eq!(need, 1000);
    }

    #[test]
    fn diff_groups_changes() {
        let a = Config::with_defaults();
        assert!(ConfigDiff::between(&a, &a).is_empty());

        let mut b = a.clone();
        b.polish.enabled = false;
        assert_eq!(
            ConfigDiff::between(&a, &b),
            ConfigDiff {
                other: true,
                ..Default::default()
            }
        );

        let mut b = a.clone();
        b.hotkey.hold_threshold_ms = 500;
        assert!(ConfigDiff::between(&a, &b).hotkey);

        let mut b = a.clone();
        b.route.polish = "x".into();
        assert!(ConfigDiff::between(&a, &b).providers);
        let mut b = a.clone();
        b.local.llm_base_url = "http://pc:1234/v1".into();
        assert!(ConfigDiff::between(&a, &b).providers);
        let mut b = a.clone();
        b.cloud.push(CloudService {
            id: "g".into(),
            name: None,
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: None,
            llm_model: None,
            asr_timeout_ms: None,
        });
        assert!(ConfigDiff::between(&a, &b).providers);

        // Engine choice: the guard handles it; Breeze stays unless the file changes.
        let mut b = a.clone();
        b.local.asr = LocalAsr::SenseVoice;
        b.gpu.profile = GpuProfile::CpuOnly;
        let d = ConfigDiff::between(&a, &b);
        assert!(d.local_asr && !d.breeze_model && !d.providers);
        let mut b = a.clone();
        b.local.breeze_quant = BreezeQuant::Q8_0;
        assert!(ConfigDiff::between(&a, &b).breeze_model);

        let mut b = a.clone();
        b.audio.preroll_ms = 100;
        assert!(ConfigDiff::between(&a, &b).audio);
        let mut b = a.clone();
        b.history.retention_days = 3;
        let d = ConfigDiff::between(&a, &b);
        assert!(d.history && !d.other);
    }

    #[test]
    fn breeze_loaded_for_an_old_quant_is_stale() {
        let a = Config::with_defaults();
        let mut b = a.clone();
        assert!(gpu_model_still_current(&a, &b));
        b.polish.enabled = false;
        assert!(gpu_model_still_current(&a, &b));
        b.local.breeze_quant = BreezeQuant::Q8_0;
        assert!(!gpu_model_still_current(&a, &b));
    }

    #[test]
    fn switching_between_breeze_and_tea_drops_the_gpu_engine() {
        let a = Config::with_defaults();
        let mut tea = a.clone();
        tea.local.asr = LocalAsr::TeaAsr;
        assert!(!gpu_model_still_current(&a, &tea));
        let d = ConfigDiff::between(&a, &tea);
        assert!(d.local_asr && d.breeze_model && !d.providers);
        // Back to Breeze (forced) too.
        let mut breeze = a.clone();
        breeze.local.asr = LocalAsr::Breeze;
        assert!(ConfigDiff::between(&tea, &breeze).breeze_model);
        // auto <-> breeze keep the same Breeze file.
        assert!(!ConfigDiff::between(&a, &breeze).breeze_model);
        // auto -> SenseVoice: the guard unloads Breeze (config reason). SenseVoice -> TEA-ASR:
        // a Breeze still loaded is the wrong engine and is dropped right away.
        let mut sv = a.clone();
        sv.local.asr = LocalAsr::SenseVoice;
        assert!(!ConfigDiff::between(&a, &sv).breeze_model);
        assert!(ConfigDiff::between(&sv, &tea).breeze_model);
        // A quantisation change does not touch a loaded TEA-ASR.
        let mut tea8 = tea.clone();
        tea8.local.breeze_quant = BreezeQuant::Q8_0;
        assert!(gpu_model_still_current(&tea, &tea8));
        assert!(!ConfigDiff::between(&tea, &tea8).breeze_model);
    }

    #[test]
    fn startup_codes() {
        assert_eq!(startup_code(true, true, Some("x")), None);
        assert_eq!(startup_code(false, false, None), Some("model_missing"));
        assert_eq!(
            startup_code(false, true, Some("sensevoice_not_available")),
            Some("sensevoice_not_available")
        );
        assert_eq!(startup_code(false, true, None), Some("model_load_failed"));
    }

    #[test]
    fn busy_token_counts() {
        let c = Arc::new(AtomicUsize::new(0));
        let a = BusyToken::new(&c);
        let b = BusyToken::new(&c);
        assert_eq!(c.load(Ordering::SeqCst), 2);
        drop(a);
        drop(b);
        assert_eq!(c.load(Ordering::SeqCst), 0);
    }

    fn engine(cfg: Config) -> Arc<Engine> {
        let dir = std::env::temp_dir().join(format!("v2c-engine-test-{}", std::process::id()));
        let paths = Paths {
            data_dir: dir.clone(),
            local_data_dir: dir,
        };
        let (status, _) = broadcast::channel(16);
        Engine::new(paths, cfg, status, Arc::new(MemorySecretStore::new()))
    }

    #[tokio::test]
    async fn prepare_without_lm_studio_or_models() {
        let e = engine(Config::with_defaults());
        e.local.set_startup(Startup::Done(Some("model_missing")));
        // Translate: no LLM -> error before ASR.
        assert_eq!(
            e.prepare(Mode::TranslateZhToEn).await.err(),
            Some(crate::controller::TRANSLATE_UNAVAILABLE)
        );
        // Dictate: no engine.
        assert_eq!(e.prepare(Mode::Dictate).await.err(), Some("model_missing"));
    }

    #[tokio::test]
    async fn vocabulary_reaches_the_pipeline_and_reloads() {
        let dir = std::env::temp_dir().join(format!("v2c-engine-vocab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            data_dir: dir.clone(),
            local_data_dir: dir.clone(),
        };
        std::fs::write(paths.vocab_file(), "Tauri\n聯發科\n").unwrap();
        let (status, _) = broadcast::channel(16);
        let e = Engine::new(
            paths.clone(),
            Config::with_defaults(),
            status,
            Arc::new(MemorySecretStore::new()),
        );
        assert_eq!(
            *e.vocabulary(),
            vec!["Tauri".to_string(), "聯發科".to_string()]
        );
        std::fs::write(paths.vocab_file(), "Breeze\n").unwrap();
        e.reload_vocab();
        assert_eq!(*e.vocabulary(), vec!["Breeze".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn translate_routed_to_cloud_without_key_and_no_fallback() {
        let mut cfg = Config::with_defaults();
        cfg.cloud.push(CloudService {
            id: "g".into(),
            name: None,
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: None,
            llm_model: None,
            asr_timeout_ms: None,
        });
        cfg.route.translate = "g".into();
        cfg.route.cloud_fallback_to_local = false;
        let e = engine(cfg);
        assert_eq!(
            e.prepare(Mode::TranslateZhToEn).await.err(),
            Some(providers::CLOUD_NOT_CONFIGURED)
        );
    }

    #[test]
    fn purpose_use_mirrors_select() {
        let cloud = Target::Cloud {
            service_id: "g".into(),
            kind: CloudKind::Gemini,
        };
        let g = || "g".to_string();
        assert_eq!(purpose_use(&Target::Local, true, true), PurposeUse::Local);
        assert_eq!(
            purpose_use(&cloud, false, true),
            PurposeUse::Cloud { service_id: g() }
        );
        assert_eq!(
            purpose_use(&cloud, true, false),
            PurposeUse::LocalInsteadOfCloud { service_id: g() }
        );
        assert_eq!(
            purpose_use(&cloud, false, false),
            PurposeUse::Unavailable { service_id: g() }
        );
    }

    #[tokio::test]
    async fn status_and_test_without_models() {
        let mut cfg = Config::with_defaults();
        cfg.cloud.push(CloudService {
            id: "g".into(),
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: None,
            llm_model: None,
            name: None,
            asr_timeout_ms: None,
        });
        cfg.route.asr = "g".into();
        let e = engine(cfg);
        let s = e.local_status();
        assert!(s.starting && s.active.is_none());
        e.local.set_startup(Startup::Done(Some("model_missing")));
        *lock(&e.cpu_reason) = Some(CpuReason::LoadFailed);
        let s = e.local_status();
        assert!(!s.unloaded && !s.switching && s.active.is_none());
        assert_eq!(
            (s.starting, s.error, s.cpu_reason),
            (false, Some("model_missing"), Some(CpuReason::LoadFailed))
        );
        // No key for "g": ASR falls back to local (fallback is on by default).
        let r = e.route_status();
        assert_eq!(
            r.asr,
            PurposeUse::LocalInsteadOfCloud {
                service_id: "g".into()
            }
        );
        assert_eq!(r.polish, PurposeUse::Local);
        // The test needs a local engine; an utterance in progress refuses it.
        assert_eq!(e.test_local_asr().await.err(), Some("model_missing"));
        let busy = BusyToken::new(&e.busy);
        assert_eq!(e.test_local_asr().await.err(), Some("busy"));
        drop(busy);
        assert!(!e.testing.load(Ordering::SeqCst));
        // A test already running wins over "busy" (its own BusyToken counts as busy).
        e.testing.store(true, Ordering::SeqCst);
        let busy = BusyToken::new(&e.busy);
        assert_eq!(e.test_local_asr().await.err(), Some("already_running"));
        drop(busy);
        e.testing.store(false, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn reload_publishes_config_and_rebuilds_providers() {
        let e = engine(Config::with_defaults());
        let mut rx = e.config_watch();
        let mut cfg = Config::with_defaults();
        cfg.local.llm_base_url = "http://127.0.0.1:9/v1".into();
        e.reload(cfg.clone());
        assert!(rx.has_changed().unwrap());
        assert_eq!(
            rx.borrow_and_update().local.llm_base_url,
            cfg.local.llm_base_url
        );
        assert_eq!(
            e.providers().lm_probe.as_ref().unwrap().base_url(),
            "http://127.0.0.1:9/v1"
        );
        // Same config again: nothing to do.
        e.reload(cfg);
        assert!(!rx.has_changed().unwrap());
    }
}
