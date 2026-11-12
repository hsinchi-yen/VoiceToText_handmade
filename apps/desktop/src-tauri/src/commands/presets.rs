//! Preset configurations (T4.1): one dialog applies a bundle of LAN endpoints and routes.
//!
//! A preset is a TOML file with three whitelisted sections: `[preset]` (title, id prefix,
//! which speech recognition endpoints the user may choose, which need an API key), `[[cloud]]`
//! (the services) and `[route]` (polish / translate targets). Two presets are built in
//! (`TN_CUSTOM_LLM_ADDR.toml`, `RD_CUSTOM_LLM_ADDR.toml`, embedded with `include_str!`); the
//! user may also load one from a `.toml` file (at most [`MAX_FILE_BYTES`]). Any other
//! top-level section is ignored, never written, and listed in the preview.
//!
//! Commands (main window only):
//! - `presets_list`: the built-ins with a short summary.
//! - `presets_preview(source)`: what applying would do (after model detection).
//! - `presets_apply(source, asr_choice, keys)`: writes the services and routes, then emits
//!   `v2c:config-changed`.
//! - `presets_pick_file`: native file dialog for 「從檔案載入」; returns the path.
//!
//! Applying reuses `cloud.rs`: the same `CONFIG_LOCK`, the comment-preserving `update_config`,
//! `upsert_service`, name rules and timeout range. It only adds or updates the preset's own
//! services (ids start with `id_prefix`); other services (another preset, the user's own) are
//! never touched, so applying TN then RD then TN keeps both groups and their keys.
//!
//! Validation rejects the whole preset (nothing is written) with a short error code.
//! Keys: only ids listed in `needs_key` are accepted; a key is stored before `config.toml` is
//! written and put back if the write fails (like `save_service`). If an existing preset
//! service's host changes, its old key is deleted unless a new one is given.
//!
//! Logs: preset name (`TN`, `RD`, or `file`), service count, error code, elapsed ms. Never
//! keys, never the file path.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use toml_edit::{Item, Table, Value};
use tracing::{info, warn};
use v2c_core::config::{CloudKind, CloudService, Config, RouteConfig, TimeoutConfig, LOCAL};
use v2c_core::route::{self, CloudHttp};
use v2c_core::secrets::{ApiKey, MemorySecretStore, SecretStore};
use zeroize::Zeroize;

use super::cloud::{
    blocking, check_caller, check_name_unique, config_lock, config_path, load_config,
    normalize_name, parse_asr_timeout, secret_store, set_str, set_value, test_draft, update_config,
    upsert_service, CloudDraft, MsInput,
};
use crate::tray::CONFIG_CHANGED_EVENT;

/// Largest preset file accepted.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Model-list query timeout during detection.
const DETECT_TIMEOUT: Duration = Duration::from_secs(3);

const BUILTINS: &[(&str, &str)] = &[
    ("TN", include_str!("../../presets/TN_CUSTOM_LLM_ADDR.toml")),
    ("RD", include_str!("../../presets/RD_CUSTOM_LLM_ADDR.toml")),
];

/// Top-level sections a preset may use.
const WHITELIST: &[&str] = &["preset", "cloud", "route"];

// ---------------------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------------------

/// Where a preset comes from: `{ "builtin": "TN" }` or `{ "file": "C:\\...\\x.toml" }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetSource {
    Builtin(String),
    File(String),
}

/// Pasted keys by service id. Zeroized on drop; `Debug` shows ids only.
#[derive(Deserialize, Default)]
#[serde(transparent)]
pub struct Keys(HashMap<String, String>);

