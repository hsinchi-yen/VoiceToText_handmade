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

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use zbus::blocking::{MessageIterator, Proxy};
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
}

/// What the signal thread knows about the selection.
#[derive(Default)]
struct SelectionState {
    /// We own the selection; what we serve on `SelectionTransfer`.
    offered: Option<ClipContent>,
    is_owner: bool,
    /// MIME types of the current selection (from the last `SelectionOwnerChanged`).
    mime_types: Vec<String>,
}

/// [`ClipboardBackend`] over the Clipboard portal. Constructing it does nothing on the bus.
pub struct PortalClipboard {
    session: Arc<PortalSession>,
    state: Arc<Mutex<SelectionState>>,
    /// Session the signal thread was started for.
    listening: Mutex<Option<OwnedObjectPath>>,
}

impl PortalClipboard {
    pub fn new(session: Arc<PortalSession>) -> Self {
        Self {
            session,
            state: Arc::default(),
            listening: Mutex::new(None),
        }
    }

    fn live(&self) -> Result<Arc<LiveSession>, InjectError> {
        let live = self.session.get()?;
        if !live.clipboard_enabled {
            return Err(InjectError::PermissionDenied);
        }
        let mut listening = self
            .listening
            .lock()
            .map_err(|_| InjectError::Os("clipboard lock poisoned".into()))?;
        if listening.as_ref() != Some(&live.session) {
            spawn_listener(&live, self.state.clone())?;
            *listening = Some(live.session.clone());
        }
        Ok(live)
    }

    fn proxy(live: &LiveSession) -> Result<Proxy<'_>, InjectError> {
        Proxy::new(&live.conn, PORTAL_DEST, PORTAL_PATH, CLIPBOARD_IFACE)
            .map_err(dbus_err("Clipboard proxy"))
    }

    fn offer(&self, content: ClipContent) -> Result<(), InjectError> {
        let live = self.live()?;
        let mime_types: Vec<String> = content.0.iter().map(|(m, _)| m.clone()).collect();
        {
            let mut st = self
                .state
                .lock()
                .map_err(|_| InjectError::Os("clipboard lock poisoned".into()))?;
            st.offered = Some(content);
            st.is_owner = true;
        }
        let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
        opts.insert("mime_types", Value::from(mime_types));
        let r = Self::proxy(&live)?
            .call_method("SetSelection", &(&live.session, opts))
            .map(|_| ())
            .map_err(dbus_err("SetSelection"));
        if r.is_err() {
            if let Ok(mut st) = self.state.lock() {
                st.offered = None;
                st.is_owner = false;
            }
            self.session.reset();
        }
        r
    }
}

impl ClipboardBackend for PortalClipboard {
    fn prepare(&self) -> Result<(), InjectError> {
        self.live().map(|_| ())
    }

    fn backup(&self) -> Result<ClipContent, InjectError> {
        let live = self.live()?;
        let available = self
            .state
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
        // A stale "not owner" signal can only make this false, which keeps the other
        // content (the safe direction).
        self.state
            .lock()
            .map(|st| st.is_owner)
            .map_err(|_| InjectError::Os("clipboard lock poisoned".into()))
    }

    fn restore(&self, content: &ClipContent) -> Result<(), InjectError> {
        if content.is_empty() {
            return Ok(());
        }
        self.offer(content.clone())
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
fn spawn_listener(
    live: &Arc<LiveSession>,
    state: Arc<Mutex<SelectionState>>,
) -> Result<(), InjectError> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
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
                            st.is_owner = ours;
                            st.mime_types = mimes;
                            if !ours {
                                // Don't keep dictated text in memory longer than needed.
                                st.offered = None;
                            }
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
        assert!(cb.listening.lock().unwrap().is_none());
    }
}
