//! `SendInput` + `KEYEVENTF_UNICODE` typing (T0.3) and clipboard paste with restore (T1.2).
//! Wait for all modifier keys to be released before sending anything.
//!
//! - [`Action::Paste`]: save clipboard -> set text -> Ctrl+V -> wait [`PASTE_SETTLE`] ->
//!   restore (see [`super::clipboard`] for which formats survive).
//! - Elevated foreground window (UIPI blocks our input): the whole text is put on the
//!   clipboard (best effort, not restored) and [`InjectError::Elevated`] is returned, so the
//!   UI can say "copied to the clipboard, paste it yourself" (tasks/plan.md section 4.5).
//! - Typing that the OS only partly accepts returns [`InjectError::Partial`];
//!   [`run_with_fallback`] then pastes the part that was not typed.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use thiserror::Error;
use v2c_core::config::NewlineKey;
use v2c_core::inject_plan::Action;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, VIRTUAL_KEY,
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RETURN, VK_RMENU,
    VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_V,
};

use super::clipboard::{self, RestoreOutcome, WinClipboard};
use super::foreground;
use crate::{InjectError, Injector};

/// Time the target app gets to read the clipboard after Ctrl+V before the original content
/// is restored. `SendInput` only queues the keys; the app reads the clipboard when it
/// handles them. 150 ms is the value fixed in tasks/plan.md section 4 (well above a normal
/// message-loop turn, short enough that a quick Ctrl+C right after dictation is rarely hit;
/// a copy made during the wait is detected and kept, see `clipboard::paste_with_restore`).
pub const PASTE_SETTLE: Duration = Duration::from_millis(150);

/// Longest wait for the user to let go of Ctrl/Shift/Alt/Win (tasks/plan.md section 4).
const MODIFIER_WAIT: Duration = Duration::from_millis(500);
const MODIFIER_POLL: Duration = Duration::from_millis(5);
/// Characters per `SendInput` call (tasks/plan.md section 4: ~50 per batch, no delay).
const CHARS_PER_BATCH: usize = 50;

const MODIFIERS: [VIRTUAL_KEY; 8] = [
    VK_LCONTROL,
    VK_RCONTROL,
    VK_LSHIFT,
    VK_RSHIFT,
    VK_LMENU,
    VK_RMENU,
    VK_LWIN,
    VK_RWIN,
];

/// [`Injector`] that types with `SendInput`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SendInputInjector;

impl Injector for SendInputInjector {
    /// Runs the actions in order. `InjectError::Partial` counts typed key events only
    /// (pastes contribute none), matching [`run_with_fallback`]'s bookkeeping.
    fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
        if actions.is_empty() {
            return Ok(());
        }
        if let Some(app) = foreground::current_app() {
            if app.elevated && !foreground::current_process_elevated() {
                // Best effort: the caller reports Elevated either way.
                let text = actions_text(actions);
                if !text.is_empty() {
                    let _ =
                        WinClipboard::new().and_then(|mut cb| clipboard::copy_text(&mut cb, &text));
                }
                return Err(InjectError::Elevated);
            }
        }
        wait_for_modifiers_released()?;

        let total: usize = actions.iter().map(typed_events).sum();
        let mut sent = 0usize;
        for action in actions {
            if let Action::Paste(text) = action {
                paste(text)?;
                continue;
            }
            for batch in action_batches(action) {
                let accepted = send(&batch);
                sent += accepted;
                if accepted < batch.len() {
                    // A half-sent Shift+Enter batch could leave Shift stuck down.
                    send(&[vk_input(VK_SHIFT, true)]);
                    return Err(InjectError::Partial { sent, total });
                }
            }
        }
        Ok(())
    }
}

/// Error from [`run_with_fallback`].
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{error}")]
pub struct FallbackError {
    pub error: InjectError,
    /// Part of the text was already typed and the error came from pasting the rest. With
    /// `InjectError::Elevated` this means only the *untyped remainder* is on the clipboard.
    pub after_partial_typing: bool,
}

