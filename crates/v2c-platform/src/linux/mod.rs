//! Linux (GNOME Wayland) implementations — Phase 3, see `tasks/plan.md` section 9.
//!
//! - [`caps`]: read-only capability probe ([`caps::LinuxCaps`], [`caps::probe`]).
//! - [`hotkey`]: backend choice ([`hotkey::select_backend`]) and the [`crate::Hotkey`]
//!   implementation ([`hotkey::LinuxHotkey`]) over the GlobalShortcuts portal or evdev.
//! - [`notify`]: status notifications over `org.freedesktop.Notifications`
//!   ([`notify::Notifier`], [`notify::DbusNotifier`]); replaces the overlay on Wayland.

pub mod caps;
pub mod hotkey;
pub mod notify;
