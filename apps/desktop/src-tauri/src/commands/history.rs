//! Input history (T1.9): store setup, hourly purge, and the commands behind the 紀錄 page:
//! list, copy, re-output, delete, clear.
//!
//! The decrypted text only goes to the main window (list) and the clipboard / target app
//! (copy, re-output); it is never logged. Errors are returned as short codes. Every command
//! refuses callers other than the main window (`forbidden`), so the overlay webview cannot
//! read the history. SQLite and DPAPI calls run on the blocking pool.
//!
//! Re-output target: a background thread remembers the last foreground window that can take
//! text (another process, not the desktop/shell/taskbar, see
//! `v2c_platform::windows::foreground::is_text_target`), i.e. the window the user was in
//! before opening v2c. Re-output minimizes the main window, activates that remembered window,
//! and only types once the foreground *is* that window; otherwise the main window is
//! restored and `no_target_window` is returned. Injection is the normal path
//! (`inject_plan::plan` + the app's rules + `run_with_fallback`).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{Manager, State};
use tracing::{info, warn};
use v2c_core::config::HistoryConfig;
use v2c_core::history::{now_unix, History, HistoryStore};
use v2c_core::redact::Redacted;
use v2c_core::Mode;

use crate::app_state::AppState;
use crate::history_sink::{self, HistorySink};

/// Entries shown on the page (the store keeps at most `history.max_entries`).
const LIST_LIMIT: u32 = 200;
const PURGE_EVERY: Duration = Duration::from_secs(60 * 60);
/// The only webview allowed to use the history commands.
const MAIN_WINDOW: &str = "main";

/// Tauri-managed handle; `None` when the history could not be opened (or on platforms
/// without a data protector yet).
pub struct HistoryState(pub Option<Arc<History>>);

/// One row on the 紀錄 page.
#[derive(Clone, Serialize)]
pub struct HistoryItem {
    pub id: i64,
    /// Unix seconds.
    pub created_at: i64,
    pub app: String,
    /// `dictate` or `translate`.
    pub mode: &'static str,
    pub chars: u32,
    /// Decrypted text, for display in the main window only.
    pub text: String,
}

impl std::fmt::Debug for HistoryItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryItem")
            .field("id", &self.id)
            .field("created_at", &self.created_at)
            .field("app", &self.app)
            .field("mode", &self.mode)
            .field("chars", &self.chars)
            .field("text", &Redacted(&self.text))
            .finish()
    }
}

/// Opens `history.db`, purges expired entries, starts the hourly purge and the re-output
/// target tracker, installs the output sink, and manages [`HistoryState`]. Call in `setup`
/// before the controller starts.
pub fn init(app: &tauri::AppHandle) {
    let store = {
        let state = app.state::<AppState>();
        open_store(&state.paths.data_dir, &state.config.history)
    };
    if let Some(store) = &store {
        purge(store);
        let s = Arc::clone(store);
        tauri::async_runtime::spawn(async move {
            let mut every = tokio::time::interval(PURGE_EVERY);
            every.tick().await; // the first tick fires at once; startup already purged
            loop {
                every.tick().await;
                let s = Arc::clone(&s);
                let _ = tokio::task::spawn_blocking(move || purge(&s)).await;
            }
        });
        if store.config().enabled {
            history_sink::install(Arc::new(HistorySink::new(Arc::clone(store))));
        }
        target::start_tracking();
    }
    app.manage(HistoryState(store));
}

#[cfg(windows)]
fn open_store(data_dir: &Path, cfg: &HistoryConfig) -> Option<Arc<History>> {
    use v2c_platform::windows::dpapi::DpapiProtector;

    if let Err(e) = std::fs::create_dir_all(data_dir) {
        warn!(error = %e, "history: data directory unavailable");
        return None;
    }
    match History::open(
        &data_dir.join("history.db"),
        Arc::new(DpapiProtector::new()),
        cfg.clone(),
    ) {
        Ok(h) => {
            info!(enabled = cfg.enabled, "history opened");
            Some(Arc::new(h))
        }
        Err(e) => {
            warn!(error = %e, "history could not be opened");
            None
        }
    }
}

