//! VRAM usage bar for the settings page (T1.10b / T1.11, `tasks/plan.md` section 5.1).
//!
//! The page polls [`gpu_usage`] about every 2 s while it is open. It uses its own NVML handle
//! (`GpuMonitor::detailed_snapshot`, which enumerates GPU processes, so it is not part of the
//! guard's 5 s poll). On Windows (WDDM) NVML does not report per-process usage, so v2c's share
//! is shown as the Breeze estimate (`breeze_need_mb`) when the per-process figure is missing.

use std::sync::OnceLock;

use serde::Serialize;
use tauri::{State, WebviewWindow};
use tracing::warn;
use v2c_asr_local::{breeze_vram_estimate_mb, GpuGuard, GpuMonitor, GpuSnapshot};
use v2c_core::config::{Config, GpuProfile};

use crate::app_state::AppState;

const MAIN_WINDOW: &str = "main";

static MONITOR: OnceLock<GpuMonitor> = OnceLock::new();

/// The NVML handle for the settings page (created on first use).
pub fn monitor() -> &'static GpuMonitor {
    MONITOR.get_or_init(GpuMonitor::new)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpuUsage {
    /// NVML found an NVIDIA GPU.
    pub available: bool,
    pub name: Option<String>,
    pub total_mb: u64,
    pub free_mb: u64,
    /// Used by all programs.
    pub used_mb: u64,
    /// v2c's own usage when NVML reports it (Linux / TCC), else `None`.
    pub v2c_mb: Option<u64>,
    /// VRAM Breeze needs with the configured quantisation.
    pub breeze_need_mb: u64,
    pub reserve_mb: u64,
    pub profile: GpuProfile,
    pub cuda_build: bool,
}

/// Builds the usage view from a snapshot and the config (pure; tested).
pub fn usage(snap: Option<GpuSnapshot>, name: Option<String>, cfg: &Config) -> GpuUsage {
    let need = GpuGuard::default().breeze_need_mb(breeze_vram_estimate_mb(cfg.local.breeze_quant));
    GpuUsage {
        available: snap.is_some(),
        name,
        total_mb: snap.map_or(0, |s| s.total_mb),
        free_mb: snap.map_or(0, |s| s.free_mb),
        used_mb: snap.map_or(0, |s| s.used_mb()),
        v2c_mb: snap.and_then(|s| s.used_by_us_mb),
        breeze_need_mb: need,
        reserve_mb: cfg.gpu.vram_reserve_mb,
        profile: cfg.gpu.profile,
        cuda_build: v2c_asr_local::breeze::cuda_enabled(),
    }
}

#[tauri::command]
pub async fn gpu_usage(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<GpuUsage, &'static str> {
    if window.label() != MAIN_WINDOW {
        warn!(window = window.label(), "gpu command refused");
        return Err("forbidden");
    }
    let cfg = state.engine.config();
    tauri::async_runtime::spawn_blocking(move || {
        let m = monitor();
        usage(m.detailed_snapshot(), m.device_name(), &cfg)
    })
    .await
    .map_err(|_| "internal_error")
}

#[cfg(test)]
mod tests {
    use v2c_core::config::BreezeQuant;

    use super::*;

    #[test]
    fn usage_without_nvidia_is_empty() {
        let u = usage(None, None, &Config::with_defaults());
        assert!(!u.available);
        assert_eq!(
            (u.total_mb, u.free_mb, u.used_mb, u.v2c_mb),
            (0, 0, 0, None)
        );
        assert_eq!(u.reserve_mb, 600);
    }

    #[test]
    fn usage_from_a_snapshot() {
        let mut cfg = Config::with_defaults();
        cfg.local.breeze_quant = BreezeQuant::Q8_0;
        let snap = GpuSnapshot {
            free_mb: 4500,
            total_mb: 6144,
            used_by_us_mb: None,
        };
        let u = usage(Some(snap), Some("RTX".into()), &cfg);
        assert!(u.available);
        assert_eq!(u.used_mb, 6144 - 4500);
        assert_eq!(u.breeze_need_mb, breeze_vram_estimate_mb(BreezeQuant::Q8_0));
        assert_eq!(u.name.as_deref(), Some("RTX"));
    }
}