impl Drop for Keys {
    fn drop(&mut self) {
        for v in self.0.values_mut() {
            v.zeroize();
        }
    }
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ServiceSummary {
    pub id: String,
    pub name: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct PresetSummary {
    /// `TN` / `RD`.
    pub name: String,
    pub title: String,
    pub id_prefix: String,
    pub services: Vec<ServiceSummary>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ServicePreview {
    pub id: String,
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub asr_model: Option<String>,
    pub llm_model: Option<String>,
    /// `detected` / `file` for the text model (`None` when the service has none).
    pub model_source: Option<&'static str>,
    pub asr_timeout_ms: Option<u64>,
    /// Subset of `asr` (if chosen), `polish`, `translate`.
    pub purposes: Vec<&'static str>,
    pub needs_key: bool,
    /// A key is already stored for this id.
    pub has_key: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AsrChoice {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RoutePreview {
    pub polish: String,
    pub translate: String,
    pub cloud_fallback_to_local: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct PresetPreview {
    pub title: String,
    pub id_prefix: String,
    pub services: Vec<ServicePreview>,
    pub route: RoutePreview,
    pub asr_choices: Vec<AsrChoice>,
    pub needs_key: Vec<String>,
    /// Top-level sections of the file that are not used.
    pub ignored: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AppliedService {
    pub id: String,
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub asr_model: Option<String>,
    pub llm_model: Option<String>,
    pub model_source: Option<&'static str>,
    pub asr_timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct PresetApplied {
    pub title: String,
    pub asr_choice: String,
    pub services: Vec<AppliedService>,
    /// Services that need a key and have none stored.
    pub key_missing: Vec<String>,
}

// ---------------------------------------------------------------------------------------
// Parsing and validation
// ---------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PresetMeta {
    title: String,
    id_prefix: String,
    #[serde(default)]
    asr_choices: Vec<String>,
    #[serde(default)]
    needs_key: Vec<String>,
    #[serde(default)]
    detect_llm_model: bool,
}

#[derive(Debug, Deserialize)]
struct PresetFile {
    preset: PresetMeta,
}

#[derive(Debug)]
struct Preset {
    meta: PresetMeta,
    services: Vec<CloudService>,
    route: RouteConfig,
    ignored: Vec<String>,
}

/// A parsed preset plus where each text model came from.
#[derive(Debug)]
struct Resolved {
    /// `TN`, `RD` or `file` (for logs).
    label: String,
    preset: Preset,
    sources: HashMap<String, &'static str>,
}

fn parse_preset(text: &str) -> Result<Preset, &'static str> {
    let file: PresetFile = toml::from_str(text).map_err(|_| "preset_invalid")?;
    let cfg = Config::from_toml_str(text).map_err(|_| "preset_invalid")?;
    let table: toml::Table = toml::from_str(text).map_err(|_| "preset_invalid")?;
    let mut ignored: Vec<String> = table
        .keys()
        .filter(|k| !WHITELIST.contains(&k.as_str()))
        .cloned()
        .collect();
    ignored.sort();
    Ok(Preset {
        meta: file.preset,
        services: cfg.cloud,
        route: cfg.route,
        ignored,
    })
}

fn is_ref(id: &str, ids: &HashSet<&str>) -> bool {
    id == LOCAL || ids.contains(id)
}

/// Checks the whole preset against the current config. Normalises names and URLs in place.
fn validate(preset: &mut Preset, cfg: &Config) -> Result<(), &'static str> {
    let prefix = preset.meta.id_prefix.trim().to_owned();
    if prefix.is_empty() || preset.services.is_empty() {
        return Err("preset_invalid");
    }
    let mut seen = HashSet::new();
    for svc in &preset.services {
        if !svc.id.starts_with(&prefix) {
            return Err("preset_id_prefix");
        }
        if !seen.insert(svc.id.clone()) {
            return Err("preset_duplicate_id");
        }
    }
    // Names must not clash with services outside this preset or with each other.
    let mut others: Vec<CloudService> = cfg
        .cloud
        .iter()
        .filter(|s| !seen.contains(&s.id))
        .cloned()
        .collect();
    for svc in &mut preset.services {
        if svc.kind != CloudKind::OpenAiCompat {
            return Err("preset_kind");
        }
        let url = svc
            .base_url
            .as_deref()
            .map(|u| u.trim().trim_end_matches('/'))
            .filter(|u| !u.is_empty())
            .ok_or("base_url_required")?
            .to_owned();
        svc.base_url = Some(url);
        route::effective_base_url(svc).map_err(|_| "invalid_base_url")?;
        svc.name = normalize_name(svc.kind, &svc.name)?;
        if let Some(name) = &svc.name {
            check_name_unique(name, &svc.id, &others)?;
        }
        parse_asr_timeout(&svc.asr_timeout_ms.map(|n| MsInput::Number(n as f64)))?;
        others.push(svc.clone());
    }
    let ids: HashSet<&str> = preset.services.iter().map(|s| s.id.as_str()).collect();
    let refs_ok = !preset.meta.asr_choices.is_empty()
        && preset.meta.asr_choices.iter().all(|c| is_ref(c, &ids))
        && preset
            .meta
            .needs_key
            .iter()
            .all(|k| ids.contains(k.as_str()))
        && is_ref(&preset.route.polish, &ids)
        && is_ref(&preset.route.translate, &ids);
    if refs_ok {
        Ok(())
    } else {
        Err("preset_bad_reference")
    }
}

fn read_preset_file(path: &str) -> Result<String, &'static str> {
    let p = Path::new(path);
    let is_toml = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
    if !is_toml {
        return Err("preset_not_toml");
    }
    let meta = std::fs::metadata(p).map_err(|_| "preset_unreadable")?;
    if !meta.is_file() {
        return Err("preset_unreadable");
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err("preset_too_large");
    }
    std::fs::read_to_string(p).map_err(|_| "preset_unreadable")
}

fn load_source(source: &PresetSource) -> Result<Resolved, &'static str> {
    let (label, text) = match source {
        PresetSource::Builtin(name) => {
            let (n, t) = BUILTINS
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .ok_or("preset_unknown")?;
            ((*n).to_owned(), (*t).to_owned())
        }
        PresetSource::File(path) => ("file".to_owned(), read_preset_file(path)?),
    };
    let preset = parse_preset(&text)?;
    Ok(Resolved {
        label,
        preset,
        sources: HashMap::new(),
    })
}

// ---------------------------------------------------------------------------------------
// Model detection
// ---------------------------------------------------------------------------------------

/// For each service with a text model: when the preset asks for it, queries the endpoint's
/// model list (no key, 3 s); exactly one model replaces the file's value. Records the source
/// (`detected` / `file`) per service.
async fn detect_models(r: &mut Resolved, http: &CloudHttp) {
    r.sources.clear();
    let detect = r.preset.meta.detect_llm_model;
    let timeouts = TimeoutConfig {
        asr_cloud_ms: DETECT_TIMEOUT.as_millis() as u64,
        llm_cloud_ms: DETECT_TIMEOUT.as_millis() as u64,
    };
    let mut detected = 0usize;
    for svc in &mut r.preset.services {
        if svc.llm_model.is_none() {
            continue;
        }
        let mut source = "file";
        if detect {
            if let Some(model) = detect_one(svc, http, &timeouts).await {
                svc.llm_model = Some(model);
                source = "detected";
                detected += 1;
            }
        }
        r.sources.insert(svc.id.clone(), source);
    }
    if detect {
        info!(preset = %r.label, detected, "preset model detection");
    }
}

async fn detect_one(svc: &CloudService, http: &CloudHttp, t: &TimeoutConfig) -> Option<String> {
    let url = svc.base_url.clone()?;
    let draft = CloudDraft {
        id: None,
        kind: CloudKind::OpenAiCompat,
        name: None,
        base_url: Some(url),
        api_key: None,
        asr_model: None,
        llm_model: None,
        asr_timeout_ms: None,
    };
    let probe = tokio::time::timeout(
        DETECT_TIMEOUT,
        test_draft(
            &draft,
            &MemorySecretStore::new(),
            &[],
            http,
            t,
            false,
            false,
        ),
    )
    .await
    .ok()?;
    let (result, _) = probe.ok()?;
    match result.models.as_deref() {
        Some([only]) if !only.trim().is_empty() => Some(only.trim().to_owned()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// Command logic (testable without Tauri)
// ---------------------------------------------------------------------------------------

fn summaries() -> Vec<PresetSummary> {
    BUILTINS
        .iter()
        .filter_map(|(name, text)| {
            let p = parse_preset(text).ok()?;
            Some(PresetSummary {
                name: (*name).to_owned(),
                title: p.meta.title,
                id_prefix: p.meta.id_prefix,
                services: p
                    .services
                    .into_iter()
                    .map(|s| ServiceSummary {
                        id: s.id,
                        name: s.name,
                        base_url: s.base_url,
                    })
                    .collect(),
            })
        })
        .collect()
}

fn build_preview(
    config_path: &Path,
    stored: &dyn SecretStore,
    r: &mut Resolved,
) -> Result<PresetPreview, &'static str> {
    let cfg = load_config(config_path)?;
    validate(&mut r.preset, &cfg)?;
    let p = &r.preset;
    let services = p
        .services
        .iter()
        .map(|s| {
            let mut purposes = Vec::new();
            if p.meta.asr_choices.contains(&s.id) {
                purposes.push("asr");
            }
            if p.route.polish == s.id {
                purposes.push("polish");
            }
            if p.route.translate == s.id {
                purposes.push("translate");
            }
            ServicePreview {
                id: s.id.clone(),
                name: s.name.clone(),
                base_url: s.base_url.clone(),
                asr_model: s.asr_model.clone(),
                llm_model: s.llm_model.clone(),
                model_source: r.sources.get(&s.id).copied(),
                asr_timeout_ms: s.asr_timeout_ms,
                purposes,
                needs_key: p.meta.needs_key.contains(&s.id),
                has_key: stored
                    .get(&s.id)
                    .ok()
                    .flatten()
                    .is_some_and(|k| !k.is_empty()),
            }
        })
        .collect();
    let asr_choices = p
        .meta
        .asr_choices
        .iter()
        .map(|id| AsrChoice {
            id: id.clone(),
            name: p
                .services
                .iter()
                .find(|s| &s.id == id)
                .and_then(|s| s.name.clone())
                .unwrap_or_else(|| id.clone()),
        })
        .collect();
    Ok(PresetPreview {
        title: p.meta.title.clone(),
        id_prefix: p.meta.id_prefix.clone(),
        services,
        route: RoutePreview {
            polish: p.route.polish.clone(),
            translate: p.route.translate.clone(),
            cloud_fallback_to_local: p.route.cloud_fallback_to_local,
        },
        asr_choices,
        needs_key: p.meta.needs_key.clone(),
        ignored: p.ignored.clone(),
    })
}

/// A key entry that was changed, so it can be put back.
struct KeyChange {
    id: String,
    old: Option<ApiKey>,
}

fn restore_keys(stored: &dyn SecretStore, changes: &[KeyChange]) {
    for c in changes.iter().rev() {
        let restored = match &c.old {
            Some(k) => stored.set(&c.id, k),
            None => stored.delete(&c.id),
        };
        if let Err(e) = restored {
            warn!(service = %c.id, error = %e, "preset: could not restore key");
        }
    }
}

/// Lower-cased host of the service's endpoint.
fn host_of(svc: &CloudService) -> Option<String> {
    let url = route::effective_base_url(svc).ok()?;
    reqwest::Url::parse(&url)
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

fn apply_resolved(
    config_path: &Path,
    stored: &dyn SecretStore,
    r: &mut Resolved,
    asr_choice: &str,
    keys: &Keys,
) -> Result<PresetApplied, &'static str> {
    let _guard = config_lock();
    let cfg = load_config(config_path)?;
    validate(&mut r.preset, &cfg)?;
    let p = &r.preset;
    if !p.meta.asr_choices.iter().any(|c| c == asr_choice) {
        return Err("preset_bad_choice");
    }
    let mut new_keys: Vec<(&str, ApiKey)> = Vec::new();
    for (id, raw) in &keys.0 {
        if !p.meta.needs_key.contains(id) {
            return Err("preset_key_not_allowed");
        }
        let k = raw.trim();
        if k.is_empty() {
            continue;
        }
        if k.chars().any(char::is_control) {
            return Err("invalid_key");
        }
        new_keys.push((id, ApiKey::new(k)));
    }

    // Keys first (restored if the config write fails): a pasted key replaces the old one; a
    // changed host without a new key drops the old key.
    let mut changes: Vec<KeyChange> = Vec::new();
    for svc in &p.services {
        let new = new_keys
            .iter()
            .find(|(id, _)| *id == svc.id)
            .map(|(_, k)| k);
        let host_changed = cfg
            .cloud_service(&svc.id)
            .is_some_and(|prev| host_of(prev) != host_of(svc));
        if new.is_none() && !host_changed {
            continue;
        }
        let Ok(old) = stored.get(&svc.id) else {
            restore_keys(stored, &changes);
            return Err("keyring_failed");
        };
        let done = match new {
            Some(k) => stored.set(&svc.id, k),
            None if old.is_some() => stored.delete(&svc.id),
            None => continue,
        };
        if done.is_err() {
            warn!(service = %svc.id, "preset: could not update key");
            restore_keys(stored, &changes);
            return Err("keyring_failed");
        }
        changes.push(KeyChange {
            id: svc.id.clone(),
            old,
        });
    }
    drop(new_keys);

    let written = update_config(config_path, |doc, _current| {
        for svc in &p.services {
            upsert_service(doc, svc)?;
        }
        let route = doc
            .entry("route")
            .or_insert(Item::Table(Table::new()))
            .as_table_like_mut()
            .ok_or("config_invalid")?;
        set_str(route, "asr", asr_choice);
        set_str(route, "polish", &p.route.polish);
        set_str(route, "translate", &p.route.translate);
        set_value(
            route,
            "cloud_fallback_to_local",
            Value::from(p.route.cloud_fallback_to_local),
        );
        Ok(())
    });
    if let Err(code) = written {
        restore_keys(stored, &changes);
        return Err(code);
    }

    let key_missing = p
        .meta
        .needs_key
        .iter()
        .filter(|id| !stored.get(id).ok().flatten().is_some_and(|k| !k.is_empty()))
        .cloned()
        .collect();
    Ok(PresetApplied {
        title: p.meta.title.clone(),
        asr_choice: asr_choice.to_owned(),
        services: p
            .services
            .iter()
            .map(|s| AppliedService {
                id: s.id.clone(),
                name: s.name.clone(),
                base_url: s.base_url.clone(),
                asr_model: s.asr_model.clone(),
                llm_model: s.llm_model.clone(),
                model_source: r.sources.get(&s.id).copied(),
                asr_timeout_ms: s.asr_timeout_ms,
            })
            .collect(),
        key_missing,
    })
}

// ---------------------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------------------

fn http_client() -> Result<CloudHttp, &'static str> {
    route::cloud_http_client().map_err(|_| "internal_error")
}

#[tauri::command]
pub fn presets_list(window: tauri::WebviewWindow) -> Result<Vec<PresetSummary>, &'static str> {
    check_caller(window.label())?;
    Ok(summaries())
}

#[tauri::command]
pub async fn presets_preview(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    source: PresetSource,
) -> Result<PresetPreview, &'static str> {
    check_caller(window.label())?;
    let started = Instant::now();
    let mut resolved = {
        let source = source.clone();
        blocking(move || load_source(&source)).await
    }
    .inspect_err(|code| warn!(code, "preset preview refused"))?;
    detect_models(&mut resolved, &http_client()?).await;
    let path = config_path(&app);
    let label = resolved.label.clone();
    let result = blocking(move || build_preview(&path, &secret_store()?, &mut resolved)).await;
    match &result {
        Ok(p) => info!(
            preset = %label,
            services = p.services.len(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "preset previewed"
        ),
        Err(code) => warn!(preset = %label, code, "preset preview refused"),
    }
    result
}

#[tauri::command]
pub async fn presets_apply(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    source: PresetSource,
    asr_choice: String,
    keys: Keys,
) -> Result<PresetApplied, &'static str> {
    check_caller(window.label())?;
    let started = Instant::now();
    let mut resolved = blocking(move || load_source(&source))
        .await
        .inspect_err(|code| warn!(code, "preset apply refused"))?;
    detect_models(&mut resolved, &http_client()?).await;
    let path = config_path(&app);
    let label = resolved.label.clone();
    let result = blocking(move || {
        apply_resolved(&path, &secret_store()?, &mut resolved, &asr_choice, &keys)
    })
    .await;
    match &result {
        Ok(a) => {
            info!(
                preset = %label,
                services = a.services.len(),
                key_missing = a.key_missing.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "preset applied"
            );
            let _ = app.emit(CONFIG_CHANGED_EVENT, ());
        }
        Err(code) => warn!(preset = %label, code, "preset apply refused"),
    }
    result
}

/// Native file dialog for 「從檔案載入」. `Ok(None)` when the user closed it.
#[tauri::command]
pub async fn presets_pick_file(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Option<String>, &'static str> {
    use tauri_plugin_dialog::DialogExt;

    check_caller(window.label())?;
    let picker = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        picker
            .dialog()
            .file()
            .set_title("選擇預設組態檔")
            .add_filter("TOML 組態檔", &["toml"])
            .blocking_pick_file()
    })
    .await
    .map_err(|_| "internal_error")?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TN: &str = BUILTINS[0].1;
    const RD: &str = BUILTINS[1].1;
    const KEY: &str = "sk-TESTSECRET-abcdefgh-WXYZ";

