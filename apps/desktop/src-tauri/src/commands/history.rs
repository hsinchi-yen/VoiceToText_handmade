//! Input history (T1.9): store setup, hourly purge, and the commands behind the 紀錄 page:
//! list, copy, re-output, delete, clear.
//!
//! The decrypted text only goes to the main window (list) and the clipboard / target app
//! (copy, re-output); it is never logged. Errors are returned as short codes.
//!
//! Re-output: the main window is minimized, which hands the foreground back to the window
//! that was active before (Windows activates the next window in the Z order). We wait until
//! the foreground belongs to another process, then plan and inject the text exactly like a
//! normal output (`inject_plan::plan` + the app's rules + `SendInputInjector`). If no other
//! window takes the foreground in time, the window is restored and `no_target_window` is
//! returned. Minimize (not hide) keeps the window reachable from the taskbar.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{Manager, State};
use tracing::{info, warn};
use v2c_core::config::HistoryConfig;
use v2c_core::history::{now_unix, History, HistoryStore};
use v2c_core::Mode;

use crate::app_state::AppState;
use crate::history_sink::{self, HistorySink};

/// Entries shown on the page (the store keeps at most `history.max_entries`).
const LIST_LIMIT: u32 = 200;
const PURGE_EVERY: Duration = Duration::from_secs(60 * 60);

/// Tauri-managed handle; `None` when the history could not be opened (or on platforms
/// without a data protector yet).
pub struct HistoryState(pub Option<Arc<History>>);

/// One row on the 紀錄 page.
#[derive(Debug, Clone, Serialize)]
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

/// Opens `history.db`, purges expired entries, starts the hourly purge, installs the output
/// sink, and manages [`HistoryState`]. Call in `setup` before the controller starts.
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

fn store<'a>(state: &'a State<'_, HistoryState>) -> Result<&'a Arc<History>, &'static str> {
    state.0.as_ref().ok_or("history_unavailable")
}

#[tauri::command]
pub async fn history_list(
    state: State<'_, HistoryState>,
) -> Result<Vec<HistoryItem>, &'static str> {
    list_items(store(&state)?)
}

#[tauri::command]
pub async fn history_delete(id: i64, state: State<'_, HistoryState>) -> Result<(), &'static str> {
    store(&state)?.delete(id).map_err(|e| {
        warn!(error = %e, id, "history delete failed");
        "history_failed"
    })
}

#[tauri::command]
pub async fn history_clear(state: State<'_, HistoryState>) -> Result<(), &'static str> {
    store(&state)?.clear().map_err(|e| {
        warn!(error = %e, "history clear failed");
        "history_failed"
    })?;
    info!("history cleared by user");
    Ok(())
}

/// Puts the entry's text on the clipboard.
#[tauri::command]
pub async fn history_copy(id: i64, state: State<'_, HistoryState>) -> Result<(), &'static str> {
    let text = entry_text(store(&state)?, id)?;
    copy_to_clipboard(&text)?;
    info!(id, chars = text.chars().count(), "history copied");
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

/// Types the entry's text into the window that was active before the main window.
#[tauri::command]
pub async fn history_reoutput(
    id: i64,
    window: tauri::WebviewWindow,
    state: State<'_, HistoryState>,
    app_state: State<'_, AppState>,
) -> Result<(), &'static str> {
    let text = entry_text(store(&state)?, id)?;
    let rules = app_state.config.apps.clone();
    reoutput(&window, text, rules).await?;
    info!(id, "history re-output");
    Ok(())
}

#[cfg(windows)]
async fn reoutput(
    window: &tauri::WebviewWindow,
    text: String,
    rules: Vec<v2c_core::config::AppRule>,
) -> Result<(), &'static str> {
    use v2c_core::inject_plan::{plan, profile_for};
    use v2c_platform::windows::foreground;
    use v2c_platform::windows::inject::SendInputInjector;
    use v2c_platform::Injector;

    const WAIT: Duration = Duration::from_millis(1500);
    const POLL: Duration = Duration::from_millis(30);
    // Let the newly activated window settle its keyboard focus.
    const SETTLE: Duration = Duration::from_millis(120);

    window.minimize().map_err(|e| {
        warn!(error = %e, "minimize failed");
        "window_failed"
    })?;
    let own = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_lowercase()));
    let target = tokio::task::spawn_blocking(move || {
        let start = std::time::Instant::now();
        while start.elapsed() < WAIT {
            if let Some(app) = foreground::current_app() {
                if own.as_deref() != Some(app.exe.to_lowercase().as_str()) {
                    std::thread::sleep(SETTLE);
                    return Some(app.exe);
                }
            }
            std::thread::sleep(POLL);
        }
        None
    })
    .await
    .ok()
    .flatten();
    let Some(exe) = target else {
        let _ = window.unminimize();
        warn!("re-output: no other window took the foreground");
        return Err("no_target_window");
    };
    let actions = plan(&text, &profile_for(&exe, &rules));
    tokio::task::spawn_blocking(move || SendInputInjector.run(&actions))
        .await
        .map_err(|_| "inject_failed")?
        .map_err(|e| {
            let code = inject_error_code(&e);
            warn!(code, app = %exe, "re-output injection failed");
            code
        })
}

#[cfg(not(windows))]
async fn reoutput(
    _window: &tauri::WebviewWindow,
    _text: String,
    _rules: Vec<v2c_core::config::AppRule>,
) -> Result<(), &'static str> {
    Err("platform_unsupported")
}

/// Same codes as the controller's injection errors (the UI shares the messages).
#[cfg(windows)]
fn inject_error_code(e: &v2c_platform::InjectError) -> &'static str {
    use v2c_platform::InjectError;
    match e {
        InjectError::Elevated => "inject_elevated",
        InjectError::Partial { .. } => "inject_partial",
        InjectError::Os(msg) if msg.contains("paste not implemented") => "inject_paste_unsupported",
        InjectError::Os(msg) if msg.contains("modifier") => "inject_modifiers_held",
        InjectError::Os(_) => "inject_failed",
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
    }

    #[cfg(windows)]
    #[test]
    fn inject_codes_match_controller() {
        use v2c_platform::InjectError;
        assert_eq!(inject_error_code(&InjectError::Elevated), "inject_elevated");
        assert_eq!(
            inject_error_code(&InjectError::Os("modifier keys held".into())),
            "inject_modifiers_held"
        );
    }
}
