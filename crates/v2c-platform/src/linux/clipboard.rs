//! Clipboard for Linux injection (T3.2, `tasks/plan.md` section 9.2).
//!
//! [`ClipboardBackend`] is the seam the injection flow ([`super::inject`]) is tested
//! through. The real backend, [`PortalClipboard`], uses the Clipboard portal on the
//! RemoteDesktop session of [`super::keys::PortalSession`] (xdg-desktop-portal-gnome >= 45;
//! GNOME 42 has none, so on the T3.0 host it is never constructed).
//!
//! Spec (checked 2026-11-10 against
//! <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Clipboard.html>,
//! interface version 1): `RequestClipboard(o, a{sv})` before the session starts;
//! `SetSelection(o, a{sv})` with option `mime_types` `as`; the portal then emits
//! `SelectionTransfer(o, s mime_type, u serial)`, answered with
//! `SelectionWrite(o, u serial) -> h` (write the data, close) and
//! `SelectionWriteDone(o, u serial, b success)`; `SelectionRead(o, s mime_type) -> h` reads
//! the current selection; `SelectionOwnerChanged(o, a{sv})` carries `mime_types` `as` and
//! `session_is_owner` `b`.
//!
//! Limits: only text formats are backed up and restored (images and other MIME types are
//! not); an empty or unreadable clipboard is left holding the pasted text, because the
//! portal has no documented "clear" call. Contents are never logged or put in errors.
//!
//! The signal thread of a session ends at the first clipboard signal after that session
//! was dropped; with no further signal it stays parked (one idle thread per session, and a
//! new session is only opened after a run failed with a D-Bus error).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use zbus::blocking::{MessageIterator, Proxy};
use zbus::message::Sequence;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue, Value};

use super::caps::{CLIPBOARD_IFACE, PORTAL_DEST, PORTAL_PATH};
use super::keys::{dbus_err, LiveSession, PortalSession};
use crate::InjectError;

/// MIME types we offer for text and prefer when backing up.
pub const TEXT_MIME_TYPES: [&str; 4] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];
/// Largest single format kept in a backup.
pub const MAX_BACKUP_BYTES: usize = 16 * 1024 * 1024;
/// Longest wait for another app to hand over its clipboard data.
const READ_TIMEOUT: Duration = Duration::from_millis(500);

/// Clipboard contents at one moment: `(mime_type, bytes)`. Empty = empty or unreadable.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ClipContent(pub Vec<(String, Vec<u8>)>);

impl ClipContent {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for ClipContent {
    // Never print clipboard bytes: they may be the user's text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total: usize = self.0.iter().map(|(_, b)| b.len()).sum();
        write!(f, "ClipContent({} formats, {} bytes)", self.0.len(), total)
    }
}

/// Clipboard primitives. Methods take `&self` because [`crate::Injector::run`] does.
pub trait ClipboardBackend {
    /// Gets ready (the portal backend opens its session here, which may show a dialog).
    /// The only method that may open a session.
    fn prepare(&self) -> Result<(), InjectError> {
        Ok(())
    }
    /// Reads the current contents for a later [`restore`](Self::restore).
    fn backup(&self) -> Result<ClipContent, InjectError>;
    /// Makes `text` the clipboard contents.
    fn set_text(&self, text: &str) -> Result<(), InjectError>;
    /// Whether the clipboard still holds what the last [`set_text`](Self::set_text) put
    /// there (false once anyone else has copied something).
    fn still_ours(&self) -> Result<bool, InjectError>;
    /// Puts a backup back. An empty backup may leave the clipboard unchanged.
    fn restore(&self, content: &ClipContent) -> Result<(), InjectError>;
    /// Called once after every run that reached `prepare`, with the run's error if any.
    fn finish(&self, _error: Option<&InjectError>) {}
}

/// What the signal thread knows about the selection of one session. `P` is the receive
/// position of a message on the connection ([`Sequence`]; plain numbers in tests), used to
/// drop `SelectionOwnerChanged` signals that were sent before our own `SetSelection`
/// returned but processed after it.
struct SelectionState<P> {
    /// What we serve on `SelectionTransfer` while we own the selection.
    offered: Option<ClipContent>,
    is_owner: bool,
    /// MIME types of the current selection (from the last `SelectionOwnerChanged`).
    mime_types: Vec<String>,
    /// Receive position of our last successful `SetSelection` reply.
    owned_since: Option<P>,
    /// A `SetSelection` call is in flight: owner signals are queued, not applied.
    offering: bool,
    queued: Vec<(P, bool)>,
}

