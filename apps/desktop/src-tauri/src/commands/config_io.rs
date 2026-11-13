//! Configuration file export / import (T5.2; `tasks/plan.md` section 14, `docs/config-file.md`).
//!
//! The file carries the cloud / LAN services, the `[route]` and the vocabulary, never API keys.
//! All format rules live in `v2c_core::portable` (shared with Android); this module only does
//! what needs the desktop: file dialogs, `config.toml` / `vocab.txt` / keyring access.
//!
//! Commands (main window only):
//! - `config_pick_open` / `config_pick_save(default_name)`: native file dialogs (path or `None`).
//! - `config_export(path)`: writes the current settings; the text is re-parsed before writing.
//! - `config_import_preview(path)`: what importing would do; writes nothing.
//! - `config_import_apply(path, file_hash, asr_choice, keys)`: merges the file, then emits
//!   `v2c:config-changed` (and `v2c:vocab-changed` when words were added).
//!
//! Apply order, so a failure at any step puts everything back: keys (via
//! `presets::update_keys`), then `vocab.txt`, then `config.toml` (atomic, comment preserving,
//! under `CONFIG_LOCK`). If a later step fails the earlier ones are restored. The file is read
//! again at apply time and must hash (SHA-256) like the preview did, else `file_changed`. The
//! merged route is checked against the merged services (`check_routes`) before anything is
//! written.
//!
//! A file with `[preset] id_prefix` (the built-in TN / RD style) keeps the preset rule: it may
//! not replace an existing service of another kind (`preset_id_conflict`). For a plain file a
//! kind change is an update that drops the old key (the plan's `drop_old_key`).
//!
//! Logs: that the user picked a file (never the path), service and word counts, error codes,
//! elapsed ms. Never file content, keys, vocabulary or service names.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{Emitter, Manager};
use toml_edit::Value;
use tracing::{info, warn};
use v2c_core::config::{CloudKind, Config, TimeoutConfig, LOCAL};
use v2c_core::portable::{self, MergePlan, PortableError, PortableFile};
use v2c_core::route::{self, suggested_models, CloudHttp};
use v2c_core::secrets::{ApiKey, SecretStore};
use v2c_core::vocab::Vocab;

use super::cloud::{
    announce_saved, check_caller, config_lock, config_path, load_config, secret_store, set_str,
    set_value, update_config, upsert_service,
};
use super::presets::{detect_one, file_sources, restore_keys, update_keys, Keys};
use crate::app_state::AppState;

/// Platform label written to `[v2c] exported_from`.
const PLATFORM: &str = if cfg!(windows) { "windows" } else { "linux" };
/// Longest default file name accepted from the UI.
const MAX_DEFAULT_NAME_CHARS: usize = 64;

// ---------------------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------------------

/// A refused request: stable `code` plus the 1-based file line when the file is at fault.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct IoError {
    pub code: &'static str,
    pub line: Option<u32>,
}

impl From<&'static str> for IoError {
    fn from(code: &'static str) -> Self {
        Self { code, line: None }
    }
}

