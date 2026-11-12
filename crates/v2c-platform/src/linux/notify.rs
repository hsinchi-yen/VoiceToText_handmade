//! Status notifications for GNOME Wayland (T3.4, `tasks/plan.md` section 9.2).
//!
//! Wayland does not let an app place an always-on-top overlay, so on Linux the status pill is
//! replaced by desktop notifications over `org.freedesktop.Notifications` (D-Bus).
//!
//! - [`notification_for`] is a pure mapping from [`NotifyStatus`] to [`Notification`]. Every
//!   string it returns is a `&'static str` UI text: a notification can never carry the
//!   transcript, the polished text, or vocabulary (AGENTS.md rule 1).
//! - [`DbusNotifier`] implements [`Notifier`]. It passes the id of the previous bubble as
//!   `replaces_id`, so one recording session updates a single bubble in place instead of
//!   stacking new ones.
//! - Notifications are best effort: every method returns a [`NotifyError`] the caller is
//!   expected to ignore (`let _ = notifier.show(..)`); a missing or broken notification
//!   daemon must never fail the dictation pipeline.
//!
//! Spec (verified 2026-11-10, <https://specifications.freedesktop.org/notification/latest/>):
//! `Notify(s app_name, u replaces_id, s app_icon, s summary, s body, as actions,
//! a{sv} hints, i expire_timeout) -> u id`; `replaces_id = 0` creates a new notification,
//! otherwise the server atomically replaces that one and returns the same id;
//! `expire_timeout = -1` uses the server default, `0` never expires. Hints: `urgency` (BYTE,
//! 0 low / 1 normal / 2 critical; critical should not auto-expire), `transient` (BOOLEAN,
//! bypass the server's persistence), `desktop-entry` (STRING, desktop file name without
//! `.desktop`).

use std::collections::HashMap;
use std::time::Duration;

use thiserror::Error;

const NOTIFY_DEST: &str = "org.freedesktop.Notifications";
const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFY_IFACE: &str = "org.freedesktop.Notifications";

/// `app_name` sent with every notification.
pub const APP_NAME: &str = "v2c";
/// `desktop-entry` hint: the Tauri bundle installs `v2c.desktop` (`productName`).
pub const DESKTOP_ENTRY: &str = "v2c";
/// Upper bound for one D-Bus call, so a hung notification daemon cannot stall the caller.
pub const DBUS_TIMEOUT: Duration = Duration::from_secs(2);

/// What the app is doing, as far as the user needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotifyStatus {
    Recording,
    Recognizing,
    Polishing,
    /// Hands-free recording started (double tap): tells the user how to stop.
    HandsFree,
    /// Text was typed at the cursor.
    Done,
    /// Text could not be pasted automatically; it is on the clipboard for the user.
    CopiedToClipboard,
    /// Nothing was typed (any cause); the text is in v2c's encrypted input history (T3.6).
    TextKept,
    /// Nothing was typed and the history could not take it: the text is listed on the input
    /// history page only until v2c exits (T3.6).
    TextKeptUntilExit,
    Error(NotifyErrorKind),
}

/// Error states shown to the user. Carries no error text, so nothing user-supplied can leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotifyErrorKind {
    /// The user cancelled (or the system refused) the RemoteDesktop "send keys" consent.
    InjectPermissionDenied,
    /// No background clipboard access (GNOME < 45 has no Clipboard portal).
    ClipboardUnsupported,
    /// No global hotkey backend works (no GlobalShortcuts portal, evdev unusable).
    HotkeyUnavailable,
    /// evdev is the only hotkey backend but the user is not in the `input` group.
    InputGroupRequired,
    /// Speech recognition failed (provider error, timeout, empty audio).
    RecognitionFailed,
    /// The connection to ibus-daemon was lost (daemon restarted); v2c is reconnecting.
    ImeDisconnected,
    /// The v2c IBus engine could not be started; the fallback hotkey / injection is used.
    ImeUnavailable,
    /// No usable microphone.
    MicrophoneUnavailable,
    /// Anything else.
    Other,
}

/// `urgency` hint values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low = 0,
    Normal = 1,
    Critical = 2,
}

/// Content of one notification. All text is static UI text (Traditional Chinese, Taiwan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notification {
    pub summary: &'static str,
    pub body: &'static str,
    /// Freedesktop icon name.
    pub icon: &'static str,
    pub urgency: Urgency,
    /// Short-lived progress states: kept out of the notification list.
    pub transient: bool,
    /// Milliseconds; `-1` = server default.
    pub expire_timeout_ms: i32,
}

