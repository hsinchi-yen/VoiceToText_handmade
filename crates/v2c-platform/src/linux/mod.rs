//! Linux (GNOME Wayland) implementations — Phase 3, see `tasks/plan.md` section 9.
//!
//! - [`caps`]: read-only capability probe ([`caps::LinuxCaps`], [`caps::probe`]).
//! - [`hotkey`]: backend choice ([`hotkey::select_backend`]) and the [`crate::Hotkey`]
//!   implementation ([`hotkey::LinuxHotkey`]) over the GlobalShortcuts portal or evdev.
//! - [`inject`]: clipboard + Ctrl+V injection ([`inject::LinuxInjector`]) and its backend
//!   choice ([`inject::select_injector`]); [`clipboard`] and [`keys`] hold the backend
//!   traits and the RemoteDesktop / Clipboard portal implementations.
//! - [`notify`]: status notifications over `org.freedesktop.Notifications`
//!   ([`notify::Notifier`], [`notify::DbusNotifier`]); replaces the overlay on Wayland.

pub mod caps;
pub mod clipboard;
pub mod hotkey;
pub mod inject;
pub mod keys;
pub mod notify;