impl<P> Default for SelectionState<P> {
    fn default() -> Self {
        Self {
            offered: None,
            is_owner: false,
            mime_types: Vec::new(),
            owned_since: None,
            offering: false,
            queued: Vec::new(),
        }
    }
}

impl<P: Ord + Copy> SelectionState<P> {
    fn on_owner_changed(&mut self, at: P, ours: bool, mime_types: Vec<String>) {
        self.mime_types = mime_types;
        if self.offering {
            self.queued.push((at, ours));
        } else {
            self.apply(at, ours);
        }
    }

    fn apply(&mut self, at: P, ours: bool) {
        if self.owned_since.is_some_and(|since| at < since) {
            return; // sent before our SetSelection took effect
        }
        self.is_owner = ours;
        if !ours {
            // Don't keep dictated text in memory longer than needed.
            self.offered = None;
            self.owned_since = None;
        }
    }

    fn begin_offer(&mut self, content: ClipContent) {
        self.offered = Some(content);
        self.offering = true;
        self.queued.clear();
    }

    /// `reply_at`: receive position of the `SetSelection` reply, `None` if it failed.
    fn end_offer(&mut self, reply_at: Option<P>) {
        self.offering = false;
        let queued = std::mem::take(&mut self.queued);
        match reply_at {
            Some(at) => {
                self.owned_since = Some(at);
                self.is_owner = true;
                for (p, ours) in queued {
                    self.apply(p, ours);
                }
            }
            None => {
                self.offered = None;
                self.is_owner = false;
                self.owned_since = None;
            }
        }
    }
}

type SharedState = Arc<Mutex<SelectionState<Sequence>>>;

fn lock_err<T>(_: T) -> InjectError {
    InjectError::Os("clipboard lock poisoned".into())
}

/// [`ClipboardBackend`] over the Clipboard portal. Constructing it does nothing on the bus.
pub struct PortalClipboard {
    session: Arc<PortalSession>,
    /// The session the signal thread runs for, and that session's own fresh state.
    current: Mutex<Option<(OwnedObjectPath, SharedState)>>,
}

impl PortalClipboard {
    pub fn new(session: Arc<PortalSession>) -> Self {
        Self {
            session,
            current: Mutex::new(None),
        }
    }

    /// The session opened by `prepare` and its selection state. Never opens a session.
    fn live(&self) -> Result<(Arc<LiveSession>, SharedState), InjectError> {
        let live = self.session.existing()?;
        let current = self.current.lock().map_err(lock_err)?;
        match current.as_ref() {
            Some((path, state)) if *path == live.session => Ok((live, state.clone())),
            _ => Err(InjectError::Os("clipboard listener is not running".into())),
        }
    }

    fn proxy(live: &LiveSession) -> Result<Proxy<'_>, InjectError> {
        Proxy::new(&live.conn, PORTAL_DEST, PORTAL_PATH, CLIPBOARD_IFACE)
            .map_err(dbus_err("Clipboard proxy"))
    }

    fn offer(&self, content: ClipContent) -> Result<(), InjectError> {
        let (live, state) = self.live()?;
        let mime_types: Vec<String> = content.0.iter().map(|(m, _)| m.clone()).collect();
        state.lock().map_err(lock_err)?.begin_offer(content);
        let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
        opts.insert("mime_types", Value::from(mime_types));
        let reply = Self::proxy(&live)?
            .call_method("SetSelection", &(&live.session, opts))
            .map_err(dbus_err("SetSelection"));
        // No session reset here: a later restore in this run must use this same session.
        state
            .lock()
            .map_err(lock_err)?
            .end_offer(reply.as_ref().ok().map(|m| m.recv_position()));
        reply.map(|_| ())
    }
}

impl ClipboardBackend for PortalClipboard {
    fn prepare(&self) -> Result<(), InjectError> {
        let live = self.session.open()?;
        if !live.clipboard_enabled {
            return Err(InjectError::PermissionDenied);
        }
        let mut current = self.current.lock().map_err(lock_err)?;
        if current.as_ref().map(|(p, _)| p) != Some(&live.session) {
            // New session: start from a clean state (nothing offered, not owner).
            let state = SharedState::default();
            spawn_listener(&live, state.clone())?;
            *current = Some((live.session.clone(), state));
        }
        Ok(())
    }

