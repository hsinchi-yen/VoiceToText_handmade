//! Global hotkey on Linux (T3.1, `tasks/plan.md` section 9.2).
//!
//! Backend order: GlobalShortcuts portal (GNOME >= 48) → evdev (`/dev/input/event*`, needs
//! the `input` group) → [`HotkeyBackend::Unavailable`] with the reasons. The choice
//! ([`select_backend`]) and the key-to-[`Event`] conversions are pure functions; the OS part
//! is only reading devices / D-Bus signals and stamping events with [`monotonic_ms`].
//!
//! Keys are never grabbed (no `EVIOCGRAB`), so other programs keep receiving them. Key
//! identities other than the [`Key`] classes never leave this module, and nothing is logged.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::raw::c_long;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use v2c_core::trigger::{Event, Key};

use super::caps::{
    access_error, DeviceAccess, LinuxCaps, Portal, GLOBAL_SHORTCUTS_IFACE, KEY_ESC, KEY_LEFTSHIFT,
    KEY_RIGHTCTRL, KEY_RIGHTSHIFT, PORTAL_DEST, PORTAL_PATH,
};
use crate::{Hotkey, PlatformError};

type Sink = Box<dyn Fn(Event) + Send + 'static>;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Monotonic milliseconds since the first call in this process. Hotkey events use this clock,
/// so callers generating `Event::Tick` must use it too.
pub fn monotonic_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

// ---------------------------------------------------------------------------------------
// Backend selection (pure)
// ---------------------------------------------------------------------------------------

/// Why no hotkey backend can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnavailableReason {
    /// The GlobalShortcuts portal is not exported (GNOME < 48), or the bus was unreachable.
    NoGlobalShortcutsPortal,
    /// Opening an input device failed with `EACCES`; the user must join the `input` group.
    InputPermissionDenied { path: PathBuf },
    /// No device reports Right Ctrl + Esc (no keyboard attached).
    NoKeyboard,
    /// Any other evdev error.
    Evdev(String),
}

impl std::fmt::Display for UnavailableReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGlobalShortcutsPortal => {
                f.write_str("the GlobalShortcuts portal is missing (needs GNOME 48 or newer)")
            }
            Self::InputPermissionDenied { path } => write!(
                f,
                "permission denied opening {}: add your user to the `input` group \
                 (sudo usermod -aG input $USER), then log out and back in",
                path.display()
            ),
            Self::NoKeyboard => f.write_str("no keyboard device found under /dev/input"),
            Self::Evdev(e) => write!(f, "evdev: {e}"),
        }
    }
}

/// The hotkey mechanism [`select_backend`] picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyBackend {
    GlobalShortcuts {
        version: Option<u32>,
    },
    Evdev {
        devices: Vec<PathBuf>,
    },
    /// Nothing works; never fail silently — show these reasons to the user.
    Unavailable(Vec<UnavailableReason>),
}

/// Picks the hotkey backend: GlobalShortcuts → evdev → `Unavailable(reasons)`.
pub fn select_backend(caps: &LinuxCaps) -> HotkeyBackend {
    if let Portal::Present { version } = caps.global_shortcuts {
        return HotkeyBackend::GlobalShortcuts { version };
    }
    let mut reasons = vec![UnavailableReason::NoGlobalShortcutsPortal];
    match &caps.evdev.access {
        DeviceAccess::Readable if !caps.evdev.keyboards.is_empty() => {
            return HotkeyBackend::Evdev {
                devices: caps.evdev.keyboards.clone(),
            };
        }
        DeviceAccess::Readable | DeviceAccess::NoDevices => {}
        DeviceAccess::PermissionDenied { path } => {
            reasons.push(UnavailableReason::InputPermissionDenied { path: path.clone() });
        }
        DeviceAccess::Error(e) => reasons.push(UnavailableReason::Evdev(e.clone())),
    }
    if caps.evdev.keyboards.is_empty() {
        reasons.push(UnavailableReason::NoKeyboard);
    }
    HotkeyBackend::Unavailable(reasons)
}

// ---------------------------------------------------------------------------------------
// evdev conversion (pure)
// ---------------------------------------------------------------------------------------

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
/// `KEY_MAX` + 1.
const KEY_CNT: usize = 0x300;

/// Size of `struct input_event`: `struct timeval` (two `long`s) + type, code (u16) + value (i32).
pub(crate) const INPUT_EVENT_SIZE: usize = 2 * std::mem::size_of::<c_long>() + 8;

/// One `struct input_event` without its timestamp (events are stamped with [`monotonic_ms`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawEvent {
    pub type_: u16,
    pub code: u16,
    /// For `EV_KEY`: 0 = release, 1 = press, 2 = auto-repeat.
    pub value: i32,
}

impl RawEvent {
    pub(crate) fn decode(buf: &[u8; INPUT_EVENT_SIZE]) -> Self {
        let o = 2 * std::mem::size_of::<c_long>();
        Self {
            type_: u16::from_ne_bytes([buf[o], buf[o + 1]]),
            code: u16::from_ne_bytes([buf[o + 2], buf[o + 3]]),
            value: i32::from_ne_bytes([buf[o + 4], buf[o + 5], buf[o + 6], buf[o + 7]]),
        }
    }
}

/// Maps an evdev key code to the class the trigger state machine cares about.
pub(crate) fn classify(code: u16) -> Key {
    match code {
        KEY_RIGHTCTRL => Key::Trigger,
        KEY_LEFTSHIFT | KEY_RIGHTSHIFT => Key::Shift,
        KEY_ESC => Key::Escape,
        _ => Key::Other,
    }
}

/// Turns raw evdev events into trigger [`Event`]s: drops auto-repeat (value 2) and repeated
/// presses, folds both Shift keys into one `Key::Shift`, and on `SYN_DROPPED` (the kernel
/// buffer overflowed, so key-ups may have been lost) releases every held key and discards
/// the incomplete packet up to the next `SYN_REPORT`, as the evdev protocol requires.
/// Shared by all keyboards, so a key held on one and released on another still pairs up.
pub(crate) struct EvdevTracker {
    down: Box<[bool; KEY_CNT]>,
    /// Set by `SYN_DROPPED`; events are discarded until the next `SYN_REPORT`.
    dropping: bool,
}

