//! Linux (GNOME Wayland) implementations — Phase 3, see `tasks/plan.md` section 9.
//!
//! - [`caps`]: read-only capability probe ([`caps::LinuxCaps`], [`caps::probe`]).
//! - [`hotkey`]: backend choice ([`hotkey::select_backend`]) and the [`crate::Hotkey`]
//!   implementation ([`hotkey::LinuxHotkey`]) over the GlobalShortcuts portal or evdev.
//! - [`inject`]: clipboard + Ctrl+V injection ([`inject::LinuxInjector`]) and its backend
//!   choice ([`inject::select_injector`]); [`clipboard`] and [`keys`] hold the backend
//!   traits and the RemoteDesktop / Clipboard portal implementations.
//! - [`ibus`]: IBus input-method engine ([`ibus::IbusEngine`], [`ibus::IbusHotkey`],
//!   [`ibus::IbusInjector`]) — the primary route (section 9.2a); the portal / evdev hotkey
//!   and clipboard injection above remain the fallback when the engine is not active.
//! - [`notify`]: status notifications over `org.freedesktop.Notifications`
//!   ([`notify::Notifier`], [`notify::DbusNotifier`]); replaces the overlay on Wayland.
//! - [`secret`]: Secret Service as the keyring-core default store
//!   ([`secret::install_default_store`]) and a read-only status probe (T3.6).
//! - [`protect`]: history encryption, AES-256-GCM with a key kept in the Secret Service
//!   ([`protect::SecretServiceProtector`]); counterpart of `windows::dpapi` (T3.6).

pub mod caps;
pub mod clipboard;
pub mod hotkey;
pub mod ibus;
pub mod inject;
pub mod keys;
pub mod notify;
pub mod protect; // T3.6
pub mod secret; // T3.6
