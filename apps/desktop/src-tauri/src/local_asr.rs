//! Local speech recognition engines: Breeze-ASR-25 (GPU) and SenseVoice-Small (CPU).
//!
//! Both can be resident at once; [`LocalEngines::active`] returns Breeze when it is loaded,
//! otherwise SenseVoice. Swapping only replaces the slot: an utterance that already took its
//! `Arc` keeps using that engine to the end, and Breeze's VRAM is freed when the last `Arc`
//! drops. Which engine to load is decided by `v2c_asr_local::select` (startup) and the VRAM
//! guard (`gpu_task.rs`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tracing::{info, warn};
use v2c_asr_local::{BreezeAsr, BreezeOptions, SenseVoiceAsr, SenseVoiceOptions};
use v2c_core::asr::{AsrContext, AsrProvider};
use v2c_core::audio::AudioClip;
use v2c_core::config::{Config, GpuProfile};

use crate::paths::{self, Paths};

/// Startup progress of the local engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    Loading,
    /// Startup finished; if no engine loaded, this is the error code to show.
    Done(Option<&'static str>),
}

/// The loaded local engines (shared; all methods take `&self`).
pub struct LocalEngines {
    breeze: RwLock<Option<Arc<BreezeAsr>>>,
    sensevoice: RwLock<Option<Arc<SenseVoiceAsr>>>,
    breeze_loading: AtomicBool,
    startup: RwLock<Startup>,
}

impl Default for LocalEngines {
    fn default() -> Self {
        Self {
            breeze: RwLock::new(None),
            sensevoice: RwLock::new(None),
            breeze_loading: AtomicBool::new(false),
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

impl LocalEngines {
    /// The engine for the next utterance, or the error code to show.
    pub fn active(&self) -> Result<Arc<dyn AsrProvider>, &'static str> {
        if let Some(b) = read(&self.breeze) {
            return Ok(b);
        }
        if let Some(s) = read(&self.sensevoice) {
            return Ok(s);
        }
        active_error(read(&self.startup), self.breeze_loading())
    }

    pub fn breeze_loaded(&self) -> bool {
        read(&self.breeze).is_some()
    }

    pub fn sensevoice_loaded(&self) -> bool {
        read(&self.sensevoice).is_some()
    }

    pub fn breeze_loading(&self) -> bool {
        self.breeze_loading.load(Ordering::SeqCst)
    }

    /// Marks a Breeze load as running; `false` if one already is.
    pub fn begin_breeze_load(&self) -> bool {
        !self.breeze_loading.swap(true, Ordering::SeqCst)
    }

    pub fn end_breeze_load(&self, breeze: Option<BreezeAsr>) {
        if let Some(b) = breeze {
            write(&self.breeze, Some(Arc::new(b)));
        }
        self.breeze_loading.store(false, Ordering::SeqCst);
    }

    /// Removes Breeze from the slot (it is freed once a running utterance releases it).
    /// Returns whether it was loaded.
    pub fn unload_breeze(&self) -> bool {
        write(&self.breeze, None).is_some()
    }

    pub fn set_sensevoice(&self, sv: SenseVoiceAsr) {
        write(&self.sensevoice, Some(Arc::new(sv)));
    }

    pub fn startup(&self) -> Startup {
        read(&self.startup)
    }

    pub fn set_startup(&self, s: Startup) {
        write(&self.startup, s);
    }
}

/// Error code when no engine is loaded.
fn active_error(
    startup: Startup,
    breeze_loading: bool,
) -> Result<Arc<dyn AsrProvider>, &'static str> {
    match startup {
        Startup::Loading => Err("asr_loading"),
        _ if breeze_loading => Err("asr_loading"),
        Startup::Done(code) => Err(code.unwrap_or("model_missing")),
    }
}

/// Path of the Breeze GGML file for the configured quantisation, if it exists.
pub fn breeze_model(cfg: &Config, paths: &Paths) -> Option<PathBuf> {
    paths::find_model(
        paths::breeze_model_file(cfg.local.breeze_quant),
        &paths.model_search_dirs(),
    )
}

/// The SenseVoice model directory, if it exists.
pub fn sensevoice_model(paths: &Paths) -> Option<PathBuf> {
    paths::find_sensevoice_dir(&paths.model_search_dirs())
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
mod tests {
    use super::*;

    #[test]
    fn errors_before_and_after_startup() {
        let e = LocalEngines::default();
        assert_eq!(e.active().err(), Some("asr_loading"));
        e.set_startup(Startup::Done(None));
        assert_eq!(e.active().err(), Some("model_missing"));
        e.set_startup(Startup::Done(Some("sensevoice_not_available")));
        assert_eq!(e.active().err(), Some("sensevoice_not_available"));
        // A Breeze load in progress (e.g. on-demand) reads as "loading".
        assert!(e.begin_breeze_load());
        assert!(!e.begin_breeze_load());
        assert_eq!(e.active().err(), Some("asr_loading"));
        e.end_breeze_load(None);
        assert!(!e.breeze_loading());
        assert_eq!(e.active().err(), Some("sensevoice_not_available"));
        assert!(!e.unload_breeze());
        assert!(!e.breeze_loaded() && !e.sensevoice_loaded());
    }
}
