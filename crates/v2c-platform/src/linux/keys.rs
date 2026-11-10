//! Key events for Linux injection (T3.2, `tasks/plan.md` section 9.2).
//!
//! [`KeyBackend`] is the seam the injection flow ([`super::inject`]) is tested through.
//! The real backend, [`PortalKeys`], sends evdev key codes with the RemoteDesktop portal's
//! `NotifyKeyboardKeycode` over a session shared with the Clipboard portal
//! ([`PortalSession`]).
//!
//! Spec (checked 2026-11-10 against
//! <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html>):
//! `CreateSession(a{sv}) -> o`, `SelectDevices(o, a{sv}) -> o` (option `types` `u`,
//! 1 = keyboard), `Start(o, s, a{sv}) -> o` (results `devices` `u`, `clipboard_enabled` `b`
//! since v2), `NotifyKeyboardKeycode(o, a{sv}, i keycode, u state)` with state 0 = released,
//! 1 = pressed. The returned `o` handles are `org.freedesktop.portal.Request` objects whose
//! `Response(u, a{sv})` code is 0 = success, 1 = cancelled by the user, 2 = other end.
//!
//! **Creating a session shows a consent dialog** (version 1 has no `persist_mode`, so on
//! every session). Nothing here is exercised by tests; sessions are opened lazily on the
//! first injection only.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::caps::{CLIPBOARD_IFACE, PORTAL_DEST, PORTAL_PATH, REMOTE_DESKTOP_IFACE};
use super::hotkey::sender_path_element;
use crate::InjectError;

/// Linux evdev key codes (`linux/input-event-codes.h`) used for injection.
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_V: u16 = 47;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_ENTER: u16 = 28;

/// What a [`KeyBackend`] knows about the user's physical modifier keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierState {
    Released,
    /// Ctrl, Shift, Alt or Super is held down.
    Held,
    /// The backend cannot observe the physical keyboard. The RemoteDesktop portal only
    /// sends input; reading key state needs evdev (the `input` group), which is also what
    /// the hotkey uses. The flow then proceeds without waiting.
    Unknown,
}

/// Sends key presses and releases. Methods take `&self` because [`crate::Injector::run`]
/// does; implementations use interior mutability.
pub trait KeyBackend {
    /// Gets ready to send keys (the portal backend opens its session here, which may show
    /// a consent dialog). Called once per [`crate::Injector::run`] before anything else.
    fn prepare(&self) -> Result<(), InjectError> {
        Ok(())
    }
    /// State of the physical modifier keys, polled while waiting for the user to let go.
    fn modifiers(&self) -> Result<ModifierState, InjectError>;
    /// Presses (`pressed = true`) or releases an evdev key code. Must not open a new
    /// session: a release has to reach the same session as its press.
    fn key(&self, code: u16, pressed: bool) -> Result<(), InjectError>;
    /// Called once after every run that reached `prepare`, with the run's error if any.
    /// The portal backend drops a possibly broken session here (never mid-run).
    fn finish(&self, _error: Option<&InjectError>) {}
}

// ---------------------------------------------------------------------------------------
// Portal plumbing (shared with the clipboard backend)
// ---------------------------------------------------------------------------------------

static TOKEN_COUNTER: AtomicU32 = AtomicU32::new(0);

