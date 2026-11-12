//! Linux wiring of the desktop app (T3.6, `tasks/plan.md` section 9 / 9.2a).
//!
//! - Hotkey + injection: the v2c IBus engine first ([`ibus_link`]); when IBus is off or
//!   unavailable, the T3.1 portal / evdev hotkey and the T3.2 portal injector.
//! - Text that could not be typed is kept ([`kept`]) and the user is told where.
//! - Notifications ([`notes`]) replace the overlay on Wayland.
//! - `v2c --ibus` launches and the single-instance hand-over ([`launch`]).
//!
//! Everything that talks D-Bus (portal probe, IBus, notifications, Secret Service) runs off
//! the UI thread.

pub mod ibus_link;
pub mod injectors;
pub mod kept;
pub mod launch;
pub mod notes;

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tauri::Manager;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use v2c_core::trigger::Event;
use v2c_platform::linux::notify::{NotifyErrorKind, NotifyStatus};
use v2c_platform::Hotkey;

use crate::app_state::AppState;
use crate::controller::{self, StatusEvent};
use crate::engine::EngineHandle;
use crate::recorder::Recorder;
use crate::sinks::SinkRegistry;
use ibus_link::{IbusLink, LinkEvent, RealFactory, RECONNECT_DELAYS};
use injectors::{SharedInjector, SwitchInjector, UnsupportedInjector};
use kept::{KeepTextSink, UnsentState, UnsentStore};
use launch::IbusMode;
use notes::{Note, Notes, StatusNotes};

/// Linux runtime handles, managed by Tauri.
pub struct LinuxRuntime {
    /// Set by the init thread when IBus is not switched off.
    link: OnceLock<Arc<IbusLink>>,
}

/// No foreground-app detection on Wayland: app rules fall back to the defaults.
struct NoForeground;

impl v2c_platform::Foreground for NoForeground {
    fn current_app(&self) -> Option<v2c_platform::AppInfo> {
        None
    }
}

/// Called from `setup` (UI thread): starts the controller right away with an injector that
/// is filled in by a background thread once IBus / the portals have been probed.
pub fn start_controller(app: &tauri::AppHandle, state: &AppState, recorder: Arc<Recorder>) {
    const TICK: Duration = Duration::from_millis(50);

    let notes = Notes::start();
    let unsent = Arc::new(UnsentStore::default());
    app.manage(UnsentState(Arc::clone(&unsent)));
    app.manage(LinuxRuntime {
        link: OnceLock::new(),
    });

    if !crate::overlay_enabled() {
        bridge_status_to_notes(state.status.subscribe(), notes.clone());
    }

    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    {
        let tx = events_tx.clone();
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(TICK);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let tick = Event::Tick {
                    at_ms: v2c_platform::linux::hotkey::monotonic_ms(),
                };
                if tx.send(tick).is_err() {
                    break;
                }
            }
        });
    }

    // Until the init thread is done, nothing can be typed (the text is kept).
    let injector = Arc::new(SwitchInjector::new(Arc::new(UnsupportedInjector(
        v2c_platform::InjectError::Unsupported("starting".into()),
    ))));

    let mut sinks = SinkRegistry::default();
    let kept_notes = notes.clone();
    sinks.register(Arc::new(KeepTextSink::new(
        crate::history_sink::installed_sink(),
        unsent,
        move |k| {
            kept::warn_if_memory(k);
            kept_notes.show(match k {
                kept::Kept::History => NotifyStatus::TextKept,
                kept::Kept::Memory => NotifyStatus::TextKeptUntilExit,
            });
        },
    )));

    let deps = controller::Deps {
        source: recorder,
        injector: Arc::clone(&injector) as SharedInjector,
        foreground: Arc::new(NoForeground),
        sinks,
        status: state.status.clone(),
        backend: Arc::new(EngineHandle(Arc::clone(&state.engine))),
        config: state.engine.config_watch(),
    };
    tauri::async_runtime::spawn(controller::run(deps, events_rx));
    info!("controller started");

    let handle = app.clone();
    let spawned = std::thread::Builder::new()
        .name("v2c-linux-init".into())
        .spawn(move || init_backends(&handle, injector, events_tx, notes));
    if spawned.is_err() {
        error!("could not start the Linux backend thread");
    }
}