impl From<PortableError> for IoError {
    fn from(e: PortableError) -> Self {
        Self {
            code: e.code,
            line: e.line,
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ImportService {
    pub id: String,
    pub name: Option<String>,
    pub kind: &'static str,
    pub base_url: Option<String>,
    pub asr_model: Option<String>,
    pub llm_model: Option<String>,
    /// `detected` / `file` for the text model (`None` when the service has none).
    pub model_source: Option<&'static str>,
    pub asr_timeout_ms: Option<u64>,
    /// `added` / `updated` / `unchanged`.
    pub status: &'static str,
    /// Subset of `asr`, `polish`, `translate` (what the file routes to this service).
    pub purposes: Vec<&'static str>,
    pub needs_key: bool,
    /// A key is stored for this id now.
    pub has_key: bool,
    /// The file ships a default key (the key itself is never sent to the UI).
    pub has_default_key: bool,
    /// The old key is deleted by this import unless a new one is typed.
    pub drops_old_key: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RouteChangeView {
    /// `asr`, `polish`, `translate` or `cloud_fallback_to_local`.
    pub field: &'static str,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AsrChoice {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ConfigPreview {
    pub title: Option<String>,
    pub exported_from: Option<String>,
    pub exported_at: Option<String>,
    /// The file has `[preset] id_prefix` (a TN / RD style preset file).
    pub is_preset: bool,
    pub services: Vec<ImportService>,
    /// Route fields the import changes. For a preset file `asr` is left out: the user picks it
    /// from `asr_choices`.
    pub route: Vec<RouteChangeView>,
    pub asr_choices: Vec<AsrChoice>,
    /// Display names by service id (current and file services), for route values.
    pub names: BTreeMap<String, String>,
    pub vocab_new: usize,
    pub vocab_blocked: usize,
    pub ignored: Vec<String>,
    /// `needs_key` services that would have no key (a key typed in the dialog can fill them).
    pub key_missing: Vec<String>,
    pub needs_key: Vec<String>,
    /// Nothing would change.
    pub nothing_to_do: bool,
    /// SHA-256 (hex) of the file bytes; `config_import_apply` refuses a file that differs.
    pub file_hash: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ConfigApplied {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub route_changed: usize,
    pub vocab_added: usize,
    /// Vocabulary size after the import (payload of `v2c:vocab-changed`).
    pub vocab_total: usize,
    /// `needs_key` services that have no key stored now.
    pub key_missing: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ConfigExported {
    pub services: usize,
    pub vocab: usize,
    /// Ids of services left out because they would not pass the file rules.
    pub skipped: Vec<String>,
    /// Route fields written as `local` (`asr`, `polish`, `translate`).
    pub route_reset: Vec<&'static str>,
}

// ---------------------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------------------

fn is_toml(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
}

fn read_text(path: &str) -> Result<String, IoError> {
    use std::io::Read;

    // No network shares: a slow or hostile server must not stall the app.
    if path.starts_with("\\\\") || path.starts_with("//") {
        return Err("file_unreadable".into());
    }
    let p = Path::new(path);
    if !is_toml(p) {
        return Err("file_not_toml".into());
    }
    let file = std::fs::File::open(p).map_err(|_| "file_unreadable")?;
    if !file.metadata().map_err(|_| "file_unreadable")?.is_file() {
        return Err("file_unreadable".into());
    }
    let limit = portable::MAX_FILE_BYTES as u64;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "file_unreadable")?;
    if bytes.len() as u64 > limit {
        return Err("too_large".into());
    }
    String::from_utf8(bytes).map_err(|_| "file_unreadable".into())
}

/// Writes `text` next to `path` and renames it over, so a crash never leaves half a file.
fn write_atomic(path: &Path, text: &str) -> Result<(), &'static str> {
    use std::io::Write;

    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("write_failed");
    }
    Ok(())
}

fn has_key_fn(stored: &dyn SecretStore) -> impl Fn(&str) -> bool + '_ {
    move |id| stored.get(id).ok().flatten().is_some_and(|k| !k.is_empty())
}

fn kind_label(kind: CloudKind) -> &'static str {
    match kind {
        CloudKind::OpenAi => "open_ai",
        CloudKind::Gemini => "gemini",
        CloudKind::Claude => "claude",
        CloudKind::OpenAiCompat => "open_ai_compat",
    }
}

// ---------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------

/// Writes the current settings to `out` (`.toml` is appended unless the name already ends so).
pub fn export_to(
    config_path: &Path,
    vocab_path: &Path,
    stored: &dyn SecretStore,
    out: &Path,
) -> Result<ConfigExported, IoError> {
    // `mine` -> `mine.toml`, `my.config` -> `my.config.toml`.
    let out: PathBuf = if is_toml(out) {
        out.to_path_buf()
    } else {
        let mut name = out.as_os_str().to_os_string();
        name.push(".toml");
        PathBuf::from(name)
    };
    let cfg = load_config(config_path)?;
    let vocab = Vocab::load(vocab_path).map_err(|_| "vocab_failed")?;
    let exported = portable::export(&cfg, Some(&vocab), &has_key_fn(stored), PLATFORM);
    // Safety net: never write a file this app (or another v2c) would refuse.
    let parsed = portable::parse(&exported.text).map_err(|e| {
        warn!(code = e.code, "export produced a file that does not parse");
        IoError::from("export_invalid")
    })?;
    write_atomic(&out, &exported.text)?;
    Ok(ConfigExported {
        services: parsed.services.len(),
        vocab: parsed.vocab.len(),
        skipped: exported.skipped,
        route_reset: exported.route_reset,
    })
}

// ---------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------

/// Rules that need the current config on top of `plan_merge`.
fn check_plan(
    file: &PortableFile,
    cfg: &Config,
    plan: &MergePlan,
    asr: Option<&str>,
) -> Result<(), IoError> {
    if !plan.is_applicable() {
        return Err("name_taken".into());
    }
    // A preset file may update its own services only, never one of another kind.
    if file.id_prefix.is_some()
        && file
            .services
            .iter()
            .any(|s| cfg.cloud_service(&s.id).is_some_and(|e| e.kind != s.kind))
    {
        return Err("preset_id_conflict".into());
    }
    check_routes(file, cfg, asr)
}

/// The merged route must still work with the merged services (same capability rules as
/// `portable::parse`): an updated service may lose the kind or model a route needs. A route
/// the file does not touch and that points at a service the file does not touch is left alone,
/// even if it was already broken.
fn check_routes(file: &PortableFile, cfg: &Config, asr: Option<&str>) -> Result<(), IoError> {
    let r = &cfg.route;
    let wanted = [
        ("asr", asr, &r.asr),
        ("polish", file.route.polish.as_deref(), &r.polish),
        ("translate", file.route.translate.as_deref(), &r.translate),
    ];
    for (field, new, old) in wanted {
        let target = new.unwrap_or(old.as_str()).trim();
        if target == LOCAL {
            continue;
        }
        let in_file = file.services.iter().find(|s| s.id == target);
        if new.is_none() && in_file.is_none() {
            continue;
        }
        let Some(svc) = in_file.or_else(|| cfg.cloud_service(target)) else {
            return Err("route_unknown_service".into());
        };
        let suggested = suggested_models(svc.kind);
        let (can_do, has_model) = if field == "asr" {
            (
                svc.kind.supports_asr(),
                svc.asr_model.is_some() || suggested.asr.is_some(),
            )
        } else {
            (true, svc.llm_model.is_some() || suggested.llm.is_some())
        };
        if !can_do {
            return Err("route_no_asr".into());
        }
        if !has_model {
            return Err("route_no_model".into());
        }
    }
    Ok(())
}

/// The speech recognition target: the user's pick for a preset file with `asr_choices`, else
/// the file's `route.asr` (if any).
fn resolve_asr(file: &PortableFile, choice: Option<&str>) -> Result<Option<String>, IoError> {
    if file.asr_choices.is_empty() {
        return Ok(file.route.asr.clone());
    }
    match choice {
        Some(c) if file.asr_choices.iter().any(|a| a == c) => Ok(Some(c.to_owned())),
        _ => Err("preset_bad_choice".into()),
    }
}

/// For a file with `detect_llm_model`: asks each endpoint with a text model for its model list
/// (no key, 3 s) and uses the one listed model. Returns the source (`detected` / `file`) per id.
async fn detect_models(file: &mut PortableFile, http: &CloudHttp) -> HashMap<String, &'static str> {
    let mut sources = HashMap::new();
    let timeouts = TimeoutConfig {
        asr_cloud_ms: 1000,
        llm_cloud_ms: 1000,
    };
    let mut detected = 0usize;
    for svc in &mut file.services {
        if svc.llm_model.is_none() {
            continue;
        }
        let mut source = "file";
        if file.detect_llm_model && svc.kind == CloudKind::OpenAiCompat {
            if let Some(model) = detect_one(svc, http, &timeouts).await {
                svc.llm_model = Some(model);
                source = "detected";
                detected += 1;
            }
        }
        sources.insert(svc.id.clone(), source);
    }
    if file.detect_llm_model {
        info!(detected, "config import: model detection");
    }
    sources
}

