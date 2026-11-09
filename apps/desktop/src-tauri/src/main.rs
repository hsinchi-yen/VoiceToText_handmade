//! v2c desktop app: hold Right Ctrl, speak, and the text is typed at the cursor.
//!
//! Startup: logging -> config -> Tauri. In `setup`: status forwarder, background model load,
//! recorder, hotkey hook, and the controller task. OS-specific calls go through `v2c-platform`.

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod commands;
mod controller;
mod logging;
mod paths;
mod recorder;
mod sinks;
#[cfg(test)]
mod smoke_tests;

use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};
use tokio::sync::{broadcast, watch};
use tracing::{error, info};

use crate::app_state::AppState;
use crate::controller::{AsrState, StatusEvent};
use crate::recorder::{Recorder, RecorderNotice};

/// Webview event carrying [`StatusEvent`]s (overlay and UI pages listen to it).
const STATUS_EVENT: &str = "v2c:status";

fn main() {
    let paths = paths::Paths::resolve();
    let _log_guard = logging::init(&paths.logs_dir());
    info!(
        version = env!("CARGO_PKG_VERSION"),
        cuda_feature = v2c_asr_local::breeze::cuda_enabled(),
        "v2c starting"
    );

    let (config, config_error) = app_state::load_or_create_config(&paths.config_file());
    let (status, _) = broadcast::channel::<StatusEvent>(256);
    let state = AppState {
        paths,
        config: Arc::new(config),
        status,
        last_status: Arc::new(Mutex::new(None)),
        keep_alive: Mutex::new(Vec::new()),
    };

    let result = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::status::app_info,
            commands::status::last_status,
        ])
        .setup(move |app| {
            start_background(app.handle(), config_error);
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(e) = result {
        error!(error = %e, "tauri exited with an error");
    }
    info!("v2c stopped");
}

fn start_background(app: &tauri::AppHandle, config_error: Option<&'static str>) {
    let state = app.state::<AppState>();

    // Forward status events to the webview and remember the latest one.
    let mut rx = state.status.subscribe();
    let last = Arc::clone(&state.last_status);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let StatusEvent::Error { code } = &ev {
                        info!(code, "status error");
                    }
                    if let Ok(mut l) = last.lock() {
                        *l = Some(ev.clone());
                    }
                    let _ = handle.emit(STATUS_EVENT, ev);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    if let Some(code) = config_error {
        let _ = state.status.send(StatusEvent::Error { code });
    }

    // Load the model in the background so the app starts immediately.
    let (asr_tx, asr_rx) = watch::channel(AsrState::Loading);
    {
        let config = Arc::clone(&state.config);
        let paths = state.paths.clone();
        let status = state.status.clone();
        let spawned = std::thread::Builder::new()
            .name("v2c-asr-load".into())
            .spawn(move || {
                let result = app_state::load_pipeline(&config, &paths);
                match &result {
                    Ok(_) => info!("speech recognition ready"),
                    Err(code) => {
                        error!(code, "speech recognition unavailable");
                        let _ = status.send(StatusEvent::Error { code });
                    }
                }
                let _ = asr_tx.send(app_state::asr_state(result));
            });
        if spawned.is_err() {
            error!("could not spawn the model loader thread");
        }
    }

    let notify_status = state.status.clone();
    // Only a recovery after an error is announced; in `on_press` mode the stream opens on
    // every press and must not interrupt the Recording status.
    let mic_failed = std::sync::atomic::AtomicBool::new(false);
    let recorder = Arc::new(Recorder::start(
        &state.config.audio,
        Arc::new(move |n| {
            use std::sync::atomic::Ordering;
            match n {
                RecorderNotice::Unavailable { code } => {
                    mic_failed.store(true, Ordering::Relaxed);
                    let _ = notify_status.send(StatusEvent::Error { code });
                }
                RecorderNotice::Opened => {
                    if mic_failed.swap(false, Ordering::Relaxed) {
                        let _ = notify_status.send(StatusEvent::MicReady);
                    }
                }
            }
        }),
    ));

    start_controller(&state, recorder, asr_rx);
}

#[cfg(windows)]
fn start_controller(state: &AppState, recorder: Arc<Recorder>, asr: watch::Receiver<AsrState>) {
    use std::time::Duration;

    use tracing::warn;
    use v2c_core::trigger::{Event, TriggerConfig};
    use v2c_platform::windows::foreground::WinForeground;
    use v2c_platform::windows::hotkey::{monotonic_ms, WindowsHotkey};
    use v2c_platform::windows::inject::SendInputInjector;
    use v2c_platform::Hotkey;

    const TICK: Duration = Duration::from_millis(50);

    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    let mut hook = WindowsHotkey::new();
    let hook_tx = events_tx.clone();
    match hook.start(Box::new(move |event| {
        let _ = hook_tx.send(event);
    })) {
        Ok(()) => info!("hotkey hook installed (Right Ctrl)"),
        Err(e) => {
            warn!(error = %e, "hotkey hook failed");
            let _ = state.status.send(StatusEvent::Error {
                code: "hotkey_failed",
            });
        }
    }
    if let Ok(mut keep) = state.keep_alive.lock() {
        keep.push(Box::new(hook));
    }

    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let tick = Event::Tick {
                at_ms: monotonic_ms(),
            };
            if events_tx.send(tick).is_err() {
                break;
            }
        }
    });

    let deps = controller::Deps {
        source: recorder,
        injector: Arc::new(SendInputInjector),
        foreground: Arc::new(WinForeground),
        sinks: sinks::SinkRegistry::with_defaults(),
        status: state.status.clone(),
        asr,
        apps: state.config.apps.clone(),
        trigger: TriggerConfig::from(&state.config.hotkey),
    };
    tauri::async_runtime::spawn(controller::run(deps, events_rx));
    info!("controller started");
}

#[cfg(not(windows))]
fn start_controller(state: &AppState, recorder: Arc<Recorder>, asr: watch::Receiver<AsrState>) {
    // Linux hotkey and injection arrive in Phase 3.
    let _ = (recorder, asr);
    tracing::warn!("hotkey and text injection are not implemented on this platform yet");
    let _ = state.status.send(StatusEvent::Error {
        code: "platform_unsupported",
    });
}
