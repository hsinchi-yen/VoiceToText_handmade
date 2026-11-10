//! Local speech recognition: exactly **one** model in memory at a time (`tasks/plan.md`
//! section 5, "同一時間只載入一個辨識模型"): Breeze-ASR-25 or TEA-ASR-1.1 on the GPU, or
//! SenseVoice-Small on the CPU.
//!
//! [`LocalEngines`] has a single slot. [`LocalEngines::switch_model`] is the only way to change
//! it: it empties the slot, waits until every running utterance has released the old model
//! (its `Arc` is unique again), drops it, and only then loads the new one. While a switch runs
//! the slot is empty, so a new utterance waits for the new model (`asr_loading`) instead of
//! running old and new side by side. Switches are serialised: a second one is refused.
//!
//! Which model to load is decided by `v2c_asr_local::select` (startup) and the VRAM guard
//! (`gpu_task::target`); SenseVoice is only loaded when it is the model to use (configured, or
//! the stand-in while the GPU model yields), never as a resident standby.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tracing::{info, warn};
use v2c_asr_local::{
    gpu_engine, BreezeAsr, BreezeOptions, LocalAsrKind, SenseVoiceAsr, SenseVoiceOptions, TeaAsr,
    TeaOptions,
};
use v2c_core::asr::{AsrContext, AsrProvider};
use v2c_core::audio::AudioClip;
use v2c_core::config::{BreezeQuant, Config, GpuProfile};

use crate::paths::{self, Paths};

/// How long a switch waits for running utterances to release the old model.
pub const RELEASE_WAIT: Duration = Duration::from_secs(120);

/// Startup progress of the local engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    Loading,
    /// Startup finished; if no engine loaded, this is the error code to show.
    Done(Option<&'static str>),
}

/// The loaded model.
#[derive(Clone)]
pub struct Loaded {
    pub kind: LocalAsrKind,
    pub asr: Arc<dyn AsrProvider>,
}

/// The single local model slot (shared; all methods take `&self`).
pub struct LocalEngines {
    slot: RwLock<Option<Loaded>>,
    switching: AtomicBool,
    startup: RwLock<Startup>,
}

impl Default for LocalEngines {
    fn default() -> Self {
        Self {
            slot: RwLock::new(None),
            switching: AtomicBool::new(false),
            startup: RwLock::new(Startup::Loading),
        }
    }
}

fn read<T: Clone>(l: &RwLock<T>) -> T {
    l.read().unwrap_or_else(|p| p.into_inner()).clone()
}

fn write<T>(l: &RwLock<T>, v: T) -> T {
    std::mem::replace(&mut *l.write().unwrap_or_else(|p| p.into_inner()), v)
}

/// Clears the `switching` flag when the switch ends, however it ends.
struct SwitchToken<'a>(&'a AtomicBool);

impl Drop for SwitchToken<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl LocalEngines {
    /// The engine for the next utterance, or the error code to show.
    pub fn active(&self) -> Result<Arc<dyn AsrProvider>, &'static str> {
        if let Some(l) = read(&self.slot) {
            return Ok(l.asr);
        }
        active_error(read(&self.startup), self.switching())
    }

    /// Which model is loaded, if any.
    pub fn current(&self) -> Option<LocalAsrKind> {
        read(&self.slot).map(|l| l.kind)
    }

    /// A switch (unload + load) is running; the slot is empty meanwhile.
    pub fn switching(&self) -> bool {
        self.switching.load(Ordering::SeqCst)
    }

    pub fn startup(&self) -> Startup {
        read(&self.startup)
    }

    pub fn set_startup(&self, s: Startup) {
        write(&self.startup, s);
    }

    /// Replaces the loaded model with `kind` from `load`: empties the slot, waits up to
    /// `release_wait` until the old model is no longer used, drops it, then calls `load`.
    /// `kind = None` only unloads. Errors: `switch_running` (another switch is in progress),
    /// `model_busy` (the old model was not released in time; it stays loaded), or `load`'s
    /// error (the slot stays empty).
    pub fn switch_model<F>(
        &self,
        kind: Option<LocalAsrKind>,
        release_wait: Duration,
        load: F,
    ) -> Result<(), &'static str>
    where
        F: FnOnce() -> Result<Arc<dyn AsrProvider>, &'static str>,
    {
        if self.switching.swap(true, Ordering::SeqCst) {
            return Err("switch_running");
        }
        let _token = SwitchToken(&self.switching);
        if let Some(old) = write(&self.slot, None) {
            let deadline = Instant::now() + release_wait;
            while Arc::strong_count(&old.asr) > 1 {
                if Instant::now() >= deadline {
                    warn!(
                        asr = old.kind.provider_name(),
                        "model still in use; switch cancelled"
                    );
                    write(&self.slot, Some(old));
                    return Err("model_busy");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let from = old.kind.provider_name();
            drop(old);
            info!(asr = from, "local ASR model unloaded");
        }
        let Some(kind) = kind else {
            return Ok(());
        };
        let asr = load()?;
        write(&self.slot, Some(Loaded { kind, asr }));
        Ok(())
    }
}

/// Error code when no engine is loaded.
fn active_error(startup: Startup, switching: bool) -> Result<Arc<dyn AsrProvider>, &'static str> {
    match startup {
        Startup::Loading => Err("asr_loading"),
        _ if switching => Err("asr_loading"),
        Startup::Done(code) => Err(code.unwrap_or("model_missing")),
    }
}

