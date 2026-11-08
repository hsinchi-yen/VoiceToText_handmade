//! `WH_KEYBOARD_LL` low-level keyboard hook (T0.2).
//!
//! The hook runs on a dedicated thread with its own message loop. The callback only maps the
//! key to a [`Key`] class, stamps it with [`monotonic_ms`], hands it to the sink, and always
//! calls `CallNextHookEx`: keys are never swallowed, so other programs' Ctrl shortcuts keep
//! working. Key identities other than the classes below never leave this module.

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{null, null_mut};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use v2c_core::trigger::{Event, Key};
use windows_sys::Win32::Foundation::{GetLastError, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_ESCAPE, VK_LSHIFT, VK_RCONTROL, VK_RSHIFT, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER,
};

use crate::{Hotkey, PlatformError};

type Sink = Box<dyn Fn(Event) + Send + 'static>;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Monotonic milliseconds since the first call in this process. Hook events use this clock,
/// so callers generating `Event::Tick` must use it too.
pub fn monotonic_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Maps a virtual-key code to the class the trigger state machine cares about.
/// Low-level hooks report left/right modifiers separately (`VK_RCONTROL`, `VK_LSHIFT`, ...).
pub(crate) fn classify(vk: u32) -> Key {
    match vk {
        v if v == u32::from(VK_RCONTROL) => Key::Trigger,
        v if v == u32::from(VK_LSHIFT) || v == u32::from(VK_RSHIFT) || v == u32::from(VK_SHIFT) => {
            Key::Shift
        }
        v if v == u32::from(VK_ESCAPE) => Key::Escape,
        _ => Key::Other,
    }
}

/// Tracks which virtual keys are down so auto-repeat key-downs are dropped, and folds both
/// Shift keys into one `Key::Shift` press.
pub(crate) struct KeyTracker {
    down: [bool; 256],
}

impl KeyTracker {
    pub(crate) fn new() -> Self {
        Self { down: [false; 256] }
    }

    fn any_shift_down(&self) -> bool {
        [VK_LSHIFT, VK_RSHIFT, VK_SHIFT]
            .iter()
            .any(|&v| self.down[usize::from(v)])
    }

    /// Returns the event to emit for a key-down, or `None` for an auto-repeat.
    /// `still_down` is consulted only when the key is already marked down; it reports whether
    /// the OS still considers the key held (recovers from a key-up the hook never saw).
    pub(crate) fn key_down(
        &mut self,
        vk: u32,
        at_ms: u64,
        still_down: impl FnOnce() -> bool,
    ) -> Option<Event> {
        let key = classify(vk);
        let idx = (vk & 0xff) as usize;
        if self.down[idx] && still_down() {
            return None; // auto-repeat
        }
        self.down[idx] = false; // a stale flag for this key must not count as "other Shift"
        let shift_was_down = self.any_shift_down();
        self.down[idx] = true;
        if key == Key::Shift && shift_was_down {
            return None; // second Shift key while the first is held
        }
        Some(Event::KeyDown { key, at_ms })
    }

    /// Returns the event to emit for a key-up.
    pub(crate) fn key_up(&mut self, vk: u32, at_ms: u64) -> Option<Event> {
        let key = classify(vk);
        self.down[(vk & 0xff) as usize] = false;
        if key == Key::Shift && self.any_shift_down() {
            return None; // the other Shift key is still held
        }
        Some(Event::KeyUp { key, at_ms })
    }
}

struct HookState {
    sink: Sink,
    keys: KeyTracker,
}

thread_local! {
    // Only touched on the hook thread: the OS calls the hook procedure on the thread that
    // installed it, from inside its message loop.
    static HOOK_STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam != 0 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points to a KBDLLHOOKSTRUCT.
        let vk = unsafe { (*(lparam as *const KBDLLHOOKSTRUCT)).vkCode };
        let msg = wparam as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if is_down || is_up {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                HOOK_STATE.with(|cell| {
                    let Ok(mut guard) = cell.try_borrow_mut() else {
                        return;
                    };
                    let Some(state) = guard.as_mut() else {
                        return;
                    };
                    let now = monotonic_ms();
                    let event = if is_down {
                        // SAFETY: plain Win32 call with a valid virtual-key code. In an LL
                        // hook the async state is not yet updated for this event, so it
                        // reports whether the key was already held before it.
                        let held = || unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0;
                        state.keys.key_down(vk, now, held)
                    } else {
                        state.keys.key_up(vk, now)
                    };
                    if let Some(event) = event {
                        (state.sink)(event);
                    }
                });
            }));
        }
    }
    // Never swallow keys.
    // SAFETY: forwarding the unmodified arguments; the hook handle parameter is ignored.
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn hook_thread(sink: Sink, ready: mpsc::Sender<Result<u32, String>>) {
    // SAFETY: plain Win32 calls on this thread; `msg` is a valid out-pointer.
    unsafe {
        let thread_id = GetCurrentThreadId();
        let mut msg: MSG = std::mem::zeroed();
        // Force creation of this thread's message queue so `stop()` can post WM_QUIT.
        PeekMessageW(&mut msg, null_mut(), WM_USER, WM_USER, PM_NOREMOVE);

        HOOK_STATE.with(|cell| {
            *cell.borrow_mut() = Some(HookState {
                sink,
                keys: KeyTracker::new(),
            })
        });

        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), GetModuleHandleW(null()), 0);
        if hook.is_null() {
            let err = GetLastError();
            HOOK_STATE.with(|cell| *cell.borrow_mut() = None);
            let _ = ready.send(Err(format!("SetWindowsHookExW failed (error {err})")));
            return;
        }
        let _ = ready.send(Ok(thread_id));

        loop {
            let r = GetMessageW(&mut msg, null_mut(), 0, 0);
            if r == 0 || r == -1 {
                break; // WM_QUIT or error
            }
            DispatchMessageW(&msg);
        }

        UnhookWindowsHookEx(hook);
        HOOK_STATE.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Windows global hotkey hook. Create with [`WindowsHotkey::new`], then [`Hotkey::start`].
