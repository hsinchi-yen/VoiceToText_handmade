//! Foreground window -> executable name, elevation and fullscreen state (T0.3).

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetDesktopWindow, GetForegroundWindow, GetShellWindow, GetWindowRect, GetWindowThreadProcessId,
};

use crate::{AppInfo, Foreground};

/// [`Foreground`] backed by `GetForegroundWindow`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinForeground;

impl Foreground for WinForeground {
    fn current_app(&self) -> Option<AppInfo> {
        current_app()
    }
}

/// Reads the foreground application. `None` if there is no foreground window or its
/// process cannot be opened (e.g. access denied for a protected process).
pub fn current_app() -> Option<AppInfo> {
    // SAFETY: no arguments; returns null when no window has focus.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `hwnd` came from the OS; `pid` is a valid out pointer.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    // SAFETY: plain FFI call; a null handle signals failure.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let process = OwnedHandle(process);
    let exe = image_file_name(process.0)?;
    let elevated = match token_elevated(process.0) {
        Some(e) => e,
        // From a non-elevated process, being denied the token of a process we *can* open
        // means it runs at a higher integrity level; UIPI would block input anyway.
        None => !current_process_elevated(),
    };
    Some(AppInfo {
        exe,
        elevated,
        fullscreen: is_fullscreen(hwnd),
    })
}

/// Whether this process runs elevated (administrator). `false` if it cannot be determined.
pub fn current_process_elevated() -> bool {
    // SAFETY: returns a pseudo-handle that must not be closed.
    token_elevated(unsafe { GetCurrentProcess() }).unwrap_or(false)
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned open by the OS and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

fn image_file_name(process: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16s; on success `len` is set to the written length.
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
    };
    if ok == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    Some(file_name(&path).to_owned())
}

/// Last component of a Windows path.
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn token_elevated(process: HANDLE) -> Option<bool> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: `token` is a valid out pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut ret_len = 0u32;
    // SAFETY: the buffer is a TOKEN_ELEVATION of the size passed.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
    };
    (ok != 0).then_some(elevation.TokenIsElevated != 0)
}

fn is_fullscreen(hwnd: HWND) -> bool {
    // The desktop and shell windows always cover the monitor but are not "fullscreen apps".
    // SAFETY: plain FFI calls without arguments.
    if hwnd == unsafe { GetDesktopWindow() } || hwnd == unsafe { GetShellWindow() } {
        return false;
    }
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `rect` is a valid out pointer.
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return false;
    }
    // SAFETY: plain FFI call; returns the nearest monitor.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return false;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        rcMonitor: rect,
        rcWork: rect,
        dwFlags: 0,
    };
    // SAFETY: `info.cbSize` is set as the API requires.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return false;
    }
    covers(&rect, &info.rcMonitor)
}

fn covers(window: &RECT, monitor: &RECT) -> bool {
    window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// A top-level window handle that can be kept across threads (T1.9 re-output target).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowId(isize);

impl WindowId {
    fn hwnd(self) -> HWND {
        self.0 as HWND
    }

    /// Raw value (for storing in an atomic); `0` is never a valid window.
    pub fn as_raw(self) -> isize {
        self.0
    }

    pub fn from_raw(raw: isize) -> Option<Self> {
        (raw != 0).then_some(Self(raw))
    }
}

/// The foreground window, if any.
pub fn foreground_window() -> Option<WindowId> {
    // SAFETY: no arguments; returns null when no window has focus.
    let hwnd = unsafe { GetForegroundWindow() };
    WindowId::from_raw(hwnd as isize)
}

/// A window that can receive typed text: it still exists, belongs to another process, and
/// is not the desktop, the shell (Progman/WorkerW) or a taskbar.
pub fn is_text_target(w: WindowId) -> bool {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;

    // SAFETY: plain FFI calls; an invalid handle makes IsWindow return 0.
    if unsafe { IsWindow(w.hwnd()) } == 0 {
        return false;
    }
    // SAFETY: plain FFI calls without arguments.
    if w.hwnd() == unsafe { GetDesktopWindow() } || w.hwnd() == unsafe { GetShellWindow() } {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: valid window handle and out pointer.
    unsafe { GetWindowThreadProcessId(w.hwnd(), &mut pid) };
    // SAFETY: no arguments.
    if pid == 0 || pid == unsafe { GetCurrentProcessId() } {
        return false;
    }
    !is_shell_class(&class_name(w).unwrap_or_default())
}

/// Shell window classes that are never a text target.
fn is_shell_class(class: &str) -> bool {
    matches!(
        class,
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    )
}

fn class_name(w: WindowId) -> Option<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;

    let mut buf = [0u16; 256];
    // SAFETY: `buf` holds `buf.len()` u16s.
    let n = unsafe { GetClassNameW(w.hwnd(), buf.as_mut_ptr(), buf.len() as i32) };
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// Asks Windows to bring `w` to the foreground. Only succeeds while this process may set
/// the foreground (e.g. it owns the current foreground window); check with
/// [`foreground_window`] afterwards.
pub fn activate_window(w: WindowId) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
    // SAFETY: plain FFI call; an invalid handle just fails.
    unsafe { SetForegroundWindow(w.hwnd()) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn file_name_takes_last_component() {
        assert_eq!(
            file_name(r"C:\Users\me\AppData\Local\LINE\bin\LINE.exe"),
            "LINE.exe"
        );
        assert_eq!(file_name("notepad.exe"), "notepad.exe");
    }

    #[test]
    fn covers_detects_fullscreen() {
        let mon = r(0, 0, 1920, 1080);
        assert!(covers(&r(0, 0, 1920, 1080), &mon));
        // Borderless windows sometimes overhang the monitor slightly.
        assert!(covers(&r(-8, -8, 1928, 1088), &mon));
        // Maximised normal window leaves the taskbar visible.
        assert!(!covers(&r(0, 0, 1920, 1040), &mon));
        // Second monitor to the right.
        assert!(covers(&r(1920, 0, 3840, 1080), &r(1920, 0, 3840, 1080)));
    }

    #[test]
    fn shell_classes_are_not_targets() {
        for c in [
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
        ] {
            assert!(is_shell_class(c), "{c}");
        }
        assert!(!is_shell_class("Notepad"));
        assert_eq!(WindowId::from_raw(0), None);
        // SAFETY: plain FFI call without arguments.
        if let Some(shell) = WindowId::from_raw(unsafe { GetShellWindow() } as isize) {
            assert!(!is_text_target(shell));
        }
    }

    #[test]
    fn current_app_does_not_panic() {
        // May be None on CI / without a desktop; must never panic.
        let _ = current_app();
        let _ = current_process_elevated();
    }
}
