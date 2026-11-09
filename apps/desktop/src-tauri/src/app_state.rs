//! Shared app state and config loading.

use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, watch};
use tracing::{info, warn};
use v2c_core::config::Config;
use v2c_core::pipeline::PipelineConfig;

use crate::controller::{RecordingInfo, StatusEvent};
use crate::engine::Engine;
use crate::paths::Paths;

/// State shared with Tauri commands.
pub struct AppState {
    pub paths: Paths,
    /// The config the app started with. The live config (after reloads) is
    /// `engine.config()`.
    pub config: Arc<Config>,
    pub status: broadcast::Sender<StatusEvent>,
    /// Latest status, for UI pages that open after the event was sent.
    pub last_status: Arc<Mutex<Option<StatusEvent>>>,
    /// Whether a recording is running, hands-free, or translating (for the overlay).
    pub recording: watch::Receiver<RecordingInfo>,
    pub engine: Arc<Engine>,
    /// Keeps the platform pieces (hook, recorder) alive until exit.
    pub keep_alive: Mutex<Vec<Box<dyn std::any::Any + Send>>>,
    /// Run once at exit (stop the recorder, background loops).
    pub on_exit: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
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

/// Reads `config.toml` for a reload; unlike [`load_or_create_config`] it never falls back to
/// defaults (the running config stays in place on error).
pub fn read_config(path: &Path) -> Result<Config, &'static str> {
    let text = std::fs::read_to_string(path).map_err(|_| "config_unreadable")?;
    Config::from_toml_str(&text).map_err(|e| {
        warn!(error = %e, "config.toml is invalid; keeping the running config");
        "config_invalid"
    })
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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
    fn reload_read_never_falls_back_to_defaults() {
        let dir = temp_dir("reload");
        let path = dir.join("config.toml");
        assert_eq!(read_config(&path), Err("config_unreadable"));
        std::fs::write(&path, "route = 5").unwrap();
        assert_eq!(read_config(&path), Err("config_invalid"));
        std::fs::write(&path, "[polish]\nenabled = false\n").unwrap();
        assert!(!read_config(&path).unwrap().polish.enabled);
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
