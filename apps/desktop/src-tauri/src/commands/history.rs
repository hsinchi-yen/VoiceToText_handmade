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

#[cfg(not(windows))]
fn open_store(_data_dir: &Path, _cfg: &HistoryConfig) -> Option<Arc<History>> {
    // Linux (Secret Service key) arrives in Phase 3; never store unencrypted text.
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
    with_store(&window, &state, list_items).await
}

#[tauri::command]
pub async fn history_delete(
    id: i64,
    window: tauri::WebviewWindow,
    state: State<'_, HistoryState>,
) -> Result<(), &'static str> {
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
    with_store(&window, &state, |s| {
        s.clear().map_err(|e| {
            warn!(error = %e, "history clear failed");
            "history_failed"
        })
    })
    .await?;
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
    let chars = with_store(&window, &state, move |s| {
        let text = entry_text(s, id)?;
        copy_to_clipboard(&text)?;
        Ok(text.chars().count())
    })
    .await?;
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
    let text = with_store(&window, &state, move |s| entry_text(s, id)).await?;
    let rules = app_state.config.apps.clone();
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
