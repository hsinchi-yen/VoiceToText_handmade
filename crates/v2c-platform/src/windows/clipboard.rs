//! Clipboard save / set / restore for paste injection (T1.2, tasks/plan.md section 4).
//!
//! Uses raw Win32 instead of `arboard` so that *every* memory-backed clipboard format is
//! restored byte-for-byte, not only text and an RGBA re-encoding of the image:
//!
//! - Restored: all `HGLOBAL` formats, i.e. `CF_UNICODETEXT`/`CF_TEXT`/`CF_OEMTEXT`/`CF_LOCALE`,
//!   `CF_DIB`/`CF_DIBV5` (images; `CF_BITMAP` is re-synthesised by Windows from them), `CF_HDROP`
//!   (copied files), and registered formats such as "HTML Format", "Rich Text Format", "PNG".
//! - Not restored: GDI-handle formats (`CF_BITMAP` itself, `CF_PALETTE`, `CF_METAFILEPICT`,
//!   `CF_ENHMETAFILE`, the `CF_DSP*` variants, `CF_OWNERDISPLAY`), `CF_PRIVATEFIRST..LAST` and
//!   `CF_GDIOBJFIRST..LAST`, and the OLE bookkeeping formats "DataObject" / "Ole Private
//!   Data" (OLE rebuilds them). An app that copies *only* an enhanced metafile loses it.
//!   After a restore v2c, not the original app, owns the clipboard.
//! - Size cap: a format over [`MAX_FORMAT_BYTES`], or one that would push the snapshot over
//!   [`MAX_SNAPSHOT_BYTES`], is dropped (e.g. a huge Excel range keeps its text but may lose
//!   its native/bitmap formats), so pasting never stalls for seconds copying memory.
//!   Delayed-rendered data is rendered during the snapshot, so the original owner app no
//!   longer needs to be running for the restore.
//!
//! Text v2c puts on the clipboard is tagged so it stays out of Win+V history, the cloud
//! clipboard and clipboard managers (the transcript must not be persisted elsewhere).

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::System::Ole::{
    CF_BITMAP, CF_DSPBITMAP, CF_DSPENHMETAFILE, CF_DSPMETAFILEPICT, CF_ENHMETAFILE, CF_GDIOBJFIRST,
    CF_GDIOBJLAST, CF_METAFILEPICT, CF_OWNERDISPLAY, CF_PALETTE, CF_PRIVATEFIRST, CF_PRIVATELAST,
    CF_UNICODETEXT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW, TranslateMessage, HWND_MESSAGE,
    MSG, PM_REMOVE,
};

use crate::InjectError;

/// How often to try `OpenClipboard` while another app holds it open.
const OPEN_ATTEMPTS: u32 = 20;
/// Pause between `OpenClipboard` attempts (20 x 10 ms = up to ~200 ms).
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(10);
/// Longest stretch without pumping our owner window's messages while waiting.
const PUMP_SLICE: Duration = Duration::from_millis(5);

/// Contents of the clipboard at one moment: `(format, bytes)` in the clipboard's own
/// enumeration order (which is the owner's order of preference). Empty = clipboard was empty.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Snapshot(pub Vec<(u32, Vec<u8>)>);

impl Snapshot {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Snapshot {
    // Never print clipboard bytes: they may be the user's text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total: usize = self.0.iter().map(|(_, b)| b.len()).sum();
        write!(f, "Snapshot({} formats, {} bytes)", self.0.len(), total)
    }
}

/// Clipboard primitives. All methods except `open`/`sequence_number`/`wait` require the
/// clipboard to be open. A trait so the save/set/restore ordering can be unit-tested.
pub trait ClipboardOps {
    /// Opens the clipboard, retrying briefly while another app holds it.
    fn open(&mut self) -> Result<(), InjectError>;
    fn close(&mut self);
    fn snapshot(&mut self) -> Snapshot;
    /// Empties the clipboard and puts `text` on it (excluded from clipboard history).
    fn set_text(&mut self, text: &str) -> Result<(), InjectError>;
    /// Empties the clipboard and puts the snapshot back (an empty snapshot leaves it empty).
    fn restore(&mut self, snapshot: &Snapshot) -> Result<(), InjectError>;
    /// `GetClipboardSequenceNumber`: changes whenever anyone changes the clipboard.
    fn sequence_number(&self) -> u32;
    /// Waits `d` while staying responsive to clipboard messages (clipboard closed).
    fn wait(&mut self, d: Duration);
}

