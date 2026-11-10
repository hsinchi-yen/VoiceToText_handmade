//! OS integration for v2c. Only this crate may call platform APIs.
//!
//! Decisions (when to record, how to split text) live in `v2c-core`; this crate only
//! executes them. Windows implementations: T0.2 (hotkey), T0.3/T1.2 (inject, foreground),
//! T1.9 (DPAPI), T1.10b (fullscreen detection). Linux: Phase 3.

use thiserror::Error;
use v2c_core::{inject_plan::Action, trigger::Event};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("OS call failed: {0}")]
    Os(String),
    #[error("not supported on this platform")]
    Unsupported,
    /// Linux: an input device (`/dev/input/event*`) could not be opened (`EACCES`). v2c never
    /// elevates itself; the user has to join the `input` group.
    #[error(
        "permission denied opening {}: add your user to the `input` group \
         (sudo usermod -aG input $USER), then log out and back in",
        path.display()
    )]
    InputPermission { path: std::path::PathBuf },
    /// Linux: no global hotkey backend works on this system; the text says why.
    #[error("no global hotkey backend available: {0}")]
    HotkeyUnavailable(String),
    /// Linux: the IBus daemon could not be found or talked to; the text says which step
    /// failed (never contains typed text).
    #[error("IBus: {0}")]
    Ibus(String),
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum InjectError {
    /// The foreground window runs elevated; a normal process cannot send input to it (UIPI).
    #[error("foreground window is elevated")]
    Elevated,
    /// Only part of the input was accepted by the OS.
    #[error("only {sent} of {total} input events were accepted")]
    Partial { sent: usize, total: usize },
    /// Ctrl/Shift/Alt/Win were still held after the wait; nothing was sent (typing would turn
    /// characters into shortcuts).
    #[error("modifier keys still held")]
    ModifiersHeld,
    #[error("OS call failed: {0}")]
    Os(String),
    /// Linux: the user declined the portal's permission dialog (or unticked the keyboard /
    /// clipboard in it). Nothing was typed.
    #[error("permission denied: the input/clipboard permission dialog was cancelled")]
    PermissionDenied,
    /// Linux: this desktop offers no way to inject (e.g. no Clipboard portal before
    /// GNOME 45); the text says what is missing. Nothing was typed.
    #[error("clipboard injection not supported here: {0}")]
    Unsupported(String),
}

/// Information about the foreground application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    /// Executable file name, e.g. `LINE.exe`.
    pub exe: String,
    pub elevated: bool,
    pub fullscreen: bool,
}

/// Global keyboard hook that reports [`Event`]s for the trigger state machine.
pub trait Hotkey {
    /// Starts delivering events to `sink` (called from the hook thread; must return quickly).
    fn start(&mut self, sink: Box<dyn Fn(Event) + Send + 'static>) -> Result<(), PlatformError>;
    fn stop(&mut self);
}

/// Executes injection actions into the foreground window.
pub trait Injector {
    fn run(&self, actions: &[Action]) -> Result<(), InjectError>;
}

/// Reads the foreground application.
pub trait Foreground {
    fn current_app(&self) -> Option<AppInfo>;
}
