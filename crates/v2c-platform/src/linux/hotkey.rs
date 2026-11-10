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
use std::os::raw::c_long;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
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
/// presses, folds both Shift keys into one `Key::Shift`, and releases every held key after
/// `SYN_DROPPED` (the kernel buffer overflowed, so key-ups may have been lost).
/// Shared by all keyboards, so a key held on one and released on another still pairs up.
pub(crate) struct EvdevTracker {
    down: Box<[bool; KEY_CNT]>,
}

impl EvdevTracker {
    pub(crate) fn new() -> Self {
        Self {
            down: Box::new([false; KEY_CNT]),
        }
    }

    fn any_shift_down(&self) -> bool {
        self.down[usize::from(KEY_LEFTSHIFT)] || self.down[usize::from(KEY_RIGHTSHIFT)]
    }

    pub(crate) fn feed(&mut self, ev: RawEvent, at_ms: u64) -> Vec<Event> {
        match (ev.type_, ev.code) {
            (EV_SYN, SYN_DROPPED) => self.release_all(at_ms),
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

struct Running {
    stop: Arc<AtomicBool>,
    /// GlobalShortcuts session to close on stop.
    portal: Option<(zbus::blocking::Connection, String)>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let portal = match &self.backend {
            HotkeyBackend::Unavailable(reasons) => {
                let text: Vec<String> = reasons.iter().map(ToString::to_string).collect();
                return Err(PlatformError::HotkeyUnavailable(text.join("; ")));
            }
            HotkeyBackend::Evdev { devices } => {
                start_evdev(devices, sink, stop.clone())?;
                None
            }
            HotkeyBackend::GlobalShortcuts { .. } => Some(start_portal(sink, stop.clone())?),
        };
        self.running = Some(Running { stop, portal });
        Ok(())
    }

    /// Stops delivering events. Reader threads blocked in `read()` are left to exit on their
    /// next event; they never call the sink after this returns.
    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            running.stop.store(true, Ordering::SeqCst);
            if let Some((conn, session)) = running.portal {
                if let Ok(p) = zbus::blocking::Proxy::new(
                    &conn,
                    PORTAL_DEST,
                    session.as_str(),
                    "org.freedesktop.portal.Session",
                ) {
                    let _ = p.call_method("Close", &());
                }
            }
        }
    }
}

impl Drop for LinuxHotkey {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Shared {
    tracker: EvdevTracker,
    sink: Sink,
}

fn start_evdev(
    devices: &[PathBuf],
    sink: Sink,
    stop: Arc<AtomicBool>,
) -> Result<(), PlatformError> {
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
    let shared = Arc::new(Mutex::new(Shared {
        tracker: EvdevTracker::new(),
        sink,
    }));
    for (i, file) in files.into_iter().enumerate() {
        let shared = shared.clone();
        let stop = stop.clone();
        thread::Builder::new()
            .name(format!("v2c-evdev-{i}"))
            .spawn(move || read_device(file, &shared, &stop))
            .map_err(|e| PlatformError::Os(format!("spawn evdev thread: {e}")))?;
    }
    Ok(())
}

fn read_device(mut file: File, shared: &Mutex<Shared>, stop: &AtomicBool) {
    let mut buf = [0u8; INPUT_EVENT_SIZE];
    loop {
        match file.read_exact(&mut buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return, // device unplugged (ENODEV) or closed
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let ev = RawEvent::decode(&buf);
        let Ok(mut guard) = shared.lock() else { return };
        let events = guard.tracker.feed(ev, monotonic_ms());
        for event in events {
            (guard.sink)(event);
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
/// `Activated`/`Deactivated` on a thread. Not exercised on the T3.0 host (GNOME 42 has no
/// GlobalShortcuts portal); see the 🧑 items of task card T3.1.
fn start_portal(
    sink: Sink,
    stop: Arc<AtomicBool>,
) -> Result<(zbus::blocking::Connection, String), PlatformError> {
    use zbus::blocking::{Connection, MessageIterator, Proxy};
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    let conn = Connection::session().map_err(dbus_err("session bus"))?;
    let gs = Proxy::new(&conn, PORTAL_DEST, PORTAL_PATH, GLOBAL_SHORTCUTS_IFACE)
        .map_err(dbus_err("GlobalShortcuts proxy"))?;

    // Subscribe to the shortcut signals before binding so no early press is lost.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(GLOBAL_SHORTCUTS_IFACE)
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
    let session_path = OwnedObjectPath::try_from(session.as_str())
        .map_err(|e| PlatformError::Os(format!("session handle: {e}")))?;

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
    portal_request(&conn, "BindShortcuts", &bind_token, || {
        gs.call_method("BindShortcuts", &(&session_path, shortcuts, "", bind_opts))
    })?;

    let session_for_thread = session.clone();
    thread::Builder::new()
        .name("v2c-global-shortcuts".into())
        .spawn(move || {
            for msg in signals {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(msg) = msg else { continue };
                let header = msg.header();
                let signal = match header.member().map(|m| m.as_str()) {
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
                for event in portal_events(&id, signal, monotonic_ms()) {
                    sink(event);
                }
            }
        })
        .map_err(|e| PlatformError::Os(format!("spawn shortcuts thread: {e}")))?;
    Ok((conn, session))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::caps::EvdevCaps;
    use crate::linux::caps::SessionType;
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
        // The real (late) release is then dropped instead of emitted twice.
        assert!(t.feed(key(KEY_RIGHTCTRL, 0), 60).is_empty());
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