fn build_preview(
    cfg: &Config,
    vocab: &Vocab,
    stored: &dyn SecretStore,
    file: &PortableFile,
    sources: &HashMap<String, &'static str>,
    file_hash: &str,
) -> Result<ConfigPreview, IoError> {
    let has_key = has_key_fn(stored);
    let plan = portable::plan_merge(cfg, vocab, file, &has_key);
    // Plain file: its own `route.asr` decides. Preset file: the user picks later, so the ASR
    // check waits for apply (`local` here means "nothing to check").
    let asr_check = if file.asr_choices.is_empty() {
        file.route.asr.as_deref()
    } else {
        Some(LOCAL)
    };
    check_plan(file, cfg, &plan, asr_check)?;
    let preset = !file.asr_choices.is_empty();
    let services = file
        .services
        .iter()
        .map(|s| {
            let mut purposes = Vec::new();
            let asr_target = if preset {
                file.asr_choices.contains(&s.id)
            } else {
                file.route.asr.as_deref() == Some(s.id.as_str())
            };
            if asr_target {
                purposes.push("asr");
            }
            if file.route.polish.as_deref() == Some(s.id.as_str()) {
                purposes.push("polish");
            }
            if file.route.translate.as_deref() == Some(s.id.as_str()) {
                purposes.push("translate");
            }
            let status = if plan.added.contains(&s.id) {
                "added"
            } else if plan.updated.contains(&s.id) {
                "updated"
            } else {
                "unchanged"
            };
            ImportService {
                id: s.id.clone(),
                name: s.name.clone(),
                kind: kind_label(s.kind),
                base_url: s.base_url.clone(),
                asr_model: s.asr_model.clone(),
                llm_model: s.llm_model.clone(),
                model_source: sources.get(&s.id).copied(),
                asr_timeout_ms: s.asr_timeout_ms,
                status,
                purposes,
                needs_key: file.needs_key.contains(&s.id),
                has_key: has_key(&s.id),
                has_default_key: file.default_keys.contains(&s.id),
                drops_old_key: plan.drop_old_key.contains(&s.id),
            }
        })
        .collect::<Vec<_>>();

    let mut names = BTreeMap::new();
    for s in cfg.cloud.iter().chain(file.services.iter()) {
        names.insert(s.id.clone(), s.name.clone().unwrap_or_else(|| s.id.clone()));
    }
    let asr_choices = file
        .asr_choices
        .iter()
        .map(|id| AsrChoice {
            id: id.clone(),
            name: names.get(id).cloned().unwrap_or_else(|| id.clone()),
        })
        .collect();
    let route = plan
        .route
        .iter()
        .filter(|c| !(preset && c.field == "asr"))
        .map(|c| RouteChangeView {
            field: c.field,
            old: c.old.clone(),
            new: c.new.clone(),
        })
        .collect();
    Ok(ConfigPreview {
        title: file.title.clone(),
        exported_from: file.exported_from.clone(),
        exported_at: file.exported_at.clone(),
        is_preset: file.id_prefix.is_some(),
        services,
        route,
        asr_choices,
        names,
        vocab_new: plan.vocab_new.len(),
        vocab_blocked: plan.vocab_blocked,
        ignored: plan.ignored.clone(),
        key_missing: plan.key_missing.clone(),
        needs_key: file.needs_key.clone(),
        nothing_to_do: plan.is_empty() && !preset,
        file_hash: file_hash.to_owned(),
    })
}

/// Preview from file text (no detection); tests only, the command detects models first.
#[cfg(test)]
pub fn preview_text(
    config_path: &Path,
    vocab_path: &Path,
    stored: &dyn SecretStore,
    text: &str,
) -> Result<ConfigPreview, IoError> {
    let file = portable::parse(text)?;
    preview_file(
        config_path,
        vocab_path,
        stored,
        &file,
        &HashMap::new(),
        &hash_hex(text.as_bytes()),
    )
}

fn preview_file(
    config_path: &Path,
    vocab_path: &Path,
    stored: &dyn SecretStore,
    file: &PortableFile,
    sources: &HashMap<String, &'static str>,
    file_hash: &str,
) -> Result<ConfigPreview, IoError> {
    let cfg = load_config(config_path)?;
    let vocab = Vocab::load(vocab_path).map_err(|_| "vocab_failed")?;
    build_preview(&cfg, &vocab, stored, file, sources, file_hash)
}

fn restore_vocab(path: &Path, backup: &Option<(bool, Vocab)>) {
    let Some((existed, old)) = backup else { return };
    let done = if *existed {
        old.save(path).map_err(|_| ())
    } else {
        std::fs::remove_file(path).map_err(|_| ())
    };
    if done.is_err() {
        warn!("config import: could not restore the vocabulary");
    }
}