/// Identifies the GPU model file a config needs: a loaded engine with another key is stale.
pub fn gpu_model_key(cfg: &Config) -> (LocalAsrKind, Option<BreezeQuant>) {
    match gpu_engine(cfg) {
        LocalAsrKind::TeaAsr => (LocalAsrKind::TeaAsr, None),
        kind => (kind, Some(cfg.local.breeze_quant)),
    }
}

/// Path of the Breeze GGML file for the configured quantisation, if it exists.
pub fn breeze_model(cfg: &Config, paths: &Paths) -> Option<PathBuf> {
    paths::find_model(
        paths::breeze_model_file(cfg.local.breeze_quant),
        &paths.model_search_dirs(),
    )
}

/// Path of the TEA-ASR GGUF file, if it exists.
pub fn tea_model(paths: &Paths) -> Option<PathBuf> {
    paths::find_model(paths::TEA_MODEL_FILE, &paths.model_search_dirs())
}

/// Model file of the config's GPU engine (`select::gpu_engine`), if it exists.
pub fn gpu_model(cfg: &Config, paths: &Paths) -> Option<PathBuf> {
    match gpu_engine(cfg) {
        LocalAsrKind::TeaAsr => tea_model(paths),
        _ => breeze_model(cfg, paths),
    }
}

/// The SenseVoice model directory, if it exists.
pub fn sensevoice_model(paths: &Paths) -> Option<PathBuf> {
    paths::find_sensevoice_dir(&paths.model_search_dirs())
}

/// Loads model `kind` for `cfg` (blocking; run off the async workers).
pub fn load_model(
    kind: LocalAsrKind,
    cfg: &Config,
    paths: &Paths,
) -> Result<Arc<dyn AsrProvider>, &'static str> {
    match kind {
        LocalAsrKind::SenseVoice => {
            let dir = sensevoice_model(paths).ok_or("model_missing")?;
            load_sensevoice(&dir).map(|s| Arc::new(s) as Arc<dyn AsrProvider>)
        }
        LocalAsrKind::TeaAsr => {
            let model = tea_model(paths).ok_or("model_missing")?;
            load_tea(&model).map(|t| Arc::new(t) as Arc<dyn AsrProvider>)
        }
        LocalAsrKind::Breeze => {
            let model = breeze_model(cfg, paths).ok_or("model_missing")?;
            load_breeze(cfg, &model).map(|b| Arc::new(b) as Arc<dyn AsrProvider>)
        }
    }
}

/// Loads Breeze and runs a warm-up (blocking; run off the async workers).
pub fn load_breeze(cfg: &Config, model: &Path) -> Result<BreezeAsr, &'static str> {
    let opts = BreezeOptions {
        use_gpu: cfg.gpu.enabled && cfg.gpu.profile != GpuProfile::CpuOnly,
        ..BreezeOptions::default()
    };
    let t = Instant::now();
    let breeze = BreezeAsr::load(model, opts).map_err(|e| {
        warn!(error = %e, "Breeze model failed to load");
        "model_load_failed"
    })?;
    info!(
        load_ms = t.elapsed().as_millis() as u64,
        cuda_feature = v2c_asr_local::breeze::cuda_enabled(),
        gpu_requested = breeze.options().use_gpu,
        model = %model.file_name().map(|f| f.to_string_lossy()).unwrap_or_default(),
        "Breeze loaded"
    );
    // The first inference initialises GPU kernels; do it now, not on the first word.
    let t = Instant::now();
    let warm =
        breeze.transcribe_blocking(&AudioClip::new(vec![0.0; 16_000]), &AsrContext::default());
    info!(
        warmup_ms = t.elapsed().as_millis() as u64,
        ok = warm.is_ok(),
        "Breeze warm-up"
    );
    Ok(breeze)
}