impl EvdevTracker {
    pub(crate) fn new() -> Self {
        Self {
            down: Box::new([false; KEY_CNT]),
            dropping: false,
        }
    }

    fn any_shift_down(&self) -> bool {
        self.down[usize::from(KEY_LEFTSHIFT)] || self.down[usize::from(KEY_RIGHTSHIFT)]
    }

    pub(crate) fn feed(&mut self, ev: RawEvent, at_ms: u64) -> Vec<Event> {
        if self.dropping {
            if (ev.type_, ev.code) == (EV_SYN, SYN_REPORT) {
                self.dropping = false;
            }
            return Vec::new();
        }
        match (ev.type_, ev.code) {
            (EV_SYN, SYN_DROPPED) => {
                self.dropping = true;
                self.release_all(at_ms)
            }
            (EV_KEY, code) if usize::from(code) < KEY_CNT => match ev.value {
                1 => self.key_down(code, at_ms).into_iter().collect(),
                0 => self.key_up(code, at_ms).into_iter().collect(),
                _ => Vec::new(), // 2 = auto-repeat
            },
            _ => Vec::new(),
        }
    }

    fn key_down(&mut self, code: u16, at_ms: u64) -> Option<Event> {
        let idx = usize::from(code);
        if self.down[idx] {
            return None;
        }
        let key = classify(code);
        let shift_was_down = self.any_shift_down();
        self.down[idx] = true;
        if key == Key::Shift && shift_was_down {
            return None; // second Shift key while the first is held
        }
        Some(Event::KeyDown { key, at_ms })
    }

    fn key_up(&mut self, code: u16, at_ms: u64) -> Option<Event> {
        let idx = usize::from(code);
        if !self.down[idx] {
            return None; // released before we started, or already released by SYN_DROPPED
        }
        self.down[idx] = false;
        let key = classify(code);
        if key == Key::Shift && self.any_shift_down() {
            return None; // the other Shift key is still held
        }
        Some(Event::KeyUp { key, at_ms })
    }

    fn release_all(&mut self, at_ms: u64) -> Vec<Event> {
        (0..KEY_CNT as u16)
            .filter(|&c| self.down[usize::from(c)])
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|c| self.key_up(c, at_ms))
            .collect()
    }
}

/// Opens an input device read-only; `EACCES` becomes [`PlatformError::InputPermission`].
pub(crate) fn open_device(path: &Path) -> Result<File, PlatformError> {
    File::open(path).map_err(|e| open_error(path, &e))
}

pub(crate) fn open_error(path: &Path, e: &io::Error) -> PlatformError {
    match access_error(path, e) {
        DeviceAccess::PermissionDenied { path } => PlatformError::InputPermission { path },
        DeviceAccess::Error(msg) => PlatformError::Os(msg),
        other => PlatformError::Os(format!("{other:?}")),
    }
}

// ---------------------------------------------------------------------------------------
// GlobalShortcuts conversion (pure)
// ---------------------------------------------------------------------------------------

/// Shortcut ids registered with the GlobalShortcuts portal. The portal only reports these
/// shortcuts, not individual keys, so Shift and Esc get their own shortcuts, and
/// "Trigger + other key" (Ctrl+C cancel) cannot be detected with this backend.
pub const SHORTCUT_DICTATE: &str = "dictate";
pub const SHORTCUT_TRANSLATE: &str = "translate";
/// Whatever key the user binds to `cancel` is grabbed by the compositor system-wide while
/// the session is open (other programs no longer receive it), so the UI must not suggest
/// Esc; recommend an otherwise unused combination.
pub const SHORTCUT_CANCEL: &str = "cancel";

/// `Activated` / `Deactivated` signals of `org.freedesktop.portal.GlobalShortcuts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSignal {
    Activated,
    Deactivated,
}

/// Converts a portal signal for shortcut `id` into trigger events. Unknown ids yield nothing.
pub fn portal_events(id: &str, signal: PortalSignal, at_ms: u64) -> Vec<Event> {
    use PortalSignal::{Activated, Deactivated};
    let down = |key| Event::KeyDown { key, at_ms };
    let up = |key| Event::KeyUp { key, at_ms };
    match (id, signal) {
        (SHORTCUT_DICTATE, Activated) => vec![down(Key::Trigger)],
        (SHORTCUT_DICTATE, Deactivated) => vec![up(Key::Trigger)],
        (SHORTCUT_TRANSLATE, Activated) => vec![down(Key::Shift), down(Key::Trigger)],
        (SHORTCUT_TRANSLATE, Deactivated) => vec![up(Key::Trigger), up(Key::Shift)],
        (SHORTCUT_CANCEL, Activated) => vec![down(Key::Escape)],
        (SHORTCUT_CANCEL, Deactivated) => vec![up(Key::Escape)],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------------------
// Hotkey implementation
// ---------------------------------------------------------------------------------------

/// [`Hotkey`] for Linux, running the backend chosen by [`select_backend`].
pub struct LinuxHotkey {
    backend: HotkeyBackend,
    running: Option<Running>,
}

/// A started backend. [`Running::shutdown`] is the only way to end it.
struct Running {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    /// evdev reader threads; each exits within [`POLL_TIMEOUT_MS`] of `stop`, so they are joined.
    threads: Vec<JoinHandle<()>>,
    /// GlobalShortcuts session to close on stop.
    portal: Option<PortalSession>,
}

struct PortalSession {
    conn: zbus::blocking::Connection,
    session: String,
}

impl Running {
    fn new(sink: Sink) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                tracker: EvdevTracker::new(),
                sink: Some(sink),
            })),
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
            portal: None,
        }
    }

    /// Drops the sink under the lock (so no thread can call it after this returns), closes
    /// the portal session, and joins the evdev readers (closing their device files).
    fn shutdown(self) {
        self.stop.store(true, Ordering::SeqCst);
        let sink = self
            .shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .sink
            .take();
        drop(sink);
        if let Some(p) = self.portal {
            close_session(&p.conn, &p.session);
            // Ends the signal stream so the portal thread wakes up and exits.
            let _ = p.conn.close();
        }
        for t in self.threads {
            let _ = t.join();
        }
    }
}

