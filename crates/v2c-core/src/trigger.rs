//! Hotkey state machine (pure logic, no OS calls, no clock reads).
//!
//! The platform hook (`v2c-platform`) turns raw key events into [`Event`]s with timestamps;
//! this module decides when to start, stop, or cancel recording. Behaviour is specified by
//! the state table in `tasks/plan.md` section 3. Logic is implemented in T0.2.

use crate::Mode;

/// Keys the state machine cares about. The platform layer maps physical keys to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// The configured dictation key (default: Right Ctrl).
    Trigger,
    /// Either Shift key; held together with `Trigger` selects translate mode.
    Shift,
    Escape,
    /// Any other key. Pressing one while `Trigger` is held means the user is typing a
    /// shortcut such as Ctrl+C, so the recording is cancelled.
    Other,
}

/// Input to the state machine. `at_ms` is a monotonic timestamp in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    KeyDown {
        key: Key,
        at_ms: u64,
    },
    KeyUp {
        key: Key,
        at_ms: u64,
    },
    /// Periodic tick (e.g. every 50 ms) so timeouts fire without key activity.
    Tick {
        at_ms: u64,
    },
}

/// Output of the state machine, executed by the desktop controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    StartRecording(Mode),
    StopAndProcess,
    Cancel,
    /// Hands-free recording hit the maximum length; process it and tell the user.
    StopAtLimit,
}

/// Thresholds, taken from `config::HotkeyConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerConfig {
    pub hold_threshold_ms: u64,
    pub double_tap_window_ms: u64,
    pub hands_free_max_ms: u64,
}

impl From<&crate::config::HotkeyConfig> for TriggerConfig {
    fn from(c: &crate::config::HotkeyConfig) -> Self {
        Self {
            hold_threshold_ms: c.hold_threshold_ms,
            double_tap_window_ms: c.double_tap_window_ms,
            hands_free_max_ms: c.hands_free_max_secs * 1000,
        }
    }
}

/// The hotkey state machine. Feed it every [`Event`] in order.
#[derive(Debug)]
pub struct Trigger {
    cfg: TriggerConfig,
}

impl Trigger {
    pub fn new(cfg: TriggerConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> TriggerConfig {
        self.cfg
    }

    /// Advances the state machine and returns the commands to execute (usually zero or one).
    pub fn on_event(&mut self, event: Event) -> Vec<Command> {
        let _ = event;
        todo!("T0.2: implement the state table in tasks/plan.md section 3")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotkeyConfig;

    #[test]
    fn config_conversion_uses_milliseconds() {
        let c = TriggerConfig::from(&HotkeyConfig::default());
        assert_eq!(c.hands_free_max_ms, 300_000);
        assert_eq!(Trigger::new(c).config().hold_threshold_ms, 350);
    }
}