impl From<InjectError> for FallbackError {
    fn from(error: InjectError) -> Self {
        FallbackError {
            error,
            after_partial_typing: false,
        }
    }
}

/// Runs `actions`; if the OS accepted only part of the typed input, pastes the rest
/// (tasks/plan.md section 4.3). Other errors, including `Elevated`, are returned unchanged.
pub fn run_with_fallback<I: Injector + ?Sized>(
    injector: &I,
    actions: &[Action],
) -> Result<(), FallbackError> {
    match injector.run(actions) {
        Err(InjectError::Partial { sent, .. }) => {
            let rest = actions_text(&remaining_after(actions, sent));
            if rest.is_empty() {
                return Ok(());
            }
            injector
                .run(&[Action::Paste(rest)])
                .map_err(|error| FallbackError {
                    error,
                    after_partial_typing: true,
                })
        }
        other => other.map_err(FallbackError::from),
    }
}

/// The text the actions produce, with `\n` for line breaks (for the clipboard).
pub fn actions_text(actions: &[Action]) -> String {
    actions
        .iter()
        .map(|a| match a {
            Action::Text(t) | Action::Paste(t) => t.as_str(),
            Action::NewLine(_) => "\n",
        })
        .collect()
}

/// Actions still to do after the first `sent` typed key events were accepted.
/// A character counts as typed once its key-down went through; a line break once Enter's
/// key-down did.
fn remaining_after(actions: &[Action], mut sent: usize) -> Vec<Action> {
    for (i, action) in actions.iter().enumerate() {
        let need = typed_events(action);
        if sent >= need {
            sent -= need;
            continue;
        }
        let mut rest = Vec::new();
        match action {
            Action::Text(t) => {
                let tail = text_after_units(t, sent.div_ceil(2));
                if !tail.is_empty() {
                    rest.push(Action::Text(tail.to_owned()));
                }
            }
            Action::NewLine(key) => {
                // Index of Enter's key-down in `newline_events`.
                let enter_down = match key {
                    NewlineKey::Enter => 0,
                    NewlineKey::ShiftEnter => 1,
                };
                if sent <= enter_down {
                    rest.push(Action::NewLine(*key));
                }
            }
            // Pastes have no typed events, so they never get here.
            Action::Paste(_) => rest.push(action.clone()),
        }
        rest.extend_from_slice(&actions[i + 1..]);
        return rest;
    }
    Vec::new()
}

/// `text` without the characters fully covered by its first `units` UTF-16 code units
/// (a half-typed surrogate pair counts as not typed).
fn text_after_units(text: &str, units: usize) -> &str {
    let mut done = 0;
    for (byte, c) in text.char_indices() {
        if done + c.len_utf16() > units {
            return &text[byte..];
        }
        done += c.len_utf16();
    }
    ""
}

/// Number of key events [`action_batches`] produces for `action`.
fn typed_events(action: &Action) -> usize {
    match action {
        Action::Text(t) => 2 * t.encode_utf16().count(),
        Action::NewLine(NewlineKey::Enter) => 2,
        Action::NewLine(NewlineKey::ShiftEnter) => 4,
        Action::Paste(_) => 0,
    }
}

/// 0 = no paste yet / last paste failed, else `outcome_code`.
static LAST_RESTORE: AtomicU8 = AtomicU8::new(0);

fn outcome_code(o: RestoreOutcome) -> u8 {
    match o {
        RestoreOutcome::Restored => 1,
        RestoreOutcome::SkippedChangedByOther => 2,
    }
}

/// What happened to the user's clipboard after the most recent successful paste (any
/// thread), so callers can log it, e.g. `outcome.as_str()`; `None` if the last paste failed
/// or none ran. This crate has no logger of its own.
pub fn last_restore_outcome() -> Option<RestoreOutcome> {
    match LAST_RESTORE.load(Ordering::Relaxed) {
        1 => Some(RestoreOutcome::Restored),
        2 => Some(RestoreOutcome::SkippedChangedByOther),
        _ => None,
    }
}