    fn backup(&self) -> Result<ClipContent, InjectError> {
        let (live, state) = self.live()?;
        let available = state
            .lock()
            .map(|st| st.mime_types.clone())
            .unwrap_or_default();
        let wanted: Vec<&str> = if available.is_empty() {
            // No SelectionOwnerChanged seen yet: try the common text type.
            vec![TEXT_MIME_TYPES[0]]
        } else {
            available
                .iter()
                .map(String::as_str)
                .filter(|m| is_text_mime(m))
                .collect()
        };
        let proxy = Self::proxy(&live)?;
        let mut out = Vec::new();
        for mime in wanted {
            let Ok(reply) = proxy.call_method("SelectionRead", &(&live.session, mime)) else {
                continue;
            };
            let Ok(fd) = reply.body().deserialize::<OwnedFd>() else {
                continue;
            };
            if let Some(bytes) = read_with_timeout(fd, READ_TIMEOUT) {
                out.push((mime.to_string(), bytes));
            }
        }
        Ok(ClipContent(out))
    }

    fn set_text(&self, text: &str) -> Result<(), InjectError> {
        let bytes = text.as_bytes().to_vec();
        self.offer(ClipContent(
            TEXT_MIME_TYPES
                .iter()
                .map(|m| (m.to_string(), bytes.clone()))
                .collect(),
        ))
    }

    fn still_ours(&self) -> Result<bool, InjectError> {
        let (_, state) = self.live()?;
        let st = state.lock().map_err(lock_err)?;
        Ok(st.is_owner)
    }

    fn restore(&self, content: &ClipContent) -> Result<(), InjectError> {
        if content.is_empty() {
            return Ok(());
        }
        self.offer(content.clone())
    }

    fn finish(&self, error: Option<&InjectError>) {
        self.session.finish(error);
    }
}

fn is_text_mime(m: &str) -> bool {
    m.starts_with("text/") || TEXT_MIME_TYPES.contains(&m) || m == "TEXT"
}

/// Reads a selection pipe to EOF on a helper thread, giving up after `timeout` (an app
/// that never writes must not block the injection).
fn read_with_timeout(fd: OwnedFd, timeout: Duration) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("v2c-clipboard-read".into())
        .spawn(move || {
            let mut file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
            let mut buf = Vec::new();
            let ok = (&mut file)
                .take(MAX_BACKUP_BYTES as u64 + 1)
                .read_to_end(&mut buf)
                .is_ok()
                && buf.len() <= MAX_BACKUP_BYTES;
            let _ = tx.send(ok.then_some(buf));
        })
        .ok()?;
    rx.recv_timeout(timeout).ok().flatten()
}

/// Follows `SelectionOwnerChanged` and serves `SelectionTransfer` for one session.
fn spawn_listener(live: &Arc<LiveSession>, state: SharedState) -> Result<(), InjectError> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(PORTAL_DEST)
        .map_err(dbus_err("match rule"))?
        .interface(CLIPBOARD_IFACE)
        .map_err(dbus_err("match rule"))?
        .build();
    let signals = MessageIterator::for_match_rule(rule, &live.conn, None)
        .map_err(dbus_err("subscribe clipboard signals"))?;
    // Weak: the thread must not keep the session open.
    let weak = Arc::downgrade(live);
    let session = live.session.clone();
    thread::Builder::new()
        .name("v2c-clipboard-portal".into())
        .spawn(move || {
            for msg in signals {
                let Some(live) = weak.upgrade() else { return };
                let Ok(msg) = msg else { continue };
                let header = msg.header();
                match header.member().map(|m| m.as_str()) {
                    Some("SelectionOwnerChanged") => {
                        type Body = (OwnedObjectPath, HashMap<String, OwnedValue>);
                        let Ok((s, opts)) = msg.body().deserialize::<Body>() else {
                            continue;
                        };
                        if s != session {
                            continue;
                        }
                        let ours = opts
                            .get("session_is_owner")
                            .and_then(|v| bool::try_from(v.clone()).ok())
                            .unwrap_or(false);
                        let mimes = opts
                            .get("mime_types")
                            .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
                            .unwrap_or_default();
                        if let Ok(mut st) = state.lock() {
                            st.on_owner_changed(msg.recv_position(), ours, mimes);
                        }
                    }
                    Some("SelectionTransfer") => {
                        let Ok((s, mime, serial)) =
                            msg.body().deserialize::<(OwnedObjectPath, String, u32)>()
                        else {
                            continue;
                        };
                        if s != session {
                            continue;
                        }
                        let data = state.lock().ok().and_then(|st| {
                            st.offered.as_ref().and_then(|c| {
                                c.0.iter().find(|(m, _)| *m == mime).map(|(_, b)| b.clone())
                            })
                        });
                        serve_transfer(&live, serial, data);
                    }
                    _ => {}
                }
            }
        })
        .map_err(|e| InjectError::Os(format!("spawn clipboard thread: {e}")))?;
    Ok(())
}

