//! Engine state shared by the D-Bus objects, [`super::IbusHotkey`] and
//! [`super::IbusInjector`], plus the injection plan (`Action` → engine output). Pure: no
//! D-Bus here, so everything is unit-tested with mocks.

use std::collections::BTreeMap;

use v2c_core::config::NewlineKey;
use v2c_core::inject_plan::Action;
use v2c_core::redact::Redacted;
use v2c_core::trigger::Event;

use super::keyevent::{KeyTracker, IBUS_RELEASE_MASK, IBUS_SHIFT_MASK, KEYCODE_ENTER, KEY_RETURN};

/// One thing the engine sends to the focused input context.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum EngineOp {
    /// `CommitText(IBusText)`.
    Commit(String),
    /// `ForwardKeyEvent(keyval, keycode, state)`.
    Key {
        keyval: u32,
        keycode: u32,
        state: u32,
    },
}

impl std::fmt::Debug for EngineOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineOp::Commit(t) => f.debug_tuple("Commit").field(&Redacted(t)).finish(),
            EngineOp::Key {
                keyval,
                keycode,
                state,
            } => f
                .debug_struct("Key")
                .field("keyval", &format_args!("{keyval:#x}"))
                .field("keycode", keycode)
                .field("state", &format_args!("{state:#x}"))
                .finish(),
        }
    }
}