    impl Keys {
        fn of(pairs: &[(&str, &str)]) -> Keys {
            Keys(
                pairs
                    .iter()
                    .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                    .collect(),
            )
        }
    }

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("v2c-preset-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn config(&self) -> std::path::PathBuf {
            self.0.join("config.toml")
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn resolved(text: &str) -> Resolved {
        Resolved {
            label: "TN".into(),
            preset: parse_preset(text).unwrap(),
            sources: HashMap::new(),
        }
    }

    fn apply(
        cfg: &Path,
        store: &dyn SecretStore,
        text: &str,
        choice: &str,
        keys: &Keys,
    ) -> Result<PresetApplied, &'static str> {
        apply_resolved(cfg, store, &mut resolved(text), choice, keys)
    }

    fn key_of(store: &MemorySecretStore, id: &str) -> Option<String> {
        store.get(id).unwrap().map(|k| k.expose().to_owned())
    }

    // ---- built-ins ----

    #[test]
    fn builtins_parse_with_specified_endpoints() {
        for (text, prefix, host, title) in [
            (TN, "tn-", "10.20.30.23", "TN 區網服務"),
            (RD, "rd-", "10.88.95.2", "RD 區網服務"),
        ] {
            let mut p = parse_preset(text).unwrap();
            validate(&mut p, &Config::default()).unwrap();
            assert_eq!(p.meta.title, title);
            assert_eq!(p.meta.id_prefix, prefix);
            assert!(p.meta.detect_llm_model);
            assert_eq!(
                p.meta.asr_choices,
                vec![format!("{prefix}tea-asr"), format!("{prefix}breeze-asr")]
            );
            assert_eq!(p.meta.needs_key, vec![format!("{prefix}breeze-asr")]);
            assert_eq!(p.route.polish, format!("{prefix}qwen"));
            assert_eq!(p.route.translate, format!("{prefix}qwen"));
            assert!(p.route.cloud_fallback_to_local);
            assert!(p.ignored.is_empty());
            let by = |suffix: &str| {
                p.services
                    .iter()
                    .find(|s| s.id == format!("{prefix}{suffix}"))
                    .unwrap()
            };
            let tea = by("tea-asr");
            assert_eq!(
                tea.base_url.as_deref(),
                Some(format!("http://{host}:8001/v1").as_str())
            );
            assert_eq!(tea.asr_model.as_deref(), Some("tea-asr-1"));
            assert_eq!(tea.asr_timeout_ms, None);
            let breeze = by("breeze-asr");
            assert_eq!(
                breeze.base_url.as_deref(),
                Some(format!("http://{host}:8002/v1").as_str())
            );
            assert_eq!(breeze.asr_model.as_deref(), Some("breeze-asr-25"));
            assert_eq!(breeze.asr_timeout_ms, Some(8000));
            let qwen = by("qwen");
            assert_eq!(
                qwen.base_url.as_deref(),
                Some(format!("http://{host}:8000/v1").as_str())
            );
            assert_eq!(qwen.llm_model.as_deref(), Some("Qwen3.8-27B-UD-Q6_K_L"));
            assert!(p.services.iter().all(|s| s
                .name
                .as_deref()
                .unwrap()
                .starts_with(&format!("{} ", prefix.trim_end_matches('-').to_uppercase()))));
        }
        let names: Vec<_> = summaries().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["TN", "RD"]);
    }

    // ---- apply: config shapes ----

    #[test]
    fn apply_to_missing_and_empty_config() {
        let dir = TempDir::new("missing");
        let store = MemorySecretStore::new();
        let out = apply(&dir.config(), &store, TN, "tn-tea-asr", &Keys::default()).unwrap();
        assert_eq!(out.services.len(), 3);
        assert_eq!(out.key_missing, vec!["tn-breeze-asr"]);
        let cfg = load_config(&dir.config()).unwrap();
        assert_eq!(cfg.route.asr, "tn-tea-asr");
        assert_eq!(cfg.route.polish, "tn-qwen");
        assert_eq!(cfg.route.translate, "tn-qwen");
        assert_eq!(cfg.cloud.len(), 3);

        let dir = TempDir::new("empty");
        std::fs::write(dir.config(), "").unwrap();
        apply(&dir.config(), &store, RD, "rd-breeze-asr", &Keys::default()).unwrap();
        let cfg = load_config(&dir.config()).unwrap();
        assert_eq!(cfg.route.asr, "rd-breeze-asr");
        assert_eq!(
            cfg.cloud_service("rd-breeze-asr").unwrap().asr_timeout_ms,
            Some(8000)
        );
    }

    #[test]
    fn apply_keeps_comments_and_user_services() {
        let dir = TempDir::new("comments");
        let original = "# my notes\n[hotkeys]\nfoo = 1 # keep me\n\n[[cloud]]\nid = \"custom\"\nname = \"Mine\"\nkind = \"open_ai_compat\"\nbase_url = \"http://localhost:1/v1\"\nllm_model = \"m\"\n";
        std::fs::write(dir.config(), original).unwrap();
        let store = MemorySecretStore::new();
        apply(&dir.config(), &store, TN, "tn-tea-asr", &Keys::default()).unwrap();
        let text = std::fs::read_to_string(dir.config()).unwrap();
        assert!(text.contains("# my notes"));
        assert!(text.contains("foo = 1 # keep me"));
        let cfg = load_config(&dir.config()).unwrap();
        let custom = cfg.cloud_service("custom").unwrap();
        assert_eq!(custom.name.as_deref(), Some("Mine"));
        assert_eq!(custom.llm_model.as_deref(), Some("m"));
        assert_eq!(cfg.cloud.len(), 4);
    }

    #[test]
    fn tn_rd_tn_keeps_both_groups_and_keys() {
        let dir = TempDir::new("roundtrip");
        let store = MemorySecretStore::new();
        apply(
            &dir.config(),
            &store,
            TN,
            "tn-breeze-asr",
            &Keys::of(&[("tn-breeze-asr", KEY)]),
        )
        .unwrap();
        let rd = apply(&dir.config(), &store, RD, "rd-tea-asr", &Keys::default()).unwrap();
        assert_eq!(rd.key_missing, vec!["rd-breeze-asr"]);
        let cfg = load_config(&dir.config()).unwrap();
        assert_eq!(cfg.cloud.len(), 6);
        assert_eq!(cfg.route.asr, "rd-tea-asr");
        assert_eq!(cfg.route.polish, "rd-qwen");
        assert_eq!(key_of(&store, "tn-breeze-asr").as_deref(), Some(KEY));

        let tn = apply(&dir.config(), &store, TN, "tn-tea-asr", &Keys::default()).unwrap();
        assert!(tn.key_missing.is_empty(), "TN key must still be there");
        let cfg = load_config(&dir.config()).unwrap();
        assert_eq!(cfg.cloud.len(), 6);
        assert_eq!(cfg.route.asr, "tn-tea-asr");
        assert_eq!(cfg.route.translate, "tn-qwen");
        assert_eq!(key_of(&store, "tn-breeze-asr").as_deref(), Some(KEY));
    }

    #[test]
    fn asr_choice_sets_route_and_unknown_is_rejected() {
        let dir = TempDir::new("choice");
        let store = MemorySecretStore::new();
        for choice in ["tn-tea-asr", "tn-breeze-asr"] {
            apply(&dir.config(), &store, TN, choice, &Keys::default()).unwrap();
            assert_eq!(load_config(&dir.config()).unwrap().route.asr, choice);
        }
        let before = std::fs::read(dir.config()).unwrap();
        for bad in ["tn-qwen", "local", "rd-tea-asr", ""] {
            assert_eq!(
                apply(&dir.config(), &store, TN, bad, &Keys::default()),
                Err("preset_bad_choice")
            );
        }
        assert_eq!(std::fs::read(dir.config()).unwrap(), before);
    }

    #[test]
    fn key_for_unlisted_service_is_rejected() {
        let dir = TempDir::new("keyid");
        let store = MemorySecretStore::new();
        assert_eq!(
            apply(
                &dir.config(),
                &store,
                TN,
                "tn-tea-asr",
                &Keys::of(&[("tn-qwen", KEY)])
            ),
            Err("preset_key_not_allowed")
        );
        assert!(!dir.config().exists());
        assert_eq!(key_of(&store, "tn-qwen"), None);
    }

    // ---- rejection leaves config.toml untouched ----

    #[test]
    fn invalid_presets_are_rejected_and_config_unchanged() {
        let dir = TempDir::new("invalid");
        let original = "# keep\n[route]\nasr = \"local\"\n";
        std::fs::write(dir.config(), original).unwrap();
        let store = MemorySecretStore::new();
        let user_named = format!("{original}\n[[cloud]]\nid = \"mine\"\nname = \"TN TEA-ASR\"\nkind = \"open_ai_compat\"\nbase_url = \"http://h/v1\"\n");

        let cases: Vec<(String, &str, Option<&str>)> = vec![
            ("not = [toml".into(), "preset_invalid", None),
            (TN.replace("id = \"tn-qwen\"", ""), "preset_invalid", None),
            (
                TN.replace("id = \"tn-qwen\"", "id = \"x-qwen\""),
                "preset_id_prefix",
                None,
            ),
            (
                TN.replace("http://10.20.30.23:8000/v1", "ftp://x/v1"),
                "invalid_base_url",
                None,
            ),
            (
                TN.replace("\"TN Qwen3.8 27B\"", "\"tn tea-asr\""),
                "name_taken",
                None,
            ),
            (
                TN.replace("asr_timeout_ms = 8000", "asr_timeout_ms = 5"),
                "asr_timeout_range",
                None,
            ),
            (
                TN.replace("kind = \"open_ai_compat\"", "kind = \"open_ai\""),
                "preset_kind",
                None,
            ),
            (
                TN.replace("polish = \"tn-qwen\"", "polish = \"nope\""),
                "preset_bad_reference",
                None,
            ),
            (
                TN.replace("needs_key = [\"tn-breeze-asr\"]", "needs_key = [\"nope\"]"),
                "preset_bad_reference",
                None,
            ),
            // Clashes with a service outside the preset.
            (TN.to_owned(), "name_taken", Some(user_named.as_str())),
        ];
        for (text, code, config) in cases {
            std::fs::write(dir.config(), config.unwrap_or(original)).unwrap();
            let before = std::fs::read(dir.config()).unwrap();
            let got = parse_preset(&text).and_then(|_| {
                apply(&dir.config(), &store, &text, "tn-tea-asr", &Keys::default()).map(|_| ())
            });
            assert_eq!(got, Err(code), "{text}");
            assert_eq!(std::fs::read(dir.config()).unwrap(), before, "{code}");
        }
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let dir = TempDir::new("dup");
        let text = TN.replace("id = \"tn-qwen\"", "id = \"tn-tea-asr\"");
        let store = MemorySecretStore::new();
        assert_eq!(
            apply(&dir.config(), &store, &text, "tn-tea-asr", &Keys::default()),
            Err("preset_duplicate_id")
        );
        assert!(!dir.config().exists());
    }

    #[test]
    fn non_whitelisted_sections_are_ignored_and_not_written() {
        let dir = TempDir::new("ignored");
        let text = format!(
            "{TN}\n[hotkeys]\nhold = \"F1\"\n\n[history]\nenabled = false\n\n[timeouts]\nasr_cloud_ms = 1\n"
        );
        let p = parse_preset(&text).unwrap();
        assert_eq!(p.ignored, ["history", "hotkeys", "timeouts"]);
        let store = MemorySecretStore::new();
        apply(&dir.config(), &store, &text, "tn-tea-asr", &Keys::default()).unwrap();
        let written = std::fs::read_to_string(dir.config()).unwrap();
        assert!(!written.contains("F1"));
        assert!(!written.contains("enabled = false"));
        let cfg = load_config(&dir.config()).unwrap();
        assert_eq!(cfg.timeouts, Config::with_defaults().timeouts);
    }

    // ---- keys ----

    #[test]
    fn config_write_failure_restores_key() {
        let dir = TempDir::new("restore");
        // A directory where the temp file goes makes the atomic write fail.
        std::fs::create_dir(dir.0.join("config.toml.tmp")).unwrap();
        let store = MemorySecretStore::new();
        store
            .set("tn-breeze-asr", &ApiKey::new("old-key-value"))
            .unwrap();
        let got = apply(
            &dir.config(),
            &store,
            TN,
            "tn-breeze-asr",
            &Keys::of(&[("tn-breeze-asr", KEY)]),
        );
        assert_eq!(got, Err("config_save_failed"));
        assert_eq!(
            key_of(&store, "tn-breeze-asr").as_deref(),
            Some("old-key-value")
        );

        // With no previous key, the new one is removed again.
        let fresh = MemorySecretStore::new();
        let got = apply(
            &dir.config(),
            &fresh,
            TN,
            "tn-breeze-asr",
            &Keys::of(&[("tn-breeze-asr", KEY)]),
        );
        assert_eq!(got, Err("config_save_failed"));
        assert_eq!(key_of(&fresh, "tn-breeze-asr"), None);
    }

    #[test]
    fn host_change_deletes_old_key_unless_new_given() {
        let dir = TempDir::new("host");
        let store = MemorySecretStore::new();
        apply(
            &dir.config(),
            &store,
            TN,
            "tn-breeze-asr",
            &Keys::of(&[("tn-breeze-asr", KEY)]),
        )
        .unwrap();
        // Same host, different path: key stays.
        let same_host = TN.replace(":8002/v1", ":9999/v1");
        let out = apply(
            &dir.config(),
            &store,
            &same_host,
            "tn-tea-asr",
            &Keys::default(),
        )
        .unwrap();
        assert!(out.key_missing.is_empty());
        assert_eq!(key_of(&store, "tn-breeze-asr").as_deref(), Some(KEY));
        // New host: key dropped and reported missing.
        let moved = TN.replace("10.20.30.23:8002", "10.9.9.9:8002");
        let out = apply(
            &dir.config(),
            &store,
            &moved,
            "tn-tea-asr",
            &Keys::default(),
        )
        .unwrap();
        assert_eq!(out.key_missing, vec!["tn-breeze-asr"]);
        assert_eq!(key_of(&store, "tn-breeze-asr"), None);
        // New host plus a new key: the new key is kept.
        let moved2 = TN.replace("10.20.30.23:8002", "10.8.8.8:8002");
        let out = apply(
            &dir.config(),
            &store,
            &moved2,
            "tn-tea-asr",
            &Keys::of(&[("tn-breeze-asr", "fresh-key-1234")]),
        )
        .unwrap();
        assert!(out.key_missing.is_empty());
        assert_eq!(
            key_of(&store, "tn-breeze-asr").as_deref(),
            Some("fresh-key-1234")
        );
    }

    // ---- model detection ----

    fn mock_preset(base: &str) -> String {
        TN.replace("http://10.20.30.23:8000", base)
            .replace("http://10.20.30.23:8001", base)
            .replace("http://10.20.30.23:8002", base)
    }

    async fn detect_with(models: ResponseTemplate) -> Resolved {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(models)
            .mount(&server)
            .await;
        let mut r = resolved(&mock_preset(&server.uri()));
        detect_models(&mut r, &route::cloud_http_client().unwrap()).await;
        r
    }

    fn qwen_model(r: &Resolved) -> String {
        r.preset
            .services
            .iter()
            .find(|s| s.id == "tn-qwen")
            .unwrap()
            .llm_model
            .clone()
            .unwrap()
    }

    #[tokio::test]
    async fn one_listed_model_is_detected() {
        let r = detect_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "my-live-model"}]})),
        )
        .await;
        assert_eq!(qwen_model(&r), "my-live-model");
        assert_eq!(r.sources.get("tn-qwen"), Some(&"detected"));
        assert!(!r.sources.contains_key("tn-tea-asr"));
    }

    #[tokio::test]
    async fn several_models_or_errors_fall_back_to_file() {
        let several = detect_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "a"}, {"id": "b"}]})),
        )
        .await;
        let server_error = detect_with(ResponseTemplate::new(500)).await;
        let slow = detect_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(6))
                .set_body_json(json!({"data": [{"id": "late"}]})),
        )
        .await;
        for r in [several, server_error, slow] {
            assert_eq!(qwen_model(&r), "Qwen3.8-27B-UD-Q6_K_L");
            assert_eq!(r.sources.get("tn-qwen"), Some(&"file"));
        }
    }

    #[test]
    fn preview_reports_sources_and_ignored() {
        let dir = TempDir::new("preview");
        let store = MemorySecretStore::new();
        let mut r = resolved(&format!("{TN}\n[hotkeys]\nx = 1\n"));
        r.sources.insert("tn-qwen".into(), "detected");
        let p = build_preview(&dir.config(), &store, &mut r).unwrap();
        assert_eq!(p.ignored, ["hotkeys"]);
        assert_eq!(p.services.len(), 3);
        let qwen = p.services.iter().find(|s| s.id == "tn-qwen").unwrap();
        assert_eq!(qwen.model_source, Some("detected"));
        assert_eq!(qwen.purposes, ["polish", "translate"]);
        let breeze = p.services.iter().find(|s| s.id == "tn-breeze-asr").unwrap();
        assert!(breeze.needs_key && !breeze.has_key);
        assert_eq!(p.asr_choices[1].name, "TN Breeze-ASR");
        assert!(!dir.config().exists(), "preview must not write");
    }

    // ---- file source ----

    #[test]
    fn file_source_limits() {
        let dir = TempDir::new("file");
        let ok = dir.0.join("p.toml");
        std::fs::write(&ok, TN).unwrap();
        let src = |p: &Path| PresetSource::File(p.to_string_lossy().into_owned());
        assert_eq!(load_source(&src(&ok)).unwrap().label, "file");
        let txt = dir.0.join("p.txt");
        std::fs::write(&txt, TN).unwrap();
        assert_eq!(load_source(&src(&txt)).unwrap_err(), "preset_not_toml");
        let big = dir.0.join("big.toml");
        std::fs::write(&big, vec![b'#'; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(load_source(&src(&big)).unwrap_err(), "preset_too_large");
        assert_eq!(
            load_source(&src(&dir.0.join("none.toml"))).unwrap_err(),
            "preset_unreadable"
        );
        assert_eq!(
            load_source(&PresetSource::Builtin("XX".into())).unwrap_err(),
            "preset_unknown"
        );
    }

    // ---- logs ----

    #[derive(Clone, Default)]
    struct LogBuf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for LogBuf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
        type Writer = LogBuf;
        fn make_writer(&'a self) -> LogBuf {
            self.clone()
        }
    }

    #[test]
    fn logs_never_contain_the_key() {
        let buf = LogBuf::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(buf.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let dir = TempDir::new("logs");
        let store = MemorySecretStore::new();
        tracing::subscriber::with_default(sub, || {
            let keys = Keys::of(&[("tn-breeze-asr", KEY)]);
            assert!(format!("{keys:?}").find("TESTSECRET").is_none());
            apply(&dir.config(), &store, TN, "tn-breeze-asr", &keys).unwrap();
            // Failure path (config write fails) logs only the code.
            std::fs::create_dir(dir.0.join("config.toml.tmp")).unwrap();
            let _ = apply(
                &dir.config(),
                &store,
                RD,
                "rd-breeze-asr",
                &Keys::of(&[("rd-breeze-asr", KEY)]),
            );
            warn!(code = "x", "probe");
        });
        let logs = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(!logs.contains("TESTSECRET"), "{logs}");
        assert!(!logs.contains(KEY));
    }
}
