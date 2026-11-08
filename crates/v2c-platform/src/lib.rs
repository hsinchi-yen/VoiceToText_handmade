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
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InjectError {
    /// The foreground window runs elevated; a normal process cannot send input to it (UIPI).
    #[error("foreground window is elevated")]
    Elevated,
    /// Only part of the input was accepted by the OS.
    #[error("only {sent} of {total} input events were accepted")]
    Partial { sent: usize, total: usize },
    #[error("OS call failed: {0}")]
    Os(String),
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
