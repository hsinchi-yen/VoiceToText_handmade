//! Clipboard + Ctrl+V injection on Linux (T3.2, `tasks/plan.md` sections 4 and 9.2).
//!
//! On GNOME Wayland the RemoteDesktop portal cannot type characters outside the keyboard
//! layout (mutter drops such keysyms), so Chinese cannot be typed key by key. Every
//! [`Action::Text`] and [`Action::Paste`] therefore becomes a paste; [`Action::NewLine`]
//! is sent as key codes (Enter, or Shift+Enter for chat apps).
//!
//! Flow of [`LinuxInjector::run`] (same rules as Windows, see `windows/inject.rs`):
//! 1. `prepare` both backends (the portal session and its consent dialog);
//! 2. wait up to [`InjectConfig::modifier_wait`] for Ctrl/Shift/Alt/Super to be released
//!    (skipped when the backend reports [`ModifierState::Unknown`]);
//! 3. back up the clipboard;
//! 4. per paste: set the text, Ctrl+V, wait [`InjectConfig::settle`];
//! 5. restore the backup only if the clipboard still holds what we set.
//!
//! Every key that was pressed is released again, even when a later event fails. Errors
//! never contain the text; this module logs nothing.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use v2c_core::config::NewlineKey;
use v2c_core::inject_plan::Action;

use super::caps::{LinuxCaps, Portal};
use super::clipboard::{ClipboardBackend, PortalClipboard};
use super::keys::{
    KeyBackend, ModifierState, PortalKeys, PortalSession, KEY_ENTER, KEY_LEFTCTRL, KEY_LEFTSHIFT,
    KEY_V,
};
use crate::{InjectError, Injector};

/// Timing of the injection flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectConfig {
    /// Longest wait for the user to let go of the modifier keys (plan section 4: 500 ms).
    pub modifier_wait: Duration,
    /// Pause between modifier polls.
    pub modifier_poll: Duration,
    /// Time the target app gets to read the clipboard after Ctrl+V (plan section 4: 150 ms).
    pub settle: Duration,
}

impl Default for InjectConfig {
    fn default() -> Self {
        Self {
            modifier_wait: Duration::from_millis(500),
            modifier_poll: Duration::from_millis(5),
            settle: Duration::from_millis(150),
        }
    }
}

/// What happened to the user's clipboard after a successful run that pasted.
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
            Self::Restored => "restored",
            Self::SkippedChangedByOther => "skipped_changed_by_other",
        }
    }
}

/// [`Injector`] over a clipboard and a key backend.
pub struct LinuxInjector<C, K> {
    clipboard: C,
    keys: K,
    config: InjectConfig,
    last_restore: Mutex<Option<RestoreOutcome>>,
}

impl<C: ClipboardBackend, K: KeyBackend> LinuxInjector<C, K> {
    pub fn new(clipboard: C, keys: K, config: InjectConfig) -> Self {
        Self {
            clipboard,
            keys,
            config,
            last_restore: Mutex::new(None),
        }
    }

    /// Outcome of the most recent run that pasted and succeeded; `None` if it failed or
    /// had no paste. For logging (`outcome.as_str()`).
    pub fn last_restore_outcome(&self) -> Option<RestoreOutcome> {
        self.last_restore.lock().ok().and_then(|g| *g)
    }

    fn wait_for_modifiers_released(&self) -> Result<(), InjectError> {
        let deadline = Instant::now() + self.config.modifier_wait;
        loop {
            match self.keys.modifiers()? {
                ModifierState::Released | ModifierState::Unknown => return Ok(()),
                ModifierState::Held if Instant::now() >= deadline => {
                    // Ctrl+V with Alt held, or Enter with Ctrl held, would be another shortcut.
                    return Err(InjectError::ModifiersHeld);
                }
                ModifierState::Held => std::thread::sleep(self.config.modifier_poll),
            }
        }
    }