/// Maps a status to its notification. Pure; never takes or returns user text.
pub fn notification_for(status: NotifyStatus) -> Notification {
    // Normal urgency throughout: GNOME Shell does not pop up banners for low-urgency
    // notifications, and critical ones never expire.
    let progress = |summary, body, icon| Notification {
        summary,
        body,
        icon,
        urgency: Urgency::Normal,
        transient: true,
        expire_timeout_ms: -1,
    };
    let lasting = |summary, body, icon, expire_timeout_ms| Notification {
        summary,
        body,
        icon,
        urgency: Urgency::Normal,
        transient: false,
        expire_timeout_ms,
    };
    const ERR_ICON: &str = "dialog-warning";
    match status {
        NotifyStatus::Recording => {
            progress("錄音中", "放開熱鍵後開始辨識", "audio-input-microphone")
        }
        NotifyStatus::Recognizing => progress("辨識中", "正在把語音轉成文字", "content-loading"),
        NotifyStatus::Polishing => progress("潤飾中", "正在整理文字", "content-loading"),
        NotifyStatus::HandsFree => progress("免持模式", "按右 Ctrl 結束", "audio-input-microphone"),
        NotifyStatus::Done => Notification {
            expire_timeout_ms: 1500,
            ..progress("完成", "文字已輸入到游標處", "emblem-ok")
        },
        NotifyStatus::CopiedToClipboard => lasting(
            "已複製到剪貼簿",
            "無法自動貼上，請按 Ctrl+V 貼上",
            "edit-paste",
            8000,
        ),
        NotifyStatus::TextKept => lasting(
            "文字沒有輸入",
            "文字沒有輸入，已保留在 v2c 的「輸入紀錄」",
            "document-save",
            8000,
        ),
        NotifyStatus::TextKeptUntilExit => lasting(
            "文字沒有輸入（暫存）",
            "文字沒有輸入，已暫時保留在 v2c 的「輸入紀錄」（結束 v2c 後消失）",
            "document-save",
            8000,
        ),
        NotifyStatus::Error(kind) => {
            let (summary, body) = match kind {
                NotifyErrorKind::InjectPermissionDenied => {
                    ("權限不足", "未允許 v2c 傳送按鍵，文字沒有自動輸入")
                }
                NotifyErrorKind::ClipboardUnsupported => (
                    "不支援剪貼簿",
                    "這個桌面環境不允許背景程式設定剪貼簿（需 GNOME 45 以上）",
                ),
                NotifyErrorKind::HotkeyUnavailable => (
                    "熱鍵無法使用",
                    "找不到可用的全域熱鍵方式，請參考 Linux 設定說明",
                ),
                NotifyErrorKind::InputGroupRequired => {
                    ("熱鍵權限不足", "請把使用者加入 input 群組後重新登入")
                }
                NotifyErrorKind::RecognitionFailed => ("辨識失敗", "請再試一次"),
                NotifyErrorKind::ImeDisconnected => {
                    ("輸入法連線中斷", "v2c 與 IBus 的連線中斷，正在重新連線")
                }
                NotifyErrorKind::ImeUnavailable => (
                    "v2c 輸入法無法啟動",
                    "改用備援熱鍵與輸入方式，請參考 Linux 設定說明",
                ),
                NotifyErrorKind::MicrophoneUnavailable => {
                    ("找不到麥克風", "請確認麥克風已連接並允許使用")
                }
                NotifyErrorKind::Other => ("發生錯誤", "請稍後再試"),
            };
            lasting(summary, body, ERR_ICON, 5000)
        }
    }
}

/// A notification could not be shown. Callers should ignore it; it never means the
/// dictation itself failed.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum NotifyError {
    #[error("notification D-Bus call failed: {0}")]
    Dbus(String),
}

/// Shows status notifications. Implementations must not block for long and must not panic.
pub trait Notifier: Send {
    /// Shows `status`, replacing the bubble shown by the previous call if there is one.
    fn show(&mut self, status: NotifyStatus) -> Result<(), NotifyError>;
    /// Closes the current bubble (if any). The next [`Notifier::show`] starts a new one.
    fn dismiss(&mut self) -> Result<(), NotifyError>;
}

/// One `Notify` call, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyRequest {
    pub replaces_id: u32,
    pub notification: Notification,
}

