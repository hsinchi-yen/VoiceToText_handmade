//! Runtime state behind the controller: live config, local engines, VRAM guard, providers,
//! and LM Studio liveness. Implements [`controller::Backend`] through [`EngineHandle`].
//!
//! Threads / tasks (started by `main.rs`):
//! - `v2c-asr` thread: [`Engine::startup_blocking`] (select + load the local engines), then the
//!   VRAM guard poll every `gpu_guard::POLL_INTERVAL` ([`Engine::guard_loop`]).
//! - async task [`Engine::lm_check_loop`]: LM Studio `GET /models` every
//!   [`providers::LM_CHECK_INTERVAL`] (and right after a config reload).
//!
//! Config reload ([`Engine::reload`]) swaps the config, rebuilds providers, and drops Breeze
//! only when the model file (quantisation) changed; other ASR changes go through the guard.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{broadcast, watch, Notify};
use tracing::{debug, info, warn};
use v2c_asr_local::gpu_guard::POLL_INTERVAL;
use v2c_asr_local::{
    breeze_vram_estimate_mb, pick_with_reason, ForegroundInfo, GpuGuard, GpuInfo, GpuMonitor,
    GuardInput, LocalAsrKind,
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
use crate::gpu_task::{self, guard_step, GuardStep, NoticeLatch};
use crate::local_asr::{self, LocalEngines, Startup};
use crate::paths::Paths;
use crate::providers::{self, select, wrap_asr, wrap_llm, LmHealth, Providers};

/// How long an utterance waits for the local engine that is still loading.
const ASR_LOAD_WAIT: Duration = Duration::from_secs(30);
/// After a failed Breeze load, the guard does not retry for this long.
const BREEZE_RETRY_AFTER: Duration = Duration::from_secs(300);

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
    breeze_failed_at: Mutex<Option<Instant>>,
    /// Personal vocabulary (`vocab.txt`), for the ASR prompt and the LLM prompts.
    vocabulary: RwLock<Arc<Vec<String>>>,
    sensevoice_loading: AtomicBool,
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
            breeze_failed_at: Mutex::new(None),
            vocabulary: RwLock::new(Arc::new(vocabulary)),
            sensevoice_loading: AtomicBool::new(false),
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

    fn cuda_available(&self) -> bool {
        v2c_asr_local::breeze::cuda_enabled() && self.monitor.available()
    }

    /// Stops the background loops (at app exit).
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.lm_wake.notify_one();
    }

    // ---- local engines ---------------------------------------------------------------

    /// Picks and loads the local engines (blocking). SenseVoice is always loaded when its
    /// model exists: it is the CPU engine the guard falls back to.
    pub fn startup_blocking(&self) {
        let cfg = self.config();
        let breeze = local_asr::breeze_model(&cfg, &self.paths);
        let sensevoice = local_asr::sensevoice_model(&self.paths);
        let snap = self.monitor.snapshot();
        let gpu = GpuInfo {
            cuda_available: self.cuda_available(),
            free_vram_mb: snap.map_or(0, |s| s.free_mb),
            total_vram_mb: snap.map_or(0, |s| s.total_mb),
            breeze_model_present: breeze.is_some(),
            breeze_vram_mb: breeze_vram_estimate_mb(cfg.local.breeze_quant),
        };
        let pick = pick_with_reason(&cfg, &gpu);
        info!(
            engine = ?pick.kind,
            reason = ?pick.reason,
            cuda_feature = v2c_asr_local::breeze::cuda_enabled(),
            nvml = self.monitor.available(),
            free_vram_mb = gpu.free_vram_mb,
            total_vram_mb = gpu.total_vram_mb,
            breeze_model = gpu.breeze_model_present,
            sensevoice_model = sensevoice.is_some(),
            "local ASR selected"
        );
        if let Some(code) = gpu_task::reason_code(v2c_asr_local::GuardReason::Engine(pick.reason)) {
            if let Some(code) = lock(&self.notices).offer(code) {
                self.emit(code);
            }
        }

        let mut error = None;
        if pick.kind == LocalAsrKind::Breeze && !self.load_breeze_blocking("startup") {
            error = Some("model_load_failed");
        }
        match &sensevoice {
            Some(dir) => match local_asr::load_sensevoice(dir) {
                Ok(sv) => self.local.set_sensevoice(sv),
                Err(code) => {
                    error.get_or_insert(code);
                }
            },
            None => info!("SenseVoice model not found"),
        }
        let ready = self.local.breeze_loaded() || self.local.sensevoice_loaded();
        let code = startup_code(ready, breeze.is_some() || sensevoice.is_some(), error);
        self.local.set_startup(Startup::Done(code));
        match code {
            None => info!(
                breeze = self.local.breeze_loaded(),
                sensevoice = self.local.sensevoice_loaded(),
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

    /// Loads Breeze and makes it the active engine (blocking). `false` if it did not load.
    fn load_breeze_blocking(&self, why: &'static str) -> bool {
        if !self.local.begin_breeze_load() {
            return false;
        }
        let cfg = self.config();
        let Some(model) = local_asr::breeze_model(&cfg, &self.paths) else {
            self.local.end_breeze_load(None);
            return false;
        };
        let estimate = breeze_vram_estimate_mb(cfg.local.breeze_quant);
        let before = self.monitor.snapshot();
        let result = local_asr::load_breeze(&cfg, &model);
        let after = self.monitor.snapshot();
        if result.is_ok() && !quant_still_current(&cfg, &self.config()) {
            // The config switched the model file while loading: this one is stale. The guard
            // loads the right one on its next poll.
            info!(why, "Breeze quantisation changed while loading; discarding");
            drop(result);
            self.local.end_breeze_load(None);
            return false;
        }
        match result {
            Ok(b) => {
                let mut guard = lock(&self.guard);
                guard.record_load(estimate, before, after, Instant::now());
                info!(
                    why,
                    vram_need_mb = guard.breeze_need_mb(estimate),
                    "Breeze active"
                );
                drop(guard);
                if lock(&self.notices).reset() {
                    // Back on the GPU after a yield the user was told about.
                    self.emit(gpu_task::GPU_RESTORED);
                }
                self.local.end_breeze_load(Some(b));
                true
            }
            Err(code) => {
                warn!(why, code, "Breeze unavailable; using the CPU engine");
                *lock(&self.breeze_failed_at) = Some(Instant::now());
                self.local.end_breeze_load(None);
                false
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
        let cfg = self.config();
        let fg = Self::foreground();
        let idle = lock(&self.last_use).elapsed();
        let input = GuardInput {
            gpu: self.monitor.snapshot(),
            cuda_available: self.cuda_available(),
            breeze_model_present: local_asr::breeze_model(&cfg, &self.paths).is_some(),
            breeze_vram_mb: breeze_vram_estimate_mb(cfg.local.breeze_quant),
            foreground: fg.as_ref().map(|(exe, fullscreen)| ForegroundInfo {
                exe,
                fullscreen: *fullscreen,
            }),
            breeze_loaded: self.local.breeze_loaded(),
            busy: self.busy.load(Ordering::SeqCst) > 0,
            idle,
            now: Instant::now(),
        };
        let action = lock(&self.guard).decide(&cfg, &input);
        if let Some(code) = lock(&self.notices).update(action) {
            info!(code, "GPU yield notice");
            self.emit(code);
        }
        let recently_failed =
            lock(&self.breeze_failed_at).is_some_and(|t| t.elapsed() < BREEZE_RETRY_AFTER);
        let blocked = self.local.breeze_loading() || recently_failed;
        match guard_step(action, self.local.sensevoice_loaded(), blocked) {
            GuardStep::Nothing => {}
            GuardStep::Load => {
                self.load_breeze_blocking("guard");
            }
            GuardStep::Unload(reason) => {
                if self.local.unload_breeze() {
                    info!(?reason, "Breeze unloaded; using the CPU engine");
                }
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
                std::thread::sleep(Duration::from_millis(200));
            }
            self.poll_guard();
        }
    }

    /// Hotkey pressed: Breeze unloaded (idle, on demand) but allowed now -> load it in the
    /// background. If it is not ready when the utterance ends, SenseVoice transcribes it.
    fn on_press(self: &Arc<Self>) {
        *lock(&self.last_use) = Instant::now();
        if self.local.breeze_loaded() || self.local.breeze_loading() {
            return;
        }
        if self.local.startup() == Startup::Loading {
            return;
        }
        let p = self.providers();
        if p.purpose(Purpose::Asr).target != Target::Local && p.cloud_asr.is_some() {
            return;
        }
        let me = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("v2c-breeze-load".into())
            .spawn(move || me.load_on_demand());
    }

    fn load_on_demand(&self) {
        let cfg = self.config();
        if local_asr::breeze_model(&cfg, &self.paths).is_none() {
            return;
        }
        let fg = Self::foreground();
        let input = GuardInput {
            gpu: self.monitor.snapshot(),
            cuda_available: self.cuda_available(),
            breeze_model_present: true,
            breeze_vram_mb: breeze_vram_estimate_mb(cfg.local.breeze_quant),
            foreground: fg.as_ref().map(|(exe, fullscreen)| ForegroundInfo {
                exe,
                fullscreen: *fullscreen,
            }),
            breeze_loaded: false,
            busy: false,
            idle: Duration::ZERO,
            now: Instant::now(),
        };
        match lock(&self.guard).can_load(&cfg, &input) {
            Ok(()) => {}
            Err(reason) => {
                debug!(?reason, "Breeze not loaded on demand");
                return;
            }
        }
        self.load_breeze_blocking("hotkey");
    }

    /// The local engine, waiting while it is still loading.
    async fn local_asr(&self, wait: bool) -> Result<Arc<dyn AsrProvider>, &'static str> {
        let deadline = Instant::now() + ASR_LOAD_WAIT;
        loop {
            match self.local.active() {
                Err("asr_loading") if wait && Instant::now() < deadline => {
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

        debug!(
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
        if diff.breeze_model && self.local.unload_breeze() {
            info!("Breeze unloaded: the model file changed");
        }
        self.reload_vocab();
        if !self.local.sensevoice_loaded() && self.local.startup() != Startup::Loading {
            if let Some(dir) = local_asr::sensevoice_model(&self.paths) {
                if !self.sensevoice_loading.swap(true, Ordering::SeqCst) {
                    let me = Arc::clone(self);
                    let spawned = std::thread::Builder::new()
                        .name("v2c-sv-load".into())
                        .spawn(move || {
                            if let Ok(sv) = local_asr::load_sensevoice(&dir) {
                                me.local.set_sensevoice(sv);
                            }
                            me.sensevoice_loading.store(false, Ordering::SeqCst);
                        });
                    if spawned.is_err() {
                        self.sensevoice_loading.store(false, Ordering::SeqCst);
                    }
                }
            }
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

/// The Breeze model file chosen by `started` is still the one `now` wants.
fn quant_still_current(started: &Config, now: &Config) -> bool {
    started.local.breeze_quant == now.local.breeze_quant
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
    /// `[local] breeze_quant`: a loaded Breeze is the wrong file and is dropped.
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
            breeze_model: a.local.breeze_quant != b.local.breeze_quant,
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
        assert!(quant_still_current(&a, &b));
        b.polish.enabled = false;
        assert!(quant_still_current(&a, &b));
        b.local.breeze_quant = BreezeQuant::Q8_0;
        assert!(!quant_still_current(&a, &b));
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