/// T3.6: AES-256-GCM with a key kept in the Secret Service (`linux::protect`). The key is
/// read lazily on the first write/read (off the UI thread: it may wait on an unlock prompt).
/// The source starts as `MustExist` and only allows creating a key once the database is
/// known to be empty, so a hidden or unreadable key never gets replaced while encrypted rows
/// exist.
#[cfg(target_os = "linux")]
fn open_store(data_dir: &Path, cfg: &HistoryConfig) -> Option<Arc<History>> {
    use v2c_platform::linux::protect::{AeadProtector, KeyPolicy, SecretServiceKeySource};

    if let Err(e) = std::fs::create_dir_all(data_dir) {
        warn!(error = %e, "history: data directory unavailable");
        return None;
    }
    let source = Arc::new(SecretServiceKeySource::new(KeyPolicy::MustExist));
    match History::open(
        &data_dir.join("history.db"),
        Arc::new(AeadProtector::with_source(Arc::clone(&source))),
        cfg.clone(),
    ) {
        Ok(h) => {
            match h.count() {
                Ok(0) => source.set_policy(KeyPolicy::CreateIfMissing),
                Ok(_) => {}
                Err(e) => warn!(error = %e, "history: row count failed; key must exist"),
            }
            info!(enabled = cfg.enabled, "history opened");
            Some(Arc::new(h))
        }
        Err(e) => {
            warn!(error = %e, "history could not be opened");
            None
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn open_store(_data_dir: &Path, _cfg: &HistoryConfig) -> Option<Arc<History>> {
    // No data protector on this platform; never store unencrypted text.
    warn!("history is not available on this platform yet");
    None
}

fn purge(store: &History) {
    match store.purge_expired(now_unix()) {
        Ok(removed) => info!(removed, "history purge"),
        Err(e) => warn!(error = %e, "history purge failed"),
    }
}

/// Decrypted entries, newest first.
pub fn list_items(store: &History) -> Result<Vec<HistoryItem>, &'static str> {
    let records = store.list_records(LIST_LIMIT).map_err(|e| {
        warn!(error = %e, "history list failed");
        "history_failed"
    })?;
    Ok(records
        .into_iter()
        .map(|r| HistoryItem {
            id: r.entry.id,
            created_at: r.entry.created_at,
            mode: match r.entry.mode {
                Mode::Dictate => "dictate",
                Mode::TranslateZhToEn => "translate",
            },
            app: r.entry.app,
            chars: r.chars,
            text: r.entry.text,
        })
        .collect())
}

/// Decrypted text of one entry.
pub fn entry_text(store: &History, id: i64) -> Result<String, &'static str> {
    match store.get(id) {
        Ok(Some(r)) => Ok(r.entry.text),
        Ok(None) => Err("history_not_found"),
        Err(e) => {
            warn!(error = %e, id, "history read failed");
            Err("history_failed")
        }
    }
}

/// Only the main window may use the history.
fn check_caller(label: &str) -> Result<(), &'static str> {
    if label == MAIN_WINDOW {
        Ok(())
    } else {
        warn!(window = label, "history command refused");
        Err("forbidden")
    }
}

/// Checks the caller and runs `f` with the store on the blocking pool.
async fn with_store<T, F>(
    window: &tauri::WebviewWindow,
    state: &State<'_, HistoryState>,
    f: F,
) -> Result<T, &'static str>
where
    T: Send + 'static,
    F: FnOnce(&History) -> Result<T, &'static str> + Send + 'static,
{
    check_caller(window.label())?;
    let store = Arc::clone(state.0.as_ref().ok_or("history_unavailable")?);
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .map_err(|_| "history_failed")?
}

#[tauri::command]
pub async fn history_list(
    window: tauri::WebviewWindow,
    state: State<'_, HistoryState>,
) -> Result<Vec<HistoryItem>, &'static str> {
    check_caller(window.label())?;
    // Linux: texts that could not be typed and are only kept in memory come first.
    let mut items = unsent::items(window.app_handle());
    match with_store(&window, &state, list_items).await {
        Ok(rows) => items.extend(rows),
        Err(code) if items.is_empty() => return Err(code),
        Err(_) => {}
    }
    Ok(items)
}

#[tauri::command]
pub async fn history_delete(
    id: i64,
    window: tauri::WebviewWindow,
    state: State<'_, HistoryState>,
) -> Result<(), &'static str> {
    check_caller(window.label())?;
    if let Some(result) = unsent::delete(window.app_handle(), id) {
        return result;
    }
    with_store(&window, &state, move |s| {
        s.delete(id).map_err(|e| {
            warn!(error = %e, id, "history delete failed");
            "history_failed"
        })
    })
    .await
}

