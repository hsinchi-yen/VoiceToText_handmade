//! `SendInput` + `KEYEVENTF_UNICODE` typing (T0.3) and clipboard paste with restore (T1.2).
//! Wait for all modifier keys to be released before sending anything.

use std::time::{Duration, Instant};

use v2c_core::config::NewlineKey;
use v2c_core::inject_plan::Action;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, VIRTUAL_KEY,
    VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RETURN, VK_RMENU, VK_RSHIFT,
    VK_RWIN, VK_SHIFT,
};

use super::foreground;
use crate::{InjectError, Injector};

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
    fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
        if actions.iter().any(|a| matches!(a, Action::Paste(_))) {
            return Err(InjectError::Os("paste not implemented (T1.2)".into()));
        }
        if let Some(app) = foreground::current_app() {
            if app.elevated && !foreground::current_process_elevated() {
                return Err(InjectError::Elevated);
            }
        }
        wait_for_modifiers_released()?;

        let batches: Vec<Vec<INPUT>> = actions.iter().flat_map(action_batches).collect();
        let total: usize = batches.iter().map(Vec::len).sum();
        let mut sent = 0usize;
        for batch in &batches {
            let accepted = send(batch);
            sent += accepted;
            if accepted < batch.len() {
                // A half-sent Shift+Enter batch could leave Shift stuck down.
                send(&[vk_input(VK_SHIFT, true)]);
                return Err(InjectError::Partial { sent, total });
            }
        }
        Ok(())
    }
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
        // Rejected before sending in `run`.
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
    fn paste_is_rejected_before_sending() {
        let err = SendInputInjector
            .run(&[Action::Paste("x".into())])
            .unwrap_err();
        assert_eq!(err, InjectError::Os("paste not implemented (T1.2)".into()));
    }
}
