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
    GpuMonitor, GuardInput, LocalAsrKind,
};
use v2c_core::asr::AsrProvider;
use v2c_core::config::Config;
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

/// How long an utterance waits for the local engine that is still loading.
const ASR_LOAD_WAIT: Duration = Duration::from_secs(30);
/// After a failed GPU engine load, the guard does not retry for this long.
const GPU_RETRY_AFTER: Duration = Duration::from_secs(300);

pub struct Engine {
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
    stop: AtomicBool,
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
            gpu_failed_at: Mutex::new(None),
            vocabulary: RwLock::new(Arc::new(vocabulary)),
            poll_now: AtomicBool::new(false),
            gpu_stale: AtomicBool::new(false),
            press_loading: AtomicBool::new(false),
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
        if let Some(code) = gpu_task::reason_code(v2c_asr_local::GuardReason::Engine(pick.reason)) {
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
        let mut snaps = (None, None);
        let result = self.local.switch_model(kind, local_asr::RELEASE_WAIT, || {
            let kind = kind.expect("load is only called with a model");
            snaps.0 = gpu.and_then(|_| self.monitor.snapshot());
            let asr = local_asr::load_model(kind, &cfg, &self.paths)?;
            snaps.1 = gpu.and_then(|_| self.monitor.snapshot());
            let now = self.config();
            if gpu.is_some() && (gpu_engine(&now) != kind || !gpu_model_still_current(&cfg, &now)) {
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
                let mut guard = lock(&self.guard);
                guard.record_load(estimate, snaps.0, snaps.1, Instant::now());
                info!(
                    why,
                    asr,
                    from,
                    load_ms = started.elapsed().as_millis() as u64,
                    vram_estimate_mb = estimate,
                    vram_need_mb = guard.breeze_need_mb(estimate),
                    "GPU ASR active"
                );
                drop(guard);
                *lock(&self.gpu_failed_at) = None;
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

    /// Hotkey pressed with no model loaded (idle / on demand): load one in the background.
    /// The GPU model when the guard allows it now, otherwise SenseVoice. The utterance waits
    /// for it (`local_asr`).
    fn on_press(self: &Arc<Self>) {
        *lock(&self.last_use) = Instant::now();
        if self.local.current().is_some() || self.local.switching() {
            return;
        }
        if self.local.startup() == Startup::Loading {
            return;
        }
        let p = self.providers();
        if p.purpose(Purpose::Asr).target != Target::Local && p.cloud_asr.is_some() {
            return;
        }
        if self.press_loading.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("v2c-asr-load".into())
            .spawn(move || {
                me.load_on_demand();
                me.press_loading.store(false, Ordering::SeqCst);
            });
        if spawned.is_err() {
            self.press_loading.store(false, Ordering::SeqCst);
        }
    }

    fn load_on_demand(&self) {
        let cfg = self.config();
        let has_cpu = local_asr::sensevoice_model(&self.paths).is_some();
        let gpu_present = local_asr::gpu_model(&cfg, &self.paths).is_some();
        let fg = Self::foreground();
        let input = GuardInput {
            gpu: self.monitor.snapshot(),
            cuda_available: self.cuda_available(&cfg),
            breeze_model_present: gpu_present,
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
        let kind = match allowed {
            Ok(()) if !gpu_blocked => gpu_engine(&cfg),
            Ok(()) | Err(_) if has_cpu => LocalAsrKind::SenseVoice,
            other => {
                debug!(?other, "no local model to load on demand");
                return;
            }
        };
        if self.switch_to(Some(kind), "hotkey").is_err()
            && kind != LocalAsrKind::SenseVoice
            && has_cpu
            && self.local.current().is_none()
        {
            let _ = self.switch_to(Some(LocalAsrKind::SenseVoice), "hotkey");
        }
    }

    /// The local engine, waiting while it is still loading or switching. Tells the user once
    /// per utterance when it waits for a switch ("切換模型中").
    async fn local_asr(&self, wait: bool) -> Result<Arc<dyn AsrProvider>, &'static str> {
        let deadline = Instant::now() + ASR_LOAD_WAIT;
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

    async fn prepare(&self, mode: Mode) -> Result<Prepared, &'static str> {
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
        let asr_sel = select(&asr_plan, p.cloud_asr.clone(), local.ok());
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
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: None,
            llm_model: None,
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
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: None,
            llm_model: None,
        });
        cfg.route.translate = "g".into();
        cfg.route.cloud_fallback_to_local = false;
        let e = engine(cfg);
        assert_eq!(
            e.prepare(Mode::TranslateZhToEn).await.err(),
            Some(providers::CLOUD_NOT_CONFIGURED)
        );
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
