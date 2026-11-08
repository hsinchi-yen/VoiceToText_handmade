//! Hotkey state machine (pure logic, no OS calls, no clock reads).
//!
//! The platform hook (`v2c-platform`) turns raw key events into [`Event`]s with timestamps;
//! this module decides when to start, stop, or cancel recording. Behaviour is specified by
//! the state table in `tasks/plan.md` section 3.

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

/// Internal state; mirrors the states of the plan.md section 3 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// Trigger held; recording started at `started_ms`.
    Pressed {
        started_ms: u64,
        mode: Mode,
    },
    /// Trigger released after a short tap at `released_ms`; recording is kept while waiting
    /// for a second tap.
    TapWait {
        started_ms: u64,
        released_ms: u64,
        mode: Mode,
    },
    /// Hands-free recording. `trigger_held` is true while the second tap that entered this
    /// state is still held, so Trigger+Other (e.g. Ctrl+C right after a tap) still cancels.
    HandsFree {
        started_ms: u64,
        mode: Mode,
        trigger_held: bool,
    },
}

/// The hotkey state machine. Feed it every [`Event`] in order.
#[derive(Debug)]
pub struct Trigger {
    cfg: TriggerConfig,
    state: State,
    /// Physical state of the trigger key, used to drop auto-repeat and stray key-ups.
    trigger_down: bool,
    shift_down: bool,
}

impl Trigger {
    pub fn new(cfg: TriggerConfig) -> Self {
        Self {
            cfg,
            state: State::Idle,
            trigger_down: false,
            shift_down: false,
        }
    }

    pub fn config(&self) -> TriggerConfig {
        self.cfg
    }

    /// Advances the state machine and returns the commands to execute (usually zero or one).
    ///
    /// Timeouts are evaluated first against the event's timestamp, so a late event can yield
    /// two commands (e.g. `Cancel` for an expired tap, then `StartRecording`).
    pub fn on_event(&mut self, event: Event) -> Vec<Command> {
        let mut out = Vec::new();
        let now = match event {
            Event::KeyDown { at_ms, .. } | Event::KeyUp { at_ms, .. } | Event::Tick { at_ms } => {
                at_ms
            }
        };
        self.check_timeouts(now, &mut out);

        match event {
            Event::Tick { .. } => {}
            Event::KeyDown { key, at_ms } => self.key_down(key, at_ms, &mut out),
            Event::KeyUp { key, at_ms } => self.key_up(key, at_ms, &mut out),
        }
        out
    }

    fn check_timeouts(&mut self, now: u64, out: &mut Vec<Command>) {
        match self.state {
            State::TapWait { released_ms, .. }
                if now.saturating_sub(released_ms) > self.cfg.double_tap_window_ms =>
            {
                // A single tap does not count.
                self.state = State::Idle;
                out.push(Command::Cancel);
            }
            State::HandsFree { started_ms, .. }
                if now.saturating_sub(started_ms) >= self.cfg.hands_free_max_ms =>
            {
                self.state = State::Idle;
                out.push(Command::StopAtLimit);
            }
            _ => {}
        }
    }

    fn key_down(&mut self, key: Key, at_ms: u64, out: &mut Vec<Command>) {
        match key {
            Key::Shift => self.shift_down = true,
            Key::Escape => {
                if self.state != State::Idle {
                    self.state = State::Idle;
                    out.push(Command::Cancel);
                }
            }
            Key::Other => match self.state {
                // The user is typing a shortcut such as Ctrl+C, or typing after a single tap.
                State::Pressed { .. }
                | State::TapWait { .. }
                | State::HandsFree {
                    trigger_held: true, ..
                } => {
                    self.state = State::Idle;
                    out.push(Command::Cancel);
                }
                State::Idle | State::HandsFree { .. } => {}
            },
            Key::Trigger => {
                if self.trigger_down {
                    return; // auto-repeat
                }
                self.trigger_down = true;
                match self.state {
                    State::Idle => {
                        let mode = if self.shift_down {
                            Mode::TranslateZhToEn
                        } else {
                            Mode::Dictate
                        };
                        self.state = State::Pressed {
                            started_ms: at_ms,
                            mode,
                        };
                        // Start immediately so the first syllable is not lost.
                        out.push(Command::StartRecording(mode));
                    }
                    State::TapWait {
                        started_ms, mode, ..
                    } => {
                        self.state = State::HandsFree {
                            started_ms,
                            mode,
                            trigger_held: true,
                        };
                    }
                    State::HandsFree { .. } => {
                        self.state = State::Idle;
                        out.push(Command::StopAndProcess);
                    }
                    // Unreachable while `trigger_down` tracks the key; ignore defensively.
                    State::Pressed { .. } => {}
                }
            }
        }
    }