/// Merges `file` into the settings. Keys, vocabulary and `config.toml` are written together:
/// if one step fails the earlier ones are put back.
pub fn apply_file(
    config_path: &Path,
    vocab_path: &Path,
    stored: &dyn SecretStore,
    file: &PortableFile,
    asr_choice: Option<&str>,
    keys: &Keys,
) -> Result<ConfigApplied, IoError> {
    let _guard = config_lock();
    let cfg = load_config(config_path)?;
    let vocab = Vocab::load(vocab_path).map_err(|_| "vocab_failed")?;
    let plan = portable::plan_merge(&cfg, &vocab, file, &has_key_fn(stored));
    let asr = resolve_asr(file, asr_choice)?;
    check_plan(file, &cfg, &plan, asr.as_deref())?;

    let mut new_keys: Vec<(&str, ApiKey)> = Vec::new();
    for (id, raw) in keys.entries() {
        if !file.needs_key.contains(id) {
            return Err("preset_key_not_allowed".into());
        }
        let k = raw.trim();
        if k.is_empty() {
            continue;
        }
        if k.chars().any(char::is_control) {
            return Err("invalid_key".into());
        }
        new_keys.push((id, ApiKey::new(k)));
    }

    let changes = update_keys(
        stored,
        &cfg,
        &file.services,
        &|id| file.default_keys.get(id).map(ApiKey::new),
        &new_keys,
    )?;
    drop(new_keys);

    // Vocabulary: union, current order first.
    let vocab_backup = (!plan.vocab_new.is_empty()).then(|| (vocab_path.exists(), vocab.clone()));
    if vocab_backup.is_some() {
        let merged = Vocab::from_words(&[vocab.words(), plan.vocab_new.as_slice()].concat());
        if merged.save(vocab_path).is_err() {
            restore_vocab(vocab_path, &vocab_backup);
            restore_keys(stored, &changes);
            return Err("vocab_failed".into());
        }
    }

    let written = update_config(config_path, |doc, _current| {
        for svc in file
            .services
            .iter()
            .filter(|s| plan.added.contains(&s.id) || plan.updated.contains(&s.id))
        {
            upsert_service(doc, svc)?;
        }
        let r = &file.route;
        let wanted = [
            ("asr", asr.as_deref()),
            ("polish", r.polish.as_deref()),
            ("translate", r.translate.as_deref()),
        ];
        if wanted.iter().any(|(_, v)| v.is_some()) || r.cloud_fallback_to_local.is_some() {
            let route = doc
                .entry("route")
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
                .as_table_like_mut()
                .ok_or("config_invalid")?;
            for (key, value) in wanted {
                if let Some(v) = value {
                    set_str(route, key, v);
                }
            }
            if let Some(fb) = r.cloud_fallback_to_local {
                set_value(route, "cloud_fallback_to_local", Value::from(fb));
            }
        }
        Ok(())
    });
    if let Err(code) = written {
        restore_vocab(vocab_path, &vocab_backup);
        restore_keys(stored, &changes);
        return Err(code.into());
    }

    let has_key = has_key_fn(stored);
    // For a preset file the plan compares the file's own `route.asr`; the user's pick counts.
    let preset = !file.asr_choices.is_empty();
    let route_changed = plan
        .route
        .iter()
        .filter(|c| !(preset && c.field == "asr"))
        .count()
        + usize::from(preset && asr.as_deref().is_some_and(|a| a != cfg.route.asr));
    Ok(ConfigApplied {
        added: plan.added.len(),
        updated: plan.updated.len(),
        unchanged: plan.unchanged.len(),
        route_changed,
        vocab_added: plan.vocab_new.len(),
        vocab_total: vocab.len() + plan.vocab_new.len(),
        key_missing: file
            .needs_key
            .iter()
            .filter(|id| !has_key(id))
            .cloned()
            .collect(),
    })
}

// ---------------------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------------------

fn vocab_path(app: &tauri::AppHandle) -> PathBuf {
    app.state::<AppState>().paths.vocab_file()
}

fn http_client() -> Result<CloudHttp, IoError> {
    route::cloud_http_client().map_err(|_| "internal_error".into())
}

/// A bare file name for the save dialog: no folders, `.toml`, short; anything else falls back.
fn safe_default_name(name: &str) -> String {
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_DEFAULT_NAME_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && is_toml(Path::new(name));
    if ok {
        name.to_owned()
    } else {
        "v2c-config.toml".to_owned()
    }
}

/// Native 「選擇檔案」 dialog. `Ok(None)` when the user closed it.
#[tauri::command]
pub async fn config_pick_open(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Option<String>, IoError> {
    use tauri_plugin_dialog::DialogExt;

    check_caller(window.label())?;
    let picker = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        picker
            .dialog()
            .file()
            .set_title("選擇組態檔")
            .add_filter("TOML 組態檔", &["toml"])
            .blocking_pick_file()
    })
    .await
    .map_err(|_| "internal_error")?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

/// Native 「儲存檔案」 dialog. `Ok(None)` when the user closed it.
#[tauri::command]
pub async fn config_pick_save(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    default_name: String,
) -> Result<Option<String>, IoError> {
    use tauri_plugin_dialog::DialogExt;

    check_caller(window.label())?;
    let name = safe_default_name(&default_name);
    let picker = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        picker
            .dialog()
            .file()
            .set_title("匯出設定")
            .set_file_name(name)
            .add_filter("TOML 組態檔", &["toml"])
            .blocking_save_file()
    })
    .await
    .map_err(|_| "internal_error")?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn config_export(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    path: String,
) -> Result<ConfigExported, IoError> {
    check_caller(window.label())?;
    let started = Instant::now();
    let (cfg_path, vocab) = (config_path(&app), vocab_path(&app));
    let result = tauri::async_runtime::spawn_blocking(move || {
        export_to(&cfg_path, &vocab, &secret_store()?, Path::new(&path))
    })
    .await
    .map_err(|_| IoError::from("internal_error"))?;
    match &result {
        Ok(e) => info!(
            services = e.services,
            vocab = e.vocab,
            skipped = e.skipped.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "config exported to a user-chosen file"
        ),
        Err(e) => warn!(code = e.code, "config export refused"),
    }
    result
}