/// Result of [`paste_with_restore`] when no error occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored,
    /// Someone else changed the clipboard while we waited; their content is kept.
    SkippedChangedByOther,
}

impl RestoreOutcome {
    /// Stable name for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            RestoreOutcome::Restored => "restored",
            RestoreOutcome::SkippedChangedByOther => "skipped_changed_by_other",
        }
    }
}

/// Save clipboard -> set `text` -> `send_paste()` -> wait `settle` -> restore.
///
/// If setting the text fails, the snapshot is put back before the error is returned.
/// If `send_paste` fails the text is deliberately left on the clipboard (the original is
/// not restored) so the user can paste it by hand; its error is returned.
/// Errors after `send_paste` succeeded start with "pasted, but": the text *was* delivered,
/// so callers must not retry the injection.
pub fn paste_with_restore<C: ClipboardOps>(
    cb: &mut C,
    text: &str,
    send_paste: impl FnOnce() -> Result<(), InjectError>,
    settle: Duration,
) -> Result<RestoreOutcome, InjectError> {
    cb.open()?;
    let saved = cb.snapshot();
    if let Err(e) = cb.set_text(text) {
        // EmptyClipboard may already have run: don't lose the user's clipboard.
        let _ = cb.restore(&saved);
        cb.close();
        return Err(e);
    }
    cb.close();
    // Read after closing: closing (format synthesis) must not count as "someone else".
    let ours = cb.sequence_number();

    send_paste()?;
    cb.wait(settle);

    let restore_failed = |e: InjectError| {
        InjectError::Os(format!("pasted, but restoring the clipboard failed: {e}"))
    };
    cb.open().map_err(restore_failed)?;
    let outcome = if cb.sequence_number() != ours {
        Ok(RestoreOutcome::SkippedChangedByOther)
    } else {
        cb.restore(&saved)
            .map(|()| RestoreOutcome::Restored)
            .map_err(restore_failed)
    };
    cb.close();
    outcome
}

/// Puts `text` on the clipboard and leaves it there (elevated-window fallback). If setting
/// fails, the previous content is put back.
pub fn copy_text<C: ClipboardOps>(cb: &mut C, text: &str) -> Result<(), InjectError> {
    cb.open()?;
    let saved = cb.snapshot();
    let r = cb.set_text(text);
    if r.is_err() {
        let _ = cb.restore(&saved);
    }
    cb.close();
    r
}

/// Largest single format kept in a snapshot (bigger ones are dropped, see module docs).
pub const MAX_FORMAT_BYTES: usize = 16 * 1024 * 1024;
/// Largest total snapshot; formats that would exceed it are dropped.
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// Whether a format of `size` bytes still fits after `used` bytes were kept.
fn fits_budget(size: usize, used: usize) -> bool {
    size <= MAX_FORMAT_BYTES && used.saturating_add(size) <= MAX_SNAPSHOT_BYTES
}

/// Clipboard line endings are CRLF: `\n` and lone `\r` become `\r\n`.
pub fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n")
}