impl LinuxHotkey {
    pub fn new(backend: HotkeyBackend) -> Self {
        Self {
            backend,
            running: None,
        }
    }

    /// Probes the system and picks the backend.
    pub fn detect() -> Self {
        Self::new(select_backend(&super::caps::probe()))
    }

    pub fn backend(&self) -> &HotkeyBackend {
        &self.backend
    }
}

impl Hotkey for LinuxHotkey {
    /// Evdev: opens every keyboard (failing with [`PlatformError::InputPermission`] if none
    /// can be opened) and reads each on its own thread.
    /// GlobalShortcuts: creates a portal session and binds the shortcuts; this blocks until
    /// the user answers GNOME's shortcut dialog.
    fn start(&mut self, sink: Sink) -> Result<(), PlatformError> {
        if self.running.is_some() {
            return Err(PlatformError::Os("hotkey already started".into()));
        }
        let running = match &self.backend {
            HotkeyBackend::Unavailable(reasons) => {
                let text: Vec<String> = reasons.iter().map(ToString::to_string).collect();
                return Err(PlatformError::HotkeyUnavailable(text.join("; ")));
            }
            HotkeyBackend::Evdev { devices } => spawn_readers(open_all(devices)?, sink)?,
            HotkeyBackend::GlobalShortcuts { .. } => start_portal(sink)?,
        };
        self.running = Some(running);
        Ok(())
    }

    /// Stops delivering events: once this returns the sink is never called again, and the
    /// evdev threads have exited (at most ~[`POLL_TIMEOUT_MS`] later than the call). Must
    /// not be called from inside the sink.
    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.shutdown();
        }
    }
}

impl Drop for LinuxHotkey {
    fn drop(&mut self) {
        self.stop();
    }
}

/// State shared by the reader threads. The sink is only called while the lock is held,
/// and `stop()` takes it out under the same lock.
struct Shared {
    tracker: EvdevTracker,
    sink: Option<Sink>,
}

/// Runs `convert` and passes its events to the sink. Returns `false` once stopped.
fn deliver(shared: &Mutex<Shared>, convert: impl FnOnce(&mut EvdevTracker) -> Vec<Event>) -> bool {
    let Ok(mut guard) = shared.lock() else {
        return false;
    };
    let Shared { tracker, sink } = &mut *guard;
    let Some(sink) = sink.as_ref() else {
        return false;
    };
    for event in convert(tracker) {
        sink(event);
    }
    true
}

/// How long a reader waits for input before re-checking the stop flag.
const POLL_TIMEOUT_MS: i32 = 100;

/// A source of raw evdev events; `File` in production, a channel in tests.
pub(crate) trait EventSource: Send + 'static {
    /// Waits up to `timeout_ms` for one event; `Ok(None)` on timeout.
    fn next_event(&mut self, timeout_ms: i32) -> io::Result<Option<RawEvent>>;
}

