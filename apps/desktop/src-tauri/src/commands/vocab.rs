//! Vocabulary editor commands (T1.8): read and save `%APPDATA%\v2c\vocab.txt`.
//!
//! The text goes only to and from the main window (the vocabulary page); the overlay window is
//! refused. Logs carry entry counts only, never entries. After a save, [`VOCAB_CHANGED_EVENT`]
//! is emitted (payload: entry count) so the pipeline can reload the vocabulary.

use serde::Serialize;
use tauri::{AppHandle, Emitter, State, WebviewWindow};
use tracing::{info, warn};
use v2c_core::vocab::{self, Vocab, MAX_ENTRIES, MAX_ENTRY_CHARS};

use crate::app_state::AppState;

/// Emitted after the vocabulary file was saved. Payload: number of entries.
pub const VOCAB_CHANGED_EVENT: &str = "v2c:vocab-changed";

/// Only this window may read or write the vocabulary.
const EDITOR_WINDOW: &str = "main";

#[derive(Debug, Serialize)]
pub struct VocabView {
    /// One entry per line.
    pub text: String,
    pub count: usize,
    pub max_entries: usize,
    pub max_entry_chars: usize,
}

#[derive(Debug, Serialize)]
pub struct VocabSaved {
    pub count: usize,
}

/// Why a save was refused. `code` is stable for the UI: `invalid` (see the other fields),
/// `storage` (file could not be read or written), `forbidden` (wrong window).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct VocabError {
    pub code: &'static str,
    /// 1-based line numbers of entries longer than `max_entry_chars`.
    pub too_long_lines: Vec<usize>,
    /// Entries beyond `max_entries`.
    pub over_limit: usize,
}

impl VocabError {
    fn code(code: &'static str) -> Self {
        Self {
            code,
            too_long_lines: Vec::new(),
            over_limit: 0,
        }
    }
}

fn check_window(window: &WebviewWindow) -> Result<(), VocabError> {
    if window.label() == EDITOR_WINDOW {
        Ok(())
    } else {
        Err(VocabError::code("forbidden"))
    }
}

fn view(v: &Vocab) -> VocabView {
    VocabView {
        text: v.to_text(),
        count: v.len(),
        max_entries: MAX_ENTRIES,
        max_entry_chars: MAX_ENTRY_CHARS,
    }
}

/// Validates editor text; `Err` lists what is over the limits.
fn validate(text: &str) -> Result<Vocab, VocabError> {
    let c = vocab::check(text);
    if c.is_ok() {
        Ok(c.vocab)
    } else {
        Err(VocabError {
            code: "invalid",
            too_long_lines: c.too_long_lines,
            over_limit: c.over_limit,
        })
    }
}

/// The saved vocabulary (normalised: one entry per line, comments and duplicates removed).
#[tauri::command]
pub fn vocab_get(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<VocabView, VocabError> {
    check_window(&window)?;
    let v = Vocab::load(&state.paths.vocab_file()).map_err(|e| {
        warn!(error = %e, "vocabulary could not be read");
        VocabError::code("storage")
    })?;
    Ok(view(&v))
}

/// Validates and saves editor text, then emits [`VOCAB_CHANGED_EVENT`].
#[tauri::command]
pub fn vocab_set(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    text: String,
) -> Result<VocabSaved, VocabError> {
    check_window(&window)?;
    let v = validate(&text)?;
    v.save(&state.paths.vocab_file()).map_err(|e| {
        warn!(error = %e, "vocabulary could not be saved");
        VocabError::code("storage")
    })?;
    info!(count = v.len(), "vocabulary saved");
    let _ = app.emit(VOCAB_CHANGED_EVENT, v.len());
    Ok(VocabSaved { count: v.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_and_normalises() {
        let v = validate("台積電\n\n# note\nLM  Studio\nlm studio\n").unwrap();
        assert_eq!(v.words(), ["台積電", "LM Studio"]);
        assert_eq!(view(&v).text, "台積電\nLM Studio\n");
    }

    #[test]
    fn validate_reports_limits_without_echoing_text() {
        let long = "長".repeat(MAX_ENTRY_CHARS + 1);
        let err = validate(&format!("ok\n{long}\n")).unwrap_err();
        assert_eq!(err.code, "invalid");
        assert_eq!(err.too_long_lines, vec![2]);
        assert!(!format!("{err:?}").contains('長'));

        let many: String = (0..=MAX_ENTRIES).map(|i| format!("w{i}\n")).collect();
        assert_eq!(validate(&many).unwrap_err().over_limit, 1);
    }
}
