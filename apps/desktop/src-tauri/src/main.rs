//! v2c desktop app: hold Right Ctrl, speak, and the text is typed at the cursor.
//!
//! Startup: logging -> config -> keyring store -> engine -> Tauri. In `setup`: overlay window,
//! tray icon, status forwarder, local ASR selection + VRAM guard (background thread), LM Studio
//! check, recorder, hotkey hook, the controller task, and the config-reload listener. OS-specific
//! calls go through `v2c-platform` (the overlay's window styles are isolated in `overlay::win`).
//! At exit (`RunEvent::Exit`) the hook, microphone, and background loops are stopped and the
//! log is flushed.

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod commands;
mod config_edit;
mod controller;
mod engine;
mod gpu_task;
mod history_sink;
mod local_asr;
mod logging;
mod overlay;
mod paths;
mod providers;
mod recorder;
mod sinks;
#[cfg(test)]
mod smoke_tests;
mod tray;

use std::sync::{Arc, Mutex};

use tauri::{Emitter, Listener, Manager};
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use v2c_core::secrets::SecretStore;

use crate::app_state::AppState;
use crate::controller::StatusEvent;
use crate::engine::{Engine, EngineHandle};
use crate::recorder::{Recorder, RecorderNotice};

/// Webview event carrying [`StatusEvent`]s (overlay and UI pages listen to it).
const STATUS_EVENT: &str = "v2c:status";

fn main() {
    let paths = paths::Paths::resolve();
    let log_guard = logging::init(&paths.logs_dir());
    info!(
        version = env!("CARGO_PKG_VERSION"),
        cuda_feature = v2c_asr_local::breeze::cuda_enabled(),
        sensevoice_feature = v2c_asr_local::sensevoice::sensevoice_enabled(),
        "v2c starting"
    );

    let (config, config_error) = app_state::load_or_create_config(&paths.config_file());
    let secrets = install_keyring();
    let (status, _) = broadcast::channel::<StatusEvent>(256);
    let engine = Engine::new(paths.clone(), config.clone(), status.clone(), secrets);
    let state = AppState {
        paths,
        config: Arc::new(config),
        status,
        last_status: Arc::new(Mutex::new(None)),
        engine,
        keep_alive: Mutex::new(Vec::new()),
        on_exit: Mutex::new(Vec::new()),
    };

    let app = tauri::Builder::default()
        .manage(state)
        // Start with Windows (HKCU Run key); the entry passes AUTOSTART_ARG to start hidden.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![commands::settings::AUTOSTART_ARG]),
        ))
        // Native file picker for the Breeze import (used from Rust only).
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::status::app_info,
            commands::status::last_status,
            commands::history::history_list,
            commands::history::history_copy,
            commands::history::history_reoutput,
            commands::history::history_delete,
            commands::history::history_clear,
            commands::cloud::cloud_kinds,
            commands::cloud::cloud_list,
            commands::cloud::cloud_models,
            commands::cloud::cloud_test_connection,
            commands::cloud::cloud_save,
            commands::cloud::cloud_remove,
            commands::cloud::cloud_open_key_page,
            commands::vocab::vocab_get,
            commands::vocab::vocab_set,
            commands::settings::settings_get,
            commands::settings::settings_apply,
            commands::settings::settings_set_autostart,
            commands::settings::settings_input_devices,
            commands::settings::settings_test_lm,
            commands::gpu::gpu_usage,
            commands::models::models_status,
            commands::models::models_download,
            commands::models::models_cancel,
            commands::models::models_import_breeze,
            commands::onboarding::onboarding_status,
            commands::onboarding::onboarding_restart,
            commands::onboarding::open_doc,
            commands::onboarding::mic_test_start,
            commands::onboarding::mic_test_stop,
        ])
        .on_window_event(|window, event| {
            // A background tool: closing the main window hides it; quit from the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(move |app| {
            let handle = app.handle();
            commands::history::init(handle); // before the controller reads the sinks
                                             // Before the background tasks so the overlay sees their first status events.
            if let Err(e) = overlay::start(handle, &handle.state::<AppState>().status) {
                error!(error = %e, "overlay window could not be created");
            }
            if let Err(e) = tray::start(handle) {
                error!(error = %e, "tray icon could not be created");
            }
            start_background(handle, config_error);
            show_main_unless_autostart(handle);
            #[cfg(debug_assertions)]
            overlay::start_demo_if_requested(&handle.state::<AppState>().status);
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            error!(error = %e, "tauri failed to start");
            drop(log_guard);
            return;
        }
    };

    // The event loop does not return normally, so clean up in the Exit event.
    let mut log_guard = Some(log_guard);
    app.run(move |handle, event| {
        if let tauri::RunEvent::Exit = event {
            shutdown(handle);
            info!("v2c stopped");
            // Flushes the non-blocking log writer.
            drop(log_guard.take());
        }
    });
}