/// UTF-16, NUL-terminated, as `CF_UNICODETEXT` bytes (little endian).
fn unicode_text_bytes(text: &str) -> Vec<u8> {
    to_crlf(text)
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Formats whose clipboard handle is not an `HGLOBAL` and therefore cannot be copied as bytes.
fn is_handle_format(format: u32) -> bool {
    let gdi = [
        CF_BITMAP,
        CF_METAFILEPICT,
        CF_PALETTE,
        CF_ENHMETAFILE,
        CF_OWNERDISPLAY,
        CF_DSPBITMAP,
        CF_DSPMETAFILEPICT,
        CF_DSPENHMETAFILE,
    ];
    gdi.iter().any(|&f| u32::from(f) == format)
        || (u32::from(CF_PRIVATEFIRST)..=u32::from(CF_PRIVATELAST)).contains(&format)
        || (u32::from(CF_GDIOBJFIRST)..=u32::from(CF_GDIOBJLAST)).contains(&format)
}

/// Real Windows clipboard. Owns a hidden message-only window as clipboard owner (with a
/// null owner `SetClipboardData` is documented to fail after `EmptyClipboard`).
pub struct WinClipboard {
    hwnd: HWND,
    open: bool,
}

impl WinClipboard {
    pub fn new() -> Result<Self, InjectError> {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: "STATIC" is a system window class; a message-only window needs no
        // message loop for synchronous clipboard use.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(InjectError::Os(
                "could not create clipboard owner window".into(),
            ));
        }
        Ok(Self { hwnd, open: false })
    }

    /// Empties the clipboard (taking ownership) and sets every `(format, bytes)` pair.
    fn replace_all(&mut self, items: &[(u32, Vec<u8>)]) -> Result<(), InjectError> {
        // SAFETY: the clipboard is open by this thread.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(InjectError::Os("EmptyClipboard failed".into()));
        }
        let mut failed = 0usize;
        for (format, bytes) in items {
            if set_bytes(*format, bytes).is_err() {
                failed += 1;
            }
        }
        if failed > 0 {
            return Err(InjectError::Os(format!(
                "{failed} of {} clipboard formats could not be set",
                items.len()
            )));
        }
        Ok(())
    }
}

impl Drop for WinClipboard {
    fn drop(&mut self) {
        self.close();
        // SAFETY: the window was created by this struct on this thread.
        unsafe { DestroyWindow(self.hwnd) };
    }
}

impl WinClipboard {
    /// Handles pending messages for our owner window (e.g. `WM_DESTROYCLIPBOARD` sent by
    /// another app's `EmptyClipboard`, which blocks that app until we answer).
    fn pump(&self) {
        // SAFETY: MSG is plain data; all-zero is a valid value.
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: `msg` is a valid out pointer; only our window's messages are removed.
        while unsafe { PeekMessageW(&mut msg, self.hwnd, 0, 0, PM_REMOVE) } != 0 {
            // SAFETY: `msg` was filled by PeekMessageW.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// Sleeps for `d` in short slices, pumping messages in between.
    fn pumping_sleep(&self, d: Duration) {
        let deadline = Instant::now() + d;
        loop {
            self.pump();
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            std::thread::sleep((deadline - now).min(PUMP_SLICE));
        }
    }
}

impl ClipboardOps for WinClipboard {
    fn open(&mut self) -> Result<(), InjectError> {
        for attempt in 0..OPEN_ATTEMPTS {
            // SAFETY: plain FFI call with our own window handle.
            if unsafe { OpenClipboard(self.hwnd) } != 0 {
                self.open = true;
                return Ok(());
            }
            if attempt + 1 < OPEN_ATTEMPTS {
                self.pumping_sleep(OPEN_RETRY_DELAY);
            }
        }
        Err(InjectError::Os(
            "clipboard is held open by another application".into(),
        ))
    }

    fn close(&mut self) {
        if self.open {
            // SAFETY: the clipboard was opened by this thread.
            unsafe { CloseClipboard() };
            self.open = false;
        }
    }

    fn snapshot(&mut self) -> Snapshot {
        // OLE bookkeeping formats describe the *previous* owner's live data object; copied
        // back under our window they would be stale, so OLE rebuilds them from the rest.
        let ole_internal = [
            register_format("DataObject"),
            register_format("Ole Private Data"),
        ];
        let mut items = Vec::new();
        let mut used = 0usize;
        let mut format = 0u32;
        loop {
            // SAFETY: the clipboard is open; 0 starts / ends the enumeration.
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            if is_handle_format(format) || ole_internal.contains(&format) {
                continue;
            }
            if let Some(bytes) = read_bytes(format, used) {
                used += bytes.len();
                items.push((format, bytes));
            }
        }
        Snapshot(items)
    }

    fn set_text(&mut self, text: &str) -> Result<(), InjectError> {
        let mut items = vec![(u32::from(CF_UNICODETEXT), unicode_text_bytes(text))];
        items.extend(history_exclusion_formats());
        self.replace_all(&items)
    }

    fn restore(&mut self, snapshot: &Snapshot) -> Result<(), InjectError> {
        self.replace_all(&snapshot.0)
    }

    fn sequence_number(&self) -> u32 {
        // SAFETY: plain FFI call.
        unsafe { GetClipboardSequenceNumber() }
    }

    fn wait(&mut self, d: Duration) {
        self.pumping_sleep(d);
    }
}

/// Registered formats that keep our text out of Win+V history, cloud clipboard and
/// clipboard monitors. See "Cloud Clipboard and Clipboard History Formats" in the Win32
/// clipboard-formats documentation.
fn history_exclusion_formats() -> Vec<(u32, Vec<u8>)> {
    let zero_dword = 0u32.to_le_bytes().to_vec();
    [
        ("ExcludeClipboardContentFromMonitorProcessing", vec![0u8]),
        ("CanIncludeInClipboardHistory", zero_dword.clone()),
        ("CanUploadToCloudClipboard", zero_dword),
    ]
    .into_iter()
    .filter_map(|(name, data)| {
        let id = register_format(name);
        (id != 0).then_some((id, data))
    })
    .collect()
}

/// Id of a registered clipboard format (registering it if needed); 0 on failure.
fn register_format(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated.
    unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
}

/// Copies one `HGLOBAL` clipboard format. `None` if absent, not memory-backed, or over
/// the size budget given `used` bytes already kept.
fn read_bytes(format: u32, used: usize) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open; the returned handle belongs to the clipboard and is
    // only read (locked / unlocked), never freed.
    unsafe {
        let handle: HANDLE = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }
        let size = GlobalSize(handle);
        if size == 0 || !fits_budget(size, used) {
            return None;
        }
        let ptr = GlobalLock(handle).cast::<u8>();
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        GlobalUnlock(handle);
        Some(bytes)
    }
}