    /// Presses `mods` then `key`, then releases in reverse. Every key that went down is
    /// released even if a later event fails. Returns whether `key` went down, and the first
    /// error.
    fn chord(&self, mods: &[u16], key: u16) -> (bool, Result<(), InjectError>) {
        let mut down: Vec<u16> = Vec::new();
        let mut first_err = None;
        for &code in mods.iter().chain(std::iter::once(&key)) {
            match self.keys.key(code, true) {
                Ok(()) => down.push(code),
                Err(e) => {
                    first_err = Some(e);
                    break;
                }
            }
        }
        let key_went_down = down.last() == Some(&key);
        for &code in down.iter().rev() {
            if let Err(e) = self.keys.key(code, false) {
                first_err.get_or_insert(e);
            }
        }
        (key_went_down, first_err.map_or(Ok(()), Err))
    }

    fn newline(&self, key: NewlineKey) -> Result<(), InjectError> {
        let mods: &[u16] = match key {
            NewlineKey::Enter => &[],
            NewlineKey::ShiftEnter => &[KEY_LEFTSHIFT],
        };
        self.chord(mods, KEY_ENTER).1
    }

    fn run_inner(&self, actions: &[Action]) -> Result<Option<RestoreOutcome>, InjectError> {
        self.clipboard.prepare()?;
        self.keys.prepare()?;
        self.wait_for_modifiers_released()?;

        let pastes = actions.iter().any(|a| !matches!(a, Action::NewLine(_)));
        let mut backup = if pastes {
            Some(self.clipboard.backup()?)
        } else {
            None
        };
        let mut have_set = false;
        // A key release that failed after the paste went through: reported at the end.
        let mut late_error = None;

        for action in actions {
            let text = match action {
                Action::NewLine(key) => {
                    self.newline(*key)?;
                    continue;
                }
                Action::Text(t) | Action::Paste(t) => t,
            };
            if text.is_empty() {
                continue;
            }
            if have_set && !self.clipboard.still_ours()? {
                // Someone copied something between our pastes: theirs is now what to put back.
                backup = Some(self.clipboard.backup()?);
            }
            if let Err(e) = self.clipboard.set_text(text) {
                if let Some(b) = backup.as_ref() {
                    let _ = self.clipboard.restore(b);
                }
                return Err(e);
            }
            have_set = true;
            let (pasted, r) = self.chord(&[KEY_LEFTCTRL], KEY_V);
            if !pasted {
                // Text deliberately left on the clipboard so the user can paste by hand.
                return Err(InjectError::Os(format!(
                    "Ctrl+V not sent ({}); text left on the clipboard",
                    r.err().map_or_else(String::new, |e| e.to_string())
                )));
            }
            if let Err(e) = r {
                // V went down, so the app pasted; retrying would paste twice.
                late_error.get_or_insert(InjectError::Os(format!(
                    "pasted, but releasing keys failed: {e}"
                )));
            }
            std::thread::sleep(self.config.settle);
        }

        let outcome = match (have_set, backup) {
            (true, Some(b)) => {
                if self.clipboard.still_ours()? {
                    self.clipboard.restore(&b).map_err(|e| {
                        InjectError::Os(format!("pasted, but restoring the clipboard failed: {e}"))
                    })?;
                    Some(RestoreOutcome::Restored)
                } else {
                    Some(RestoreOutcome::SkippedChangedByOther)
                }
            }
            _ => None,
        };
        match late_error {
            Some(e) => Err(e),
            None => Ok(outcome),
        }
    }
}

