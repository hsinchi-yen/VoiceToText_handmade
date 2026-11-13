//! v2c desktop app: hold Right Ctrl, speak, and the text is typed at the cursor.
//!
//! Startup: logging -> config -> keyring store -> engine -> status subscriptions -> Tauri. The
//! local ASR selection + VRAM guard thread starts from a plugin right after the single-instance
//! check (so a second launch never loads a model) and overlaps window creation. In `setup`:
//! tray icon, status forwarder, LM Studio check, recorder, hotkey hook, the controller task, and
//! the config-reload listener; the overlay window is created right after the main window is shown. OS-specific
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
#[cfg(target_os = "linux")]
mod linux_app;
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
use crate::engine::Engine;
#[cfg(not(target_os = "linux"))]
use crate::engine::EngineHandle;
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
    // Subscribed before the ASR thread starts (it starts from a plugin, below): events it sends
    // while WebView2 and the windows are still being created wait in these receivers until
    // `setup` hands them to the forwarder, overlay and notification bridge.
    let early = EarlyStatus::subscribe(&status);
    let asr_engine = Arc::clone(&engine);
    #[cfg(target_os = "linux")]
    install_linux_keyring(Arc::clone(&engine));
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
        // Must be the first plugin: a second launch (e.g. from the Start menu while the autostart
        // instance runs hidden) only shows the running instance's window and exits, so there is
        // never a second keyboard hook typing everything twice.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Linux: ibus-daemon's `v2c --ibus` reconnects the engine; no window.
            #[cfg(target_os = "linux")]
            if linux_app::on_second_instance(app, &_args) {
                return;
            }
            info!("second launch: showing the running instance");
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        // Model loading is I/O bound and needs nothing from Tauri. Plugins are set up in order
        // after the single-instance check (which exits a second launch), and the windows are
        // created later in `run()`, so loading overlaps WebView2 and window creation.
        .plugin(
            tauri::plugin::Builder::<tauri::Wry>::new("v2c-asr")
                .setup(move |_app, _api| {
                    start_asr_thread(&asr_engine);
                    Ok(())
                })
                .build(),
        )
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
            commands::presets::presets_list,
            commands::presets::presets_preview,
            commands::presets::presets_apply,
            commands::config_io::config_pick_open,
            commands::config_io::config_pick_save,
            commands::config_io::config_export,
            commands::config_io::config_import_preview,
            commands::config_io::config_import_apply,
            commands::vocab::vocab_get,
            commands::vocab::vocab_set,
            commands::settings::settings_get,
            commands::settings::settings_apply,
            commands::settings::settings_set_autostart,
            commands::settings::settings_input_devices,
            commands::settings::settings_preview,
            commands::settings::settings_runtime_status,
            commands::settings::settings_test_local_asr,
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
            let EarlyStatus {
                forwarder,
                overlay: overlay_rx,
                notes,
            } = early;
            if overlay_rx.is_none() {
                info!("overlay disabled on Wayland (notifications instead)");
            }
            if let Err(e) = tray::start(handle) {
                error!(error = %e, "tray icon could not be created");
            }
            start_background(handle, config_error, forwarder, notes);
            show_main_unless_autostart(handle);
            // The overlay webview (~115 ms) is created after the main window is shown, off the
            // setup path.
            if let Some(rx) = overlay_rx {
                let handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = overlay::start(&handle, rx) {
                        error!(error = %e, "overlay window could not be created");
                    }
                });
            }
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
/// the 開機自動啟動 entry, which runs it in the background (tray only). It is shown anyway
/// after the onboarding restart and while no local speech model is installed (onboarding).
fn show_main_unless_autostart(app: &tauri::AppHandle) {
    let autostart = std::env::args().any(|a| a == commands::settings::AUTOSTART_ARG);
    // Linux: started by ibus-daemon (`v2c --ibus`): never pop up a window.
    #[cfg(target_os = "linux")]
    let ibus_launch = linux_app::launch::is_ibus_launch(&std::env::args().collect::<Vec<_>>());
    #[cfg(not(target_os = "linux"))]
    let ibus_launch = false;
    let paths = &app.state::<AppState>().paths;
    let resumed = commands::onboarding::take_show_flag(paths);
    let has_model = commands::models::any_asr_model(paths);
    let show =
        !ibus_launch && commands::onboarding::show_main_at_start(autostart, resumed, has_model);
    info!(
        autostart,
        ibus_launch, resumed, has_model, show, "main window"
    );
    if show {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
        }
    }
}

