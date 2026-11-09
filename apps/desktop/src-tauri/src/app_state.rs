//! Shared app state, config loading, and the background ASR loader.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::broadcast;
use tracing::{info, warn};
use v2c_asr_local::{BreezeAsr, BreezeOptions};
use v2c_core::asr::AsrContext;
use v2c_core::audio::AudioClip;
use v2c_core::config::{Config, GpuProfile, LocalAsr, LOCAL};
use v2c_core::pipeline::{Pipeline, PipelineConfig};

use crate::controller::{AsrState, StatusEvent};
use crate::paths::{self, Paths};

/// State shared with Tauri commands.
pub struct AppState {
    pub paths: Paths,
    pub config: Arc<Config>,
    pub status: broadcast::Sender<StatusEvent>,
    /// Latest status, for UI pages that open after the event was sent.
    pub last_status: Arc<Mutex<Option<StatusEvent>>>,
    /// Keeps the platform pieces (hook, recorder) alive for the app's lifetime.
    pub keep_alive: Mutex<Vec<Box<dyn std::any::Any + Send>>>,
}

/// Loads `config.toml`, creating it with defaults if missing. On a parse error the defaults are
/// used and the file is left untouched; the error code is returned for the status display.
pub fn load_or_create_config(path: &Path) -> (Config, Option<&'static str>) {
    match std::fs::read_to_string(path) {
        Ok(s) => match Config::from_toml_str(&s) {
            Ok(cfg) => (cfg, None),
            Err(e) => {
                warn!(error = %e, "config.toml is invalid; using defaults");
                (Config::with_defaults(), Some("config_invalid"))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let cfg = Config::with_defaults();
            match save_config(path, &cfg) {
                Ok(()) => info!("created default config.toml"),
                Err(e) => warn!(error = %e, "could not write default config.toml"),
            }
            (cfg, None)
        }
        Err(e) => {
            warn!(error = %e, "could not read config.toml; using defaults");
            (Config::with_defaults(), Some("config_unreadable"))
        }
    }
}

/// Writes `cfg` to `path` (creating the folder), via a temporary file so a crash never leaves
/// a half-written `config.toml`.
pub fn save_config(path: &Path, cfg: &Config) -> std::io::Result<()> {
    let text = cfg
        .to_toml_string()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn pipeline_config(cfg: &Config) -> PipelineConfig {
    PipelineConfig {
        polish_enabled: cfg.polish.enabled,
        polish_timeout_ms: cfg.polish.timeout_ms,
        min_clip_ms: cfg.audio.min_clip_ms,
        // TODO(T1.8): personal vocabulary.
        vocabulary: Vec::new(),
    }
}

/// Which local model file to load, or the error code if none can be used.
pub fn choose_model(cfg: &Config, search_dirs: &[PathBuf]) -> Result<PathBuf, &'static str> {
    if cfg.local.asr == LocalAsr::SenseVoice {
        // TODO(T1.10): SenseVoice provider.
        return Err("sensevoice_not_available");
    }
    let file = paths::breeze_model_file(cfg.local.breeze_quant);
    paths::find_model(file, search_dirs).ok_or("model_missing")
}

/// Loads Breeze and builds the pipeline (blocking; run off the async workers).
pub fn load_pipeline(cfg: &Config, paths: &Paths) -> Result<Arc<Pipeline>, &'static str> {
    if cfg.route.asr != LOCAL {
        // TODO(T1.7): cloud ASR. Local-first: fall back to the local model.
        warn!("cloud ASR is not implemented yet; using the local model");
    }
    let model = choose_model(cfg, &paths.model_search_dirs())?;
    let opts = BreezeOptions {
        use_gpu: cfg.gpu.enabled && cfg.gpu.profile != GpuProfile::CpuOnly,
        ..BreezeOptions::default()
    };
    let t = Instant::now();
    let breeze = BreezeAsr::load(&model, opts).map_err(|e| {
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
    // Warm-up: the first inference initialises GPU kernels; do it now, not on the first word.
    let t = Instant::now();
    let warm =
        breeze.transcribe_blocking(&AudioClip::new(vec![0.0; 16_000]), &AsrContext::default());
    info!(
        warmup_ms = t.elapsed().as_millis() as u64,
        ok = warm.is_ok(),
        "Breeze warm-up"
    );
    // TODO(T1.5): LM Studio LLM for polish / translate.
    Ok(Arc::new(Pipeline::new(
        Arc::new(breeze),
        None,
        pipeline_config(cfg),
    )))
}

/// Maps a load result to the controller's ASR state.
pub fn asr_state(result: Result<Arc<Pipeline>, &'static str>) -> AsrState {
    match result {
        Ok(p) => AsrState::Ready(p),
        Err(code) => AsrState::Unavailable(code),
    }
}

#[cfg(test)]
mod tests {
    use v2c_core::config::BreezeQuant;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("v2c-appstate-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn missing_config_is_created_with_defaults() {
        let dir = temp_dir("create");
        let path = dir.join("sub").join("config.toml");
        let (cfg, err) = load_or_create_config(&path);
        assert_eq!(err, None);
        assert_eq!(cfg, Config::with_defaults());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(Config::from_toml_str(&written).unwrap(), cfg);
        // Second load reads the file.
        let (again, err) = load_or_create_config(&path);
        assert_eq!((again, err), (cfg, None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_config_falls_back_without_overwriting() {
        let dir = temp_dir("invalid");
        let path = dir.join("config.toml");
        std::fs::write(&path, "route = 5").unwrap();
        let (cfg, err) = load_or_create_config(&path);
        assert_eq!(err, Some("config_invalid"));
        assert_eq!(cfg, Config::with_defaults());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "route = 5");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_choice_follows_quant_and_reports_missing() {
        let dir = temp_dir("model");
        let mut cfg = Config::with_defaults();
        assert_eq!(
            choose_model(&cfg, std::slice::from_ref(&dir)),
            Err("model_missing")
        );
        std::fs::write(dir.join("ggml-breeze-asr-25-q8_0.bin"), b"x").unwrap();
        assert_eq!(
            choose_model(&cfg, std::slice::from_ref(&dir)),
            Err("model_missing")
        );
        cfg.local.breeze_quant = BreezeQuant::Q8_0;
        assert_eq!(
            choose_model(&cfg, std::slice::from_ref(&dir)),
            Ok(dir.join("ggml-breeze-asr-25-q8_0.bin"))
        );
        cfg.local.asr = LocalAsr::SenseVoice;
        assert_eq!(
            choose_model(&cfg, std::slice::from_ref(&dir)),
            Err("sensevoice_not_available")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipeline_config_comes_from_config() {
        let mut cfg = Config::with_defaults();
        cfg.polish.enabled = false;
        cfg.audio.min_clip_ms = 450;
        let p = pipeline_config(&cfg);
        assert!(!p.polish_enabled);
        assert_eq!(p.min_clip_ms, 450);
        assert_eq!(p.polish_timeout_ms, 1000);
    }
}