/// `Text` and `Paste` both become `CommitText` (IBus commits any Unicode directly, no
/// clipboard needed); `NewLine` becomes a forwarded Return press + release, with the Shift
/// modifier for `ShiftEnter`. Empty text is skipped.
pub(crate) fn ops_for(actions: &[Action]) -> Vec<EngineOp> {
    let mut out = Vec::new();
    for a in actions {
        match a {
            Action::Text(t) | Action::Paste(t) => {
                if !t.is_empty() {
                    out.push(EngineOp::Commit(t.clone()));
                }
            }
            Action::NewLine(k) => {
                let mods = match k {
                    NewlineKey::Enter => 0,
                    NewlineKey::ShiftEnter => IBUS_SHIFT_MASK,
                };
                for state in [mods, mods | IBUS_RELEASE_MASK] {
                    out.push(EngineOp::Key {
                        keyval: KEY_RETURN,
                        keycode: KEYCODE_ENTER,
                        state,
                    });
                }
            }
        }
    }
    out
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Flags {
    enabled: bool,
    focused: bool,
    /// A key event arrived since the last FocusIn. See [`EngineState::target`].
    keyed: bool,
}

/// Per-engine-object flags and the key tracker. IBus usually keeps one engine object and
/// moves it between input contexts (global engine mode), but per-context engines are
/// handled too: the target for output is the most recently focused, enabled engine.
#[derive(Default)]
pub(crate) struct EngineState {
    engines: BTreeMap<String, Flags>,
    /// Engine paths in focus order, most recent last.
    focus_order: Vec<String>,
    tracker: KeyTracker,
}

/// What a key event resulted in.
pub(crate) struct KeyOutcome {
    pub handled: bool,
    pub events: Vec<Event>,
}

impl EngineState {
    pub(crate) fn created(&mut self, path: &str) {
        self.engines.insert(path.to_string(), Flags::default());
    }

    pub(crate) fn destroyed(&mut self, path: &str, at_ms: u64) -> Vec<Event> {
        let was_active = self.is_active(path);
        self.engines.remove(path);
        self.focus_order.retain(|p| p != path);
        if was_active {
            self.tracker.release_all(at_ms)
        } else {
            Vec::new()
        }
    }

    fn set(&mut self, path: &str, f: impl FnOnce(&mut Flags), at_ms: u64) -> Vec<Event> {
        let was_active = self.is_active(path);
        if let Some(flags) = self.engines.get_mut(path) {
            f(flags);
            if !(flags.enabled && flags.focused) {
                flags.keyed = false;
            }
        }
        let now_active = self.is_active(path);
        if now_active {
            self.focus_order.retain(|p| p != path);
            self.focus_order.push(path.to_string());
        }
        if was_active && !now_active {
            self.tracker.release_all(at_ms)
        } else {
            Vec::new()
        }
    }

    pub(crate) fn focus_in(&mut self, path: &str, at_ms: u64) -> Vec<Event> {
        self.set(path, |f| f.focused = true, at_ms)
    }

    pub(crate) fn focus_out(&mut self, path: &str, at_ms: u64) -> Vec<Event> {
        self.set(path, |f| f.focused = false, at_ms)
    }

    pub(crate) fn enable(&mut self, path: &str, at_ms: u64) -> Vec<Event> {
        self.set(path, |f| f.enabled = true, at_ms)
    }

    pub(crate) fn disable(&mut self, path: &str, at_ms: u64) -> Vec<Event> {
        self.set(path, |f| f.enabled = false, at_ms)
    }

    pub(crate) fn is_active(&self, path: &str) -> bool {
        self.engines
            .get(path)
            .is_some_and(|f| f.enabled && f.focused)
    }

    /// The engine output goes to, if any: the most recently focused engine that is enabled,
    /// focused, and has received a key event since it was focused.
    ///
    /// The key requirement exists because in IBus' default global-engine mode, when the
    /// text field loses focus the daemon moves the engine to its internal "fake" input
    /// context and sends `FocusIn` + `Enable` again (ibus 1.5.26 `bus/ibusimpl.c`
    /// `bus_ibus_impl_set_focused_context`); commits there are silently dropped. The fake
    /// context never forwards key events, so "a key arrived since FocusIn" tells a real
    /// text field apart. Every dictation starts with the trigger key in the target field,
    /// so a focus change during recording makes the injection fail instead of typing
    /// into nowhere (or into a different field).
    pub(crate) fn target(&self) -> Option<&str> {
        self.focus_order
            .iter()
            .rev()
            .find(|p| self.is_active(p) && self.engines.get(*p).is_some_and(|f| f.keyed))
            .map(String::as_str)
    }

    /// `ProcessKeyEvent` on engine `path`. Keys are only interpreted while that engine is
    /// enabled and focused and somebody listens; otherwise everything passes through.
    pub(crate) fn key(
        &mut self,
        path: &str,
        listening: bool,
        keyval: u32,
        keycode: u32,
        state: u32,
        at_ms: u64,
    ) -> KeyOutcome {
        if !self.is_active(path) {
            return KeyOutcome {
                handled: false,
                events: Vec::new(),
            };
        }
        if let Some(f) = self.engines.get_mut(path) {
            f.keyed = true;
        }
        if !listening {
            return KeyOutcome {
                handled: false,
                events: Vec::new(),
            };
        }
        let d = self.tracker.process(keyval, keycode, state, at_ms);
        KeyOutcome {
            handled: d.handled,
            events: d.event.into_iter().collect(),
        }
    }

    /// Hotkey stopped: forget held keys so a later start does not see stale state.
    pub(crate) fn reset_keys(&mut self) {
        self.tracker = KeyTracker::default();
    }
}

#[cfg(test)]
mod tests {
    use super::super::keyevent::{IBUS_RELEASE_MASK as REL, KEY_CONTROL_R};
    use super::*;
    use v2c_core::trigger::Key;

    const E1: &str = "/org/freedesktop/IBus/Engine/1";
    const E2: &str = "/org/freedesktop/IBus/Engine/2";

    #[test]
    fn text_and_paste_commit_newlines_forward_return() {
        let ops = ops_for(&[
            Action::Text("第一行".into()),
            Action::NewLine(NewlineKey::Enter),
            Action::Paste("second".into()),
            Action::NewLine(NewlineKey::ShiftEnter),
            Action::Text(String::new()),
        ]);
        let key = |state| EngineOp::Key {
            keyval: 0xff0d,
            keycode: 28,
            state,
        };
        assert_eq!(
            ops,
            vec![
                EngineOp::Commit("第一行".into()),
                key(0),
                key(REL),
                EngineOp::Commit("second".into()),
                key(1),
                key(1 | REL),
            ]
        );
    }

    #[test]
    fn op_debug_hides_text() {
        let s = format!("{:?}", ops_for(&[Action::Text("秘密內容".into())]));
        assert!(!s.contains("秘密"), "{s}");
    }

    fn type_a(s: &mut EngineState, path: &str) {
        s.key(path, false, 0x61, 30, 0, 0);
    }

    #[test]
    fn target_needs_enabled_focused_and_a_key() {
        let mut s = EngineState::default();
        s.created(E1);
        assert_eq!(s.target(), None);
        s.focus_in(E1, 0);
        assert_eq!(s.target(), None);
        s.enable(E1, 0);
        assert_eq!(s.target(), None, "no key since FocusIn yet");
        type_a(&mut s, E1);
        assert_eq!(s.target(), Some(E1));
        s.focus_out(E1, 0);
        assert_eq!(s.target(), None);
        s.focus_in(E1, 0);
        type_a(&mut s, E1);
        s.disable(E1, 0);
        assert_eq!(s.target(), None);
    }

    /// What ibus-daemon does when the text field loses focus in global-engine mode: the
    /// engine is focused out, then focused in and enabled again on the fake context, which
    /// never sends keys. Output must not go there.
    #[test]
    fn engine_moved_to_the_fake_context_is_not_a_target() {
        let mut s = EngineState::default();
        s.created(E1);
        s.focus_in(E1, 0);
        s.enable(E1, 0);
        type_a(&mut s, E1);
        assert_eq!(s.target(), Some(E1));
        s.focus_out(E1, 0);
        s.focus_in(E1, 0);
        s.enable(E1, 0);
        assert!(s.is_active(E1));
        assert_eq!(s.target(), None);
    }

    #[test]
    fn most_recently_focused_engine_is_the_target() {
        let mut s = EngineState::default();
        for e in [E1, E2] {
            s.created(e);
            s.enable(e, 0);
        }
        s.focus_in(E1, 0);
        type_a(&mut s, E1);
        s.focus_in(E2, 0);
        type_a(&mut s, E2);
        assert_eq!(s.target(), Some(E2));
        s.focus_out(E2, 0);
        assert_eq!(s.target(), Some(E1));
        s.destroyed(E1, 0);
        assert_eq!(s.target(), None);
    }

    #[test]
    fn keys_only_count_while_active_and_listening() {
        let mut s = EngineState::default();
        s.created(E1);
        let k = s.key(E1, true, KEY_CONTROL_R, 97, 0, 0);
        assert!(!k.handled && k.events.is_empty());
        s.focus_in(E1, 0);
        s.enable(E1, 0);
        let k = s.key(E1, false, KEY_CONTROL_R, 97, 0, 0);
        assert!(!k.handled && k.events.is_empty());
        let k = s.key(E1, true, KEY_CONTROL_R, 97, 0, 5);
        assert!(k.handled);
        assert_eq!(
            k.events,
            vec![Event::KeyDown {
                key: Key::Trigger,
                at_ms: 5
            }]
        );
        // Another engine object is not active: passes through.
        assert!(!s.key(E2, true, 0x61, 30, 0, 6).handled);
    }

    #[test]
    fn losing_focus_releases_a_held_trigger() {
        let mut s = EngineState::default();
        s.created(E1);
        s.focus_in(E1, 0);
        s.enable(E1, 0);
        s.key(E1, true, KEY_CONTROL_R, 97, 0, 5);
        assert_eq!(
            s.focus_out(E1, 9),
            vec![Event::KeyUp {
                key: Key::Trigger,
                at_ms: 9
            }]
        );
        assert!(s.disable(E1, 10).is_empty());
    }
}