/// The floating overlay: always on Windows; off on a Linux Wayland session, where a window
/// cannot be placed near the cursor or kept from taking focus (T3.6).
fn overlay_enabled() -> bool {
    #[cfg(target_os = "linux")]
    {
        use v2c_platform::linux::caps::SessionType;
        let session = std::env::var("XDG_SESSION_TYPE").ok();
        SessionType::from_env_value(session.as_deref()) != SessionType::Wayland
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Windows Credential Manager as the keyring-core default store, for cloud API keys (Linux:
/// [`install_linux_keyring`], after the engine exists).
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

/// T3.6: freedesktop Secret Service (GNOME Keyring / KWallet) over zbus. Connecting can wait
/// for D-Bus activation of the keyring daemon (up to ~25 s), so it runs in the background.
/// `Engine::new` has already built the providers by then, without a store, so every cloud
/// service failed with "no credential store": rebuild them once the store is in.
#[cfg(target_os = "linux")]
fn install_linux_keyring(engine: Arc<Engine>) {
    let spawned = std::thread::Builder::new()
        .name("v2c-keyring".into())
        .spawn(
            move || match v2c_platform::linux::secret::install_default_store() {
                Ok(()) => {
                    info!("keyring: Secret Service");
                    engine.rebuild_providers();
                }
                Err(e) => warn!(error = %e, "keyring store unavailable"),
            },
        );
    if spawned.is_err() {
        warn!("could not start the keyring thread");
    }
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

/// The status receivers every consumer needs, subscribed before the ASR thread starts so none of
/// them misses the events it sends during startup (`model_missing`, GPU fallback notices, ...).
struct EarlyStatus {
    /// Webview forwarder + `last_status`.
    forwarder: broadcast::Receiver<StatusEvent>,
    /// Overlay window (Windows, Linux X11); `None` where the overlay is off.
    overlay: Option<broadcast::Receiver<StatusEvent>>,
    /// Linux Wayland notification bridge, used instead of the overlay.
    notes: Option<broadcast::Receiver<StatusEvent>>,
}

impl EarlyStatus {
    fn subscribe(status: &broadcast::Sender<StatusEvent>) -> Self {
        let overlay_on = overlay_enabled();
        Self {
            forwarder: status.subscribe(),
            overlay: overlay_on.then(|| status.subscribe()),
            notes: (!overlay_on).then(|| status.subscribe()),
        }
    }
}

/// Local ASR selection and loading, then the VRAM guard, off the async workers. Started from
/// the `v2c-asr` plugin (after the single-instance check).
fn start_asr_thread(engine: &Arc<Engine>) {
    let engine = Arc::clone(engine);
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

fn start_background(
    app: &tauri::AppHandle,
    config_error: Option<&'static str>,
    forwarder: broadcast::Receiver<StatusEvent>,
    notes: Option<broadcast::Receiver<StatusEvent>>,
) {
    #[cfg(not(target_os = "linux"))]
    let _ = notes; // only the Linux Wayland bridge uses it
    let state = app.state::<AppState>();

    // Forward status events to the webview and remember the latest one.
    let mut rx = forwarder;
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

    #[cfg(target_os = "linux")]
    linux_app::start_controller(app, &state, recorder, notes);
    #[cfg(not(target_os = "linux"))]
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

#[cfg(not(any(windows, target_os = "linux")))]
fn start_controller(state: &AppState, recorder: Arc<Recorder>) {
    // No hotkey / injection backend on this platform.
    let _ = (recorder, EngineHandle);
    tracing::warn!("hotkey and text injection are not implemented on this platform yet");
    let _ = state.status.send(StatusEvent::Error {
        code: "platform_unsupported",
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_consumer_gets_events_sent_before_setup() {
        let (tx, _) = broadcast::channel::<StatusEvent>(8);
        let early = EarlyStatus::subscribe(&tx);
        // The ASR thread reports while WebView2 and the windows are still being created.
        tx.send(StatusEvent::Error {
            code: "model_missing",
        })
        .unwrap();
        let mut got = vec![early.forwarder];
        got.extend(early.overlay);
        got.extend(early.notes);
        assert!(got.len() >= 2, "forwarder plus overlay or notes");
        for mut rx in got {
            assert_eq!(
                rx.try_recv(),
                Ok(StatusEvent::Error {
                    code: "model_missing"
                })
            );
        }
    }
}