impl EventSource for File {
    fn next_event(&mut self, timeout_ms: i32) -> io::Result<Option<RawEvent>> {
        let mut pfd = libc::pollfd {
            fd: self.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd` for a file descriptor owned by `self`.
        let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            return Ok(None);
        }
        if pfd.revents & libc::POLLIN == 0 {
            // POLLERR / POLLHUP / POLLNVAL: the device went away.
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        let mut buf = [0u8; INPUT_EVENT_SIZE];
        self.read_exact(&mut buf)?;
        Ok(Some(RawEvent::decode(&buf)))
    }
}

fn open_all(devices: &[PathBuf]) -> Result<Vec<File>, PlatformError> {
    let mut files = Vec::new();
    let mut first_err = None;
    for path in devices {
        match open_device(path) {
            Ok(f) => files.push(f),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    if files.is_empty() {
        return Err(first_err.unwrap_or(PlatformError::HotkeyUnavailable(
            UnavailableReason::NoKeyboard.to_string(),
        )));
    }
    Ok(files)
}

fn spawn_readers<S: EventSource>(sources: Vec<S>, sink: Sink) -> Result<Running, PlatformError> {
    let mut running = Running::new(sink);
    for (i, source) in sources.into_iter().enumerate() {
        let shared = running.shared.clone();
        let stop = running.stop.clone();
        match thread::Builder::new()
            .name(format!("v2c-evdev-{i}"))
            .spawn(move || read_loop(source, &shared, &stop))
        {
            Ok(handle) => running.threads.push(handle),
            Err(e) => {
                running.shutdown();
                return Err(PlatformError::Os(format!("spawn evdev thread: {e}")));
            }
        }
    }
    Ok(running)
}

fn read_loop(mut source: impl EventSource, shared: &Mutex<Shared>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match source.next_event(POLL_TIMEOUT_MS) {
            Ok(Some(ev)) => {
                if !deliver(shared, |t| t.feed(ev, monotonic_ms())) {
                    return;
                }
            }
            Ok(None) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return, // device unplugged (ENODEV) or closed
        }
    }
}

/// Converts a D-Bus unique name (`:1.42`) into the request-path form (`1_42`).
pub(crate) fn sender_path_element(unique_name: &str) -> String {
    unique_name.trim_start_matches(':').replace('.', "_")
}

static TOKEN_COUNTER: AtomicU32 = AtomicU32::new(0);

fn new_token() -> String {
    format!(
        "v2c_{}_{}",
        std::process::id(),
        TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn dbus_err(what: &str) -> impl Fn(zbus::Error) -> PlatformError + '_ {
    move |e| PlatformError::Os(format!("{what}: {e}"))
}

const SESSION_IFACE: &str = "org.freedesktop.portal.Session";

/// Calls `org.freedesktop.portal.Session.Close` (best effort).
fn close_session(conn: &zbus::blocking::Connection, session: &str) {
    if let Ok(p) = zbus::blocking::Proxy::new(conn, PORTAL_DEST, session, SESSION_IFACE) {
        let _ = p.call_method("Close", &());
    }
}

/// Calls a portal method that returns a Request handle and waits for its `Response`.
/// Subscribes before the call (at the predictable request path) so the reply cannot be missed.
fn portal_request(
    conn: &zbus::blocking::Connection,
    method: &str,
    token: &str,
    call: impl FnOnce() -> zbus::Result<zbus::Message>,
) -> Result<HashMap<String, zbus::zvariant::OwnedValue>, PlatformError> {
    use zbus::blocking::Proxy;
    let sender = conn
        .unique_name()
        .ok_or_else(|| PlatformError::Os("no D-Bus unique name".into()))?;
    let path = format!(
        "{PORTAL_PATH}/request/{}/{token}",
        sender_path_element(sender.as_str())
    );
    let request = Proxy::new(
        conn,
        PORTAL_DEST,
        path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(dbus_err("portal request proxy"))?;
    let mut responses = request
        .receive_signal("Response")
        .map_err(dbus_err("subscribe Response"))?;
    call().map_err(dbus_err(method))?;
    let msg = responses
        .next()
        .ok_or_else(|| PlatformError::Os(format!("{method}: no response")))?;
    let (code, results): (u32, HashMap<String, zbus::zvariant::OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(dbus_err("portal response"))?;
    match code {
        0 => Ok(results),
        1 => Err(PlatformError::Os(format!(
            "{method}: cancelled by the user"
        ))),
        c => Err(PlatformError::Os(format!("{method}: portal error {c}"))),
    }
}

/// Creates a GlobalShortcuts session, binds the v2c shortcuts, and forwards
/// `Activated`/`Deactivated` on a thread, which exits when the session is closed (by
/// `stop()` or by the portal). Not exercised on the T3.0 host (GNOME 42 has no
/// GlobalShortcuts portal); see the 🧑 items of task card T3.1.
fn start_portal(sink: Sink) -> Result<Running, PlatformError> {
    use zbus::blocking::{Connection, MessageIterator, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    let conn = Connection::session().map_err(dbus_err("session bus"))?;
    let gs = Proxy::new(&conn, PORTAL_DEST, PORTAL_PATH, GLOBAL_SHORTCUTS_IFACE)
        .map_err(dbus_err("GlobalShortcuts proxy"))?;

    // Subscribe before binding so no early press is lost. Only the portal's own signals are
    // accepted (the bus resolves the well-known sender name); the interface is checked
    // below so `Session.Closed` is seen too.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(PORTAL_DEST)
        .map_err(dbus_err("match rule"))?
        .build();
    let signals = MessageIterator::for_match_rule(rule, &conn, None)
        .map_err(dbus_err("subscribe shortcuts"))?;

    let token = new_token();
    let session_token = new_token();
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("handle_token", Value::from(token.as_str()));
    opts.insert("session_handle_token", Value::from(session_token.as_str()));
    let results = portal_request(&conn, "CreateSession", &token, || {
        gs.call_method("CreateSession", &(opts,))
    })?;
    let session = results
        .get("session_handle")
        .and_then(|v| {
            String::try_from(v.clone()).ok().or_else(|| {
                OwnedObjectPath::try_from(v.clone())
                    .ok()
                    .map(|p| p.to_string())
            })
        })
        .ok_or_else(|| PlatformError::Os("CreateSession: no session_handle".into()))?;
    let session_path = match OwnedObjectPath::try_from(session.as_str()) {
        Ok(p) => p,
        Err(e) => {
            close_session(&conn, &session);
            return Err(PlatformError::Os(format!("session handle: {e}")));
        }
    };

    let shortcuts: Vec<(&str, HashMap<&str, Value<'_>>)> = [
        (SHORTCUT_DICTATE, "v2c: hold to dictate"),
        (
            SHORTCUT_TRANSLATE,
            "v2c: hold to translate Chinese to English",
        ),
        (SHORTCUT_CANCEL, "v2c: cancel recording"),
    ]
    .into_iter()
    .map(|(id, description)| {
        let mut m = HashMap::new();
        m.insert("description", Value::from(description));
        (id, m)
    })
    .collect();
    let bind_token = new_token();
    let mut bind_opts: HashMap<&str, Value<'_>> = HashMap::new();
    bind_opts.insert("handle_token", Value::from(bind_token.as_str()));
    if let Err(e) = portal_request(&conn, "BindShortcuts", &bind_token, || {
        gs.call_method("BindShortcuts", &(&session_path, shortcuts, "", bind_opts))
    }) {
        // Includes the user pressing Cancel in the dialog.
        close_session(&conn, &session);
        return Err(e);
    }

    let mut running = Running::new(sink);
    let shared = running.shared.clone();
    let stop = running.stop.clone();
    let session_for_thread = session.clone();
    let spawned = thread::Builder::new()
        .name("v2c-global-shortcuts".into())
        .spawn(move || {
            for msg in signals {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(msg) = msg else {
                    return; // connection closed
                };
                let header = msg.header();
                let iface = header.interface().map(|i| i.as_str());
                let member = header.member().map(|m| m.as_str());
                if iface == Some(SESSION_IFACE)
                    && member == Some("Closed")
                    && header.path().map(|p| p.as_str()) == Some(session_for_thread.as_str())
                {
                    return;
                }
                if iface != Some(GLOBAL_SHORTCUTS_IFACE) {
                    continue;
                }
                let signal = match member {
                    Some("Activated") => PortalSignal::Activated,
                    Some("Deactivated") => PortalSignal::Deactivated,
                    _ => continue,
                };
                type Body = (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>);
                let Ok((sess, id, _ts, _)) = msg.body().deserialize::<Body>() else {
                    continue;
                };
                if sess.as_str() != session_for_thread {
                    continue;
                }
                if !deliver(&shared, |_| portal_events(&id, signal, monotonic_ms())) {
                    return;
                }
            }
        });
    running.portal = Some(PortalSession { conn, session });
    if let Err(e) = spawned {
        running.shutdown();
        return Err(PlatformError::Os(format!("spawn shortcuts thread: {e}")));
    }
    // The portal thread is not joined: it may sit in the blocking signal iterator until the
    // closed connection wakes it, but it can no longer reach the sink.
    Ok(running)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::caps::EvdevCaps;
    use crate::linux::caps::SessionType;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::Duration;
    use v2c_core::trigger::{Command, Trigger, TriggerConfig};
    use v2c_core::Mode;

    const KEY_C: u16 = 46;
    const KEY_LEFTCTRL: u16 = 29;

    fn key(code: u16, value: i32) -> RawEvent {
        RawEvent {
            type_: EV_KEY,
            code,
            value,
        }
    }

    fn caps(gs: Portal, keyboards: &[&str], access: DeviceAccess) -> LinuxCaps {
        LinuxCaps {
            session: SessionType::Wayland,
            desktop: Some("ubuntu:GNOME".into()),
            global_shortcuts: gs,
            remote_desktop: Portal::Present { version: Some(1) },
            clipboard: Portal::Missing,
            portal_error: None,
            evdev: EvdevCaps {
                keyboards: keyboards.iter().map(PathBuf::from).collect(),
                access,
            },
        }
    }

    fn machine() -> Trigger {
        Trigger::new(TriggerConfig {
            hold_threshold_ms: 350,
            double_tap_window_ms: 300,
            hands_free_max_ms: 300_000,
        })
    }

    /// Feeds timed raw evdev events through the tracker and the real trigger state machine.
    fn run_evdev(seq: &[(u64, RawEvent)]) -> Vec<Command> {
        let mut tracker = EvdevTracker::new();
        let mut m = machine();
        let mut out = Vec::new();
        for &(t, ev) in seq {
            for e in tracker.feed(ev, t) {
                out.extend(m.on_event(e));
            }
            out.extend(m.on_event(Event::Tick { at_ms: t }));
        }
        out
    }

    // ---- conversion ----

    #[test]
    fn classifies_keys() {
        assert_eq!(classify(KEY_RIGHTCTRL), Key::Trigger);
        assert_eq!(classify(KEY_LEFTSHIFT), Key::Shift);
        assert_eq!(classify(KEY_RIGHTSHIFT), Key::Shift);
        assert_eq!(classify(KEY_ESC), Key::Escape);
        assert_eq!(classify(KEY_LEFTCTRL), Key::Other); // only Right Ctrl triggers
        assert_eq!(classify(KEY_C), Key::Other);
    }

    #[test]
    fn press_and_release_convert() {
        let mut t = EvdevTracker::new();
        assert_eq!(
            t.feed(key(KEY_RIGHTCTRL, 1), 10),
            vec![Event::KeyDown {
                key: Key::Trigger,
                at_ms: 10
            }]
        );
        assert_eq!(
            t.feed(key(KEY_RIGHTCTRL, 0), 20),
            vec![Event::KeyUp {
                key: Key::Trigger,
                at_ms: 20
            }]
        );
    }

    #[test]
    fn autorepeat_is_ignored() {
        let mut t = EvdevTracker::new();
        assert_eq!(t.feed(key(KEY_RIGHTCTRL, 1), 0).len(), 1);
        for at in [500, 533, 566] {
            assert!(t.feed(key(KEY_RIGHTCTRL, 2), at).is_empty());
            assert!(t.feed(key(KEY_ESC, 2), at).is_empty());
        }
        // A duplicated press (value 1 while down) is dropped too.
        assert!(t.feed(key(KEY_RIGHTCTRL, 1), 600).is_empty());
        assert_eq!(t.feed(key(KEY_RIGHTCTRL, 0), 700).len(), 1);
    }

    #[test]
    fn non_key_events_and_unpaired_releases_are_ignored() {
        let mut t = EvdevTracker::new();
        let syn = RawEvent {
            type_: EV_SYN,
            code: 0,
            value: 0,
        };
        let msc = RawEvent {
            type_: 4,
            code: 4,
            value: 0x1d,
        };
        assert!(t.feed(syn, 0).is_empty());
        assert!(t.feed(msc, 0).is_empty());
        // Key held before start: its release has no matching press.
        assert!(t.feed(key(KEY_RIGHTCTRL, 0), 0).is_empty());
        assert!(t.feed(key(0x2ff + 1, 1), 0).is_empty()); // out of range
    }

    #[test]
    fn both_shifts_fold_into_one() {
        let mut t = EvdevTracker::new();
        assert_eq!(t.feed(key(KEY_LEFTSHIFT, 1), 0).len(), 1);
        assert!(t.feed(key(KEY_RIGHTSHIFT, 1), 1).is_empty());
        assert!(t.feed(key(KEY_LEFTSHIFT, 0), 2).is_empty());
        assert_eq!(
            t.feed(key(KEY_RIGHTSHIFT, 0), 3),
            vec![Event::KeyUp {
                key: Key::Shift,
                at_ms: 3
            }]
        );
    }

    #[test]
    fn syn_dropped_releases_held_keys() {
        let mut t = EvdevTracker::new();
        t.feed(key(KEY_RIGHTCTRL, 1), 0);
        t.feed(key(KEY_LEFTSHIFT, 1), 0);
        let dropped = RawEvent {
            type_: EV_SYN,
            code: SYN_DROPPED,
            value: 0,
        };
        let mut ev = t.feed(dropped, 50);
        ev.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(
            ev,
            vec![
                Event::KeyUp {
                    key: Key::Shift,
                    at_ms: 50
                },
                Event::KeyUp {
                    key: Key::Trigger,
                    at_ms: 50
                },
            ]
        );
        assert!(t.feed(syn_report(), 55).is_empty());
        // The real (late) release is then dropped instead of emitted twice.
        assert!(t.feed(key(KEY_RIGHTCTRL, 0), 60).is_empty());
    }

    fn syn_report() -> RawEvent {
        RawEvent {
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        }
    }

    #[test]
    fn syn_dropped_discards_until_syn_report() {
        let mut t = EvdevTracker::new();
        let dropped = RawEvent {
            type_: EV_SYN,
            code: SYN_DROPPED,
            value: 0,
        };
        // Nothing is held, so there is nothing to release.
        assert!(t.feed(dropped, 0).is_empty());
        // The rest of the broken packet is ignored, including a fresh press.
        assert!(t.feed(key(KEY_RIGHTCTRL, 1), 1).is_empty());
        assert!(t.feed(key(KEY_ESC, 1), 1).is_empty());
        assert!(t.feed(dropped, 2).is_empty());
        assert!(t.feed(syn_report(), 3).is_empty());
        // Normal delivery resumes after SYN_REPORT; the discarded press left no state.
        assert_eq!(
            t.feed(key(KEY_RIGHTCTRL, 1), 10),
            vec![Event::KeyDown {
                key: Key::Trigger,
                at_ms: 10
            }]
        );
        assert!(t.feed(syn_report(), 10).is_empty());
        assert_eq!(t.feed(key(KEY_RIGHTCTRL, 0), 20).len(), 1);
    }

    /// Test event source fed from a channel, like a device that blocks between keys.
    struct ChannelSource(mpsc::Receiver<RawEvent>);

    impl EventSource for ChannelSource {
        fn next_event(&mut self, timeout_ms: i32) -> io::Result<Option<RawEvent>> {
            match self
                .0
                .recv_timeout(Duration::from_millis(timeout_ms as u64))
            {
                Ok(ev) => Ok(Some(ev)),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(io::Error::from(io::ErrorKind::BrokenPipe))
                }
            }
        }
    }

    #[test]
    fn file_source_polls_with_timeout() {
        use std::io::Write;
        use std::os::fd::OwnedFd;
        let (reader, mut writer) = io::pipe().unwrap();
        let mut file = File::from(OwnedFd::from(reader));
        let t0 = Instant::now();
        assert!(file.next_event(50).unwrap().is_none());
        assert!(t0.elapsed() >= Duration::from_millis(40));

        let mut buf = [0u8; INPUT_EVENT_SIZE];
        let o = 2 * std::mem::size_of::<c_long>();
        buf[o..o + 2].copy_from_slice(&EV_KEY.to_ne_bytes());
        buf[o + 2..o + 4].copy_from_slice(&KEY_ESC.to_ne_bytes());
        buf[o + 4..o + 8].copy_from_slice(&1i32.to_ne_bytes());
        writer.write_all(&buf).unwrap();
        assert_eq!(file.next_event(1_000).unwrap(), Some(key(KEY_ESC, 1)));

        drop(writer); // hang-up: the reader thread would exit
        assert!(file.next_event(1_000).is_err());
    }

    fn counting_sink() -> (Arc<AtomicUsize>, Sink) {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        (
            count,
            Box::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
        )
    }

    fn wait_for(count: &AtomicUsize, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while count.load(Ordering::SeqCst) < n {
            assert!(Instant::now() < deadline, "sink not called");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn no_sink_call_after_stop_and_threads_exit() {
        let (count, sink) = counting_sink();
        let (tx, rx) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        let running = spawn_readers(vec![ChannelSource(rx), ChannelSource(rx2)], sink).unwrap();
        let mut h = LinuxHotkey {
            backend: HotkeyBackend::Evdev { devices: vec![] },
            running: Some(running),
        };
        tx.send(key(KEY_RIGHTCTRL, 1)).unwrap();
        wait_for(&count, 1);

        // Both readers are idle (blocked waiting for input) when stop is called; it must
        // return only after they exited, well within a few poll timeouts.
        let t0 = Instant::now();
        h.stop();
        assert!(
            t0.elapsed() < Duration::from_millis(1_000),
            "{:?}",
            t0.elapsed()
        );
        // The threads have exited, so their receivers are gone.
        assert!(tx.send(key(KEY_RIGHTCTRL, 0)).is_err());
        assert!(tx2.send(key(KEY_ESC, 1)).is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stop_while_events_are_flowing_never_calls_sink_afterwards() {
        let (count, sink) = counting_sink();
        let (tx, rx) = mpsc::channel();
        let running = spawn_readers(vec![ChannelSource(rx)], sink).unwrap();
        let feeder = thread::spawn(move || {
            // Alternate press/release as fast as possible until the reader is gone.
            for i in 0u64.. {
                if tx.send(key(KEY_ESC, (i % 2) as i32 ^ 1)).is_err() {
                    return;
                }
            }
        });
        wait_for(&count, 10);
        running.shutdown();
        let after_stop = count.load(Ordering::SeqCst);
        feeder.join().unwrap();
        thread::sleep(Duration::from_millis(20));
        assert_eq!(count.load(Ordering::SeqCst), after_stop);
    }

    #[test]
    fn decodes_input_event_bytes() {
        let mut buf = [0u8; INPUT_EVENT_SIZE];
        let o = 2 * std::mem::size_of::<c_long>();
        buf[..8].copy_from_slice(&123i64.to_ne_bytes()[..8]); // timestamp is ignored
        buf[o..o + 2].copy_from_slice(&EV_KEY.to_ne_bytes());
        buf[o + 2..o + 4].copy_from_slice(&KEY_RIGHTCTRL.to_ne_bytes());
        buf[o + 4..o + 8].copy_from_slice(&2i32.to_ne_bytes());
        assert_eq!(RawEvent::decode(&buf), key(KEY_RIGHTCTRL, 2));
    }

    #[test]
    fn portal_signals_convert() {
        use PortalSignal::*;
        let d = |key| Event::KeyDown { key, at_ms: 5 };
        let u = |key| Event::KeyUp { key, at_ms: 5 };
        assert_eq!(
            portal_events(SHORTCUT_DICTATE, Activated, 5),
            vec![d(Key::Trigger)]
        );
        assert_eq!(
            portal_events(SHORTCUT_DICTATE, Deactivated, 5),
            vec![u(Key::Trigger)]
        );
        assert_eq!(
            portal_events(SHORTCUT_TRANSLATE, Activated, 5),
            vec![d(Key::Shift), d(Key::Trigger)]
        );
        assert_eq!(
            portal_events(SHORTCUT_TRANSLATE, Deactivated, 5),
            vec![u(Key::Trigger), u(Key::Shift)]
        );
        assert_eq!(
            portal_events(SHORTCUT_CANCEL, Activated, 5),
            vec![d(Key::Escape)]
        );
        assert!(portal_events("someone-else", Activated, 5).is_empty());
    }

    // ---- permission ----

    #[test]
    fn eacces_is_input_group_error() {
        let path = Path::new("/dev/input/event3");
        let err = open_error(path, &io::Error::from_raw_os_error(13));
        match &err {
            PlatformError::InputPermission { path: p } => assert_eq!(p, path),
            other => panic!("expected InputPermission, got {other:?}"),
        }
        let text = err.to_string();
        assert!(text.contains("input` group"), "{text}");
        assert!(text.contains("/dev/input/event3"), "{text}");
        assert!(matches!(
            open_error(path, &io::Error::from_raw_os_error(19)), // ENODEV
            PlatformError::Os(_)
        ));
    }

    #[test]
    fn opening_missing_device_is_not_a_permission_error() {
        let err = open_device(Path::new("/nonexistent/v2c/event0")).unwrap_err();
        assert!(matches!(err, PlatformError::Os(_)), "{err:?}");
    }

    // ---- backend selection ----

    #[test]
    fn prefers_global_shortcuts() {
        let c = caps(
            Portal::Present { version: Some(1) },
            &["/dev/input/event3"],
            DeviceAccess::Readable,
        );
        assert_eq!(
            select_backend(&c),
            HotkeyBackend::GlobalShortcuts { version: Some(1) }
        );
    }

    #[test]
    fn falls_back_to_evdev() {
        let c = caps(
            Portal::Missing,
            &["/dev/input/event3", "/dev/input/event7"],
            DeviceAccess::Readable,
        );
        assert_eq!(
            select_backend(&c),
            HotkeyBackend::Evdev {
                devices: vec!["/dev/input/event3".into(), "/dev/input/event7".into()]
            }
        );
    }

    #[test]
    fn unavailable_when_permission_denied() {
        let path = PathBuf::from("/dev/input/event3");
        let c = caps(
            Portal::Missing,
            &["/dev/input/event3"],
            DeviceAccess::PermissionDenied { path: path.clone() },
        );
        let b = select_backend(&c);
        assert_eq!(
            b,
            HotkeyBackend::Unavailable(vec![
                UnavailableReason::NoGlobalShortcutsPortal,
                UnavailableReason::InputPermissionDenied { path },
            ])
        );
    }

    #[test]
    fn unavailable_reports_every_reason() {
        // The T3.0 host: no portal, no keyboard attached, and no access to event devices.
        let path = PathBuf::from("/dev/input/event0");
        let c = caps(
            Portal::Missing,
            &[],
            DeviceAccess::PermissionDenied { path: path.clone() },
        );
        assert_eq!(
            select_backend(&c),
            HotkeyBackend::Unavailable(vec![
                UnavailableReason::NoGlobalShortcutsPortal,
                UnavailableReason::InputPermissionDenied { path },
                UnavailableReason::NoKeyboard,
            ])
        );
        let c = caps(Portal::Missing, &[], DeviceAccess::Readable);
        assert_eq!(
            select_backend(&c),
            HotkeyBackend::Unavailable(vec![
                UnavailableReason::NoGlobalShortcutsPortal,
                UnavailableReason::NoKeyboard,
            ])
        );
        let c = caps(
            Portal::Missing,
            &["/dev/input/event3"],
            DeviceAccess::Error("boom".into()),
        );
        assert_eq!(
            select_backend(&c),
            HotkeyBackend::Unavailable(vec![
                UnavailableReason::NoGlobalShortcutsPortal,
                UnavailableReason::Evdev("boom".into()),
            ])
        );
    }

    #[test]
    fn start_fails_loudly_when_unavailable() {
        let mut h = LinuxHotkey::new(HotkeyBackend::Unavailable(vec![
            UnavailableReason::NoGlobalShortcutsPortal,
            UnavailableReason::InputPermissionDenied {
                path: "/dev/input/event3".into(),
            },
        ]));
        match h.start(Box::new(|_| {})) {
            Err(PlatformError::HotkeyUnavailable(text)) => {
                assert!(text.contains("GlobalShortcuts"), "{text}");
                assert!(text.contains("input` group"), "{text}");
            }
            other => panic!("expected HotkeyUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn evdev_start_without_openable_device_errors() {
        let mut h = LinuxHotkey::new(HotkeyBackend::Evdev {
            devices: vec!["/nonexistent/v2c/event0".into()],
        });
        assert!(h.start(Box::new(|_| {})).is_err());
    }

    #[test]
    fn request_path_sender_element() {
        assert_eq!(sender_path_element(":1.42"), "1_42");
    }

    // ---- through the real trigger state machine ----

    #[test]
    fn evdev_hold_to_talk() {
        let cmds = run_evdev(&[
            (0, key(KEY_RIGHTCTRL, 1)),
            (250, key(KEY_RIGHTCTRL, 2)),
            (283, key(KEY_RIGHTCTRL, 2)),
            (800, key(KEY_RIGHTCTRL, 0)),
        ]);
        assert_eq!(
            cmds,
            vec![
                Command::StartRecording(Mode::Dictate),
                Command::StopAndProcess
            ]
        );
    }

    #[test]
    fn evdev_double_tap_hands_free() {
        let cmds = run_evdev(&[
            (0, key(KEY_RIGHTCTRL, 1)),
            (100, key(KEY_RIGHTCTRL, 0)),
            (250, key(KEY_RIGHTCTRL, 1)),
            (330, key(KEY_RIGHTCTRL, 0)),
            // Hands-free: other typing does not cancel, time passes.
            (5_000, key(KEY_C, 1)),
            (5_050, key(KEY_C, 0)),
            (10_000, key(KEY_RIGHTCTRL, 1)),
            (10_080, key(KEY_RIGHTCTRL, 0)),
        ]);
        assert_eq!(
            cmds,
            vec![
                Command::StartRecording(Mode::Dictate),
                Command::StopAndProcess
            ]
        );
    }

    #[test]
    fn evdev_ctrl_c_cancels() {
        let cmds = run_evdev(&[
            (0, key(KEY_RIGHTCTRL, 1)),
            (120, key(KEY_C, 1)),
            (180, key(KEY_C, 0)),
            (600, key(KEY_RIGHTCTRL, 0)),
        ]);
        assert_eq!(
            cmds,
            vec![Command::StartRecording(Mode::Dictate), Command::Cancel]
        );
    }

    #[test]
    fn evdev_shift_translates() {
        let cmds = run_evdev(&[
            (0, key(KEY_RIGHTSHIFT, 1)),
            (40, key(KEY_RIGHTCTRL, 1)),
            (300, key(KEY_RIGHTSHIFT, 2)),
            (900, key(KEY_RIGHTCTRL, 0)),
            (950, key(KEY_RIGHTSHIFT, 0)),
        ]);
        assert_eq!(
            cmds,
            vec![
                Command::StartRecording(Mode::TranslateZhToEn),
                Command::StopAndProcess
            ]
        );
    }

    #[test]
    fn evdev_escape_cancels() {
        let cmds = run_evdev(&[
            (0, key(KEY_RIGHTCTRL, 1)),
            (100, key(KEY_RIGHTCTRL, 0)),
            (200, key(KEY_RIGHTCTRL, 1)),
            (260, key(KEY_RIGHTCTRL, 0)),
            (3_000, key(KEY_ESC, 1)),
            (3_050, key(KEY_ESC, 0)),
        ]);
        assert_eq!(
            cmds,
            vec![Command::StartRecording(Mode::Dictate), Command::Cancel]
        );
    }

    #[test]
    fn evdev_single_tap_times_out() {
        let cmds = run_evdev(&[
            (0, key(KEY_RIGHTCTRL, 1)),
            (100, key(KEY_RIGHTCTRL, 0)),
            (450, key(KEY_LEFTCTRL, 1)), // later activity drives the timeout tick
        ]);
        assert_eq!(
            cmds,
            vec![Command::StartRecording(Mode::Dictate), Command::Cancel]
        );
    }

    /// Feeds timed portal signals through the real trigger state machine.
    fn run_portal(seq: &[(u64, &str, PortalSignal)]) -> Vec<Command> {
        let mut m = machine();
        let mut out = Vec::new();
        for &(t, id, s) in seq {
            for e in portal_events(id, s, t) {
                out.extend(m.on_event(e));
            }
        }
        out
    }

    #[test]
    fn portal_hold_double_tap_translate_cancel() {
        use PortalSignal::*;
        // Hold to talk.
        assert_eq!(
            run_portal(&[
                (0, SHORTCUT_DICTATE, Activated),
                (900, SHORTCUT_DICTATE, Deactivated)
            ]),
            vec![
                Command::StartRecording(Mode::Dictate),
                Command::StopAndProcess
            ]
        );
        // Double tap, then a third tap stops.
        assert_eq!(
            run_portal(&[
                (0, SHORTCUT_DICTATE, Activated),
                (100, SHORTCUT_DICTATE, Deactivated),
                (200, SHORTCUT_DICTATE, Activated),
                (260, SHORTCUT_DICTATE, Deactivated),
                (8_000, SHORTCUT_DICTATE, Activated),
                (8_100, SHORTCUT_DICTATE, Deactivated),
            ]),
            vec![
                Command::StartRecording(Mode::Dictate),
                Command::StopAndProcess
            ]
        );
        // Translate shortcut.
        assert_eq!(
            run_portal(&[
                (0, SHORTCUT_TRANSLATE, Activated),
                (700, SHORTCUT_TRANSLATE, Deactivated)
            ]),
            vec![
                Command::StartRecording(Mode::TranslateZhToEn),
                Command::StopAndProcess
            ]
        );
        // Cancel shortcut while holding.
        assert_eq!(
            run_portal(&[
                (0, SHORTCUT_DICTATE, Activated),
                (200, SHORTCUT_CANCEL, Activated),
                (250, SHORTCUT_CANCEL, Deactivated),
                (600, SHORTCUT_DICTATE, Deactivated),
            ]),
            vec![Command::StartRecording(Mode::Dictate), Command::Cancel]
        );
    }

    /// Opens a real input device through [`open_device`]; on the T3.0 host this is EACCES.
    #[test]
    #[ignore = "opens a real /dev/input device"]
    fn linux_real_open_device_permission_denied() {
        let mut devices: Vec<PathBuf> = std::fs::read_dir("/dev/input")
            .expect("read /dev/input")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().contains("/event"))
            .collect();
        devices.sort();
        let path = devices.first().expect("no /dev/input/event* device");
        let err = open_device(path).expect_err("device unexpectedly readable");
        println!("{path:?}: {err}");
        assert!(matches!(err, PlatformError::InputPermission { .. }));
    }

    /// Real end-to-end selection on this machine (read-only probe, no session is created).
    #[test]
    #[ignore = "reads the real session bus and /dev/input"]
    fn linux_real_select_backend() {
        let caps = crate::linux::caps::probe();
        let backend = select_backend(&caps);
        println!("{backend:#?}");
        if let HotkeyBackend::Unavailable(reasons) = &backend {
            for r in reasons {
                println!("- {r}");
            }
        }
        assert!(matches!(backend, HotkeyBackend::Unavailable(_)));
    }
}
