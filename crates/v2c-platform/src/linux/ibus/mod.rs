//! IBus input-method engine (T3.5, `tasks/plan.md` section 9.2a): the primary Linux route
//! for both the hotkey and text output.
//!
//! While the user's input source is "v2c", IBus routes every key of the focused text field
//! through the engine's `ProcessKeyEvent`; Right Ctrl / Shift / Esc become trigger events
//! ([`IbusHotkey`]) and the result is committed with `CommitText` ([`IbusInjector`]) — any
//! Unicode, no clipboard, no portal dialog, no `input` group.
//!
//! - [`address`]: finding the daemon's private bus ([`IbusAddress`]).
//! - `keyevent`: key event → trigger event and the "handled" reply (pure).
//! - [`serial`]: `IBusText` / `IBusComponent` / `IBusEngineDesc` variants (pure).
//! - `state`: engine focus state and `Action` → engine output (pure).
//! - [`engine`]: the zbus Factory / Engine objects and [`IbusEngine`].
//!
//! Interfaces and wire formats were checked against the ibus 1.5.26 sources (see the
//! module docs of each file). Typed text is never logged; errors carry no text.

pub mod address;
pub mod engine;
pub(crate) mod keyevent;
pub mod serial;
pub(crate) mod state;

#[cfg(test)]
mod real_tests;

pub use address::IbusAddress;
pub use engine::{
    v2c_component, IbusEngine, IbusHotkey, IbusInjector, Registration, NO_FOCUSED_INPUT,
};
pub use serial::{ComponentDesc, EngineDesc};