/// Clipboard paste with restore; modifiers must already be released.
fn paste(text: &str) -> Result<(), InjectError> {
    LAST_RESTORE.store(0, Ordering::Relaxed);
    let mut cb = WinClipboard::new()?;
    let outcome = clipboard::paste_with_restore(&mut cb, text, send_ctrl_v, PASTE_SETTLE)?;
    LAST_RESTORE.store(outcome_code(outcome), Ordering::Relaxed);
    Ok(())
}

fn send_ctrl_v() -> Result<(), InjectError> {
    let events = ctrl_v_events();
    let accepted = send(&events);
    let (key_ups, pasted) = ctrl_v_cleanup(accepted);
    let ups: Vec<INPUT> = key_ups.iter().map(|&vk| vk_input(vk, true)).collect();
    send(&ups);
    if !pasted {
        return Err(InjectError::Os(format!(
            "Ctrl+V not accepted ({accepted} of {} events); text left on the clipboard",
            events.len()
        )));
    }
    Ok(())
}

/// After `accepted` of the 4 [`ctrl_v_events`]: which key-ups still need sending (so no
/// key stays down), and whether the paste happened. Once Ctrl-down and V-down went through
/// the app has pasted, so we treat it as success (and restore) rather than risk a
/// duplicate paste on retry.
fn ctrl_v_cleanup(accepted: usize) -> (Vec<VIRTUAL_KEY>, bool) {
    match accepted {
        0 => (vec![], false),
        1 => (vec![VK_CONTROL], false),
        2 => (vec![VK_V, VK_CONTROL], true),
        3 => (vec![VK_CONTROL], true),
        _ => (vec![], true),
    }
}

fn ctrl_v_events() -> Vec<INPUT> {
    vec![
        vk_input(VK_CONTROL, false),
        vk_input(VK_V, false),
        vk_input(VK_V, true),
        vk_input(VK_CONTROL, true),
    ]
}

fn wait_for_modifiers_released() -> Result<(), InjectError> {
    let deadline = Instant::now() + MODIFIER_WAIT;
    loop {
        if !any_modifier_down() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            // Typing now would turn characters into shortcuts (Ctrl+W, Alt+F4, ...).
            return Err(InjectError::Os(
                "modifier keys still held after 500 ms".into(),
            ));
        }
        std::thread::sleep(MODIFIER_POLL);
    }
}

fn any_modifier_down() -> bool {
    MODIFIERS.iter().any(|&vk| {
        // SAFETY: plain FFI call; the high bit means "currently down".
        let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
        state < 0
    })
}

