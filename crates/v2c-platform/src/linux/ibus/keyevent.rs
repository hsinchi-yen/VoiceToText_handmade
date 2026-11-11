//! `ProcessKeyEvent(keyval, keycode, state)` → trigger [`Event`] and the "handled" reply
//! (pure). Constants are from ibus 1.5.26 `src/ibustypes.h` and `src/ibuskeysyms.h`.
//!
//! - Right Ctrl (`Control_R`) is the trigger. Its presses and releases are swallowed
//!   (reply `true`) so the app does not see a bare Ctrl; every other key is passed through
//!   (`false`), so normal typing and Ctrl+C keep working.
//! - IBus sends no separate auto-repeat flag: a held key just repeats its press. Repeats
//!   are dropped (the trigger repeat is still swallowed).
//! - A release of a key whose press we never saw (pressed before the engine was enabled)
//!   yields no event and is passed through, so the app gets a matching key-up.

use std::collections::HashMap;

use v2c_core::trigger::{Event, Key};

pub(crate) const IBUS_SHIFT_MASK: u32 = 1 << 0;
#[cfg(test)]
pub(crate) const IBUS_CONTROL_MASK: u32 = 1 << 2;
/// Set on events IBus forwarded itself; they must not be processed again.
pub(crate) const IBUS_FORWARD_MASK: u32 = 1 << 25;
pub(crate) const IBUS_RELEASE_MASK: u32 = 1 << 30;

pub(crate) const KEY_RETURN: u32 = 0xff0d;
pub(crate) const KEY_ESCAPE: u32 = 0xff1b;
pub(crate) const KEY_SHIFT_L: u32 = 0xffe1;
pub(crate) const KEY_SHIFT_R: u32 = 0xffe2;
pub(crate) const KEY_CONTROL_R: u32 = 0xffe4;
/// IBus keycodes are evdev codes (`KEY_ENTER`).
pub(crate) const KEYCODE_ENTER: u32 = 28;

/// Maps a keyval to the class the trigger state machine cares about.
pub(crate) fn classify(keyval: u32) -> Key {
    match keyval {
        KEY_CONTROL_R => Key::Trigger,
        KEY_SHIFT_L | KEY_SHIFT_R => Key::Shift,
        KEY_ESCAPE => Key::Escape,
        _ => Key::Other,
    }
}

/// Result of one `ProcessKeyEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyDecision {
    pub event: Option<Event>,
    /// The D-Bus reply: `true` = the engine consumed the key, the app never sees it.
    pub handled: bool,
}

const PASS: KeyDecision = KeyDecision {
    event: None,
    handled: false,
};

/// Tracks held keys across `ProcessKeyEvent` calls. Keys are identified by keycode (the
/// keyval of a held letter can change with Shift), or by keyval when the keycode is 0.
#[derive(Default)]
pub(crate) struct KeyTracker {
    down: HashMap<u64, Key>,
}

fn identity(keyval: u32, keycode: u32) -> u64 {
    if keycode != 0 {
        u64::from(keycode)
    } else {
        (1 << 32) | u64::from(keyval)
    }
}

impl KeyTracker {
    fn shift_down(&self) -> bool {
        self.down.values().any(|k| *k == Key::Shift)
    }

    pub(crate) fn process(
        &mut self,
        keyval: u32,
        keycode: u32,
        state: u32,
        at_ms: u64,
    ) -> KeyDecision {
        if state & IBUS_FORWARD_MASK != 0 {
            return PASS;
        }
        let key = classify(keyval);
        let id = identity(keyval, keycode);
        let swallow = key == Key::Trigger;
        if state & IBUS_RELEASE_MASK == 0 {
            if self.down.contains_key(&id) {
                // Auto-repeat.
                return KeyDecision {
                    event: None,
                    handled: swallow,
                };
            }
            let shift_was_down = self.shift_down();
            self.down.insert(id, key);
            let event = if key == Key::Shift && shift_was_down {
                None // second Shift while the first is held
            } else {
                Some(Event::KeyDown { key, at_ms })
            };
            KeyDecision {
                event,
                handled: swallow,
            }
        } else {
            let Some(key) = self.down.remove(&id) else {
                return PASS;
            };
            let event = if key == Key::Shift && self.shift_down() {
                None // the other Shift is still held
            } else {
                Some(Event::KeyUp { key, at_ms })
            };
            KeyDecision {
                event,
                handled: key == Key::Trigger,
            }
        }
    }