    fn key_up(&mut self, key: Key, at_ms: u64, out: &mut Vec<Command>) {
        match key {
            Key::Shift => self.shift_down = false,
            Key::Escape | Key::Other => {}
            Key::Trigger => {
                self.trigger_down = false;
                match self.state {
                    State::Pressed { started_ms, mode } => {
                        if at_ms.saturating_sub(started_ms) >= self.cfg.hold_threshold_ms {
                            // Hold-to-talk.
                            self.state = State::Idle;
                            out.push(Command::StopAndProcess);
                        } else {
                            self.state = State::TapWait {
                                started_ms,
                                released_ms: at_ms,
                                mode,
                            };
                        }
                    }
                    State::HandsFree {
                        started_ms, mode, ..
                    } => {
                        self.state = State::HandsFree {
                            started_ms,
                            mode,
                            trigger_held: false,
                        };
                    }
                    State::Idle | State::TapWait { .. } => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Command::*;
    use super::*;
    use crate::config::HotkeyConfig;

    fn trigger() -> Trigger {
        Trigger::new(TriggerConfig::from(&HotkeyConfig::default()))
    }

    fn down(key: Key, at_ms: u64) -> Event {
        Event::KeyDown { key, at_ms }
    }

    fn up(key: Key, at_ms: u64) -> Event {
        Event::KeyUp { key, at_ms }
    }

    fn tick(at_ms: u64) -> Event {
        Event::Tick { at_ms }
    }

    /// Feeds events in order and returns all emitted commands.
    fn run(t: &mut Trigger, events: &[Event]) -> Vec<Command> {
        events.iter().flat_map(|e| t.on_event(*e)).collect()
    }

    /// Short tap, then a second tap inside the window: hands-free, recording started at 1000.
    fn enter_hands_free(t: &mut Trigger) -> Vec<Command> {
        run(
            t,
            &[
                down(Key::Trigger, 1000),
                up(Key::Trigger, 1100),
                down(Key::Trigger, 1250),
                up(Key::Trigger, 1300),
            ],
        )
    }

    // Row: Idle + RCtrl down -> Pressed, StartRecording immediately.
    #[test]
    fn trigger_down_starts_recording_immediately() {
        let mut t = trigger();
        assert_eq!(
            t.on_event(down(Key::Trigger, 1000)),
            vec![StartRecording(Mode::Dictate)]
        );
    }

    // Row: Idle + RCtrl down with Shift held -> mode = Translate.
    #[test]
    fn shift_held_selects_translate_mode() {
        let mut t = trigger();
        assert_eq!(
            run(&mut t, &[down(Key::Shift, 900), down(Key::Trigger, 1000)]),
            vec![StartRecording(Mode::TranslateZhToEn)]
        );
    }

    #[test]
    fn released_shift_does_not_select_translate_mode() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Shift, 900),
                    up(Key::Shift, 950),
                    down(Key::Trigger, 1000)
                ]
            ),
            vec![StartRecording(Mode::Dictate)]
        );
    }

    #[test]
    fn translate_hold_to_talk_stops_and_processes() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Shift, 900),
                    down(Key::Trigger, 1000),
                    up(Key::Shift, 1500),
                    up(Key::Trigger, 2000)
                ]
            ),
            vec![StartRecording(Mode::TranslateZhToEn), StopAndProcess]
        );
    }

    // Row: Pressed + other key -> Idle, Cancel (the user is pressing Ctrl+C).
    #[test]
    fn ctrl_c_cancels_and_does_not_record() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    down(Key::Other, 1080),
                    up(Key::Other, 1150),
                    up(Key::Trigger, 1200),
                    tick(2000),
                ]
            ),
            vec![StartRecording(Mode::Dictate), Cancel]
        );
    }

    #[test]
    fn repeated_ctrl_combos_while_held_cancel_once() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    down(Key::Other, 1500),
                    up(Key::Other, 1550),
                    down(Key::Other, 1600),
                    up(Key::Trigger, 2000),
                ]
            ),
            vec![StartRecording(Mode::Dictate), Cancel]
        );
    }

    #[test]
    fn ctrl_shift_combo_cancels_translate() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Shift, 900),
                    down(Key::Trigger, 1000),
                    down(Key::Other, 1100),
                ]
            ),
            vec![StartRecording(Mode::TranslateZhToEn), Cancel]
        );
    }

    // Row: Pressed + release after >= 350 ms -> Idle, StopAndProcess (hold-to-talk).
    #[test]
    fn hold_to_talk_stops_and_processes() {
        let mut t = trigger();
        assert_eq!(
            run(&mut t, &[down(Key::Trigger, 1000), up(Key::Trigger, 3000)]),
            vec![StartRecording(Mode::Dictate), StopAndProcess]
        );
    }

    #[test]
    fn hold_exactly_at_threshold_counts_as_hold() {
        let mut t = trigger();
        assert_eq!(
            run(&mut t, &[down(Key::Trigger, 1000), up(Key::Trigger, 1350)]),
            vec![StartRecording(Mode::Dictate), StopAndProcess]
        );
    }

    // Row: Pressed + release after < 350 ms -> TapWait, recording kept (no command).
    #[test]
    fn short_tap_waits_without_command() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[down(Key::Trigger, 1000), up(Key::Trigger, 1349), tick(1500)]
            ),
            vec![StartRecording(Mode::Dictate)]
        );
    }

    // Row: TapWait + RCtrl within 300 ms -> HandsFree, keep recording.
    #[test]
    fn double_tap_enters_hands_free() {
        let mut t = trigger();
        assert_eq!(
            enter_hands_free(&mut t),
            vec![StartRecording(Mode::Dictate)]
        );
        // Still recording long after the tap window.
        assert_eq!(run(&mut t, &[tick(5000), tick(60_000)]), vec![]);
    }

    #[test]
    fn second_tap_at_window_edge_enters_hands_free() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    up(Key::Trigger, 1100),
                    down(Key::Trigger, 1400),
                    up(Key::Trigger, 1450),
                    tick(2000),
                ]
            ),
            vec![StartRecording(Mode::Dictate)]
        );
    }

    #[test]
    fn double_tap_keeps_translate_mode() {
        let mut t = trigger();
        let cmds = run(
            &mut t,
            &[
                down(Key::Shift, 900),
                down(Key::Trigger, 1000),
                up(Key::Trigger, 1100),
                up(Key::Shift, 1150),
                down(Key::Trigger, 1250),
                up(Key::Trigger, 1300),
                down(Key::Trigger, 9000),
            ],
        );
        assert_eq!(
            cmds,
            vec![StartRecording(Mode::TranslateZhToEn), StopAndProcess]
        );
    }

    // Row: TapWait + timeout -> Idle, Cancel (a single tap does not count).
    #[test]
    fn single_tap_times_out_and_cancels() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    up(Key::Trigger, 1100),
                    tick(1400),
                    tick(1450),
                    tick(1500),
                ]
            ),
            vec![StartRecording(Mode::Dictate), Cancel]
        );
    }

    #[test]
    fn late_second_tap_cancels_then_starts_new_recording() {
        let mut t = trigger();
        run(&mut t, &[down(Key::Trigger, 1000), up(Key::Trigger, 1100)]);
        assert_eq!(
            t.on_event(down(Key::Trigger, 1500)),
            vec![Cancel, StartRecording(Mode::Dictate)]
        );
    }

    #[test]
    fn typing_after_single_tap_cancels() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    up(Key::Trigger, 1100),
                    down(Key::Other, 1200),
                    tick(2000),
                ]
            ),
            vec![StartRecording(Mode::Dictate), Cancel]
        );
    }

    // Row: HandsFree + RCtrl -> Idle, StopAndProcess.
    #[test]
    fn trigger_in_hands_free_stops_and_processes() {
        let mut t = trigger();
        enter_hands_free(&mut t);
        assert_eq!(t.on_event(down(Key::Trigger, 20_000)), vec![StopAndProcess]);
        // The release of that press does nothing.
        assert_eq!(
            run(&mut t, &[up(Key::Trigger, 20_100), tick(21_000)]),
            vec![]
        );
    }

    #[test]
    fn other_keys_in_hands_free_are_ignored() {
        let mut t = trigger();
        enter_hands_free(&mut t);
        assert_eq!(
            run(&mut t, &[down(Key::Other, 5000), up(Key::Other, 5050)]),
            vec![]
        );
    }

    #[test]
    fn ctrl_c_on_second_tap_cancels_hands_free() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    up(Key::Trigger, 1100),
                    down(Key::Trigger, 1250),
                    down(Key::Other, 1300),
                ]
            ),
            vec![StartRecording(Mode::Dictate), Cancel]
        );
    }

    // Row: HandsFree + recording reaches the limit (5 min) -> Idle, StopAtLimit.
    #[test]
    fn hands_free_limit_stops_at_limit() {
        let mut t = trigger();
        enter_hands_free(&mut t);
        // Recording started at 1000 ms; the default limit is 300 000 ms.
        assert_eq!(t.on_event(tick(300_950)), vec![]);
        assert_eq!(t.on_event(tick(301_000)), vec![StopAtLimit]);
        assert_eq!(t.on_event(tick(301_050)), vec![]);
        // The next press starts a fresh recording.
        assert_eq!(
            t.on_event(down(Key::Trigger, 302_000)),
            vec![StartRecording(Mode::Dictate)]
        );
    }

    #[test]
    fn hands_free_limit_uses_config() {
        let mut t = Trigger::new(TriggerConfig {
            hold_threshold_ms: 350,
            double_tap_window_ms: 300,
            hands_free_max_ms: 10_000,
        });
        enter_hands_free(&mut t);
        assert_eq!(t.on_event(tick(11_000)), vec![StopAtLimit]);
    }

    // Row: any recording state + Esc -> Idle, Cancel.
    #[test]
    fn escape_cancels_while_pressed() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    down(Key::Escape, 1500),
                    up(Key::Trigger, 2000)
                ]
            ),
            vec![StartRecording(Mode::Dictate), Cancel]
        );
    }

    #[test]
    fn escape_cancels_during_tap_wait() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    up(Key::Trigger, 1100),
                    down(Key::Escape, 1200),
                    tick(2000)
                ]
            ),
            vec![StartRecording(Mode::Dictate), Cancel]
        );
    }

    #[test]
    fn escape_cancels_hands_free() {
        let mut t = trigger();
        enter_hands_free(&mut t);
        assert_eq!(t.on_event(down(Key::Escape, 8000)), vec![Cancel]);
        assert_eq!(t.on_event(tick(400_000)), vec![]);
    }

    #[test]
    fn escape_in_idle_does_nothing() {
        let mut t = trigger();
        assert_eq!(
            run(&mut t, &[down(Key::Escape, 10), up(Key::Escape, 20)]),
            vec![]
        );
    }

    // Robustness.
    #[test]
    fn idle_ignores_other_keys_ticks_and_stray_release() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Other, 10),
                    up(Key::Other, 20),
                    up(Key::Trigger, 30),
                    tick(1_000_000)
                ]
            ),
            vec![]
        );
    }

    #[test]
    fn auto_repeat_trigger_down_is_ignored() {
        let mut t = trigger();
        assert_eq!(
            run(
                &mut t,
                &[
                    down(Key::Trigger, 1000),
                    down(Key::Trigger, 1500),
                    down(Key::Trigger, 1530),
                    up(Key::Trigger, 2000),
                ]
            ),
            vec![StartRecording(Mode::Dictate), StopAndProcess]
        );
    }

    #[test]
    fn release_after_ctrl_c_cancel_does_not_enter_tap_wait() {
        let mut t = trigger();
        run(&mut t, &[down(Key::Trigger, 1000), down(Key::Other, 1050)]);
        assert_eq!(
            run(
                &mut t,
                &[
                    up(Key::Trigger, 1100),
                    down(Key::Trigger, 1200),
                    up(Key::Trigger, 1900)
                ]
            ),
            vec![StartRecording(Mode::Dictate), StopAndProcess]
        );
    }

    #[test]
    fn timestamps_going_backwards_do_not_panic() {
        let mut t = trigger();
        assert_eq!(
            run(&mut t, &[down(Key::Trigger, 1000), up(Key::Trigger, 500)]),
            vec![StartRecording(Mode::Dictate)]
        );
        assert_eq!(t.on_event(tick(400)), vec![]);
    }

    #[test]
    fn config_conversion_uses_milliseconds() {
        let c = TriggerConfig::from(&HotkeyConfig::default());
        assert_eq!(c.hands_free_max_ms, 300_000);
        assert_eq!(Trigger::new(c).config().hold_threshold_ms, 350);
    }
}