/// Blocking: probes the desktop, connects IBus or starts the fallback hotkey.
fn init_backends(
    app: &tauri::AppHandle,
    injector: Arc<SwitchInjector>,
    events_tx: tokio::sync::mpsc::UnboundedSender<Event>,
    notes: Notes,
) {
    use v2c_platform::linux::hotkey::{select_backend, LinuxHotkey};
    use v2c_platform::linux::inject::{portal_injector, InjectConfig};

    let args: Vec<String> = std::env::args().collect();
    let env = std::env::var(launch::IBUS_ENV).ok();
    let (mode, unknown) = launch::ibus_mode(&args, env.as_deref());
    if unknown {
        warn!(env = launch::IBUS_ENV, "unknown value ignored");
    }
    let installed = launch::component_installed();
    let registration = launch::registration(mode, installed);
    info!(
        ?mode,
        component_installed = installed,
        ?registration,
        "IBus"
    );

    let caps = v2c_platform::linux::caps::probe();
    let fallback: Option<SharedInjector> = match portal_injector(&caps, InjectConfig::default()) {
        Ok(i) => {
            info!("fallback injector: Clipboard + RemoteDesktop portals");
            let i: SharedInjector = Arc::new(i);
            injector.set(Arc::clone(&i));
            Some(i)
        }
        Err(e) => {
            info!(error = %e, "no portal injector here");
            injector.set(Arc::new(UnsupportedInjector(e)));
            None
        }
    };

    let sink_tx = events_tx.clone();
    let sink: ibus_link::EventSink = Arc::new(move |ev| {
        let _ = sink_tx.send(ev);
    });

    let link = (mode != IbusMode::Off).then(|| {
        let link_notes = notes.clone();
        IbusLink::new(
            Box::new(RealFactory {
                // A later `v2c --ibus` hand-over claims the installed component's name.
                registration: registration
                    .unwrap_or(v2c_platform::linux::ibus::Registration::Launched),
            }),
            Arc::clone(&injector),
            fallback.clone(),
            Arc::clone(&sink),
            move |ev| match ev {
                LinkEvent::Lost => {
                    link_notes.show(NotifyStatus::Error(NotifyErrorKind::ImeDisconnected))
                }
                LinkEvent::Reconnected | LinkEvent::Connected => link_notes.send(Note::Dismiss),
            },
            RECONNECT_DELAYS.to_vec(),
        )
    });

    let state = app.state::<AppState>();
    let connected = match (&link, registration) {
        (Some(link), Some(_)) => match link.connect() {
            Ok(()) => {
                info!("hotkey + injection: v2c IBus engine");
                true
            }
            Err(e) => {
                // ibus-daemon may not be up yet (login): keep trying in the background; the
                // fallback hotkey stops once the engine connects.
                warn!(error = %e, "IBus engine unavailable; using the fallback meanwhile");
                notes.show(NotifyStatus::Error(NotifyErrorKind::ImeUnavailable));
                link.start_reconnect();
                false
            }
        },
        _ => false,
    };

    if !connected {
        let mut hook = LinuxHotkey::new(select_backend(&caps));
        let hook_tx = events_tx;
        match hook.start(Box::new(move |event| {
            let _ = hook_tx.send(event);
        })) {
            Ok(()) => info!(backend = ?hook.backend(), "fallback hotkey started"),
            Err(e) => {
                warn!(error = %e, "hotkey failed");
                let _ = state.status.send(StatusEvent::Error {
                    code: "hotkey_failed",
                });
            }
        }
        match &link {
            Some(link) => link.set_fallback_hotkey(Box::new(hook)),
            None => {
                if let Ok(mut keep) = state.keep_alive.lock() {
                    keep.push(Box::new(hook));
                }
            }
        }
    }

    if let Some(link) = link {
        let l = Arc::clone(&link);
        if let Ok(mut hooks) = state.on_exit.lock() {
            hooks.push(Box::new(move || l.shutdown()));
        }
        let _ = app.state::<LinuxRuntime>().link.set(link);
    }
}

/// Status events → notifications (Wayland, where the overlay is off).
fn bridge_status_to_notes(mut rx: broadcast::Receiver<StatusEvent>, notes: Notes) {
    tauri::async_runtime::spawn(async move {
        let mut mapper = StatusNotes::default();
        loop {
            // While the hands-free hint is open, wake up after a quiet spell to close it
            // once its recording ended (the controller sends no event when it stops).
            let received = if mapper.wants_idle() {
                match tokio::time::timeout(notes::RECORDING_GAP, rx.recv()).await {
                    Ok(r) => r,
                    Err(_) => {
                        if let Some(note) = mapper.on_idle(Instant::now()) {
                            notes.send(note);
                        }
                        continue;
                    }
                }
            } else {
                rx.recv().await
            };
            match received {
                Ok(ev) => {
                    if let Some(note) = mapper.on_status(&ev, Instant::now()) {
                        notes.send(note);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Single-instance hand-over. Returns `true` when the second launch was ibus-daemon's
/// `v2c --ibus`: then the running app (re)connects its engine instead of showing a window.
pub fn on_second_instance(app: &tauri::AppHandle, args: &[String]) -> bool {
    if !launch::is_ibus_launch(args) {
        return false;
    }
    info!("second launch from ibus-daemon: connecting the engine");
    if let Some(link) = app
        .try_state::<LinuxRuntime>()
        .and_then(|rt| rt.link.get().cloned())
    {
        // Blocking D-Bus: off the UI thread.
        let _ = std::thread::Builder::new()
            .name("v2c-ibus-poke".into())
            .spawn(move || link.poke());
    }
    true
}