    /// Releases every held key (focus lost / engine disabled), so the trigger never stays
    /// stuck down. Returns the key-ups to deliver.
    pub(crate) fn release_all(&mut self, at_ms: u64) -> Vec<Event> {
        let mut keys: Vec<(u64, Key)> = self.down.drain().collect();
        keys.sort_by_key(|(id, _)| *id);
        let mut out = Vec::new();
        let mut shift_up = false;
        for (_, key) in keys {
            if key == Key::Shift {
                if shift_up {
                    continue;
                }
                shift_up = true;
            }
            out.push(Event::KeyUp { key, at_ms });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2c_core::config::HotkeyConfig;
    use v2c_core::trigger::{Command, Trigger, TriggerConfig};
    use v2c_core::Mode;

    // evdev keycodes as IBus delivers them.
    const KC_RCTRL: u32 = 97;
    const KC_LSHIFT: u32 = 42;
    const KC_RSHIFT: u32 = 54;
    const KC_ESC: u32 = 1;
    const KC_C: u32 = 46;
    const KC_A: u32 = 30;
    const KEY_C: u32 = 0x63;
    const KEY_A: u32 = 0x61;
    const KEY_CAP_A: u32 = 0x41;

    fn press(t: &mut KeyTracker, keyval: u32, keycode: u32, state: u32, at: u64) -> KeyDecision {
        t.process(keyval, keycode, state, at)
    }
    fn release(t: &mut KeyTracker, keyval: u32, keycode: u32, state: u32, at: u64) -> KeyDecision {
        t.process(keyval, keycode, state | IBUS_RELEASE_MASK, at)
    }

    #[test]
    fn release_mask_matches_ibustypes_h() {
        assert_eq!(IBUS_RELEASE_MASK, 0x4000_0000);
        assert_eq!(IBUS_FORWARD_MASK, 0x0200_0000);
        assert_eq!((IBUS_SHIFT_MASK, IBUS_CONTROL_MASK), (1, 4));
    }

    #[test]
    fn conversion_table() {
        use Key::*;
        let rows: &[(u32, u32, Key)] = &[
            (KEY_CONTROL_R, KC_RCTRL, Trigger),
            (KEY_SHIFT_L, KC_LSHIFT, Shift),
            (KEY_SHIFT_R, KC_RSHIFT, Shift),
            (KEY_ESCAPE, KC_ESC, Escape),
            (0xffe3, 29, Other),  // Control_L
            (0xffe9, 56, Other),  // Alt_L
            (0xffeb, 125, Other), // Super_L
            (KEY_C, KC_C, Other),
            (KEY_RETURN, KEYCODE_ENTER, Other),
            (0x4e2d, 0, Other), // a CJK keyval with no keycode
        ];
        for &(keyval, keycode, key) in rows {
            let mut t = KeyTracker::default();
            let d = press(&mut t, keyval, keycode, 0, 10);
            assert_eq!(
                d.event,
                Some(Event::KeyDown { key, at_ms: 10 }),
                "{keyval:#x}"
            );
            assert_eq!(d.handled, key == Trigger, "press {keyval:#x}");
            let d = release(&mut t, keyval, keycode, 0, 20);
            assert_eq!(
                d.event,
                Some(Event::KeyUp { key, at_ms: 20 }),
                "{keyval:#x}"
            );
            assert_eq!(d.handled, key == Trigger, "release {keyval:#x}");
        }
    }

    #[test]
    fn autorepeat_is_dropped_but_trigger_repeat_still_swallowed() {
        let mut t = KeyTracker::default();
        assert!(press(&mut t, KEY_CONTROL_R, KC_RCTRL, 0, 0).event.is_some());
        for at in [500, 530, 560] {
            let d = press(&mut t, KEY_CONTROL_R, KC_RCTRL, IBUS_CONTROL_MASK, at);
            assert_eq!(
                d,
                KeyDecision {
                    event: None,
                    handled: true
                }
            );
        }
        let mut t = KeyTracker::default();
        press(&mut t, KEY_A, KC_A, 0, 0);
        assert_eq!(press(&mut t, KEY_A, KC_A, 0, 500), PASS);
    }

    #[test]
    fn letter_released_with_a_different_keyval_still_pairs_by_keycode() {
        let mut t = KeyTracker::default();
        press(&mut t, KEY_A, KC_A, 0, 0);
        let d = release(&mut t, KEY_CAP_A, KC_A, IBUS_SHIFT_MASK, 10);
        assert_eq!(
            d.event,
            Some(Event::KeyUp {
                key: Key::Other,
                at_ms: 10
            })
        );
    }

    #[test]
    fn release_without_press_is_passed_through_silently() {
        let mut t = KeyTracker::default();
        assert_eq!(
            release(&mut t, KEY_CONTROL_R, KC_RCTRL, IBUS_CONTROL_MASK, 0),
            PASS
        );
    }

    #[test]
    fn forwarded_events_are_ignored() {
        let mut t = KeyTracker::default();
        assert_eq!(
            press(&mut t, KEY_CONTROL_R, KC_RCTRL, IBUS_FORWARD_MASK, 0),
            PASS
        );
        assert!(t.down.is_empty());
    }

    #[test]
    fn two_shift_keys_fold_into_one() {
        let mut t = KeyTracker::default();
        assert!(press(&mut t, KEY_SHIFT_L, KC_LSHIFT, 0, 0).event.is_some());
        assert_eq!(press(&mut t, KEY_SHIFT_R, KC_RSHIFT, 1, 10).event, None);
        assert_eq!(release(&mut t, KEY_SHIFT_L, KC_LSHIFT, 1, 20).event, None);
        assert!(release(&mut t, KEY_SHIFT_R, KC_RSHIFT, 1, 30)
            .event
            .is_some());
    }

    #[test]
    fn release_all_lets_go_of_everything_once() {
        let mut t = KeyTracker::default();
        press(&mut t, KEY_SHIFT_L, KC_LSHIFT, 0, 0);
        press(&mut t, KEY_SHIFT_R, KC_RSHIFT, 1, 0);
        press(&mut t, KEY_CONTROL_R, KC_RCTRL, 1, 0);
        let ups = t.release_all(99);
        assert_eq!(ups.len(), 2);
        assert!(ups.contains(&Event::KeyUp {
            key: Key::Trigger,
            at_ms: 99
        }));
        assert!(ups.contains(&Event::KeyUp {
            key: Key::Shift,
            at_ms: 99
        }));
        assert!(t.release_all(100).is_empty());
    }

    // ---- sequences through the real trigger state machine ----

    /// (keyval, keycode, press?, at_ms)
    type Step = (u32, u32, bool, u64);

    fn run(steps: &[Step], ticks: &[u64]) -> (Vec<Command>, Vec<bool>) {
        let mut trig = Trigger::new(TriggerConfig::from(&HotkeyConfig::default()));
        let mut t = KeyTracker::default();
        let mut cmds = Vec::new();
        let mut handled = Vec::new();
        for &(keyval, keycode, is_press, at) in steps {
            let state = if is_press { 0 } else { IBUS_RELEASE_MASK };
            let d = t.process(keyval, keycode, state, at);
            handled.push(d.handled);
            if let Some(e) = d.event {
                cmds.extend(trig.on_event(e));
            }
        }
        for &at in ticks {
            cmds.extend(trig.on_event(Event::Tick { at_ms: at }));
        }
        (cmds, handled)
    }

    const RC: (u32, u32) = (KEY_CONTROL_R, KC_RCTRL);

    #[test]
    fn hold_to_talk() {
        let (cmds, handled) = run(
            &[
                (RC.0, RC.1, true, 1000),
                (RC.0, RC.1, true, 1500), // repeat
                (RC.0, RC.1, false, 3000),
            ],
            &[],
        );
        assert_eq!(
            cmds,
            vec![
                Command::StartRecording(Mode::Dictate),
                Command::StopAndProcess
            ]
        );
        assert_eq!(handled, vec![true, true, true]);
    }

    #[test]
    fn double_tap_hands_free_then_tap_to_stop() {
        let (cmds, _) = run(
            &[
                (RC.0, RC.1, true, 1000),
                (RC.0, RC.1, false, 1100),
                (RC.0, RC.1, true, 1250),
                (RC.0, RC.1, false, 1300),
                (RC.0, RC.1, true, 9000),
                (RC.0, RC.1, false, 9050),
            ],
            &[5000],
        );
        assert_eq!(
            cmds,
            vec![
                Command::StartRecording(Mode::Dictate),
                Command::StopAndProcess
            ]
        );
    }

    #[test]
    fn ctrl_c_cancels_and_c_reaches_the_app() {
        let (cmds, handled) = run(
            &[
                (RC.0, RC.1, true, 1000),
                (KEY_C, KC_C, true, 1080),
                (KEY_C, KC_C, false, 1150),
                (RC.0, RC.1, false, 1200),
            ],
            &[2000],
        );
        assert_eq!(
            cmds,
            vec![Command::StartRecording(Mode::Dictate), Command::Cancel]
        );
        assert_eq!(handled, vec![true, false, false, true]);
    }

    #[test]
    fn shift_selects_translate_and_shift_reaches_the_app() {
        let (cmds, handled) = run(
            &[
                (KEY_SHIFT_L, KC_LSHIFT, true, 900),
                (RC.0, RC.1, true, 1000),
                (RC.0, RC.1, false, 3000),
                (KEY_SHIFT_L, KC_LSHIFT, false, 3100),
            ],
            &[],
        );
        assert_eq!(
            cmds,
            vec![
                Command::StartRecording(Mode::TranslateZhToEn),
                Command::StopAndProcess
            ]
        );
        assert_eq!(handled, vec![false, true, true, false]);
    }

    #[test]
    fn escape_cancels_and_reaches_the_app() {
        let (cmds, handled) = run(
            &[
                (RC.0, RC.1, true, 1000),
                (KEY_ESCAPE, KC_ESC, true, 1500),
                (KEY_ESCAPE, KC_ESC, false, 1550),
                (RC.0, RC.1, false, 2000),
            ],
            &[],
        );
        assert_eq!(
            cmds,
            vec![Command::StartRecording(Mode::Dictate), Command::Cancel]
        );
        assert_eq!(handled, vec![true, false, false, true]);
    }

    #[test]
    fn ordinary_typing_passes_through_and_starts_nothing() {
        let steps: Vec<Step> = "hello"
            .bytes()
            .enumerate()
            .flat_map(|(i, b)| {
                let at = 1000 + 100 * i as u64;
                [
                    (u32::from(b), 30 + i as u32, true, at),
                    (u32::from(b), 30 + i as u32, false, at + 50),
                ]
            })
            .collect();
        let (cmds, handled) = run(&steps, &[5000]);
        assert!(cmds.is_empty());
        assert!(handled.iter().all(|h| !h));
    }
}