#[tauri::command]
pub async fn history_clear(
    window: tauri::WebviewWindow,
    state: State<'_, HistoryState>,
) -> Result<(), &'static str> {
    check_caller(window.label())?;
    let had_unsent = unsent::clear(window.app_handle());
    let cleared = with_store(&window, &state, |s| {
        s.clear().map_err(|e| {
            warn!(error = %e, "history clear failed");
            "history_failed"
        })
    })
    .await;
    match cleared {
        Ok(()) => {}
        Err("history_unavailable") if had_unsent => {}
        Err(code) => return Err(code),
    }
    info!("history cleared by user");
    Ok(())
}

/// Puts the entry's text on the clipboard.
#[tauri::command]
pub async fn history_copy(
    id: i64,
    window: tauri::WebviewWindow,
    state: State<'_, HistoryState>,
) -> Result<(), &'static str> {
    check_caller(window.label())?;
    let chars = match unsent::text(window.app_handle(), id) {
        Some(text) => {
            let text = text?;
            tokio::task::spawn_blocking(move || {
                copy_to_clipboard(&text).map(|()| text.chars().count())
            })
            .await
            .map_err(|_| "history_failed")??
        }
        None => {
            with_store(&window, &state, move |s| {
                let text = entry_text(s, id)?;
                copy_to_clipboard(&text)?;
                Ok(text.chars().count())
            })
            .await?
        }
    };
    info!(id, chars, "history copied");
    Ok(())
}

#[cfg(windows)]
fn copy_to_clipboard(text: &str) -> Result<(), &'static str> {
    use v2c_platform::windows::clipboard::{copy_text, WinClipboard};
    WinClipboard::new()
        .and_then(|mut cb| copy_text(&mut cb, text))
        .map_err(|e| {
            warn!(error = %e, "history copy failed");
            "clipboard_failed"
        })
}

#[cfg(not(windows))]
fn copy_to_clipboard(_text: &str) -> Result<(), &'static str> {
    Err("platform_unsupported")
}

/// Types the entry's text into the window the user was in before opening v2c.
#[tauri::command]
pub async fn history_reoutput(
    id: i64,
    window: tauri::WebviewWindow,
    state: State<'_, HistoryState>,
    app_state: State<'_, AppState>,
) -> Result<(), &'static str> {
    check_caller(window.label())?;
    let text = match unsent::text(window.app_handle(), id) {
        Some(text) => text?,
        None => with_store(&window, &state, move |s| entry_text(s, id)).await?,
    };
    let rules = app_state.engine.config().apps.clone();
    target::reoutput(&window, text, rules).await?;
    info!(id, "history re-output");
    Ok(())
}

/// Maps injection errors to the controller's codes (the UI shares the messages). Written
/// without an exhaustive `match` so new `InjectError` variants fall back to `inject_failed`.
#[cfg(windows)]
fn inject_error_code(e: &v2c_platform::InjectError) -> &'static str {
    use v2c_platform::InjectError;
    if matches!(e, InjectError::Elevated) {
        "inject_elevated"
    } else if matches!(e, InjectError::Partial { .. }) {
        "inject_partial"
    } else {
        let msg = e.to_string().to_lowercase();
        if msg.contains("paste not implemented") {
            "inject_paste_unsupported"
        } else if msg.contains("modifier") {
            "inject_modifiers_held"
        } else {
            "inject_failed"
        }
    }
}

#[cfg(windows)]
mod target {
    //! Remembers the last text-target window and re-outputs into it.

    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::Once;
    use std::time::{Duration, Instant};

    use tracing::warn;
    use v2c_core::config::AppRule;
    use v2c_core::inject_plan::{plan, profile_for};
    use v2c_platform::windows::foreground::{
        self, activate_window, foreground_window, is_text_target, WindowId,
    };
    use v2c_platform::windows::inject::{run_with_fallback, SendInputInjector};

    use super::inject_error_code;