/// Passes one batch to `SendInput`; returns the number of events the OS accepted.
fn send(batch: &[INPUT]) -> usize {
    if batch.is_empty() {
        return 0;
    }
    // SAFETY: `batch` is a valid slice of initialised INPUT structs of the given size.
    let n = unsafe {
        SendInput(
            batch.len() as u32,
            batch.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    n as usize
}

/// One `SendInput` batch per entry.
fn action_batches(action: &Action) -> Vec<Vec<INPUT>> {
    match action {
        Action::Text(t) => utf16_batches(t, CHARS_PER_BATCH)
            .iter()
            .map(|units| unicode_events(units))
            .collect(),
        Action::NewLine(key) => vec![newline_events(*key)],
        // Handled by `paste` in `run`.
        Action::Paste(_) => Vec::new(),
    }
}

/// Splits `text` into UTF-16 chunks of at most `max_chars` characters each, never between
/// the two halves of a surrogate pair.
fn utf16_batches(text: &str, max_chars: usize) -> Vec<Vec<u16>> {
    let max_chars = max_chars.max(1);
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut count = 0;
    let mut buf = [0u16; 2];
    for c in text.chars() {
        current.extend_from_slice(c.encode_utf16(&mut buf));
        count += 1;
        if count == max_chars {
            batches.push(std::mem::take(&mut current));
            count = 0;
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// Key down + key up per UTF-16 code unit.
fn unicode_events(units: &[u16]) -> Vec<INPUT> {
    units
        .iter()
        .flat_map(|&u| {
            [
                key_input(0, u, KEYEVENTF_UNICODE),
                key_input(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ]
        })
        .collect()
}

fn newline_events(key: NewlineKey) -> Vec<INPUT> {
    let enter = [vk_input(VK_RETURN, false), vk_input(VK_RETURN, true)];
    match key {
        NewlineKey::Enter => enter.to_vec(),
        NewlineKey::ShiftEnter => {
            let mut v = vec![vk_input(VK_SHIFT, false)];
            v.extend(enter);
            v.push(vk_input(VK_SHIFT, true));
            v
        }
    }
}

fn vk_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    // SAFETY: plain FFI call; returns 0 when there is no mapping.
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
    key_input(vk, scan, if up { KEYEVENTF_KEYUP } else { 0 })
}

fn key_input(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: super::INJECT_MARKER,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ki(i: &INPUT) -> KEYBDINPUT {
        assert_eq!(i.r#type, INPUT_KEYBOARD);
        // SAFETY: every INPUT built here is a keyboard input.
        unsafe { i.Anonymous.ki }
    }

    #[test]
    fn batches_respect_char_limit() {
        let b = utf16_batches("abcde", 2);
        assert_eq!(b.len(), 3);
        assert_eq!(b[2], vec![u16::from(b'e')]);
        assert!(utf16_batches("", 50).is_empty());
    }

    #[test]
    fn surrogate_pairs_are_never_split_across_batches() {
        // Each emoji / CJK Ext-B char is 2 UTF-16 units; batch size 1 char.
        let b = utf16_batches("👍𠮷a", 1);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0], "👍".encode_utf16().collect::<Vec<_>>());
        assert_eq!(b[1], "𠮷".encode_utf16().collect::<Vec<_>>());
        for batch in &b {
            let s = String::from_utf16(batch).expect("each batch is valid UTF-16 on its own");
            assert!(!s.is_empty());
        }
        // Odd batch size with mixed text: still valid per batch.
        let mixed = "這個 meeting 👍 我們 delay 𠮷 一下".repeat(5);
        let rebuilt: String = utf16_batches(&mixed, 7)
            .iter()
            .map(|u| String::from_utf16(u).unwrap())
            .collect();
        assert_eq!(rebuilt, mixed);
    }

    #[test]
    fn unicode_events_are_down_up_per_unit() {
        let units: Vec<u16> = "👍".encode_utf16().collect();
        let ev = unicode_events(&units);
        assert_eq!(ev.len(), 4);
        let k: Vec<KEYBDINPUT> = ev.iter().map(ki).collect();
        assert_eq!(k[0].wScan, units[0]);
        assert_eq!(k[0].dwFlags, KEYEVENTF_UNICODE);
        assert_eq!(k[1].wScan, units[0]);
        assert_eq!(k[1].dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        assert_eq!(k[2].wScan, units[1]);
        assert_eq!(k[3].dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        assert!(k.iter().all(|k| k.wVk == 0));
    }

    #[test]
    fn shift_enter_wraps_enter_in_shift() {
        let k: Vec<KEYBDINPUT> = newline_events(NewlineKey::ShiftEnter)
            .iter()
            .map(ki)
            .collect();
        let seq: Vec<(VIRTUAL_KEY, bool)> = k
            .iter()
            .map(|k| (k.wVk, k.dwFlags & KEYEVENTF_KEYUP != 0))
            .collect();
        assert_eq!(
            seq,
            vec![
                (VK_SHIFT, false),
                (VK_RETURN, false),
                (VK_RETURN, true),
                (VK_SHIFT, true)
            ]
        );
        assert!(k.iter().all(|k| k.dwFlags & KEYEVENTF_UNICODE == 0));

        let enter: Vec<VIRTUAL_KEY> = newline_events(NewlineKey::Enter)
            .iter()
            .map(|i| ki(i).wVk)
            .collect();
        assert_eq!(enter, vec![VK_RETURN, VK_RETURN]);
    }

    #[test]
    fn text_action_event_count() {
        // 3 chars, 4 UTF-16 units -> 8 events in one batch.
        let b = action_batches(&Action::Text("讚👍a".into()));
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].len(), 8);
        // 120 chars -> 50 + 50 + 20.
        let b = action_batches(&Action::Text("字".repeat(120)));
        assert_eq!(
            b.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![100, 100, 40]
        );
    }

    #[test]
    fn ctrl_v_is_ctrl_down_v_down_v_up_ctrl_up() {
        let k: Vec<KEYBDINPUT> = ctrl_v_events().iter().map(ki).collect();
        let seq: Vec<(VIRTUAL_KEY, bool)> = k
            .iter()
            .map(|k| (k.wVk, k.dwFlags & KEYEVENTF_KEYUP != 0))
            .collect();
        assert_eq!(
            seq,
            vec![
                (VK_CONTROL, false),
                (VK_V, false),
                (VK_V, true),
                (VK_CONTROL, true)
            ]
        );
        assert!(k.iter().all(|k| k.dwFlags & KEYEVENTF_UNICODE == 0));
    }

    #[test]
    fn every_injected_event_carries_the_marker() {
        let actions = [
            Action::Text("字👍".into()),
            Action::NewLine(NewlineKey::ShiftEnter),
            Action::NewLine(NewlineKey::Enter),
        ];
        let mut events: Vec<INPUT> = actions.iter().flat_map(action_batches).flatten().collect();
        events.extend(ctrl_v_events());
        events.push(vk_input(VK_SHIFT, true));
        assert!(!events.is_empty());
        assert!(events
            .iter()
            .all(|i| ki(i).dwExtraInfo == super::super::INJECT_MARKER));
    }

    #[test]
    fn typed_events_match_batches() {
        for a in [
            Action::Text("讚👍a".into()),
            Action::Text("字".repeat(120)),
            Action::NewLine(NewlineKey::Enter),
            Action::NewLine(NewlineKey::ShiftEnter),
            Action::Paste("x".into()),
        ] {
            let built: usize = action_batches(&a).iter().map(Vec::len).sum();
            assert_eq!(typed_events(&a), built, "{a:?}");
        }
    }

    #[test]
    fn actions_text_joins_with_newlines() {
        let a = [
            Action::Text("第一行".into()),
            Action::NewLine(NewlineKey::ShiftEnter),
            Action::Text("b".into()),
            Action::Paste("c".into()),
        ];
        assert_eq!(actions_text(&a), "第一行\nbc");
        assert_eq!(actions_text(&[]), "");
    }

    fn t(s: &str) -> Action {
        Action::Text(s.into())
    }

    #[test]
    fn remaining_after_partial_text() {
        let a = [t("abc"), Action::NewLine(NewlineKey::Enter), t("de")];
        // Nothing sent: everything remains.
        assert_eq!(remaining_after(&a, 0), a.to_vec());
        // "a" down+up, "b" down only: b counts as typed.
        assert_eq!(
            remaining_after(&a, 3),
            vec![t("c"), Action::NewLine(NewlineKey::Enter), t("de")]
        );
        // All of "abc" sent, Enter not.
        assert_eq!(
            remaining_after(&a, 6),
            vec![Action::NewLine(NewlineKey::Enter), t("de")]
        );
        // Enter key-down went through: line break done.
        assert_eq!(remaining_after(&a, 7), vec![t("de")]);
        assert!(remaining_after(&a, 12).is_empty());
    }

    #[test]
    fn remaining_after_shift_enter_needs_enter_down() {
        let a = [Action::NewLine(NewlineKey::ShiftEnter), t("x")];
        assert_eq!(remaining_after(&a, 1), a.to_vec()); // only Shift down
        assert_eq!(remaining_after(&a, 2), vec![t("x")]);
    }

    #[test]
    fn remaining_after_never_splits_surrogate_pairs() {
        let a = [t("👍b")];
        // High surrogate down only (1 event): the emoji is not complete.
        assert_eq!(remaining_after(&a, 1), vec![t("👍b")]);
        // Both halves down (3 events: hi down, hi up, lo down).
        assert_eq!(remaining_after(&a, 3), vec![t("b")]);
    }

    #[test]
    fn remaining_after_skips_completed_pastes() {
        let a = [Action::Paste("p".into()), t("ab")];
        assert_eq!(remaining_after(&a, 2), vec![t("b")]);
    }

    /// Returns `first`, then `then`, then `Ok`.
    struct FakeInjector {
        first: std::cell::RefCell<Option<Result<(), InjectError>>>,
        then: std::cell::RefCell<Option<Result<(), InjectError>>>,
        calls: std::cell::RefCell<Vec<Vec<Action>>>,
    }

    impl Injector for FakeInjector {
        fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
            self.calls.borrow_mut().push(actions.to_vec());
            self.first
                .borrow_mut()
                .take()
                .or_else(|| self.then.borrow_mut().take())
                .unwrap_or(Ok(()))
        }
    }

    fn fake(first: Result<(), InjectError>) -> FakeInjector {
        FakeInjector {
            first: Some(first).into(),
            then: None.into(),
            calls: Default::default(),
        }
    }

    #[test]
    fn fallback_pastes_the_untyped_rest() {
        let a = [t("ab"), Action::NewLine(NewlineKey::ShiftEnter), t("c")];
        let inj = fake(Err(InjectError::Partial { sent: 2, total: 8 }));
        assert_eq!(run_with_fallback(&inj, &a), Ok(()));
        let calls = inj.calls.into_inner();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], vec![Action::Paste("b\nc".into())]);
    }

    #[test]
    fn fallback_passes_other_results_through() {
        let a = [t("x")];
        let inj = fake(Err(InjectError::Elevated));
        assert_eq!(
            run_with_fallback(&inj, &a),
            Err(FallbackError {
                error: InjectError::Elevated,
                after_partial_typing: false
            })
        );
        assert_eq!(inj.calls.into_inner().len(), 1);
        let inj = fake(Ok(()));
        assert_eq!(run_with_fallback(&inj, &a), Ok(()));
        assert_eq!(inj.calls.into_inner().len(), 1);
    }

    #[test]
    fn elevated_after_partial_typing_is_flagged() {
        let a = [t("abc")];
        let inj = FakeInjector {
            first: Some(Err(InjectError::Partial { sent: 2, total: 6 })).into(),
            then: Some(Err(InjectError::Elevated)).into(),
            calls: Default::default(),
        };
        let err = run_with_fallback(&inj, &a).unwrap_err();
        assert_eq!(err.error, InjectError::Elevated);
        assert!(err.after_partial_typing);
        assert_eq!(inj.calls.into_inner()[1], vec![Action::Paste("bc".into())]);
    }

    #[test]
    fn ctrl_v_cleanup_releases_keys_and_judges_paste() {
        assert_eq!(ctrl_v_cleanup(0), (vec![], false));
        assert_eq!(ctrl_v_cleanup(1), (vec![VK_CONTROL], false));
        // Ctrl down + V down reached the app: pasted; release both.
        assert_eq!(ctrl_v_cleanup(2), (vec![VK_V, VK_CONTROL], true));
        assert_eq!(ctrl_v_cleanup(3), (vec![VK_CONTROL], true));
        assert_eq!(ctrl_v_cleanup(4), (vec![], true));
    }

    #[test]
    fn empty_actions_are_a_no_op() {
        assert_eq!(SendInputInjector.run(&[]), Ok(()));
    }
}
