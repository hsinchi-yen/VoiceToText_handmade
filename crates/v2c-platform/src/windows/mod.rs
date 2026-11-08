//! Windows implementations. Each file is owned by one task card.

pub mod foreground; // T0.3
pub mod hotkey; // T0.2
pub mod inject; // T0.3, T1.2

/// `dwExtraInfo` tag on every input v2c injects, so our own keyboard hook can ignore it
/// (otherwise injected keys would cancel a recording or flip Shift state).
pub const INJECT_MARKER: usize = 0x5632_4300;