#[derive(Default)]
pub struct WindowsHotkey {
    thread: Option<(JoinHandle<()>, u32)>,
}

impl WindowsHotkey {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Hotkey for WindowsHotkey {
    fn start(&mut self, sink: Box<dyn Fn(Event) + Send + 'static>) -> Result<(), PlatformError> {
        if self.thread.is_some() {
            return Err(PlatformError::Os("hotkey hook already started".into()));
        }
        monotonic_ms(); // pin the epoch before the first event
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("v2c-hotkey".into())
            .spawn(move || hook_thread(sink, tx))
            .map_err(|e| PlatformError::Os(format!("spawn hook thread: {e}")))?;
        match rx.recv() {
            Ok(Ok(thread_id)) => {
                self.thread = Some((handle, thread_id));
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(PlatformError::Os(e))
            }
            Err(_) => {
                let _ = handle.join();
                Err(PlatformError::Os("hook thread exited during start".into()))
            }
        }
    }

    fn stop(&mut self) {
        if let Some((handle, thread_id)) = self.thread.take() {
            // SAFETY: plain Win32 call; the queue exists because the thread created it before
            // reporting ready.
            unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
            let _ = handle.join();
        }
    }
}

impl Drop for WindowsHotkey {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VK_C: u32 = 0x43;
    const VK_LCONTROL: u32 = 0xA2;

    fn vk(v: u16) -> u32 {
        u32::from(v)
    }

    #[test]
    fn classifies_keys() {
        assert_eq!(classify(vk(VK_RCONTROL)), Key::Trigger);
        assert_eq!(classify(vk(VK_LSHIFT)), Key::Shift);
        assert_eq!(classify(vk(VK_RSHIFT)), Key::Shift);
        assert_eq!(classify(vk(VK_ESCAPE)), Key::Escape);
        assert_eq!(classify(VK_LCONTROL), Key::Other);
        assert_eq!(classify(VK_C), Key::Other);
    }

    #[test]
    fn auto_repeat_is_dropped() {
        let mut t = KeyTracker::new();
        let rc = vk(VK_RCONTROL);
        assert_eq!(
            t.key_down(rc, 1, || true),
            Some(Event::KeyDown {
                key: Key::Trigger,
                at_ms: 1
            })
        );
        assert_eq!(t.key_down(rc, 2, || true), None);
        assert_eq!(
            t.key_up(rc, 3),
            Some(Event::KeyUp {
                key: Key::Trigger,
                at_ms: 3
            })
        );
        assert!(t.key_down(rc, 4, || true).is_some());
    }

    #[test]
    fn missed_key_up_recovers_when_os_says_released() {
        let mut t = KeyTracker::new();
        let rc = vk(VK_RCONTROL);
        t.key_down(rc, 1, || true);
        // Key-up never reached the hook; the OS reports the key as not held.
        assert!(t.key_down(rc, 50, || false).is_some());
    }

    #[test]
    fn both_shift_keys_fold_into_one_press() {
        let mut t = KeyTracker::new();
        assert!(t.key_down(vk(VK_LSHIFT), 1, || true).is_some());
        assert_eq!(t.key_down(vk(VK_RSHIFT), 2, || true), None);
        assert_eq!(t.key_up(vk(VK_LSHIFT), 3), None);
        assert_eq!(
            t.key_up(vk(VK_RSHIFT), 4),
            Some(Event::KeyUp {
                key: Key::Shift,
                at_ms: 4
            })
        );
    }

    #[test]
    fn start_and_stop_cleanly() {
        let mut h = WindowsHotkey::new();
        h.start(Box::new(|_| {})).expect("hook installs");
        assert!(h.start(Box::new(|_| {})).is_err());
        h.stop();
        h.stop(); // idempotent
        h.start(Box::new(|_| {})).expect("restart works");
    }
}