/// The two D-Bus methods [`DbusNotifier`] needs. The real one is [`SessionBus`]; tests use a
/// recorder.
pub trait NotifyBus: Send {
    /// `Notify`; returns the server's notification id.
    fn notify(&mut self, req: &NotifyRequest) -> Result<u32, NotifyError>;
    /// `CloseNotification`.
    fn close(&mut self, id: u32) -> Result<(), NotifyError>;
}

/// [`NotifyBus`] over the user's session bus (pure-Rust zbus, blocking API).
pub struct SessionBus {
    conn: zbus::blocking::Connection,
}

impl SessionBus {
    /// Connects to the session bus with a [`DBUS_TIMEOUT`] method timeout. Does not call the
    /// notification daemon yet (nothing is shown).
    pub fn connect() -> Result<Self, NotifyError> {
        let conn = zbus::blocking::connection::Builder::session()
            .and_then(|b| b.method_timeout(DBUS_TIMEOUT).build())
            .map_err(dbus_err)?;
        Ok(Self { conn })
    }

    fn proxy(&self) -> Result<zbus::blocking::Proxy<'_>, NotifyError> {
        zbus::blocking::Proxy::new(&self.conn, NOTIFY_DEST, NOTIFY_PATH, NOTIFY_IFACE)
            .map_err(dbus_err)
    }
}

fn dbus_err(e: zbus::Error) -> NotifyError {
    NotifyError::Dbus(e.to_string())
}

impl NotifyBus for SessionBus {
    fn notify(&mut self, req: &NotifyRequest) -> Result<u32, NotifyError> {
        use zbus::zvariant::Value;
        let n = &req.notification;
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("urgency", Value::U8(n.urgency as u8));
        hints.insert("desktop-entry", Value::from(DESKTOP_ENTRY));
        if n.transient {
            hints.insert("transient", Value::Bool(true));
        }
        let actions: Vec<&str> = Vec::new();
        let reply = self
            .proxy()?
            .call_method(
                "Notify",
                &(
                    APP_NAME,
                    req.replaces_id,
                    n.icon,
                    n.summary,
                    n.body,
                    actions,
                    hints,
                    n.expire_timeout_ms,
                ),
            )
            .map_err(dbus_err)?;
        reply.body().deserialize::<u32>().map_err(dbus_err)
    }

    fn close(&mut self, id: u32) -> Result<(), NotifyError> {
        self.proxy()?
            .call_method("CloseNotification", &(id,))
            .map_err(dbus_err)?;
        Ok(())
    }
}

/// [`Notifier`] over `org.freedesktop.Notifications`, reusing one bubble via `replaces_id`.
pub struct DbusNotifier<B: NotifyBus = SessionBus> {
    bus: B,
    /// Id of the bubble currently on screen (`0` = none).
    current: u32,
}

impl DbusNotifier<SessionBus> {
    /// Connects to the session bus. If this fails, run without notifications.
    pub fn connect() -> Result<Self, NotifyError> {
        Ok(Self::with_bus(SessionBus::connect()?))
    }
}

impl<B: NotifyBus> DbusNotifier<B> {
    pub fn with_bus(bus: B) -> Self {
        Self { bus, current: 0 }
    }

    /// Id of the bubble the next [`Notifier::show`] will replace (`0` = a new one).
    pub fn current_id(&self) -> u32 {
        self.current
    }
}

impl<B: NotifyBus> Notifier for DbusNotifier<B> {
    fn show(&mut self, status: NotifyStatus) -> Result<(), NotifyError> {
        let req = NotifyRequest {
            replaces_id: self.current,
            notification: notification_for(status),
        };
        match self.bus.notify(&req) {
            Ok(id) => {
                // The server may hand out a new id (e.g. the old bubble was already closed).
                self.current = id;
                Ok(())
            }
            Err(e) => {
                // The daemon may have restarted; don't keep pointing at a stale id.
                self.current = 0;
                Err(e)
            }
        }
    }

