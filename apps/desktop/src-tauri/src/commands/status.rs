//! App status for the UI. Live updates arrive as the `v2c:status` event.

use serde::Serialize;
use tauri::State;

use crate::app_state::AppState;
use crate::controller::StatusEvent;

#[derive(Debug, Serialize)]
pub struct AppInfo {
    pub version: &'static str,
    pub cuda: bool,
    pub data_dir: String,
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        cuda: v2c_asr_local::breeze::cuda_enabled(),
        data_dir: state.paths.data_dir.display().to_string(),
    }
}

/// The most recent status event, if any.
#[tauri::command]
pub fn last_status(state: State<'_, AppState>) -> Option<StatusEvent> {
    state
        .last_status
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}