/// Puts `bytes` on the open clipboard as `format`.
fn set_bytes(format: u32, bytes: &[u8]) -> Result<(), ()> {
    // SAFETY: we allocate a movable global block of at least `bytes.len()` (min 1) bytes,
    // fill it, and hand it to the clipboard, which owns it on success. On failure we free it.
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if mem.is_null() {
            return Err(());
        }
        let ptr = GlobalLock(mem).cast::<u8>();
        if ptr.is_null() {
            GlobalFree(mem);
            return Err(());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        GlobalUnlock(mem);
        if SetClipboardData(format, mem).is_null() {
            GlobalFree(mem);
            return Err(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct State {
        content: Snapshot,
        seq: u32,
        is_open: bool,
        log: Vec<&'static str>,
        fail_open: bool,
        /// `set_text` empties the clipboard, then fails (like a failing SetClipboardData).
        fail_set: bool,
        /// Another app copies this while we wait.
        copy_during_wait: Option<Snapshot>,
    }

    /// In-memory clipboard; clones share state so the paste / settle closures can inspect it.
    #[derive(Clone, Default)]
    struct Fake(Rc<RefCell<State>>);

    impl Fake {
        fn with(content: Snapshot) -> Self {
            let f = Fake::default();
            f.0.borrow_mut().content = content;
            f
        }
        fn content(&self) -> Snapshot {
            self.0.borrow().content.clone()
        }
        fn log(&self) -> Vec<&'static str> {
            self.0.borrow().log.clone()
        }
        fn is_open(&self) -> bool {
            self.0.borrow().is_open
        }
    }

    impl ClipboardOps for Fake {
        fn open(&mut self) -> Result<(), InjectError> {
            let mut st = self.0.borrow_mut();
            st.log.push("open");
            if st.fail_open {
                return Err(InjectError::Os("busy".into()));
            }
            assert!(!st.is_open, "double open");
            st.is_open = true;
            Ok(())
        }
        fn close(&mut self) {
            let mut st = self.0.borrow_mut();
            st.log.push("close");
            st.is_open = false;
        }
        fn snapshot(&mut self) -> Snapshot {
            let mut st = self.0.borrow_mut();
            assert!(st.is_open);
            st.log.push("snapshot");
            st.content.clone()
        }
        fn set_text(&mut self, text: &str) -> Result<(), InjectError> {
            let mut st = self.0.borrow_mut();
            assert!(st.is_open);
            st.log.push("set_text");
            st.seq += 1;
            if st.fail_set {
                st.content = Snapshot::default();
                return Err(InjectError::Os("SetClipboardData failed".into()));
            }
            st.content = text_snapshot(text);
            Ok(())
        }
        fn restore(&mut self, s: &Snapshot) -> Result<(), InjectError> {
            let mut st = self.0.borrow_mut();
            assert!(st.is_open);
            st.log.push("restore");
            st.content = s.clone();
            st.seq += 1;
            Ok(())
        }
        fn sequence_number(&self) -> u32 {
            self.0.borrow().seq
        }
        fn wait(&mut self, d: Duration) {
            let mut st = self.0.borrow_mut();
            assert!(!st.is_open, "must not hold the clipboard while waiting");
            assert_eq!(d, SETTLE);
            st.log.push("wait");
            if let Some(s) = st.copy_during_wait.take() {
                st.content = s;
                st.seq += 1;
            }
        }
    }

    const SETTLE: Duration = Duration::from_millis(150);

    fn text_snapshot(text: &str) -> Snapshot {
        Snapshot(vec![(u32::from(CF_UNICODETEXT), unicode_text_bytes(text))])
    }

    /// Text + image, as e.g. Office puts on the clipboard.
    fn original() -> Snapshot {
        Snapshot(vec![
            (13, vec![0x41, 0, 0, 0]),
            (8, vec![1, 2, 3]),
            (17, vec![4, 5]),
        ])
    }

    #[test]
    fn saves_sets_pastes_then_restores_in_order() {
        let mut cb = Fake::with(original());
        let probe = cb.clone();
        let out = paste_with_restore(
            &mut cb,
            "嗨\nhi",
            || {
                assert!(
                    !probe.is_open(),
                    "target app must be able to open the clipboard"
                );
                assert_eq!(probe.content(), text_snapshot("嗨\r\nhi"));
                Ok(())
            },
            SETTLE,
        );
        assert_eq!(out, Ok(RestoreOutcome::Restored));
        assert_eq!(cb.content(), original());
        assert_eq!(
            cb.log(),
            ["open", "snapshot", "set_text", "close", "wait", "open", "restore", "close"]
        );
        assert!(!cb.is_open());
    }

    #[test]
    fn empty_clipboard_is_restored_as_empty() {
        let mut cb = Fake::default();
        let out = paste_with_restore(&mut cb, "x", || Ok(()), SETTLE);
        assert_eq!(out, Ok(RestoreOutcome::Restored));
        assert!(cb.content().is_empty());
    }

    #[test]
    fn failed_set_puts_the_original_back() {
        let mut cb = Fake::with(original());
        cb.0.borrow_mut().fail_set = true;
        let mut pasted = false;
        let out = paste_with_restore(
            &mut cb,
            "x",
            || {
                pasted = true;
                Ok(())
            },
            SETTLE,
        );
        assert_eq!(out, Err(InjectError::Os("SetClipboardData failed".into())));
        assert!(!pasted);
        assert_eq!(cb.content(), original());
        assert_eq!(
            cb.log(),
            ["open", "snapshot", "set_text", "restore", "close"]
        );

        let mut cb = Fake::with(original());
        cb.0.borrow_mut().fail_set = true;
        assert!(copy_text(&mut cb, "x").is_err());
        assert_eq!(cb.content(), original());
        assert!(!cb.is_open());
    }

    #[test]
    fn failed_paste_leaves_text_and_skips_restore() {
        let mut cb = Fake::with(original());
        let out = paste_with_restore(
            &mut cb,
            "keep me",
            || Err(InjectError::Os("ctrl+v rejected".into())),
            SETTLE,
        );
        assert_eq!(out, Err(InjectError::Os("ctrl+v rejected".into())));
        assert_eq!(cb.content(), text_snapshot("keep me"));
        assert!(!cb.log().contains(&"wait"));
        assert!(!cb.log().contains(&"restore"));
        assert!(!cb.is_open());
    }

    #[test]
    fn newer_clipboard_content_is_not_overwritten() {
        let mut cb = Fake::with(original());
        let newer = Snapshot(vec![(13, vec![0x42, 0, 0, 0])]);
        cb.0.borrow_mut().copy_during_wait = Some(newer.clone());
        let out = paste_with_restore(&mut cb, "x", || Ok(()), SETTLE);
        assert_eq!(out, Ok(RestoreOutcome::SkippedChangedByOther));
        assert_eq!(cb.content(), newer);
        assert!(!cb.is_open());
    }

    #[test]
    fn size_budget() {
        assert!(fits_budget(10, 0));
        assert!(fits_budget(MAX_FORMAT_BYTES, 0));
        assert!(!fits_budget(MAX_FORMAT_BYTES + 1, 0));
        assert!(fits_budget(
            MAX_FORMAT_BYTES,
            MAX_SNAPSHOT_BYTES - MAX_FORMAT_BYTES
        ));
        assert!(!fits_budget(1, MAX_SNAPSHOT_BYTES));
        assert!(!fits_budget(1, usize::MAX));
    }

    #[test]
    fn outcome_names() {
        assert_eq!(RestoreOutcome::Restored.as_str(), "restored");
        assert_eq!(
            RestoreOutcome::SkippedChangedByOther.as_str(),
            "skipped_changed_by_other"
        );
    }

    #[test]
    fn open_failure_is_reported_before_anything_changes() {
        let mut cb = Fake::with(original());
        cb.0.borrow_mut().fail_open = true;
        let mut pasted = false;
        let out = paste_with_restore(
            &mut cb,
            "x",
            || {
                pasted = true;
                Ok(())
            },
            SETTLE,
        );
        assert!(out.is_err());
        assert!(!pasted);
        assert_eq!(cb.content(), original());
    }

    #[test]
    fn copy_text_leaves_text() {
        let mut cb = Fake::with(original());
        copy_text(&mut cb, "a").unwrap();
        assert_eq!(cb.content(), text_snapshot("a"));
        assert_eq!(cb.log(), ["open", "snapshot", "set_text", "close"]);
    }

    #[test]
    fn crlf_conversion() {
        assert_eq!(to_crlf("a\nb\r\nc\rd"), "a\r\nb\r\nc\r\nd");
        assert_eq!(to_crlf("no breaks"), "no breaks");
    }

    #[test]
    fn unicode_bytes_are_nul_terminated_utf16le() {
        assert_eq!(unicode_text_bytes("A"), vec![0x41, 0, 0, 0]);
        // Surrogate pair stays intact: 2 units + NUL = 6 bytes.
        assert_eq!(unicode_text_bytes("👍").len(), 6);
    }

    #[test]
    fn handle_formats_are_skipped() {
        assert!(is_handle_format(2)); // CF_BITMAP
        assert!(is_handle_format(14)); // CF_ENHMETAFILE
        assert!(is_handle_format(0x200)); // private
        assert!(!is_handle_format(13)); // CF_UNICODETEXT
        assert!(!is_handle_format(8)); // CF_DIB
        assert!(!is_handle_format(17)); // CF_DIBV5
        assert!(!is_handle_format(15)); // CF_HDROP
        assert!(!is_handle_format(0xC123)); // registered, e.g. "HTML Format"
    }

    #[test]
    fn snapshot_debug_hides_bytes() {
        let s = format!("{:?}", text_snapshot("祕"));
        assert_eq!(s, "Snapshot(1 formats, 4 bytes)");
    }

    #[test]
    #[ignore = "touches the real clipboard; run manually on a desktop"]
    fn real_clipboard_roundtrip() {
        let mut cb = WinClipboard::new().unwrap();
        cb.open().unwrap();
        let before = cb.snapshot();
        cb.set_text("v2c test").unwrap();
        let during = cb.snapshot();
        cb.restore(&before).unwrap();
        let after = cb.snapshot();
        cb.close();
        assert!(during
            .0
            .iter()
            .any(|(f, b)| *f == 13 && *b == unicode_text_bytes("v2c test")));
        for item in &before.0 {
            assert!(after.0.contains(item), "format {} not restored", item.0);
        }
    }

    #[test]
    #[ignore = "touches the real clipboard; run manually on a desktop"]
    fn real_paste_with_restore_restores() {
        let mut cb = WinClipboard::new().unwrap();
        // A no-op "paste": nobody else touches the clipboard, so it must be restored, i.e.
        // closing the clipboard after set_text must not look like a foreign change.
        let out = paste_with_restore(&mut cb, "v2c test", || Ok(()), SETTLE);
        assert_eq!(out, Ok(RestoreOutcome::Restored));
    }
}