fn hash_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Reads and parses the file; with `expected` (the preview's hash) a file whose bytes changed
/// since is refused with `file_changed`. Returns the file and its hash.
fn read_file(path: &str, expected: Option<&str>) -> Result<(PortableFile, String), IoError> {
    let text = read_text(path)?;
    let hash = hash_hex(text.as_bytes());
    if expected.is_some_and(|e| e != hash) {
        return Err("file_changed".into());
    }
    Ok((portable::parse(&text)?, hash))
}

/// [`read_file`] off the async workers.
async fn load_file(
    path: String,
    expected: Option<String>,
) -> Result<(PortableFile, String), IoError> {
    tauri::async_runtime::spawn_blocking(move || read_file(&path, expected.as_deref()))
        .await
        .map_err(|_| IoError::from("internal_error"))?
}

#[tauri::command]
pub async fn config_import_preview(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    path: String,
) -> Result<ConfigPreview, IoError> {
    check_caller(window.label())?;
    let started = Instant::now();
    let (file, hash) = load_file(path, None)
        .await
        .inspect_err(|e| warn!(code = e.code, "config import preview refused"))?;
    // No model detection here: the preview must open at once (detection runs on apply).
    let sources = file_sources(&file.services);
    let (cfg_path, vocab) = (config_path(&app), vocab_path(&app));
    let result = tauri::async_runtime::spawn_blocking(move || {
        preview_file(&cfg_path, &vocab, &secret_store()?, &file, &sources, &hash)
    })
    .await
    .map_err(|_| IoError::from("internal_error"))?;
    match &result {
        Ok(p) => info!(
            services = p.services.len(),
            vocab_new = p.vocab_new,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "config import previewed (user-chosen file)"
        ),
        Err(e) => warn!(code = e.code, "config import preview refused"),
    }
    result
}

#[tauri::command]
pub async fn config_import_apply(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    path: String,
    file_hash: String,
    asr_choice: Option<String>,
    keys: Keys,
) -> Result<ConfigApplied, IoError> {
    check_caller(window.label())?;
    let started = Instant::now();
    let (mut file, _) = load_file(path, Some(file_hash))
        .await
        .inspect_err(|e| warn!(code = e.code, "config import refused"))?;
    detect_models(&mut file, &http_client()?).await;
    let (cfg_path, vocab) = (config_path(&app), vocab_path(&app));
    let result = tauri::async_runtime::spawn_blocking(move || {
        apply_file(
            &cfg_path,
            &vocab,
            &secret_store()?,
            &file,
            asr_choice.as_deref(),
            &keys,
        )
    })
    .await
    .map_err(|_| IoError::from("internal_error"))?;
    match &result {
        Ok(a) => {
            info!(
                added = a.added,
                updated = a.updated,
                vocab_added = a.vocab_added,
                key_missing = a.key_missing.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "config imported (user-chosen file)"
            );
            announce_saved(&app);
            if a.vocab_added > 0 {
                let _ = app.emit(super::vocab::VOCAB_CHANGED_EVENT, a.vocab_total);
            }
        }
        Err(e) => warn!(code = e.code, "config import refused"),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2c_core::secrets::MemorySecretStore;

    const GEMINI_CLAUDE: &str =
        include_str!("../../../../../tests/fixtures/config/valid-gemini-claude.toml");
    const LAN: &str = include_str!("../../../../../tests/fixtures/config/valid-lan.toml");
    const VOCAB_ONLY: &str =
        include_str!("../../../../../tests/fixtures/config/valid-vocab-only.toml");
    const EXTRA: &str =
        include_str!("../../../../../tests/fixtures/config/valid-extra-sections.toml");
    const FORMAT2: &str = include_str!("../../../../../tests/fixtures/config/reject-format-2.toml");
    const CLAUDE_ASR: &str =
        include_str!("../../../../../tests/fixtures/config/reject-claude-asr.toml");
    const TN: &str = include_str!("../../presets/TN_CUSTOM_LLM_ADDR.toml");
    const KEY: &str = "sk-TESTSECRET-abcdefgh-WXYZ";

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("v2c-cfgio-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn config(&self) -> PathBuf {
            self.0.join("config.toml")
        }
        fn vocab(&self) -> PathBuf {
            self.0.join("vocab.txt")
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn keys(pairs: &[(&str, &str)]) -> Keys {
        Keys::of(pairs)
    }

    fn apply(
        d: &TempDir,
        store: &dyn SecretStore,
        text: &str,
        choice: Option<&str>,
        k: &Keys,
    ) -> Result<ConfigApplied, IoError> {
        apply_file(
            &d.config(),
            &d.vocab(),
            store,
            &portable::parse(text).unwrap(),
            choice,
            k,
        )
    }

    fn preview(d: &TempDir, store: &dyn SecretStore, text: &str) -> Result<ConfigPreview, IoError> {
        preview_text(&d.config(), &d.vocab(), store, text)
    }

    #[test]
    fn preview_lists_services_route_and_missing_keys() {
        let d = TempDir::new("pv1");
        let store = MemorySecretStore::new();
        let p = preview(&d, &store, GEMINI_CLAUDE).unwrap();
        assert_eq!(p.services.len(), 2);
        assert!(p.services.iter().all(|s| s.status == "added"));
        assert_eq!(p.services[0].purposes, ["asr"]);
        assert_eq!(p.services[1].purposes, ["translate"]);
        assert_eq!(
            p.route.iter().map(|c| c.field).collect::<Vec<_>>(),
            ["asr", "translate"]
        );
        assert_eq!(p.key_missing, ["my-gemini", "my-claude"]);
        assert!(!p.nothing_to_do && !p.is_preset);
        // Preview writes nothing.
        assert!(!d.config().exists() && !d.vocab().exists());
    }

    #[test]
    fn preview_reports_vocab_ignored_and_default_keys() {
        let d = TempDir::new("pv2");
        let store = MemorySecretStore::new();
        let p = preview(&d, &store, VOCAB_ONLY).unwrap();
        assert_eq!((p.vocab_new, p.vocab_blocked), (3, 0));
        assert!(p.services.is_empty());
        let p = preview(&d, &store, EXTRA).unwrap();
        assert_eq!(p.ignored.len(), 2, "{:?}", p.ignored);
        assert!(p.ignored.iter().any(|i| i.contains("hotkey")));
        let p = preview(&d, &store, LAN).unwrap();
        assert!(p.services[0].has_default_key);
        assert!(p.key_missing.is_empty(), "a default key fills the gap");
    }

    #[test]
    fn rejected_files_report_code_and_line() {
        let d = TempDir::new("rej");
        let store = MemorySecretStore::new();
        assert_eq!(
            preview(&d, &store, FORMAT2).unwrap_err().code,
            "format_too_new"
        );
        assert_eq!(preview(&d, &store, FORMAT2).unwrap_err().line, Some(2));
        assert_eq!(
            preview(&d, &store, CLAUDE_ASR).unwrap_err().code,
            "route_no_asr"
        );
        assert_eq!(
            preview(&d, &store, "not [ toml").unwrap_err().code,
            "syntax"
        );
    }

    #[test]
    fn apply_writes_services_route_vocab_and_keys() {
        let d = TempDir::new("ap1");
        let store = MemorySecretStore::new();
        let a = apply(
            &d,
            &store,
            GEMINI_CLAUDE,
            None,
            &keys(&[("my-gemini", KEY)]),
        )
        .unwrap();
        assert_eq!(
            (a.added, a.updated, a.unchanged, a.route_changed),
            (2, 0, 0, 2)
        );
        assert_eq!(a.key_missing, ["my-claude"]);
        let cfg = load_config(&d.config()).unwrap();
        assert_eq!(cfg.cloud.len(), 2);
        assert_eq!(cfg.route.asr, "my-gemini");
        assert_eq!(cfg.route.translate, "my-claude");
        assert_eq!(store.get("my-gemini").unwrap().unwrap().expose(), KEY);

        let a = apply(&d, &store, VOCAB_ONLY, None, &keys(&[])).unwrap();
        assert_eq!(a.vocab_added, 3);
        assert_eq!(
            Vocab::load(&d.vocab()).unwrap().words(),
            ["Breeze-ASR", "台積電", "v2c"]
        );
        // Other settings stay.
        assert_eq!(load_config(&d.config()).unwrap().cloud.len(), 2);
    }

    #[test]
    fn default_keys_are_stored_and_foreign_keys_refused() {
        let d = TempDir::new("ap2");
        let store = MemorySecretStore::new();
        let a = apply(&d, &store, LAN, None, &keys(&[])).unwrap();
        assert!(a.key_missing.is_empty());
        assert_eq!(
            store.get("lan-asr").unwrap().unwrap().expose(),
            "shared-key"
        );
        let err = apply(&d, &store, LAN, None, &keys(&[("lan-llm", KEY)])).unwrap_err();
        assert_eq!(err.code, "preset_key_not_allowed");
    }

    #[test]
    fn export_then_import_changes_nothing() {
        let d = TempDir::new("rt");
        let store = MemorySecretStore::new();
        apply(
            &d,
            &store,
            GEMINI_CLAUDE,
            None,
            &keys(&[("my-gemini", KEY)]),
        )
        .unwrap();
        apply(&d, &store, LAN, None, &keys(&[])).unwrap();
        apply(&d, &store, VOCAB_ONLY, None, &keys(&[])).unwrap();
        let before = std::fs::read_to_string(d.config()).unwrap();

        let out = d.0.join("out.toml");
        let e = export_to(&d.config(), &d.vocab(), &store, &out).unwrap();
        assert_eq!((e.services, e.vocab), (4, 3));
        assert!(e.skipped.is_empty() && e.route_reset.is_empty());
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(
            !text.contains(KEY) && !text.contains("shared-key"),
            "no keys in the file"
        );

        let p = preview_text(&d.config(), &d.vocab(), &store, &text).unwrap();
        assert!(p.nothing_to_do, "{p:?}");
        assert!(p.services.iter().all(|s| s.status == "unchanged"));
        assert_eq!(
            p.key_missing,
            ["my-claude"],
            "stored keys count, the unset official one is missing"
        );
        let a = apply(&d, &store, &text, None, &keys(&[])).unwrap();
        assert_eq!(
            (a.added, a.updated, a.route_changed, a.vocab_added),
            (0, 0, 0, 0)
        );
        assert_eq!(a.unchanged, 4);
        assert_eq!(std::fs::read_to_string(d.config()).unwrap(), before);
    }

    #[test]
    fn export_adds_toml_extension() {
        let d = TempDir::new("ext");
        let store = MemorySecretStore::new();
        export_to(&d.config(), &d.vocab(), &store, &d.0.join("mine")).unwrap();
        assert!(d.0.join("mine.toml").exists());
        export_to(&d.config(), &d.vocab(), &store, &d.0.join("my.config")).unwrap();
        assert!(d.0.join("my.config.toml").exists());
    }

    #[test]
    fn deleted_service_comes_back_after_import() {
        let d = TempDir::new("back");
        let store = MemorySecretStore::new();
        apply(
            &d,
            &store,
            GEMINI_CLAUDE,
            None,
            &keys(&[("my-gemini", KEY)]),
        )
        .unwrap();
        let out = d.0.join("out.toml");
        export_to(&d.config(), &d.vocab(), &store, &out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        // Remove one service from config.toml.
        let cfg_text = std::fs::read_to_string(d.config()).unwrap();
        let mut doc: toml_edit::DocumentMut = cfg_text.parse().unwrap();
        doc["cloud"].as_array_of_tables_mut().unwrap().remove(1);
        std::fs::write(d.config(), doc.to_string()).unwrap();
        let p = preview_text(&d.config(), &d.vocab(), &store, &text).unwrap();
        assert_eq!(p.services.iter().filter(|s| s.status == "added").count(), 1);
        apply(&d, &store, &text, None, &keys(&[])).unwrap();
        assert_eq!(load_config(&d.config()).unwrap().cloud.len(), 2);
    }

    #[test]
    fn write_failure_restores_config_vocab_and_keys() {
        let d = TempDir::new("fail");
        let store = MemorySecretStore::new();
        std::fs::write(d.vocab(), "old\n").unwrap();
        store
            .set("my-gemini", &ApiKey::new("old-key-value"))
            .unwrap();
        // A directory where the temp file goes makes the atomic config write fail.
        std::fs::create_dir(d.0.join("config.toml.tmp")).unwrap();
        let text = format!("{GEMINI_CLAUDE}\n[vocabulary]\nwords = [\"new-word\"]\n");
        let err = apply(&d, &store, &text, None, &keys(&[("my-gemini", KEY)])).unwrap_err();
        assert_eq!(err.code, "config_save_failed");
        assert!(!d.config().exists(), "config.toml untouched");
        assert_eq!(std::fs::read_to_string(d.vocab()).unwrap(), "old\n");
        assert_eq!(
            store.get("my-gemini").unwrap().unwrap().expose(),
            "old-key-value"
        );

        // No vocabulary file before: it is removed again.
        std::fs::remove_file(d.vocab()).unwrap();
        let fresh = MemorySecretStore::new();
        let err = apply(&d, &fresh, &text, None, &keys(&[("my-gemini", KEY)])).unwrap_err();
        assert_eq!(err.code, "config_save_failed");
        assert!(!d.vocab().exists());
        assert!(fresh.get("my-gemini").unwrap().is_none());
    }

    #[test]
    fn vocab_write_failure_restores_keys_and_leaves_config() {
        let d = TempDir::new("vfail");
        let store = MemorySecretStore::new();
        // A directory at vocab.txt makes the rename fail.
        std::fs::create_dir(d.vocab()).unwrap();
        let text = format!("{GEMINI_CLAUDE}\n[vocabulary]\nwords = [\"w\"]\n");
        let err = apply(&d, &store, &text, None, &keys(&[("my-gemini", KEY)]));
        assert!(err.is_err());
        assert!(!d.config().exists());
        assert!(store.get("my-gemini").unwrap().is_none());
    }

    #[test]
    fn endpoint_change_drops_old_key_and_kind_change_is_an_update() {
        let d = TempDir::new("kind");
        let store = MemorySecretStore::new();
        apply(&d, &store, LAN, None, &keys(&[("lan-asr", KEY)])).unwrap();
        let moved = LAN
            .replace("192.168.1.10:8002", "192.168.1.99:8002")
            .replace("default_keys = { \"lan-asr\" = \"shared-key\" }", "");
        let p = preview(&d, &store, &moved).unwrap();
        let asr = p.services.iter().find(|s| s.id == "lan-asr").unwrap();
        assert_eq!(asr.status, "updated");
        assert!(asr.drops_old_key);
        assert_eq!(p.key_missing, ["lan-asr"]);
        apply(&d, &store, &moved, None, &keys(&[])).unwrap();
        assert!(
            store.get("lan-asr").unwrap().is_none(),
            "key must not go to a new host"
        );

        // Plain file: gemini replaces the compat service of the same id.
        let kind = "[v2c]\nformat = 1\n[[cloud]]\nid = \"lan-asr\"\nkind = \"gemini\"\n";
        let p = preview(&d, &store, kind).unwrap();
        assert_eq!(p.services[0].status, "updated");
        assert!(p.services[0].drops_old_key);
    }

    #[test]
    fn preset_file_keeps_kind_conflict_rule_and_asr_choice() {
        let d = TempDir::new("preset");
        let store = MemorySecretStore::new();
        // Existing official service named like a preset service.
        let other = "[v2c]\nformat = 1\n[[cloud]]\nid = \"tn-tea-asr\"\nkind = \"gemini\"\n";
        apply(&d, &store, other, None, &keys(&[])).unwrap();
        assert_eq!(
            preview(&d, &store, TN).unwrap_err().code,
            "preset_id_conflict"
        );
        assert_eq!(
            apply(&d, &store, TN, Some("tn-breeze-asr"), &keys(&[]))
                .unwrap_err()
                .code,
            "preset_id_conflict"
        );

        let d = TempDir::new("preset2");
        let p = preview(&d, &store, TN).unwrap();
        assert!(p.is_preset);
        assert_eq!(p.asr_choices.len(), 2);
        assert!(p.route.iter().all(|c| c.field != "asr"));
        assert_eq!(
            apply(&d, &store, TN, None, &keys(&[])).unwrap_err().code,
            "preset_bad_choice"
        );
        assert_eq!(
            apply(&d, &store, TN, Some("nope"), &keys(&[]))
                .unwrap_err()
                .code,
            "preset_bad_choice"
        );
        apply(&d, &store, TN, Some("tn-breeze-asr"), &keys(&[])).unwrap();
        assert_eq!(load_config(&d.config()).unwrap().route.asr, "tn-breeze-asr");
    }

    #[test]
    fn no_asr_choices_uses_file_route_or_leaves_it() {
        let d = TempDir::new("asr");
        let store = MemorySecretStore::new();
        apply(&d, &store, VOCAB_ONLY, None, &keys(&[])).unwrap();
        assert_eq!(load_config(&d.config()).unwrap().route.asr, "local");
        apply(&d, &store, GEMINI_CLAUDE, Some("ignored"), &keys(&[])).unwrap();
        assert_eq!(load_config(&d.config()).unwrap().route.asr, "my-gemini");
    }

    #[test]
    fn name_clash_with_untouched_service_is_refused() {
        let d = TempDir::new("name");
        let store = MemorySecretStore::new();
        let mine = "[v2c]\nformat = 1\n[[cloud]]\nid = \"a\"\nname = \"Same\"\nkind = \"open_ai_compat\"\nbase_url = \"http://10.0.0.1:1/v1\"\n";
        apply(&d, &store, mine, None, &keys(&[])).unwrap();
        let clash = "[v2c]\nformat = 1\n[[cloud]]\nid = \"b\"\nname = \"same\"\nkind = \"open_ai_compat\"\nbase_url = \"http://10.0.0.2:1/v1\"\n";
        assert_eq!(preview(&d, &store, clash).unwrap_err().code, "name_taken");
        assert_eq!(
            apply(&d, &store, clash, None, &keys(&[])).unwrap_err().code,
            "name_taken"
        );
        assert_eq!(load_config(&d.config()).unwrap().cloud.len(), 1);
    }

    #[test]
    fn read_text_limits() {
        let d = TempDir::new("read");
        let p = d.0.join("a.toml");
        std::fs::write(&p, "[v2c]\nformat = 1\n").unwrap();
        assert!(read_text(p.to_str().unwrap()).is_ok());
        let txt = d.0.join("a.txt");
        std::fs::write(&txt, "x").unwrap();
        assert_eq!(
            read_text(txt.to_str().unwrap()).unwrap_err().code,
            "file_not_toml"
        );
        let big = d.0.join("big.toml");
        std::fs::write(&big, vec![b'#'; portable::MAX_FILE_BYTES + 1]).unwrap();
        assert_eq!(
            read_text(big.to_str().unwrap()).unwrap_err().code,
            "too_large"
        );
        assert_eq!(
            read_text("\\\\server\\share\\a.toml").unwrap_err().code,
            "file_unreadable"
        );
        assert_eq!(
            read_text(d.0.join("none.toml").to_str().unwrap())
                .unwrap_err()
                .code,
            "file_unreadable"
        );
    }

    #[test]
    fn default_name_is_sanitised() {
        assert_eq!(
            safe_default_name("v2c-config-20261113.toml"),
            "v2c-config-20261113.toml"
        );
        assert_eq!(safe_default_name("..\\evil.toml"), "v2c-config.toml");
        assert_eq!(safe_default_name("x.exe"), "v2c-config.toml");
    }

    #[test]
    fn errors_never_carry_file_content() {
        let d = TempDir::new("priv");
        let store = MemorySecretStore::new();
        let bad = "[v2c]\nformat = 1\n[[cloud]]\nid = \"secret-id\"\nkind = \"gemni\"\n";
        let e = preview(&d, &store, bad).unwrap_err();
        assert!(!format!("{e:?}").contains("secret-id") && !format!("{e:?}").contains("gemni"));
    }

    const COMPAT_X: &str = "[v2c]\nformat = 1\n[route]\nasr = \"x\"\n[[cloud]]\nid = \"x\"\nkind = \"open_ai_compat\"\nbase_url = \"http://10.0.0.1:1/v1\"\nasr_model = \"m\"\n";

    #[test]
    fn update_that_breaks_a_routed_service_is_refused() {
        let d = TempDir::new("route1");
        let store = MemorySecretStore::new();
        apply(&d, &store, COMPAT_X, None, &keys(&[])).unwrap();
        assert_eq!(load_config(&d.config()).unwrap().route.asr, "x");
        let before = std::fs::read(d.config()).unwrap();

        // Same id as a Claude service, no [route] in the file: asr would point at Claude.
        let to_claude = "[v2c]\nformat = 1\n[[cloud]]\nid = \"x\"\nkind = \"claude\"\n";
        assert_eq!(
            preview(&d, &store, to_claude).unwrap_err().code,
            "route_no_asr"
        );
        assert_eq!(
            apply(&d, &store, to_claude, None, &keys(&[]))
                .unwrap_err()
                .code,
            "route_no_asr"
        );

        // Same kind, but the asr model is dropped.
        let no_model = "[v2c]\nformat = 1\n[[cloud]]\nid = \"x\"\nkind = \"open_ai_compat\"\nbase_url = \"http://10.0.0.1:1/v1\"\nllm_model = \"t\"\n";
        assert_eq!(
            preview(&d, &store, no_model).unwrap_err().code,
            "route_no_model"
        );
        assert_eq!(
            apply(&d, &store, no_model, None, &keys(&[]))
                .unwrap_err()
                .code,
            "route_no_model"
        );

        assert_eq!(
            std::fs::read(d.config()).unwrap(),
            before,
            "config.toml untouched"
        );
        // A file that also moves the route away is fine.
        let ok = format!("{no_model}[route]\nasr = \"local\"\n");
        apply(&d, &store, &ok, None, &keys(&[])).unwrap();
    }

    #[test]
    fn file_moves_asr_away_from_a_service_it_also_breaks() {
        let d = TempDir::new("route3");
        let store = MemorySecretStore::new();
        apply(&d, &store, COMPAT_X, None, &keys(&[])).unwrap();
        let text = "[v2c]
format = 1
[route]
asr = \"my-gemini\"
[[cloud]]
id = \"x\"
kind = \"claude\"
[[cloud]]
id = \"my-gemini\"
kind = \"gemini\"
";
        let p = preview(&d, &store, text).unwrap();
        assert_eq!(p.route[0].new, "my-gemini");
        apply(&d, &store, text, None, &keys(&[])).unwrap();
        assert_eq!(load_config(&d.config()).unwrap().route.asr, "my-gemini");
    }

    #[test]
    fn preexisting_broken_route_to_untouched_service_does_not_block() {
        let d = TempDir::new("route2");
        let store = MemorySecretStore::new();
        std::fs::write(d.config(), "[route]\nasr = \"gone\"\n").unwrap();
        apply(&d, &store, VOCAB_ONLY, None, &keys(&[])).unwrap();
    }

    #[test]
    fn changed_file_is_refused_by_hash() {
        let d = TempDir::new("hash");
        let p = d.0.join("a.toml");
        std::fs::write(&p, VOCAB_ONLY).unwrap();
        let path = p.to_str().unwrap();
        let (_, hash) = read_file(path, None).unwrap();
        assert_eq!(hash.len(), 64);
        assert!(read_file(path, Some(&hash)).is_ok());
        std::fs::write(&p, format!("{VOCAB_ONLY}\n# edited\n")).unwrap();
        assert_eq!(
            read_file(path, Some(&hash)).unwrap_err().code,
            "file_changed"
        );
    }

    #[test]
    fn key_only_import_is_possible() {
        let d = TempDir::new("keyonly");
        let store = MemorySecretStore::new();
        apply(&d, &store, GEMINI_CLAUDE, None, &keys(&[])).unwrap();
        assert!(preview(&d, &store, GEMINI_CLAUDE).unwrap().nothing_to_do);
        let a = apply(
            &d,
            &store,
            GEMINI_CLAUDE,
            None,
            &keys(&[("my-claude", KEY)]),
        )
        .unwrap();
        assert_eq!(a.key_missing, ["my-gemini"]);
        assert_eq!(store.get("my-claude").unwrap().unwrap().expose(), KEY);
    }
}