    fn dismiss(&mut self) -> Result<(), NotifyError> {
        let id = std::mem::take(&mut self.current);
        if id == 0 {
            return Ok(());
        }
        self.bus.close(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const ALL: [NotifyStatus; 17] = [
        NotifyStatus::Recording,
        NotifyStatus::Recognizing,
        NotifyStatus::Polishing,
        NotifyStatus::HandsFree,
        NotifyStatus::Done,
        NotifyStatus::CopiedToClipboard,
        NotifyStatus::TextKept,
        NotifyStatus::TextKeptUntilExit,
        NotifyStatus::Error(NotifyErrorKind::ImeDisconnected),
        NotifyStatus::Error(NotifyErrorKind::ImeUnavailable),
        NotifyStatus::Error(NotifyErrorKind::InjectPermissionDenied),
        NotifyStatus::Error(NotifyErrorKind::ClipboardUnsupported),
        NotifyStatus::Error(NotifyErrorKind::HotkeyUnavailable),
        NotifyStatus::Error(NotifyErrorKind::InputGroupRequired),
        NotifyStatus::Error(NotifyErrorKind::RecognitionFailed),
        NotifyStatus::Error(NotifyErrorKind::MicrophoneUnavailable),
        NotifyStatus::Error(NotifyErrorKind::Other),
    ];

    #[test]
    fn mapping_table() {
        let cases: [(NotifyStatus, &str, bool); 8] = [
            (NotifyStatus::Recording, "錄音中", true),
            (NotifyStatus::HandsFree, "免持模式", true),
            (NotifyStatus::Recognizing, "辨識中", true),
            (NotifyStatus::Polishing, "潤飾中", true),
            (NotifyStatus::Done, "完成", true),
            (NotifyStatus::CopiedToClipboard, "已複製到剪貼簿", false),
            (
                NotifyStatus::Error(NotifyErrorKind::InjectPermissionDenied),
                "權限不足",
                false,
            ),
            (
                NotifyStatus::Error(NotifyErrorKind::ClipboardUnsupported),
                "不支援剪貼簿",
                false,
            ),
        ];
        for (status, summary, transient) in cases {
            let n = notification_for(status);
            assert_eq!(n.summary, summary, "{status:?}");
            assert_eq!(n.transient, transient, "{status:?}");
        }
        assert!(notification_for(NotifyStatus::CopiedToClipboard)
            .body
            .contains("Ctrl+V"));
        assert_eq!(
            notification_for(NotifyStatus::Error(NotifyErrorKind::HotkeyUnavailable)).summary,
            "熱鍵無法使用"
        );
        // Kept text must tell the user where it is, and must stay on screen.
        for status in [NotifyStatus::TextKept, NotifyStatus::TextKeptUntilExit] {
            let kept = notification_for(status);
            assert!(
                kept.body.contains("輸入紀錄") && !kept.transient,
                "{status:?}"
            );
            // Neutral about the cause (no focused field, disconnect, permission, ...).
            assert!(!kept.body.contains("輸入法"), "{status:?}");
        }
    }

    #[test]
    fn every_status_has_text_and_sane_hints() {
        for status in ALL {
            let n = notification_for(status);
            assert!(!n.summary.is_empty() && !n.body.is_empty(), "{status:?}");
            assert!(!n.icon.is_empty(), "{status:?}");
            // Critical never expires; nothing here should stick on screen forever.
            assert_ne!(n.urgency, Urgency::Critical, "{status:?}");
            assert!(n.expire_timeout_ms != 0, "{status:?}");
            if let NotifyStatus::Error(_) = status {
                assert!(!n.transient, "errors must stay visible: {status:?}");
            }
        }
    }

    #[test]
    fn error_kinds_have_distinct_summaries() {
        let mut seen = std::collections::HashSet::new();
        for status in ALL {
            assert!(seen.insert(notification_for(status).summary), "{status:?}");
        }
    }

    /// Records every bus call; can be told to fail or to hand out fresh ids.
    #[derive(Clone, Default)]
    struct Recorder {
        log: Arc<Mutex<Vec<Call>>>,
        fail: Arc<Mutex<bool>>,
        next_id: Arc<Mutex<u32>>,
        /// When set, ignores `replaces_id` and always returns a fresh id.
        fresh_ids: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Notify(u32, &'static str),
        Close(u32),
    }

    impl Recorder {
        fn calls(&self) -> Vec<Call> {
            self.log.lock().unwrap().clone()
        }
        fn set_fail(&self, fail: bool) {
            *self.fail.lock().unwrap() = fail;
        }
    }

    impl NotifyBus for Recorder {
        fn notify(&mut self, req: &NotifyRequest) -> Result<u32, NotifyError> {
            self.log
                .lock()
                .unwrap()
                .push(Call::Notify(req.replaces_id, req.notification.summary));
            if *self.fail.lock().unwrap() {
                return Err(NotifyError::Dbus(
                    "org.freedesktop.DBus.Error.ServiceUnknown".into(),
                ));
            }
            if req.replaces_id != 0 && !self.fresh_ids {
                return Ok(req.replaces_id);
            }
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            Ok(*next)
        }
        fn close(&mut self, id: u32) -> Result<(), NotifyError> {
            self.log.lock().unwrap().push(Call::Close(id));
            if *self.fail.lock().unwrap() {
                return Err(NotifyError::Dbus("closed".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn one_session_updates_one_bubble() {
        let rec = Recorder::default();
        let mut n = DbusNotifier::with_bus(rec.clone());
        assert_eq!(n.current_id(), 0);
        for s in [
            NotifyStatus::Recording,
            NotifyStatus::Recognizing,
            NotifyStatus::Polishing,
            NotifyStatus::Done,
        ] {
            n.show(s).unwrap();
        }
        assert_eq!(
            rec.calls(),
            vec![
                Call::Notify(0, "錄音中"),
                Call::Notify(1, "辨識中"),
                Call::Notify(1, "潤飾中"),
                Call::Notify(1, "完成"),
            ]
        );
        assert_eq!(n.current_id(), 1);
    }

    #[test]
    fn follows_id_returned_by_server() {
        let rec = Recorder {
            fresh_ids: true,
            ..Recorder::default()
        };
        let mut n = DbusNotifier::with_bus(rec.clone());
        n.show(NotifyStatus::Recording).unwrap();
        n.show(NotifyStatus::Recognizing).unwrap();
        n.show(NotifyStatus::Done).unwrap();
        assert_eq!(
            rec.calls(),
            vec![
                Call::Notify(0, "錄音中"),
                Call::Notify(1, "辨識中"),
                Call::Notify(2, "完成"),
            ]
        );
    }

    #[test]
    fn dismiss_closes_and_next_show_starts_new_bubble() {
        let rec = Recorder::default();
        let mut n = DbusNotifier::with_bus(rec.clone());
        n.dismiss().unwrap(); // nothing shown yet: no bus call
        n.show(NotifyStatus::Recording).unwrap();
        n.dismiss().unwrap();
        n.show(NotifyStatus::Recording).unwrap();
        assert_eq!(
            rec.calls(),
            vec![
                Call::Notify(0, "錄音中"),
                Call::Close(1),
                Call::Notify(0, "錄音中"),
            ]
        );
    }

    #[test]
    fn dbus_failure_is_an_ignorable_error_and_recovers() {
        let rec = Recorder::default();
        let mut n = DbusNotifier::with_bus(rec.clone());
        n.show(NotifyStatus::Recording).unwrap();
        rec.set_fail(true);
        let err = n.show(NotifyStatus::Recognizing).unwrap_err();
        assert!(matches!(err, NotifyError::Dbus(_)));
        assert_eq!(n.current_id(), 0, "stale id dropped after failure");
        assert!(n.dismiss().is_ok(), "nothing to close after failure");

        // The pipeline ignores the result (`let _ =`) and keeps going; no panic.
        let _ = n.show(NotifyStatus::Polishing);

        rec.set_fail(false);
        n.show(NotifyStatus::Done).unwrap();
        assert_eq!(rec.calls().last(), Some(&Call::Notify(0, "完成")));
        assert_ne!(n.current_id(), 0);
    }

    #[test]
    fn close_failure_still_forgets_id() {
        let rec = Recorder::default();
        let mut n = DbusNotifier::with_bus(rec.clone());
        n.show(NotifyStatus::Done).unwrap();
        rec.set_fail(true);
        assert!(n.dismiss().is_err());
        assert_eq!(n.current_id(), 0);
    }

    #[test]
    fn notifier_is_object_safe_and_send() {
        fn takes(_: Box<dyn Notifier>) {}
        takes(Box::new(DbusNotifier::with_bus(Recorder::default())));
    }

    /// Pops up real notifications on this desktop. Never run by agents or CI:
    /// `cargo test -p v2c-platform -- --ignored linux_real_notify`
    #[test]
    #[ignore = "shows real notifications on the desktop"]
    fn linux_real_notify() {
        let mut n = DbusNotifier::connect().expect("session bus");
        for s in [
            NotifyStatus::Recording,
            NotifyStatus::Recognizing,
            NotifyStatus::Polishing,
            NotifyStatus::Done,
        ] {
            n.show(s).expect("Notify");
            std::thread::sleep(Duration::from_millis(800));
        }
        n.show(NotifyStatus::CopiedToClipboard).expect("Notify");
        std::thread::sleep(Duration::from_secs(2));
        n.dismiss().expect("CloseNotification");
    }
}
