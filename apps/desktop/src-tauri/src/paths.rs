//! File locations: `%APPDATA%\v2c\{config.toml, logs\}` (plan section 7) and
//! `%LOCALAPPDATA%\v2c\models\` (plan section 5.1).

use std::path::{Path, PathBuf};

use v2c_core::config::BreezeQuant;

/// Overrides the data directory (tests, portable installs).
const DATA_DIR_ENV: &str = "V2C_DATA_DIR";
/// Extra directory searched first for model files.
pub const MODEL_DIR_ENV: &str = "V2C_MODEL_DIR";

#[derive(Debug, Clone)]
pub struct Paths {
    /// Config and logs: `%APPDATA%\v2c` (roaming).
    pub data_dir: PathBuf,
    /// Large local files (models): `%LOCALAPPDATA%\v2c` (not roaming, plan section 5.1).
    pub local_data_dir: PathBuf,
}

impl Paths {
    /// `%V2C_DATA_DIR%` (used for both), else `%APPDATA%\v2c` + `%LOCALAPPDATA%\v2c` on
    /// Windows, `$XDG_CONFIG_HOME/v2c` + `$XDG_DATA_HOME/v2c` elsewhere.
    pub fn resolve() -> Self {
        if let Some(d) = env_path(DATA_DIR_ENV) {
            return Self {
                data_dir: d.clone(),
                local_data_dir: d,
            };
        }
        let home = || env_path("HOME");
        let (roaming, local) = if cfg!(windows) {
            (env_path("APPDATA"), env_path("LOCALAPPDATA"))
        } else {
            (
                env_path("XDG_CONFIG_HOME").or_else(|| home().map(|h| h.join(".config"))),
                env_path("XDG_DATA_HOME").or_else(|| home().map(|h| h.join(".local/share"))),
            )
        };
        let v2c = |base: Option<PathBuf>| base.unwrap_or_else(|| PathBuf::from(".")).join("v2c");
        Self {
            data_dir: v2c(roaming),
            local_data_dir: v2c(local),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.data_dir.join("config.toml")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// `%LOCALAPPDATA%\v2c\models`.
    pub fn models_dir(&self) -> PathBuf {
        self.local_data_dir.join("models")
    }

    /// Directories searched for model files (any model: Breeze, SenseVoice, ...), in order:
    /// `%V2C_MODEL_DIR%`, `%LOCALAPPDATA%\v2c\models`, then (debug builds only) the repo's `models/`.
    pub fn model_search_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Some(d) = env_path(MODEL_DIR_ENV) {
            dirs.push(d);
        }
        dirs.push(self.models_dir());
        if cfg!(debug_assertions) {
            dirs.push(repo_models_dir());
        }
        dirs
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `<repo>/models`, derived from this crate's location (`apps/desktop/src-tauri`).
fn repo_models_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../models")
}

/// GGML file name for a Breeze quantisation, as produced by `tools/convert-breeze`.
pub fn breeze_model_file(quant: BreezeQuant) -> &'static str {
    match quant {
        BreezeQuant::Q5_0 => "ggml-breeze-asr-25-q5_0.bin",
        BreezeQuant::Q8_0 => "ggml-breeze-asr-25-q8_0.bin",
    }
}

/// First `dir/file_name` that exists.
pub fn find_model(file_name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(file_name)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_file_names_follow_quant() {
        assert_eq!(
            breeze_model_file(BreezeQuant::Q5_0),
            "ggml-breeze-asr-25-q5_0.bin"
        );
        assert_eq!(
            breeze_model_file(BreezeQuant::Q8_0),
            "ggml-breeze-asr-25-q8_0.bin"
        );
    }

    #[test]
    fn find_model_respects_search_order() {
        let root = std::env::temp_dir().join(format!("v2c-paths-test-{}", std::process::id()));
        let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));
        for d in [&a, &b, &c] {
            std::fs::create_dir_all(d).unwrap();
        }
        let name = "ggml-breeze-asr-25-q5_0.bin";
        std::fs::write(b.join(name), b"x").unwrap();
        std::fs::write(c.join(name), b"x").unwrap();

        let dirs = vec![a.clone(), b.clone(), c.clone()];
        assert_eq!(find_model(name, &dirs), Some(b.join(name)));
        assert_eq!(find_model("missing.bin", &dirs), None);
        assert_eq!(find_model(name, &[a]), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn standard_files_live_under_data_dir() {
        let p = Paths {
            data_dir: PathBuf::from("X"),
            local_data_dir: PathBuf::from("L"),
        };
        assert_eq!(p.config_file(), PathBuf::from("X").join("config.toml"));
        assert_eq!(p.logs_dir(), PathBuf::from("X").join("logs"));
        assert_eq!(p.models_dir(), PathBuf::from("L").join("models"));
        assert!(p
            .model_search_dirs()
            .contains(&PathBuf::from("L").join("models")));
    }
}