/// Loads TEA-ASR on the GPU (CUDA required) and runs a warm-up (blocking).
pub fn load_tea(model: &Path) -> Result<TeaAsr, &'static str> {
    let t = Instant::now();
    let tea = TeaAsr::load(model, TeaOptions::default()).map_err(|e| {
        // The message holds paths, backend and format details only (no user data).
        warn!(error = %e, asr = v2c_asr_local::tea::PROVIDER_NAME, "TEA-ASR model failed to load");
        "model_load_failed"
    })?;
    info!(
        asr = v2c_asr_local::tea::PROVIDER_NAME,
        load_ms = t.elapsed().as_millis() as u64,
        tea_feature = v2c_asr_local::tea::tea_enabled(),
        model = %model.file_name().map(|f| f.to_string_lossy()).unwrap_or_default(),
        "TEA-ASR loaded"
    );
    let t = Instant::now();
    let warm = tea.transcribe_blocking(&AudioClip::new(vec![0.0; 16_000]), &AsrContext::default());
    info!(
        asr = v2c_asr_local::tea::PROVIDER_NAME,
        warmup_ms = t.elapsed().as_millis() as u64,
        ok = warm.is_ok(),
        "TEA-ASR warm-up"
    );
    Ok(tea)
}

/// Loads SenseVoice from its model directory (blocking).
pub fn load_sensevoice(dir: &Path) -> Result<SenseVoiceAsr, &'static str> {
    let t = Instant::now();
    let opts = SenseVoiceOptions::default();
    let threads = opts.n_threads;
    let sv = SenseVoiceAsr::load(dir, opts).map_err(|e| {
        // The message holds paths and versions only (no user data).
        warn!(error = %e, "SenseVoice failed to load");
        "sensevoice_not_available"
    })?;
    info!(
        load_ms = t.elapsed().as_millis() as u64,
        threads, "SenseVoice loaded"
    );
    Ok(sv)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use v2c_core::asr::Transcript;
    use v2c_core::config::LocalAsr;
    use v2c_core::Lang;

    use super::*;

    /// Counts live fake models, and the most that were ever alive at once.
    #[derive(Default)]
    pub(crate) struct Census {
        live: AtomicUsize,
        max: AtomicUsize,
    }

    impl Census {
        pub(crate) fn max(&self) -> usize {
            self.max.load(Ordering::SeqCst)
        }
        pub(crate) fn live(&self) -> usize {
            self.live.load(Ordering::SeqCst)
        }
    }

    pub(crate) struct FakeAsr(Arc<Census>);

    impl FakeAsr {
        pub(crate) fn load(c: &Arc<Census>) -> Result<Arc<dyn AsrProvider>, &'static str> {
            let n = c.live.fetch_add(1, Ordering::SeqCst) + 1;
            c.max.fetch_max(n, Ordering::SeqCst);
            Ok(Arc::new(FakeAsr(Arc::clone(c))))
        }
    }

    impl Drop for FakeAsr {
        fn drop(&mut self) {
            self.0.live.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl AsrProvider for FakeAsr {
        fn name(&self) -> &str {
            "fake"
        }
        async fn transcribe(&self, _: &AudioClip, _: &AsrContext) -> v2c_core::Result<Transcript> {
            Ok(Transcript {
                text: String::new(),
                lang: Lang::Zh,
            })
        }
    }

    #[test]
    fn errors_before_and_after_startup() {
        let e = LocalEngines::default();
        assert_eq!(e.active().err(), Some("asr_loading"));
        e.set_startup(Startup::Done(None));
        assert_eq!(e.active().err(), Some("model_missing"));
        e.set_startup(Startup::Done(Some("sensevoice_not_available")));
        assert_eq!(e.active().err(), Some("sensevoice_not_available"));
        assert_eq!(e.current(), None);
        assert!(!e.switching());
    }

    #[test]
    fn switches_unload_before_loading_and_never_overlap() {
        let census = Arc::new(Census::default());
        let e = Arc::new(LocalEngines::default());
        e.set_startup(Startup::Done(None));
        let short = Duration::from_secs(5);

        e.switch_model(Some(LocalAsrKind::Breeze), short, || FakeAsr::load(&census))
            .unwrap();
        assert_eq!(e.current(), Some(LocalAsrKind::Breeze));
        // An utterance is using Breeze while the switch to SenseVoice starts.
        let in_use = e.active().unwrap();
        let e2 = Arc::clone(&e);
        let c2 = Arc::clone(&census);
        let switch = std::thread::spawn(move || {
            e2.switch_model(
                Some(LocalAsrKind::SenseVoice),
                Duration::from_secs(5),
                || FakeAsr::load(&c2),
            )
        });
        // The slot is empty during the switch: the next utterance waits.
        while !e.switching() {
            std::thread::yield_now();
        }
        assert_eq!(e.active().err(), Some("asr_loading"));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(census.live(), 1, "new model must wait for the old one");
        drop(in_use);
        switch.join().unwrap().unwrap();
        assert_eq!(e.current(), Some(LocalAsrKind::SenseVoice));
        assert_eq!(census.max(), 1);
        assert_eq!(census.live(), 1);

        // Unload only.
        e.switch_model(None, short, || unreachable!()).unwrap();
        assert_eq!(e.current(), None);
        assert_eq!(census.live(), 0);
        assert_eq!(e.active().err(), Some("model_missing"));
    }

    #[test]
    fn busy_old_model_cancels_the_switch_and_stays() {
        let census = Arc::new(Census::default());
        let e = LocalEngines::default();
        e.switch_model(Some(LocalAsrKind::TeaAsr), Duration::ZERO, || {
            FakeAsr::load(&census)
        })
        .unwrap();
        let held = e.active().unwrap();
        assert_eq!(
            e.switch_model(
                Some(LocalAsrKind::SenseVoice),
                Duration::from_millis(50),
                || FakeAsr::load(&census)
            ),
            Err("model_busy")
        );
        assert_eq!(e.current(), Some(LocalAsrKind::TeaAsr));
        assert_eq!(census.max(), 1);
        drop(held);
    }

    #[test]
    fn failed_load_leaves_the_slot_empty_and_a_second_switch_is_refused() {
        let census = Arc::new(Census::default());
        let e = Arc::new(LocalEngines::default());
        e.switch_model(Some(LocalAsrKind::Breeze), Duration::ZERO, || {
            FakeAsr::load(&census)
        })
        .unwrap();
        assert_eq!(
            e.switch_model(Some(LocalAsrKind::TeaAsr), Duration::ZERO, || Err(
                "model_load_failed"
            )),
            Err("model_load_failed")
        );
        assert_eq!(e.current(), None);
        assert_eq!(census.live(), 0);
        // While one switch runs, another is refused.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let e2 = Arc::clone(&e);
        let c2 = Arc::clone(&census);
        let t = std::thread::spawn(move || {
            e2.switch_model(Some(LocalAsrKind::SenseVoice), Duration::ZERO, || {
                rx.recv().unwrap();
                FakeAsr::load(&c2)
            })
        });
        while !e.switching() {
            std::thread::yield_now();
        }
        assert_eq!(
            e.switch_model(
                Some(LocalAsrKind::Breeze),
                Duration::ZERO,
                || FakeAsr::load(&census)
            ),
            Err("switch_running")
        );
        tx.send(()).unwrap();
        t.join().unwrap().unwrap();
        assert_eq!(e.current(), Some(LocalAsrKind::SenseVoice));
        assert_eq!(census.max(), 1);
    }

    #[test]
    fn model_key_tracks_engine_and_quant() {
        let mut a = Config::with_defaults();
        assert_eq!(
            gpu_model_key(&a),
            (LocalAsrKind::Breeze, Some(BreezeQuant::Q5_0))
        );
        a.local.asr = LocalAsr::TeaAsr;
        assert_eq!(gpu_model_key(&a), (LocalAsrKind::TeaAsr, None));
        // The Breeze quantisation does not matter for TEA-ASR.
        let mut b = a.clone();
        b.local.breeze_quant = BreezeQuant::Q8_0;
        assert_eq!(gpu_model_key(&a), gpu_model_key(&b));
    }

    #[test]
    fn gpu_model_follows_the_selected_engine() {
        let dir = std::env::temp_dir().join(format!("v2c-local-asr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths {
            data_dir: dir.clone(),
            local_data_dir: dir.clone(),
        };
        let models = paths.models_dir();
        std::fs::create_dir_all(&models).unwrap();
        let mut cfg = Config::with_defaults();
        cfg.local.asr = LocalAsr::TeaAsr;
        // Only look at our temp dir (debug builds also search the repo's models/).
        let only_ours = |p: Option<PathBuf>| p.filter(|p| p.starts_with(&dir));
        assert_eq!(only_ours(gpu_model(&cfg, &paths)), None);
        std::fs::write(models.join(paths::TEA_MODEL_FILE), b"x").unwrap();
        assert_eq!(
            only_ours(gpu_model(&cfg, &paths)),
            Some(models.join(paths::TEA_MODEL_FILE))
        );
        // Breeze is not there: a TEA file does not count for Breeze.
        cfg.local.asr = LocalAsr::Breeze;
        assert_eq!(only_ours(gpu_model(&cfg, &paths)), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