impl<C: ClipboardBackend, K: KeyBackend> Injector for LinuxInjector<C, K> {
    fn run(&self, actions: &[Action]) -> Result<(), InjectError> {
        if let Ok(mut g) = self.last_restore.lock() {
            *g = None;
        }
        if actions.is_empty() {
            return Ok(());
        }
        let outcome = self.run_inner(actions)?;
        if let Ok(mut g) = self.last_restore.lock() {
            *g = outcome;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Backend selection (pure)
// ---------------------------------------------------------------------------------------

/// The injection backend that can work on this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectBackend {
    /// RemoteDesktop portal for keys + Clipboard portal for the text, on one session.
    Portal,
}

/// Lowest RemoteDesktop version that reports `clipboard_enabled` from `Start`.
const MIN_REMOTE_DESKTOP_FOR_CLIPBOARD: u32 = 2;

/// Decides from the probed capabilities whether clipboard injection is possible. Every
/// missing piece is listed in the [`InjectError::Unsupported`] text. Read-only.
pub fn select_injector(caps: &LinuxCaps) -> Result<InjectBackend, InjectError> {
    let mut reasons = Vec::new();
    if let Some(e) = &caps.portal_error {
        reasons.push(format!("the desktop portal could not be reached ({e})"));
    } else {
        match caps.clipboard {
            Portal::Missing => reasons.push(
                "the Clipboard portal is missing (needs xdg-desktop-portal-gnome 45 / \
                 GNOME 45 or newer), so v2c cannot set the clipboard in the background"
                    .to_string(),
            ),
            Portal::Present { .. } => {}
        }
        match caps.remote_desktop {
            Portal::Missing => reasons
                .push("the RemoteDesktop portal is missing, so v2c cannot press Ctrl+V".into()),
            Portal::Present { version: Some(v) } if v < MIN_REMOTE_DESKTOP_FOR_CLIPBOARD => reasons
                .push(format!(
                    "the RemoteDesktop portal is version {v}; clipboard sharing needs \
                     version {MIN_REMOTE_DESKTOP_FOR_CLIPBOARD}"
                )),
            Portal::Present { .. } => {}
        }
    }
    if reasons.is_empty() {
        Ok(InjectBackend::Portal)
    } else {
        Err(InjectError::Unsupported(reasons.join("; ")))
    }
}

/// Builds the portal injector if [`select_injector`] allows it. Opens nothing: the
/// session (and its consent dialog) starts on the first [`Injector::run`].
pub fn portal_injector(
    caps: &LinuxCaps,
    config: InjectConfig,
) -> Result<LinuxInjector<PortalClipboard, PortalKeys>, InjectError> {
    select_injector(caps)?;
    let session: Arc<PortalSession> = PortalSession::new(true);
    Ok(LinuxInjector::new(
        PortalClipboard::new(session.clone()),
        PortalKeys::new(session),
        config,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::caps::{DeviceAccess, EvdevCaps, SessionType};
    use crate::linux::clipboard::ClipContent;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        ClipPrepare,
        KeysPrepare,
        Modifiers,
        Backup,
        Set(String),
        StillOurs,
        Restore(ClipContent),
        Down(u16),
        Up(u16),
    }
    use Call::*;

    type Log = Rc<RefCell<Vec<Call>>>;

    struct MockClipboard {
        log: Log,
        content: RefCell<ClipContent>,
        /// `still_ours` answers, front first; `true` once empty.
        still_ours: RefCell<VecDeque<bool>>,
        fail_prepare: Option<InjectError>,
        /// Fail the n-th `set_text` (0-based).
        fail_set: Option<usize>,
        sets: Cell<usize>,
    }

    impl MockClipboard {
        fn new(log: &Log) -> Self {
            Self {
                log: log.clone(),
                content: RefCell::new(original()),
                still_ours: RefCell::default(),
                fail_prepare: None,
                fail_set: None,
                sets: Cell::new(0),
            }
        }
    }

    fn original() -> ClipContent {
        ClipContent(vec![("text/plain".into(), b"user's own".to_vec())])
    }

    fn theirs() -> ClipContent {
        ClipContent(vec![("text/plain".into(), b"copied meanwhile".to_vec())])
    }

    fn text_content(t: &str) -> ClipContent {
        ClipContent(vec![("text/plain".into(), t.as_bytes().to_vec())])
    }

    impl ClipboardBackend for MockClipboard {
        fn prepare(&self) -> Result<(), InjectError> {
            self.log.borrow_mut().push(ClipPrepare);
            self.fail_prepare.clone().map_or(Ok(()), Err)
        }
        fn backup(&self) -> Result<ClipContent, InjectError> {
            self.log.borrow_mut().push(Backup);
            Ok(self.content.borrow().clone())
        }
        fn set_text(&self, text: &str) -> Result<(), InjectError> {
            self.log.borrow_mut().push(Set(text.into()));
            let n = self.sets.get();
            self.sets.set(n + 1);
            if self.fail_set == Some(n) {
                return Err(InjectError::Os("set failed".into()));
            }
            *self.content.borrow_mut() = text_content(text);
            Ok(())
        }
        fn still_ours(&self) -> Result<bool, InjectError> {
            self.log.borrow_mut().push(StillOurs);
            let ours = self.still_ours.borrow_mut().pop_front().unwrap_or(true);
            if !ours {
                // Model another app's copy that happened just before this check.
                *self.content.borrow_mut() = theirs();
            }
            Ok(ours)
        }
        fn restore(&self, c: &ClipContent) -> Result<(), InjectError> {
            self.log.borrow_mut().push(Restore(c.clone()));
            *self.content.borrow_mut() = c.clone();
            Ok(())
        }
    }

    struct MockKeys {
        log: Log,
        modifiers: RefCell<VecDeque<ModifierState>>,
        default_modifiers: ModifierState,
        fail_prepare: Option<InjectError>,
        /// Fail the n-th key event (0-based, counting presses and releases).
        fail_event: Option<usize>,
        events: Cell<usize>,
    }

    impl MockKeys {
        fn new(log: &Log) -> Self {
            Self {
                log: log.clone(),
                modifiers: RefCell::default(),
                default_modifiers: ModifierState::Released,
                fail_prepare: None,
                fail_event: None,
                events: Cell::new(0),
            }
        }
    }

    impl KeyBackend for MockKeys {
        fn prepare(&self) -> Result<(), InjectError> {
            self.log.borrow_mut().push(KeysPrepare);
            self.fail_prepare.clone().map_or(Ok(()), Err)
        }
        fn modifiers(&self) -> Result<ModifierState, InjectError> {
            self.log.borrow_mut().push(Modifiers);
            Ok(self
                .modifiers
                .borrow_mut()
                .pop_front()
                .unwrap_or(self.default_modifiers))
        }
        fn key(&self, code: u16, pressed: bool) -> Result<(), InjectError> {
            let n = self.events.get();
            self.events.set(n + 1);
            if self.fail_event == Some(n) {
                return Err(InjectError::Os("portal call failed".into()));
            }
            self.log
                .borrow_mut()
                .push(if pressed { Down(code) } else { Up(code) });
            Ok(())
        }
    }

    fn config() -> InjectConfig {
        InjectConfig {
            modifier_wait: Duration::from_millis(30),
            modifier_poll: Duration::from_millis(1),
            settle: Duration::ZERO,
        }
    }

    fn injector(cb: MockClipboard, keys: MockKeys) -> LinuxInjector<MockClipboard, MockKeys> {
        LinuxInjector::new(cb, keys, config())
    }

    fn setup() -> (Log, MockClipboard, MockKeys) {
        let log = Log::default();
        let cb = MockClipboard::new(&log);
        let keys = MockKeys::new(&log);
        (log, cb, keys)
    }

    const CTRL_V: [Call; 4] = [Down(KEY_LEFTCTRL), Down(KEY_V), Up(KEY_V), Up(KEY_LEFTCTRL)];

    fn t(s: &str) -> Action {
        Action::Text(s.into())
    }

    /// Asserts no key is left pressed in the log.
    fn assert_all_released(log: &[Call]) {
        let mut down = std::collections::HashSet::new();
        for c in log {
            match c {
                Down(k) => assert!(down.insert(*k), "key {k} pressed twice"),
                Up(k) => assert!(down.remove(k), "key {k} released while up"),
                _ => {}
            }
        }
        assert!(down.is_empty(), "keys left pressed: {down:?}");
    }

    #[test]
    fn multi_line_with_shift_enter() {
        let (log, cb, keys) = setup();
        let inj = injector(cb, keys);
        let actions = [
            t("第一行"),
            Action::NewLine(NewlineKey::ShiftEnter),
            t("second"),
        ];
        assert_eq!(inj.run(&actions), Ok(()));
        let mut expected = vec![
            ClipPrepare,
            KeysPrepare,
            Modifiers,
            Backup,
            Set("第一行".into()),
        ];
        expected.extend(CTRL_V);
        expected.extend([
            Down(KEY_LEFTSHIFT),
            Down(KEY_ENTER),
            Up(KEY_ENTER),
            Up(KEY_LEFTSHIFT),
            StillOurs,
            Set("second".into()),
        ]);
        expected.extend(CTRL_V);
        expected.extend([StillOurs, Restore(original())]);
        assert_eq!(*log.borrow(), expected);
        assert_eq!(inj.last_restore_outcome(), Some(RestoreOutcome::Restored));
        assert_eq!(*inj.clipboard.content.borrow(), original());
    }

    #[test]
    fn plain_enter_and_paste_action() {
        let (log, cb, keys) = setup();
        let inj = injector(cb, keys);
        let actions = [
            Action::Paste("a\nb".into()),
            Action::NewLine(NewlineKey::Enter),
        ];
        assert_eq!(inj.run(&actions), Ok(()));
        let mut expected = vec![
            ClipPrepare,
            KeysPrepare,
            Modifiers,
            Backup,
            Set("a\nb".into()),
        ];
        expected.extend(CTRL_V);
        expected.extend([
            Down(KEY_ENTER),
            Up(KEY_ENTER),
            StillOurs,
            Restore(original()),
        ]);
        assert_eq!(*log.borrow(), expected);
    }

    #[test]
    fn only_newlines_never_touch_the_clipboard() {
        let (log, cb, keys) = setup();
        let inj = injector(cb, keys);
        assert_eq!(inj.run(&[Action::NewLine(NewlineKey::Enter)]), Ok(()));
        assert_eq!(
            *log.borrow(),
            vec![
                ClipPrepare,
                KeysPrepare,
                Modifiers,
                Down(KEY_ENTER),
                Up(KEY_ENTER)
            ]
        );
        assert_eq!(inj.last_restore_outcome(), None);
    }

    #[test]
    fn empty_actions_do_nothing() {
        let (log, cb, keys) = setup();
        let inj = injector(cb, keys);
        assert_eq!(inj.run(&[]), Ok(()));
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn waits_for_modifiers_then_proceeds() {
        let (log, cb, keys) = setup();
        keys.modifiers
            .borrow_mut()
            .extend([ModifierState::Held, ModifierState::Held]);
        let inj = injector(cb, keys);
        assert_eq!(inj.run(&[t("x")]), Ok(()));
        let l = log.borrow();
        assert_eq!(l.iter().filter(|c| **c == Modifiers).count(), 3);
        assert!(l.contains(&Set("x".into())));
    }

    #[test]
    fn modifier_timeout_sends_nothing() {
        let (log, cb, mut keys) = setup();
        keys.default_modifiers = ModifierState::Held;
        let inj = injector(cb, keys);
        let start = Instant::now();
        assert_eq!(inj.run(&[t("x")]), Err(InjectError::ModifiersHeld));
        assert!(start.elapsed() >= config().modifier_wait);
        let l = log.borrow();
        assert!(l
            .iter()
            .all(|c| matches!(c, ClipPrepare | KeysPrepare | Modifiers)));
        assert_eq!(*inj.clipboard.content.borrow(), original());
    }

    #[test]
    fn unknown_modifier_state_does_not_wait() {
        let (log, cb, mut keys) = setup();
        keys.default_modifiers = ModifierState::Unknown;
        let inj = LinuxInjector::new(
            cb,
            keys,
            InjectConfig {
                modifier_wait: Duration::from_secs(60),
                ..config()
            },
        );
        assert_eq!(inj.run(&[t("x")]), Ok(()));
        assert_eq!(log.borrow().iter().filter(|c| **c == Modifiers).count(), 1);
    }

    #[test]
    fn permission_denied_sends_nothing() {
        let (log, cb, mut keys) = setup();
        keys.fail_prepare = Some(InjectError::PermissionDenied);
        let inj = injector(cb, keys);
        assert_eq!(inj.run(&[t("x")]), Err(InjectError::PermissionDenied));
        assert_eq!(*log.borrow(), vec![ClipPrepare, KeysPrepare]);
        assert_eq!(inj.last_restore_outcome(), None);
    }

    #[test]
    fn unsupported_clipboard_sends_nothing() {
        let (log, mut cb, keys) = setup();
        cb.fail_prepare = Some(InjectError::Unsupported("no clipboard portal".into()));
        let inj = injector(cb, keys);
        assert!(matches!(
            inj.run(&[t("x")]),
            Err(InjectError::Unsupported(_))
        ));
        assert_eq!(*log.borrow(), vec![ClipPrepare]);
    }

    #[test]
    fn clipboard_set_failure_restores_and_sends_no_keys() {
        let (log, mut cb, keys) = setup();
        cb.fail_set = Some(0);
        let inj = injector(cb, keys);
        assert_eq!(
            inj.run(&[t("x")]),
            Err(InjectError::Os("set failed".into()))
        );
        assert_eq!(
            *log.borrow(),
            vec![
                ClipPrepare,
                KeysPrepare,
                Modifiers,
                Backup,
                Set("x".into()),
                Restore(original())
            ]
        );
        assert_eq!(*inj.clipboard.content.borrow(), original());
    }

    #[test]
    fn ctrl_v_failing_after_ctrl_down_still_releases_ctrl() {
        let (log, cb, mut keys) = setup();
        keys.fail_event = Some(1); // V down
        let inj = injector(cb, keys);
        let err = inj.run(&[t("x")]).unwrap_err();
        assert!(matches!(&err, InjectError::Os(m) if m.contains("text left on the clipboard")));
        let l = log.borrow();
        assert_eq!(l[l.len() - 2..], [Down(KEY_LEFTCTRL), Up(KEY_LEFTCTRL)]);
        assert_all_released(&l);
        // Not restored: the user can paste the text by hand.
        assert!(!l.iter().any(|c| matches!(c, Restore(_))));
        assert_eq!(*inj.clipboard.content.borrow(), text_content("x"));
    }

    #[test]
    fn v_release_failing_still_releases_ctrl_and_restores() {
        let (log, cb, mut keys) = setup();
        keys.fail_event = Some(2); // V up
        let inj = injector(cb, keys);
        let err = inj.run(&[t("x")]).unwrap_err();
        assert!(matches!(&err, InjectError::Os(m) if m.starts_with("pasted, but")));
        let l = log.borrow();
        assert!(l.contains(&Up(KEY_LEFTCTRL)));
        assert!(l.contains(&Restore(original())));
        // V's release was the failed call, so only Ctrl shows as released.
        assert_eq!(l.iter().filter(|c| matches!(c, Up(_))).count(), 1);
    }

    #[test]
    fn shift_enter_failing_still_releases_shift() {
        let (log, cb, mut keys) = setup();
        keys.fail_event = Some(1); // Enter down
        let inj = injector(cb, keys);
        assert!(inj.run(&[Action::NewLine(NewlineKey::ShiftEnter)]).is_err());
        let l = log.borrow();
        assert_eq!(l[l.len() - 2..], [Down(KEY_LEFTSHIFT), Up(KEY_LEFTSHIFT)]);
        assert_all_released(&l);
    }

    #[test]
    fn changed_by_someone_else_is_not_restored() {
        let (log, cb, keys) = setup();
        cb.still_ours.borrow_mut().push_back(false);
        let inj = injector(cb, keys);
        assert_eq!(inj.run(&[t("x")]), Ok(()));
        let l = log.borrow();
        assert_eq!(l.last(), Some(&StillOurs));
        assert!(!l.iter().any(|c| matches!(c, Restore(_))));
        assert_eq!(*inj.clipboard.content.borrow(), theirs());
        assert_eq!(
            inj.last_restore_outcome(),
            Some(RestoreOutcome::SkippedChangedByOther)
        );
    }

    #[test]
    fn change_between_pastes_is_what_gets_restored() {
        let (log, cb, keys) = setup();
        // Someone copies after the first paste, before the second.
        cb.still_ours.borrow_mut().push_back(false);
        let inj = injector(cb, keys);
        let actions = [t("a"), Action::NewLine(NewlineKey::Enter), t("b")];
        assert_eq!(inj.run(&actions), Ok(()));
        let l = log.borrow();
        assert_eq!(l.iter().filter(|c| **c == Backup).count(), 2);
        let second_backup = l.iter().rposition(|c| *c == Backup).unwrap();
        assert_eq!(l[second_backup - 1], StillOurs);
        assert_eq!(l[second_backup + 1], Set("b".into()));
        // Their copy, not the original, is what ends up on the clipboard.
        assert_eq!(l.last(), Some(&Restore(theirs())));
        assert_eq!(*inj.clipboard.content.borrow(), theirs());
        assert_eq!(inj.last_restore_outcome(), Some(RestoreOutcome::Restored));
    }

    #[test]
    fn errors_never_contain_the_text() {
        let secret = "機密內容 secret";
        for fail in [0usize, 1, 2, 3] {
            let (_log, cb, mut keys) = setup();
            keys.fail_event = Some(fail);
            let inj = injector(cb, keys);
            if let Err(e) = inj.run(&[Action::Text(secret.into())]) {
                let msg = format!("{e} {e:?}");
                assert!(!msg.contains("secret") && !msg.contains("機密"), "{msg}");
            }
        }
    }

    fn caps(rd: Portal, cb: Portal) -> LinuxCaps {
        LinuxCaps {
            session: SessionType::Wayland,
            desktop: Some("ubuntu:GNOME".into()),
            global_shortcuts: Portal::Missing,
            remote_desktop: rd,
            clipboard: cb,
            portal_error: None,
            evdev: EvdevCaps {
                keyboards: vec![],
                access: DeviceAccess::NoDevices,
            },
        }
    }

    #[test]
    fn gnome_42_is_unsupported_with_reasons() {
        let c = caps(Portal::Present { version: Some(1) }, Portal::Missing);
        let Err(InjectError::Unsupported(why)) = select_injector(&c) else {
            panic!("expected Unsupported");
        };
        assert!(why.contains("Clipboard portal is missing"), "{why}");
        assert!(why.contains("version 1"), "{why}");
        assert!(portal_injector(&c, InjectConfig::default()).is_err());
    }

    #[test]
    fn gnome_45_selects_portal() {
        let c = caps(
            Portal::Present { version: Some(2) },
            Portal::Present { version: Some(1) },
        );
        assert_eq!(select_injector(&c), Ok(InjectBackend::Portal));
        // Unknown version: allowed; `Start` reports clipboard_enabled at run time.
        let c = caps(
            Portal::Present { version: None },
            Portal::Present { version: None },
        );
        assert_eq!(select_injector(&c), Ok(InjectBackend::Portal));
        // Constructing opens nothing on the bus.
        assert!(portal_injector(&c, InjectConfig::default()).is_ok());
    }

    #[test]
    fn no_portal_at_all() {
        let c = caps(Portal::Missing, Portal::Missing);
        let Err(InjectError::Unsupported(why)) = select_injector(&c) else {
            panic!("expected Unsupported");
        };
        assert!(why.contains("RemoteDesktop portal is missing"), "{why}");
        let mut c = c;
        c.portal_error = Some("session bus: no address".into());
        let Err(InjectError::Unsupported(why)) = select_injector(&c) else {
            panic!("expected Unsupported");
        };
        assert!(why.contains("could not be reached"), "{why}");
    }

    #[test]
    fn unsupported_message_is_readable() {
        let e = InjectError::Unsupported("the Clipboard portal is missing".into());
        assert_eq!(
            e.to_string(),
            "clipboard injection not supported here: the Clipboard portal is missing"
        );
    }

    /// Real, read-only: probes this machine and runs the selection. Opens no portal
    /// session. Run with `-- --ignored linux_real --nocapture`.
    #[test]
    #[ignore = "reads the real session bus"]
    fn linux_real_select_injector() {
        let caps = crate::linux::caps::probe();
        let choice = select_injector(&caps);
        println!(
            "remote_desktop={:?} clipboard={:?}",
            caps.remote_desktop, caps.clipboard
        );
        match &choice {
            Ok(b) => println!("injector: {b:?}"),
            Err(e) => println!("injector: {e}"),
        }
        // T3.0 host (GNOME 42): no Clipboard portal.
        assert!(matches!(choice, Err(InjectError::Unsupported(_))));
    }
}