fn serve_transfer(live: &LiveSession, serial: u32, data: Option<Vec<u8>>) {
    let Ok(proxy) = PortalClipboard::proxy(live) else {
        return;
    };
    let success = data.is_some_and(|bytes| {
        proxy
            .call_method("SelectionWrite", &(&live.session, serial))
            .ok()
            .and_then(|reply| reply.body().deserialize::<OwnedFd>().ok())
            .is_some_and(|fd| {
                let mut file = std::fs::File::from(std::os::fd::OwnedFd::from(fd));
                file.write_all(&bytes).is_ok()
            })
    });
    let _ = proxy.call_method("SelectionWriteDone", &(&live.session, serial, success));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::keys::tests::{counting_slot, denied};
    use std::sync::atomic::Ordering;

    #[test]
    fn content_debug_hides_bytes() {
        let c = ClipContent(vec![("text/plain".into(), b"secret words".to_vec())]);
        let s = format!("{c:?}");
        assert_eq!(s, "ClipContent(1 formats, 12 bytes)");
        assert!(!s.contains("secret"));
    }

    #[test]
    fn text_mime_filter() {
        assert!(is_text_mime("text/plain;charset=utf-8"));
        assert!(is_text_mime("text/html"));
        assert!(is_text_mime("UTF8_STRING"));
        assert!(!is_text_mime("image/png"));
        assert!(!is_text_mime("x-special/gnome-copied-files"));
    }

    #[test]
    fn read_with_timeout_reads_and_gives_up() {
        let (r, mut w) = std::io::pipe().unwrap();
        w.write_all(b"abc").unwrap();
        drop(w);
        let fd = OwnedFd::from(std::os::fd::OwnedFd::from(r));
        assert_eq!(read_with_timeout(fd, READ_TIMEOUT), Some(b"abc".to_vec()));

        // Writer kept open and silent: must time out instead of hanging.
        let (r, w) = std::io::pipe().unwrap();
        let fd = OwnedFd::from(std::os::fd::OwnedFd::from(r));
        assert_eq!(read_with_timeout(fd, Duration::from_millis(20)), None);
        drop(w);
    }

    #[test]
    fn constructing_touches_nothing() {
        let s = PortalSession::new(true);
        let cb = PortalClipboard::new(s);
        assert!(cb.current.lock().unwrap().is_none());
    }

    #[test]
    fn only_prepare_may_open_the_session() {
        let (slot, opens) = counting_slot(denied);
        let cb = PortalClipboard::new(slot);
        // Restore / set / backup after a failure must never pop up a new dialog.
        assert!(cb
            .restore(&ClipContent(vec![("text/plain".into(), vec![1])]))
            .is_err());
        assert!(cb.set_text("x").is_err());
        assert!(cb.backup().is_err());
        assert!(cb.still_ours().is_err());
        assert_eq!(opens.load(Ordering::SeqCst), 0);
        assert_eq!(cb.prepare(), Err(InjectError::PermissionDenied));
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert!(cb
            .restore(&ClipContent(vec![("text/plain".into(), vec![1])]))
            .is_err());
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }

    fn text(t: &str) -> ClipContent {
        ClipContent(vec![("text/plain".into(), t.as_bytes().to_vec())])
    }

    #[test]
    fn stale_owner_signal_is_ignored() {
        let mut st = SelectionState::<u64>::default();
        st.begin_offer(text("ours"));
        // Another app's copy, sent before our SetSelection took effect, arrives meanwhile.
        st.on_owner_changed(5, false, vec!["text/plain".into()]);
        st.end_offer(Some(7));
        assert!(st.is_owner);
        assert!(st.offered.is_some());
        // Same stale signal processed after the reply: still ignored.
        st.on_owner_changed(6, false, vec![]);
        assert!(st.is_owner);
        // Our own ownership confirmation, then a real later copy.
        st.on_owner_changed(8, true, vec![]);
        assert!(st.is_owner);
        st.on_owner_changed(9, false, vec!["text/plain".into()]);
        assert!(!st.is_owner);
        assert!(st.offered.is_none(), "dictated text must not linger");
    }

    #[test]
    fn copy_after_our_reply_while_queued_wins() {
        let mut st = SelectionState::<u64>::default();
        st.begin_offer(text("ours"));
        st.on_owner_changed(9, false, vec![]);
        st.end_offer(Some(7));
        assert!(!st.is_owner);
        assert!(st.offered.is_none());
    }

    #[test]
    fn failed_offer_clears_state() {
        let mut st = SelectionState::<u64>::default();
        st.begin_offer(text("ours"));
        st.end_offer(None);
        assert!(!st.is_owner);
        assert!(st.offered.is_none());
        assert!(st.owned_since.is_none());
    }
}