    const POLL_EVERY: Duration = Duration::from_millis(200);
    const ACTIVATE_WAIT: Duration = Duration::from_millis(1500);
    const ACTIVATE_POLL: Duration = Duration::from_millis(30);
    const ACTIVATE_RETRY: Duration = Duration::from_millis(300);
    /// Let the activated window settle its keyboard focus.
    const SETTLE: Duration = Duration::from_millis(120);

    /// Raw `WindowId` of the last foreground text target (0 = none). Only a handle, no text.
    static LAST_TARGET: AtomicIsize = AtomicIsize::new(0);

    pub fn start_tracking() {
        static STARTED: Once = Once::new();
        STARTED.call_once(|| {
            let spawned = std::thread::Builder::new()
                .name("v2c-history-target".into())
                .spawn(|| loop {
                    if let Some(w) = foreground_window().filter(|w| is_text_target(*w)) {
                        LAST_TARGET.store(w.as_raw(), Ordering::Relaxed);
                    }
                    std::thread::sleep(POLL_EVERY);
                });
            if spawned.is_err() {
                warn!("could not start the re-output target tracker");
            }
        });
    }

    fn last_target() -> Option<WindowId> {
        WindowId::from_raw(LAST_TARGET.load(Ordering::Relaxed)).filter(|w| is_text_target(*w))
    }

    pub async fn reoutput(
        window: &tauri::WebviewWindow,
        text: String,
        rules: Vec<AppRule>,
    ) -> Result<(), &'static str> {
        let Some(target) = last_target() else {
            return Err("no_target_window");
        };
        window.minimize().map_err(|e| {
            warn!(error = %e, "minimize failed");
            "window_failed"
        })?;
        let activated = tokio::task::spawn_blocking(move || activate(target))
            .await
            .unwrap_or(false);
        if !activated {
            let _ = window.unminimize();
            warn!("re-output: target window did not take the foreground");
            return Err("no_target_window");
        }
        tokio::task::spawn_blocking(move || {
            let exe = foreground::current_app().map(|a| a.exe).unwrap_or_default();
            let actions = plan(&text, &profile_for(&exe, &rules));
            run_with_fallback(&SendInputInjector, &actions).map_err(|e| {
                let code = inject_error_code(&e.error);
                warn!(code, app = %exe, "re-output injection failed");
                code
            })
        })
        .await
        .map_err(|_| "inject_failed")?
    }

    /// Brings `target` to the foreground; true once it is the foreground window.
    fn activate(target: WindowId) -> bool {
        let start = Instant::now();
        let mut last_try: Option<Instant> = None;
        while start.elapsed() < ACTIVATE_WAIT {
            if foreground_window() == Some(target) {
                std::thread::sleep(SETTLE);
                return foreground_window() == Some(target);
            }
            if last_try.is_none_or(|t| t.elapsed() >= ACTIVATE_RETRY) {
                activate_window(target);
                last_try = Some(Instant::now());
            }
            std::thread::sleep(ACTIVATE_POLL);
        }
        false
    }
}

#[cfg(not(windows))]
mod target {
    pub fn start_tracking() {}

    pub async fn reoutput(
        _window: &tauri::WebviewWindow,
        _text: String,
        _rules: Vec<v2c_core::config::AppRule>,
    ) -> Result<(), &'static str> {
        Err("platform_unsupported")
    }
}

/// Linux (T3.6): outputs that could not be typed and could not go to the history either are
/// kept in memory (`linux_app::kept`) and listed first, with negative ids.
#[cfg(target_os = "linux")]
mod unsent {
    use tauri::Manager;

    use super::HistoryItem;
    use crate::linux_app::kept::{is_unsent_id, Unsent, UnsentState};

    fn store(
        app: &tauri::AppHandle,
    ) -> Option<std::sync::Arc<crate::linux_app::kept::UnsentStore>> {
        app.try_state::<UnsentState>()
            .map(|s| std::sync::Arc::clone(&s.0))
    }

    pub fn to_item(u: Unsent) -> HistoryItem {
        HistoryItem {
            id: u.id,
            created_at: u.created_at,
            app: String::new(),
            mode: match u.mode {
                v2c_core::Mode::Dictate => "dictate",
                v2c_core::Mode::TranslateZhToEn => "translate",
            },
            chars: u32::try_from(u.text.chars().count()).unwrap_or(u32::MAX),
            text: u.text,
        }
    }