pub(crate) fn new_token() -> String {
    format!(
        "v2c_inject_{}_{}",
        std::process::id(),
        TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Maps a D-Bus error. A missing portal service/interface/method is `Unsupported`.
pub(crate) fn dbus_err(what: &'static str) -> impl Fn(zbus::Error) -> InjectError {
    move |e| {
        let unsupported = match &e {
            zbus::Error::MethodError(name, _, _) => matches!(
                name.as_str(),
                "org.freedesktop.DBus.Error.ServiceUnknown"
                    | "org.freedesktop.DBus.Error.UnknownInterface"
                    | "org.freedesktop.DBus.Error.UnknownMethod"
                    | "org.freedesktop.DBus.Error.UnknownObject"
            ),
            _ => false,
        };
        if unsupported {
            InjectError::Unsupported(format!("{what}: portal not available ({e})"))
        } else {
            InjectError::Os(format!("{what}: {e}"))
        }
    }
}

/// Maps a portal `Response` code (0 = success, 1 = user cancelled, 2 = ended otherwise).
pub(crate) fn response_result(method: &str, code: u32) -> Result<(), InjectError> {
    match code {
        0 => Ok(()),
        1 => Err(InjectError::PermissionDenied),
        c => Err(InjectError::Os(format!(
            "{method}: portal interaction ended (code {c})"
        ))),
    }
}

/// Calls a portal method that returns a Request handle and waits for its `Response`.
/// Subscribes first (at the predictable request path) so the reply cannot be missed.
fn portal_request(
    conn: &Connection,
    method: &'static str,
    token: &str,
    call: impl FnOnce() -> zbus::Result<zbus::Message>,
) -> Result<HashMap<String, OwnedValue>, InjectError> {
    let sender = conn
        .unique_name()
        .ok_or_else(|| InjectError::Os("no D-Bus unique name".into()))?;
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
        .ok_or_else(|| InjectError::Os(format!("{method}: no response")))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(dbus_err("portal response"))?;
    response_result(method, code)?;
    Ok(results)
}

/// An open RemoteDesktop session (keyboard, plus clipboard access when requested).
pub struct LiveSession {
    pub(crate) conn: Connection,
    pub(crate) session: OwnedObjectPath,
    pub(crate) clipboard_enabled: bool,
}

impl LiveSession {
    /// CreateSession → SelectDevices(keyboard) → [RequestClipboard] → Start.
    /// Shows the consent dialog. Never called by tests.
    fn open(want_clipboard: bool) -> Result<Self, InjectError> {
        let conn = Connection::session().map_err(dbus_err("session bus"))?;
        let rd = Proxy::new(&conn, PORTAL_DEST, PORTAL_PATH, REMOTE_DESKTOP_IFACE)
            .map_err(dbus_err("RemoteDesktop proxy"))?;

        let token = new_token();
        let session_token = new_token();
        let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
        opts.insert("handle_token", Value::from(token.as_str()));
        opts.insert("session_handle_token", Value::from(session_token.as_str()));
        let results = portal_request(&conn, "CreateSession", &token, || {
            rd.call_method("CreateSession", &(opts,))
        })?;
        // The spec types `session_handle` as `s`; accept `o` too.
        let session = results
            .get("session_handle")
            .and_then(|v| {
                String::try_from(v.clone()).ok().or_else(|| {
                    OwnedObjectPath::try_from(v.clone())
                        .ok()
                        .map(|p| p.to_string())
                })
            })
            .ok_or_else(|| InjectError::Os("CreateSession: no session_handle".into()))?;
        let session = OwnedObjectPath::try_from(session.as_str())
            .map_err(|e| InjectError::Os(format!("session handle: {e}")))?;
        let live = LiveSession {
            conn,
            session,
            clipboard_enabled: false,
        };
        // From here on, `live` closes the session on drop if a later step fails.
        live.start(&rd, want_clipboard)
    }

    fn start(mut self, rd: &Proxy<'_>, want_clipboard: bool) -> Result<Self, InjectError> {
        let token = new_token();
        let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
        opts.insert("handle_token", Value::from(token.as_str()));
        opts.insert("types", Value::from(1u32)); // KEYBOARD
        portal_request(&self.conn, "SelectDevices", &token, || {
            rd.call_method("SelectDevices", &(&self.session, opts))
        })?;

        if want_clipboard {
            // Must happen before Start (Clipboard portal spec).
            let cb = Proxy::new(&self.conn, PORTAL_DEST, PORTAL_PATH, CLIPBOARD_IFACE)
                .map_err(dbus_err("Clipboard proxy"))?;
            let none: HashMap<&str, Value<'_>> = HashMap::new();
            cb.call_method("RequestClipboard", &(&self.session, none))
                .map_err(dbus_err("RequestClipboard"))?;
        }

        let token = new_token();
        let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
        opts.insert("handle_token", Value::from(token.as_str()));
        let results = portal_request(&self.conn, "Start", &token, || {
            rd.call_method("Start", &(&self.session, "", opts))
        })?;
        let devices = results
            .get("devices")
            .and_then(|v| u32::try_from(v.clone()).ok())
            .unwrap_or(0);
        if devices & 1 == 0 {
            // The user unticked the keyboard in the dialog.
            return Err(InjectError::PermissionDenied);
        }
        let clipboard_enabled = results
            .get("clipboard_enabled")
            .and_then(|v| bool::try_from(v.clone()).ok());
        self.clipboard_enabled = clipboard_enabled == Some(true);
        if want_clipboard {
            clipboard_grant(clipboard_enabled)?;
        }
        Ok(self)
    }

    fn notify_keycode(&self, code: u16, pressed: bool) -> Result<(), InjectError> {
        let rd = Proxy::new(&self.conn, PORTAL_DEST, PORTAL_PATH, REMOTE_DESKTOP_IFACE)
            .map_err(dbus_err("RemoteDesktop proxy"))?;
        let opts: HashMap<&str, Value<'_>> = HashMap::new();
        rd.call_method(
            "NotifyKeyboardKeycode",
            &(&self.session, opts, i32::from(code), u32::from(pressed)),
        )
        .map_err(dbus_err("NotifyKeyboardKeycode"))?;
        Ok(())
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        if let Ok(p) = Proxy::new(
            &self.conn,
            PORTAL_DEST,
            self.session.as_str(),
            "org.freedesktop.portal.Session",
        ) {
            let _ = p.call_method("Close", &());
        }
    }
}

/// Maps `Start`'s `clipboard_enabled` when clipboard access was requested: `false` means
/// the user (or policy) refused it; a missing key means the portal does not support
/// clipboard sharing at all (RemoteDesktop before version 2).
pub(crate) fn clipboard_grant(clipboard_enabled: Option<bool>) -> Result<(), InjectError> {
    match clipboard_enabled {
        Some(true) => Ok(()),
        Some(false) => Err(InjectError::PermissionDenied),
        None => Err(InjectError::Unsupported(
            "the RemoteDesktop portal did not report clipboard access (needs version 2)".into(),
        )),
    }
}

type Opener<S> = Box<dyn Fn() -> Result<S, InjectError> + Send + Sync>;

/// Holds at most one session. Only [`open`](Self::open) (called from the backends'
/// `prepare`, i.e. at the start of a run) may create one; everything else uses
/// [`existing`](Self::existing), so a failure in the middle of a run can never pop up a
/// second consent dialog or send a key release to a different session than the press.
/// [`discard`](Self::discard) is called after the run (backends' `finish`).
pub struct SessionSlot<S> {
    opener: Opener<S>,
    live: Mutex<Option<Arc<S>>>,
}

/// The lazily opened RemoteDesktop session shared by [`PortalKeys`] and
/// [`super::clipboard::PortalClipboard`]. Constructing it does nothing on the bus.
pub type PortalSession = SessionSlot<LiveSession>;

impl SessionSlot<LiveSession> {
    pub fn new(want_clipboard: bool) -> Arc<Self> {
        Self::with_opener(Box::new(move || LiveSession::open(want_clipboard)))
    }
}

impl<S> SessionSlot<S> {
    pub(crate) fn with_opener(opener: Opener<S>) -> Arc<Self> {
        Arc::new(Self {
            opener,
            live: Mutex::new(None),
        })
    }

    fn slot(&self) -> Result<std::sync::MutexGuard<'_, Option<Arc<S>>>, InjectError> {
        self.live
            .lock()
            .map_err(|_| InjectError::Os("portal session lock poisoned".into()))
    }

    /// The open session, opening it (consent dialog) if there is none.
    pub(crate) fn open(&self) -> Result<Arc<S>, InjectError> {
        let mut slot = self.slot()?;
        if let Some(live) = slot.as_ref() {
            return Ok(live.clone());
        }
        let live = Arc::new((self.opener)()?);
        *slot = Some(live.clone());
        Ok(live)
    }

    /// The session [`open`](Self::open) created. Never opens one.
    pub(crate) fn existing(&self) -> Result<Arc<S>, InjectError> {
        self.slot()?
            .clone()
            .ok_or_else(|| InjectError::Os("portal session is not open".into()))
    }

    /// Forgets the session, so the next run opens a new one.
    pub(crate) fn discard(&self) {
        if let Ok(mut slot) = self.live.lock() {
            *slot = None;
        }
    }

    /// Drops the session after a run that failed with a D-Bus/OS error (it may be broken).
    /// Other errors (modifiers held, permission denied before opening) keep it.
    pub(crate) fn finish(&self, error: Option<&InjectError>) {
        if matches!(error, Some(InjectError::Os(_))) {
            self.discard();
        }
    }
}

/// [`KeyBackend`] over the RemoteDesktop portal.
pub struct PortalKeys {
    session: Arc<PortalSession>,
}

impl PortalKeys {
    pub fn new(session: Arc<PortalSession>) -> Self {
        Self { session }
    }
}

impl KeyBackend for PortalKeys {
    fn prepare(&self) -> Result<(), InjectError> {
        self.session.open().map(|_| ())
    }

    fn modifiers(&self) -> Result<ModifierState, InjectError> {
        Ok(ModifierState::Unknown)
    }

    fn key(&self, code: u16, pressed: bool) -> Result<(), InjectError> {
        // Same session as the press, even after an error (no reset mid-run).
        self.session.existing()?.notify_keycode(code, pressed)
    }

    fn finish(&self, error: Option<&InjectError>) {
        self.session.finish(error);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn response_codes() {
        assert_eq!(response_result("Start", 0), Ok(()));
        assert_eq!(
            response_result("Start", 1),
            Err(InjectError::PermissionDenied)
        );
        assert!(matches!(
            response_result("Start", 2),
            Err(InjectError::Os(_))
        ));
    }

    #[test]
    fn missing_portal_is_unsupported() {
        let e = zbus::Error::MethodError(
            zbus::names::OwnedErrorName::try_from("org.freedesktop.DBus.Error.UnknownInterface")
                .unwrap(),
            None,
            zbus::Message::method_call("/", "Ping")
                .unwrap()
                .build(&())
                .unwrap(),
        );
        assert!(matches!(
            dbus_err("RequestClipboard")(e),
            InjectError::Unsupported(_)
        ));
        let e = zbus::Error::Failure("boom".into());
        assert!(matches!(dbus_err("x")(e), InjectError::Os(_)));
    }

    #[test]
    fn constructing_a_session_touches_nothing() {
        let s = PortalSession::new(true);
        assert!(s.live.lock().unwrap().is_none());
        let _keys = PortalKeys::new(s.clone());
        assert!(s.live.lock().unwrap().is_none());
    }

    #[test]
    fn clipboard_grant_mapping() {
        assert_eq!(clipboard_grant(Some(true)), Ok(()));
        assert_eq!(
            clipboard_grant(Some(false)),
            Err(InjectError::PermissionDenied)
        );
        assert!(matches!(
            clipboard_grant(None),
            Err(InjectError::Unsupported(_))
        ));
    }

    use std::sync::atomic::AtomicUsize;

    /// A slot whose opener counts calls and yields numbered fake sessions.
    pub(crate) fn counting_slot<S: 'static>(
        make: fn(usize) -> Result<S, InjectError>,
    ) -> (Arc<SessionSlot<S>>, Arc<AtomicUsize>) {
        let opens = Arc::new(AtomicUsize::new(0));
        let o = opens.clone();
        let slot =
            SessionSlot::with_opener(Box::new(move || make(o.fetch_add(1, Ordering::SeqCst) + 1)));
        (slot, opens)
    }

    /// Stands in for the consent dialog being cancelled (a real session needs D-Bus).
    pub(crate) fn denied(_: usize) -> Result<LiveSession, InjectError> {
        Err(InjectError::PermissionDenied)
    }

    #[test]
    fn only_open_creates_a_session() {
        let (slot, opens) = counting_slot::<usize>(Ok);
        assert!(slot.existing().is_err());
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(*slot.open().unwrap(), 1);
        assert_eq!(*slot.open().unwrap(), 1);
        assert_eq!(*slot.existing().unwrap(), 1);
        assert_eq!(opens.load(Ordering::SeqCst), 1);

        // Non-OS errors keep the session; OS errors drop it, and nothing but `open`
        // brings a new one.
        slot.finish(Some(&InjectError::ModifiersHeld));
        slot.finish(None);
        assert_eq!(*slot.existing().unwrap(), 1);
        slot.finish(Some(&InjectError::Os("bus gone".into())));
        assert!(slot.existing().is_err());
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert_eq!(*slot.open().unwrap(), 2);
    }

    #[test]
    fn portal_keys_never_open_a_session_outside_prepare() {
        let (slot, opens) = counting_slot(denied);
        let keys = PortalKeys::new(slot.clone());
        // A release after a failure must not trigger a (second) consent dialog.
        assert!(keys.key(KEY_LEFTCTRL, false).is_err());
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(keys.prepare(), Err(InjectError::PermissionDenied));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert!(keys.key(KEY_LEFTCTRL, false).is_err());
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }
}