/// The main window is created hidden (`tauri.conf.json`); it is shown unless v2c was started by
/// the 開機自動啟動 entry, which runs it in the background (tray only).
fn show_main_unless_autostart(app: &tauri::AppHandle) {
    let autostart = std::env::args().any(|a| a == commands::settings::AUTOSTART_ARG);
    info!(autostart, "main window");
    if !autostart {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
        }
    }
}

/// Windows Credential Manager as the keyring-core default store, for cloud API keys.
fn install_keyring() -> Arc<dyn SecretStore> {
    #[cfg(windows)]
    match windows_native_keyring_store::Store::new() {
        Ok(store) => {
            keyring_core::set_default_store(store);
            info!("keyring: Windows Credential Manager");
        }
        // Cloud services then report a key error; local use is unaffected.
        Err(e) => warn!(error = %e, "keyring store unavailable"),
    }
    Arc::new(v2c_core::secrets::KeyringSecretStore::new())
}

fn shutdown(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    state.engine.shutdown();
    let hooks = std::mem::take(&mut *state.on_exit.lock().unwrap_or_else(|p| p.into_inner()));
    for hook in hooks {
        hook();
    }
    // Uninstalls the keyboard hook.
    state
        .keep_alive
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
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

    // Local ASR selection and loading, then the VRAM guard, off the async workers.
    {
        let engine = Arc::clone(&state.engine);
        let spawned = std::thread::Builder::new()
            .name("v2c-asr".into())
            .spawn(move || {
                engine.startup_blocking();
                engine.guard_loop();
            });
        if spawned.is_err() {
            error!("could not spawn the ASR thread");
        }
    }
    tauri::async_runtime::spawn(Arc::clone(&state.engine).lm_check_loop());

    // Config reload after the tray (or, later, the settings page) saved config.toml.
    {
        let handle = app.clone();
        app.listen_any(tray::CONFIG_CHANGED_EVENT, move |_| {
            let state = handle.state::<AppState>();
            match app_state::read_config(&state.paths.config_file()) {
                Ok(cfg) => state.engine.reload(cfg),
                Err(code) => {
                    warn!(code, "config reload failed; keeping the running config");
                    let _ = state.status.send(StatusEvent::Error {
                        code: "config_invalid_not_saved",
                    });
                }
            }
        });
        // Vocabulary saved on the vocabulary page.
        let handle = app.clone();
        app.listen_any(commands::vocab::VOCAB_CHANGED_EVENT, move |_| {
            handle.state::<AppState>().engine.reload_vocab();
        });
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
    {
        let r = Arc::clone(&recorder);
        if let Ok(mut hooks) = state.on_exit.lock() {
            hooks.push(Box::new(move || r.shutdown()));
        }
    }

    start_controller(&state, recorder);
}

#[cfg(windows)]
fn start_controller(state: &AppState, recorder: Arc<Recorder>) {
    use std::time::Duration;

    use v2c_core::trigger::Event;
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
        backend: Arc::new(EngineHandle(Arc::clone(&state.engine))),
        config: state.engine.config_watch(),
    };
    tauri::async_runtime::spawn(controller::run(deps, events_rx));
    info!("controller started");
}

#[cfg(not(windows))]
fn start_controller(state: &AppState, recorder: Arc<Recorder>) {
    // Linux hotkey and injection arrive in Phase 3.
    let _ = (recorder, EngineHandle);
    tracing::warn!("hotkey and text injection are not implemented on this platform yet");
    let _ = state.status.send(StatusEvent::Error {
        code: "platform_unsupported",
    });
}