    pub fn items(app: &tauri::AppHandle) -> Vec<HistoryItem> {
        store(app)
            .map(|s| s.list().into_iter().map(to_item).collect())
            .unwrap_or_default()
    }

    /// `None`: not an unsent id (look in the history).
    pub fn text(app: &tauri::AppHandle, id: i64) -> Option<Result<String, &'static str>> {
        is_unsent_id(id).then(|| {
            store(app)
                .and_then(|s| s.text(id))
                .ok_or("history_not_found")
        })
    }

    pub fn delete(app: &tauri::AppHandle, id: i64) -> Option<Result<(), &'static str>> {
        is_unsent_id(id).then(|| match store(app) {
            Some(s) if s.remove(id) => Ok(()),
            _ => Err("history_not_found"),
        })
    }

    /// Returns whether anything was kept.
    pub fn clear(app: &tauri::AppHandle) -> bool {
        store(app).is_some_and(|s| {
            let had = s.len() > 0;
            s.clear();
            had
        })
    }
}

#[cfg(not(target_os = "linux"))]
mod unsent {
    use super::HistoryItem;

    pub fn items(_: &tauri::AppHandle) -> Vec<HistoryItem> {
        Vec::new()
    }
    pub fn text(_: &tauri::AppHandle, _: i64) -> Option<Result<String, &'static str>> {
        None
    }
    pub fn delete(_: &tauri::AppHandle, _: i64) -> Option<Result<(), &'static str>> {
        None
    }
    pub fn clear(_: &tauri::AppHandle) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use v2c_core::history::{AddOutcome, NewEntry, Protector};

    use super::*;

    struct Plain;
    impl Protector for Plain {
        fn protect(&self, p: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Ok(p.iter().rev().copied().collect())
        }
        fn unprotect(&self, s: &[u8]) -> v2c_core::Result<Vec<u8>> {
            Ok(s.iter().rev().copied().collect())
        }
    }

    #[test]
    fn list_and_lookup() {
        let h = History::open_in_memory(Arc::new(Plain), HistoryConfig::default()).unwrap();
        let add = |mode, text| {
            h.add_entry(&NewEntry {
                created_at: 100,
                mode,
                lang: None,
                app: "LINE.exe",
                text,
                timings: None,
            })
            .unwrap()
        };
        let AddOutcome::Stored(a) = add(Mode::Dictate, "早安") else {
            panic!()
        };
        add(Mode::TranslateZhToEn, "good morning");
        let items = list_items(&h).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].mode, "translate");
        assert_eq!(items[1].mode, "dictate");
        assert_eq!(items[1].chars, 2);
        assert_eq!(items[1].app, "LINE.exe");
        assert_eq!(entry_text(&h, a).unwrap(), "早安");
        assert_eq!(entry_text(&h, 999), Err("history_not_found"));
        let dbg = format!("{items:?}");
        assert!(
            !dbg.contains("早安") && !dbg.contains("good morning"),
            "{dbg}"
        );
    }

    #[test]
    fn only_the_main_window_may_call() {
        assert_eq!(check_caller("main"), Ok(()));
        assert_eq!(check_caller("overlay"), Err("forbidden"));
        assert_eq!(check_caller(""), Err("forbidden"));
        assert_eq!(check_caller("Main"), Err("forbidden"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unsent_items_look_like_history_rows() {
        let item = unsent::to_item(crate::linux_app::kept::Unsent {
            id: -3,
            created_at: 7,
            mode: Mode::TranslateZhToEn,
            text: "早安 hi".into(),
        });
        assert_eq!(
            (
                item.id,
                item.created_at,
                item.mode,
                item.chars,
                item.app.as_str()
            ),
            (-3, 7, "translate", 5, "")
        );
        assert!(!format!("{item:?}").contains("早安"));
    }

    #[cfg(windows)]
    #[test]
    fn inject_codes_match_controller() {
        use v2c_platform::InjectError;
        assert_eq!(inject_error_code(&InjectError::Elevated), "inject_elevated");
        assert_eq!(
            inject_error_code(&InjectError::Partial { sent: 1, total: 2 }),
            "inject_partial"
        );
        assert_eq!(
            inject_error_code(&InjectError::Os("modifier keys held".into())),
            "inject_modifiers_held"
        );
        assert_eq!(
            inject_error_code(&InjectError::Os("other".into())),
            "inject_failed"
        );
    }
}
