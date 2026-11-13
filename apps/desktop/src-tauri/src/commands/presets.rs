//! Preset configurations (T4.1): one dialog applies a bundle of LAN endpoints and routes.
//!
//! A preset is a TOML file with three whitelisted sections: `[preset]` (title, id prefix,
//! which speech recognition endpoints the user may choose, which need an API key), `[[cloud]]`
//! (the services) and `[route]` (polish / translate targets). Two presets are built in
//! (`TN_CUSTOM_LLM_ADDR.toml`, `RD_CUSTOM_LLM_ADDR.toml`, embedded with `include_str!`); the
//! user's own files go through `config_io.rs` (`v2c_core::portable`). Any other top-level
//! section is ignored, never written, and listed in the preview.
//!
//! Commands (main window only):
//! - `presets_list`: the built-ins with a short summary.
//! - `presets_preview(source)`: what applying would do (after model detection).
//! - `presets_apply(source, asr_choice, keys)`: writes the services and routes, then emits
//!   `v2c:config-changed`.
//!
//! Applying reuses `cloud.rs`: the same `CONFIG_LOCK`, the comment-preserving `update_config`,
//! `upsert_service`, name rules and timeout range. It only adds or updates the preset's own
//! services (ids start with `id_prefix`); other services (another preset, the user's own) are
//! never touched, so applying TN then RD then TN keeps both groups and their keys.
//!
//! Validation rejects the whole preset (nothing is written) with a short error code.
//! Keys: only ids listed in `needs_key` are accepted; a key is stored before `config.toml` is
//! written and put back if the write fails (like `save_service`). If an existing preset
//! service's kind or endpoint (scheme, host, port, path) changes, its old key is deleted unless a
//! new one is given (the wizard's rule); an identical endpoint keeps it. `[route] asr` in a
//! file is ignored: the user's choice always wins.
//!
//! Default keys (T4.8): `[preset] default_keys = { "<id>" = "<key>" }` ships a shared LAN key
//! for a `needs_key` service. Per such service the key is chosen in this order: typed in the
//! dialog; an existing stored key whose endpoint is unchanged; the default key; none (then the
//! service is reported in `key_missing`). Defaults are written for every `needs_key` service
//! without a key, not only the chosen ASR. Key values never reach the UI or the logs.
//!
//! Logs: preset name (`TN`, `RD`, or `file`), service count, error code, elapsed ms. Never
//! keys, never the file path.

use std::collections::{BTreeMap, HashMap, HashSet};
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
    normalize_name, parse_asr_timeout, same_endpoint, secret_store, set_str, set_value, test_draft,
    update_config, upsert_service, CloudDraft, MsInput,
};
use crate::tray::CONFIG_CHANGED_EVENT;

/// Model-list query timeout during detection (apply only). A LAN server answers in tens of
/// milliseconds; off the LAN the file's model is used after this wait.
const DETECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Model source per service with a text model, without asking the network (`file`). The
/// preview uses it so opening a preset or file never waits on an unreachable LAN server.
pub(crate) fn file_sources(services: &[CloudService]) -> HashMap<String, &'static str> {
    services
        .iter()
        .filter(|s| s.llm_model.is_some())
        .map(|s| (s.id.clone(), "file"))
        .collect()
}

const BUILTINS: &[(&str, &str)] = &[
    ("TN", include_str!("../../presets/TN_CUSTOM_LLM_ADDR.toml")),
    ("RD", include_str!("../../presets/RD_CUSTOM_LLM_ADDR.toml")),
];

/// Base ids of the built-in service kinds (`cloud.rs` `id_base`) plus `local`: no preset
/// prefix may start with one of these.
const RESERVED_BASES: &[&str] = &["openai", "gemini", "claude", "custom", "local"];

/// Top-level sections a preset may use.
const WHITELIST: &[&str] = &["preset", "cloud", "route"];

// ---------------------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------------------

/// Where a preset comes from: `{ "builtin": "TN" }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresetSource {
    Builtin(String),
}

/// Pasted keys by service id. Zeroized on drop; `Debug` shows ids only.
#[derive(Deserialize, Default)]
#[serde(transparent)]
pub struct Keys(HashMap<String, String>);

impl Keys {
    /// Pasted `(service id, key)` pairs.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0.iter()
    }

    #[cfg(test)]
    pub(crate) fn of(pairs: &[(&str, &str)]) -> Keys {
        Keys(
            pairs
                .iter()
                .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
                .collect(),
        )
    }
}

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

/// Default keys shipped inside a preset, by service id. `Debug` shows ids only.
#[derive(Deserialize, Default)]
#[serde(transparent)]
struct DefaultKeys(BTreeMap<String, String>);

impl std::fmt::Debug for DefaultKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

/// Longest key accepted in `default_keys`.
const MAX_DEFAULT_KEY_CHARS: usize = 256;

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
    /// The preset ships a default key for this id (the key itself is never sent).
    pub has_default_key: bool,
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
    default_keys: DefaultKeys,
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
    // A prefix must not start like a built-in service id (`gemini`, `custom-2`, `local`...).
    let lower = prefix.to_lowercase();
    if RESERVED_BASES.iter().any(|b| lower.starts_with(b)) {
        return Err("preset_id_prefix");
    }
    let mut seen = HashSet::new();
    for svc in &preset.services {
        if !svc.id.starts_with(&prefix) {
            return Err("preset_id_prefix");
        }
        if !seen.insert(svc.id.clone()) {
            return Err("preset_duplicate_id");
        }
        // A preset may update its own services only, never a service of another kind.
        if cfg
            .cloud_service(&svc.id)
            .is_some_and(|existing| existing.kind != CloudKind::OpenAiCompat)
        {
            return Err("preset_id_conflict");
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
    // Default keys: only for `needs_key` ids; same value rules as typed keys.
    for (id, raw) in &mut preset.meta.default_keys.0 {
        if !preset.meta.needs_key.contains(id) {
            return Err("preset_bad_reference");
        }
        let k = raw.trim().to_owned();
        if k.is_empty()
            || k.chars().count() > MAX_DEFAULT_KEY_CHARS
            || k.chars().any(char::is_control)
        {
            return Err("invalid_key");
        }
        *raw = k;
    }
    let ids: HashSet<&str> = preset.services.iter().map(|s| s.id.as_str()).collect();
    // Capabilities: every speech recognition choice needs an ASR model and every text target
    // a text model. `[route] asr` in the file is ignored: the user's choice always wins.
    let has_asr = |id: &String| {
        id == LOCAL
            || preset
                .services
                .iter()
                .any(|s| &s.id == id && s.asr_model.is_some())
    };
    let has_llm = |id: &String| {
        id == LOCAL
            || preset
                .services
                .iter()
                .any(|s| &s.id == id && s.llm_model.is_some())
    };
    if !preset.meta.asr_choices.iter().all(has_asr)
        || !has_llm(&preset.route.polish)
        || !has_llm(&preset.route.translate)
    {
        return Err("preset_bad_reference");
    }
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

fn load_source(source: &PresetSource) -> Result<Resolved, &'static str> {
    let PresetSource::Builtin(name) = source;
    let (n, t) = BUILTINS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .ok_or("preset_unknown")?;
    let (label, text) = ((*n).to_owned(), (*t).to_owned());
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

pub(crate) async fn detect_one(
    svc: &CloudService,
    http: &CloudHttp,
    t: &TimeoutConfig,
) -> Option<String> {
    let url = svc.base_url.clone()?;
    // The id only labels the log line of the probe; it matches no saved service.
    let draft = CloudDraft {
        id: Some(svc.id.clone()),
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
                has_default_key: p.meta.default_keys.0.contains_key(&s.id),
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
pub(crate) struct KeyChange {
    id: String,
    old: Option<ApiKey>,
}

pub(crate) fn restore_keys(stored: &dyn SecretStore, changes: &[KeyChange]) {
    for c in changes.iter().rev() {
        let restored = match &c.old {
            Some(k) => stored.set(&c.id, k),
            None => stored.delete(&c.id),
        };
        if let Err(e) = restored {
            warn!(error = %e, "preset: could not restore key");
        }
    }
}

/// Stores the keys for `services` before `config.toml` is written; returns what changed so the
/// caller can [`restore_keys`] if a later step fails (this function restores its own partial
/// work on error). Per service: a typed key wins; else an existing key on an unchanged endpoint
/// stays; else the default key (`default_for`); else a key on a changed endpoint is dropped.
pub(crate) fn update_keys(
    stored: &dyn SecretStore,
    cfg: &Config,
    services: &[CloudService],
    default_for: &dyn Fn(&str) -> Option<ApiKey>,
    new_keys: &[(&str, ApiKey)],
) -> Result<Vec<KeyChange>, &'static str> {
    let mut changes: Vec<KeyChange> = Vec::new();
    for svc in services {
        let typed = new_keys
            .iter()
            .find(|(id, _)| *id == svc.id)
            .map(|(_, k)| k);
        // Same rule as the wizard: a changed kind or endpoint (scheme, host, port, path)
        // drops the old key; an identical endpoint keeps it.
        let endpoint_changed = cfg
            .cloud_service(&svc.id)
            .is_some_and(|prev| !same_endpoint(prev, svc));
        let default = default_for(&svc.id);
        if typed.is_none() && !endpoint_changed && default.is_none() {
            continue;
        }
        // Storing only the preset default (nothing typed, endpoint unchanged) is best effort:
        // a broken or locked keyring must not block applying the preset.
        let best_effort = typed.is_none() && !endpoint_changed;
        let old = match stored.get(&svc.id) {
            Ok(old) => old,
            Err(_) if best_effort => {
                warn!("preset: keyring unavailable, default key skipped");
                continue;
            }
            Err(_) => {
                restore_keys(stored, &changes);
                return Err("keyring_failed");
            }
        };
        let has_old = old.as_ref().is_some_and(|k| !k.is_empty());
        let done = match (typed, &default) {
            (Some(k), _) => stored.set(&svc.id, k),
            (None, _) if has_old && !endpoint_changed => continue,
            (None, Some(k)) => stored.set(&svc.id, k),
            (None, None) if old.is_some() => stored.delete(&svc.id),
            (None, None) => continue,
        };
        if done.is_err() {
            warn!("preset: could not update key");
            if best_effort {
                continue;
            }
            restore_keys(stored, &changes);
            return Err("keyring_failed");
        }
        changes.push(KeyChange {
            id: svc.id.clone(),
            old,
        });
    }
    Ok(changes)
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

    // Keys first (restored if the config write fails).
    let changes = update_keys(
        stored,
        &cfg,
        &p.services,
        &|id| {
            p.meta
                .default_keys
                .0
                .get(id)
                .map(|k| ApiKey::new(k.as_str()))
        },
        &new_keys,
    )?;
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
    // No model detection here: the preview must open at once (detection runs on apply).
    resolved.sources = file_sources(&resolved.preset.services);
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
    const DEFAULT: &str = "tn8227";

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
            assert_eq!(
                p.meta.needs_key,
                vec![format!("{prefix}tea-asr"), format!("{prefix}breeze-asr")]
            );
            assert_eq!(p.meta.default_keys.0.len(), 2);
            for asr in ["tea-asr", "breeze-asr"] {
                assert_eq!(
                    p.meta
                        .default_keys
                        .0
                        .get(&format!("{prefix}{asr}"))
                        .map(String::as_str),
                    Some(DEFAULT)
                );
            }
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
        assert!(out.key_missing.is_empty());
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
        assert!(rd.key_missing.is_empty());
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
                TN.replace(
                    "needs_key = [\"tn-tea-asr\", \"tn-breeze-asr\"]",
                    "needs_key = [\"nope\"]",
                ),
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
    fn endpoint_change_deletes_old_key_unless_new_given() {
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
        // Identical endpoint (trailing slash, host case): key stays.
        let same = TN.replace("http://10.20.30.23:8002/v1", "http://10.20.30.23:8002/v1/");
        let out = apply(&dir.config(), &store, &same, "tn-tea-asr", &Keys::default()).unwrap();
        assert!(out.key_missing.is_empty());
        assert_eq!(key_of(&store, "tn-breeze-asr").as_deref(), Some(KEY));
        // Port, path, scheme or host changes each drop the key.
        for (from, to) in [
            (":8002/v1", ":9999/v1"),
            (":8002/v1", ":8002/v2"),
            ("http://10.20.30.23:8002", "https://10.20.30.23:8002"),
            ("10.20.30.23:8002", "10.9.9.9:8002"),
        ] {
            apply(&dir.config(), &store, TN, "tn-tea-asr", &Keys::default()).unwrap();
            store.set("tn-breeze-asr", &ApiKey::new(KEY)).unwrap();
            assert_eq!(key_of(&store, "tn-breeze-asr").as_deref(), Some(KEY));
            let moved = TN.replace(from, to);
            let out = apply(
                &dir.config(),
                &store,
                &moved,
                "tn-tea-asr",
                &Keys::default(),
            )
            .unwrap();
            // The default key replaces the old one.
            assert!(out.key_missing.is_empty(), "{to}");
            assert_eq!(
                key_of(&store, "tn-breeze-asr").as_deref(),
                Some(DEFAULT),
                "{to}"
            );
        }
        // Changed endpoint plus a new key: the new key is kept.
        apply(&dir.config(), &store, TN, "tn-tea-asr", &Keys::default()).unwrap();
        let moved = TN.replace(":8002/v1", ":9999/v1");
        let out = apply(
            &dir.config(),
            &store,
            &moved,
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

    /// A copy of TN without its default key line.
    fn tn_without_default() -> String {
        let text: String = TN
            .lines()
            .filter(|l| !l.starts_with("default_keys"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(parse_preset(&text).unwrap().meta.default_keys.0.is_empty());
        text
    }

    #[test]
    fn endpoint_change_without_default_still_deletes_key() {
        let dir = TempDir::new("nodefault");
        let store = MemorySecretStore::new();
        let plain = tn_without_default();
        apply(
            &dir.config(),
            &store,
            &plain,
            "tn-tea-asr",
            &Keys::default(),
        )
        .unwrap();
        store.set("tn-breeze-asr", &ApiKey::new(KEY)).unwrap();
        let moved = plain.replace(":8002/v1", ":9999/v1");
        let out = apply(
            &dir.config(),
            &store,
            &moved,
            "tn-tea-asr",
            &Keys::default(),
        )
        .unwrap();
        // TEA needs a key too and has none without the default line.
        assert_eq!(out.key_missing, vec!["tn-tea-asr", "tn-breeze-asr"]);
        assert_eq!(key_of(&store, "tn-breeze-asr"), None);
    }

    #[test]
    fn default_key_is_stored_for_chosen_and_unchosen_asr() {
        for (text, breeze) in [(TN, "tn-breeze-asr"), (RD, "rd-breeze-asr")] {
            for choice in [breeze.to_owned(), breeze.replace("breeze", "tea")] {
                let dir = TempDir::new("defkey");
                let store = MemorySecretStore::new();
                let out = apply(&dir.config(), &store, text, &choice, &Keys::default()).unwrap();
                assert!(out.key_missing.is_empty(), "{choice}");
                assert_eq!(key_of(&store, breeze).as_deref(), Some(DEFAULT), "{choice}");
            }
        }
    }

    #[test]
    fn typed_key_beats_default_and_stored_key_is_kept() {
        let dir = TempDir::new("prio");
        let store = MemorySecretStore::new();
        let breeze = "tn-breeze-asr";
        apply(
            &dir.config(),
            &store,
            TN,
            breeze,
            &Keys::of(&[(breeze, KEY)]),
        )
        .unwrap();
        assert_eq!(key_of(&store, breeze).as_deref(), Some(KEY));
        // Nothing typed, same endpoint: the stored key stays.
        apply(&dir.config(), &store, TN, breeze, &Keys::default()).unwrap();
        assert_eq!(key_of(&store, breeze).as_deref(), Some(KEY));
        // A blank typed key counts as not typed.
        apply(
            &dir.config(),
            &store,
            TN,
            breeze,
            &Keys::of(&[(breeze, "  ")]),
        )
        .unwrap();
        assert_eq!(key_of(&store, breeze).as_deref(), Some(KEY));
        // Changed endpoint plus a typed key: the typed key beats the default.
        let moved = TN.replace(":8002/v1", ":9999/v1");
        apply(
            &dir.config(),
            &store,
            &moved,
            breeze,
            &Keys::of(&[(breeze, "typed-key-1")]),
        )
        .unwrap();
        assert_eq!(key_of(&store, breeze).as_deref(), Some("typed-key-1"));
    }

    #[test]
    fn config_write_failure_rolls_back_default_key() {
        let dir = TempDir::new("defroll");
        std::fs::create_dir(dir.0.join("config.toml.tmp")).unwrap();
        let fresh = MemorySecretStore::new();
        let got = apply(&dir.config(), &fresh, TN, "tn-breeze-asr", &Keys::default());
        assert_eq!(got, Err("config_save_failed"));
        assert_eq!(key_of(&fresh, "tn-breeze-asr"), None);
        // A key replaced by the default (changed endpoint) is put back.
        let old = MemorySecretStore::new();
        old.set("tn-breeze-asr", &ApiKey::new("old-key-value"))
            .unwrap();
        std::fs::write(
            dir.config(),
            "[[cloud]]\nid = \"tn-breeze-asr\"\nname = \"TN Breeze-ASR\"\nkind = \"open_ai_compat\"\nbase_url = \"http://10.20.30.23:9999/v1\"\nasr_model = \"breeze-asr-25\"\n",
        )
        .unwrap();
        let got = apply(&dir.config(), &old, TN, "tn-breeze-asr", &Keys::default());
        assert_eq!(got, Err("config_save_failed"));
        assert_eq!(
            key_of(&old, "tn-breeze-asr").as_deref(),
            Some("old-key-value")
        );
    }

    #[test]
    fn bad_default_keys_are_rejected_and_config_unchanged() {
        let dir = TempDir::new("baddef");
        let original = "# keep\n[route]\nasr = \"local\"\n";
        let store = MemorySecretStore::new();
        let line = "default_keys = { \"tn-tea-asr\" = \"tn8227\", \"tn-breeze-asr\" = \"tn8227\" }";
        assert!(TN.contains(line));
        let long = "k".repeat(MAX_DEFAULT_KEY_CHARS + 1);
        let cases = [
            (
                line.replace("tn-tea-asr", "tn-qwen"),
                "preset_bad_reference",
            ),
            (line.replace("tn8227", "a\\u0007b"), "invalid_key"),
            (line.replace("tn8227", "   "), "invalid_key"),
            (line.replace("tn8227", &long), "invalid_key"),
        ];
        for (new_line, code) in cases {
            std::fs::write(dir.config(), original).unwrap();
            let text = TN.replace(line, &new_line);
            assert_eq!(
                apply(&dir.config(), &store, &text, "tn-tea-asr", &Keys::default()),
                Err(code),
                "{new_line}"
            );
            assert_eq!(std::fs::read_to_string(dir.config()).unwrap(), original);
        }
        assert_eq!(key_of(&store, "tn-breeze-asr"), None);
        // Whitespace around a good value is trimmed.
        let text = TN.replace(line, &line.replace("tn8227", "  tn8227  "));
        apply(&dir.config(), &store, &text, "tn-tea-asr", &Keys::default()).unwrap();
        assert_eq!(key_of(&store, "tn-breeze-asr").as_deref(), Some(DEFAULT));
    }

    #[test]
    fn preset_cannot_overwrite_a_foreign_service() {
        let dir = TempDir::new("conflict");
        let original = "[[cloud]]
id = \"gemini\"
kind = \"gemini\"

[route]
polish = \"gemini\"
";
        std::fs::write(dir.config(), original).unwrap();
        let store = MemorySecretStore::new();
        store.set("gemini", &ApiKey::new(KEY)).unwrap();
        let text = TN
            .replace("tn-", "g")
            .replace("gqwen", "gemini")
            .replace("\"gtea-asr\"", "\"gtea\"");
        assert_eq!(
            apply(&dir.config(), &store, &text, "gtea", &Keys::default()),
            Err("preset_id_conflict")
        );
        assert_eq!(std::fs::read_to_string(dir.config()).unwrap(), original);
        assert_eq!(key_of(&store, "gemini").as_deref(), Some(KEY));
        let cfg = load_config(&dir.config()).unwrap();
        assert_eq!(cfg.cloud_service("gemini").unwrap().kind, CloudKind::Gemini);
    }

    #[test]
    fn reserved_or_empty_prefixes_are_rejected() {
        let dir = TempDir::new("prefix");
        let store = MemorySecretStore::new();
        for prefix in [
            "", "  ", "openai-", "Gemini", "claude-x", "custom", "custom-2", "local", "localx",
        ] {
            let text = TN.replace("id_prefix = \"tn-\"", &format!("id_prefix = \"{prefix}\""));
            let got = apply(&dir.config(), &store, &text, "tn-tea-asr", &Keys::default());
            assert!(
                matches!(got, Err("preset_invalid" | "preset_id_prefix")),
                "{prefix:?}: {got:?}"
            );
        }
        assert!(!dir.config().exists());
    }

    #[test]
    fn capabilities_are_validated_and_file_route_asr_is_ignored() {
        let dir = TempDir::new("caps");
        let store = MemorySecretStore::new();
        let cases = [
            // An ASR choice without an ASR model.
            TN.replace("asr_model = \"tea-asr-1\"", ""),
            // Polish / translate target without a text model.
            TN.replace("llm_model = \"Qwen3.8-27B-UD-Q6_K_L\"", ""),
            TN.replace("translate = \"tn-qwen\"", "translate = \"tn-tea-asr\""),
        ];
        for text in cases {
            assert_eq!(
                apply(
                    &dir.config(),
                    &store,
                    &text,
                    "tn-breeze-asr",
                    &Keys::default()
                ),
                Err("preset_bad_reference")
            );
        }
        assert!(!dir.config().exists());
        // `[route] asr` in the file never wins over the user's choice.
        let text = TN.replace(
            "polish = \"tn-qwen\"",
            "asr = \"nope\"
polish = \"tn-qwen\"",
        );
        apply(
            &dir.config(),
            &store,
            &text,
            "tn-breeze-asr",
            &Keys::default(),
        )
        .unwrap();
        assert_eq!(
            load_config(&dir.config()).unwrap().route.asr,
            "tn-breeze-asr"
        );
    }

    /// Fails `set` for one id, like a keyring that stops working half way.
    struct FlakyStore {
        inner: MemorySecretStore,
        fail_set: &'static str,
    }
    impl SecretStore for FlakyStore {
        fn get(&self, id: &str) -> v2c_core::Result<Option<ApiKey>> {
            self.inner.get(id)
        }
        fn set(&self, id: &str, key: &ApiKey) -> v2c_core::Result<()> {
            if id == self.fail_set {
                return Err(v2c_core::V2cError::Storage("boom".into()));
            }
            self.inner.set(id, key)
        }
        fn delete(&self, id: &str) -> v2c_core::Result<()> {
            self.inner.delete(id)
        }
    }

    /// Keyring where reads and writes of one id fail.
    struct BrokenStore {
        inner: MemorySecretStore,
        broken: &'static str,
    }
    impl SecretStore for BrokenStore {
        fn get(&self, id: &str) -> v2c_core::Result<Option<ApiKey>> {
            if id == self.broken {
                return Err(v2c_core::V2cError::Storage("locked".into()));
            }
            self.inner.get(id)
        }
        fn set(&self, id: &str, key: &ApiKey) -> v2c_core::Result<()> {
            if id == self.broken {
                return Err(v2c_core::V2cError::Storage("locked".into()));
            }
            self.inner.set(id, key)
        }
        fn delete(&self, id: &str) -> v2c_core::Result<()> {
            self.inner.delete(id)
        }
    }

    #[test]
    fn default_key_failure_is_best_effort_but_typed_key_is_strict() {
        let dir = TempDir::new("besteffort");
        let store = BrokenStore {
            inner: MemorySecretStore::new(),
            broken: "tn-breeze-asr",
        };
        let out = apply(&dir.config(), &store, TN, "tn-tea-asr", &Keys::default()).unwrap();
        assert_eq!(out.key_missing, vec!["tn-breeze-asr"]);
        assert_eq!(load_config(&dir.config()).unwrap().route.asr, "tn-tea-asr");

        // get works but set fails: same outcome.
        let dir2 = TempDir::new("besteffort2");
        let flaky = FlakyStore {
            inner: MemorySecretStore::new(),
            fail_set: "tn-breeze-asr",
        };
        let out = apply(&dir2.config(), &flaky, TN, "tn-tea-asr", &Keys::default()).unwrap();
        assert_eq!(out.key_missing, vec!["tn-breeze-asr"]);
        assert!(dir2.config().exists());

        // A typed key still fails the whole apply.
        let dir3 = TempDir::new("strict");
        let got = apply(
            &dir3.config(),
            &store,
            TN,
            "tn-breeze-asr",
            &Keys::of(&[("tn-breeze-asr", KEY)]),
        );
        assert_eq!(got, Err("keyring_failed"));
        assert!(!dir3.config().exists());
    }

    #[test]
    fn custom_file_preset_may_carry_default_keys() {
        let dir = TempDir::new("customdef");
        let text = r#"
[preset]
title = "Office"
id_prefix = "office-"
asr_choices = ["office-asr"]
needs_key = ["office-asr"]
default_keys = { "office-asr" = " lan-key-1 " }

[route]
polish = "office-llm"
translate = "office-llm"

[[cloud]]
id = "office-asr"
name = "Office ASR"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8002/v1"
asr_model = "m"

[[cloud]]
id = "office-llm"
name = "Office LLM"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8000/v1"
llm_model = "t"
"#;
        let store = MemorySecretStore::new();
        let out = apply(&dir.config(), &store, text, "office-asr", &Keys::default()).unwrap();
        assert!(out.key_missing.is_empty());
        assert_eq!(key_of(&store, "office-asr").as_deref(), Some("lan-key-1"));
    }

    #[test]
    fn keyring_failure_mid_loop_rolls_back_earlier_keys() {
        let dir = TempDir::new("flaky");
        // The built-in TN preset lists both ASR services in needs_key.
        assert!(TN.contains("needs_key = [\"tn-tea-asr\", \"tn-breeze-asr\"]"));
        let text = TN.to_owned();
        let store = FlakyStore {
            inner: MemorySecretStore::new(),
            fail_set: "tn-breeze-asr",
        };
        store
            .inner
            .set("tn-tea-asr", &ApiKey::new("old-tea-key"))
            .unwrap();
        let got = apply(
            &dir.config(),
            &store,
            &text,
            "tn-tea-asr",
            &Keys::of(&[("tn-tea-asr", KEY), ("tn-breeze-asr", KEY)]),
        );
        assert_eq!(got, Err("keyring_failed"));
        assert_eq!(
            key_of(&store.inner, "tn-tea-asr").as_deref(),
            Some("old-tea-key")
        );
        assert_eq!(key_of(&store.inner, "tn-breeze-asr"), None);
        assert!(!dir.config().exists(), "config must not be written");
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

    #[test]
    fn preview_sources_never_touch_the_network() {
        let r = resolved(TN);
        let s = file_sources(&r.preset.services);
        // Only the text model service gets a source, and it is the file's own value.
        assert_eq!(s.len(), 1);
        assert_eq!(s.get("tn-qwen"), Some(&"file"));
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
        assert!(breeze.needs_key && !breeze.has_key && breeze.has_default_key);
        assert!(!qwen.has_default_key);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains(DEFAULT), "preview must not carry key values");
        assert_eq!(p.asr_choices[1].name, "TN Breeze-ASR");
        assert!(!dir.config().exists(), "preview must not write");
    }

    // ---- parity with portable ----

    #[test]
    fn builtins_match_the_portable_parse() {
        for (name, text) in BUILTINS {
            let mut old = parse_preset(text).unwrap();
            validate(&mut old, &Config::with_defaults()).unwrap();
            let new = v2c_core::portable::parse(text).unwrap();
            assert_eq!(new.id_prefix.as_deref(), Some(old.meta.id_prefix.as_str()));
            assert_eq!(new.asr_choices, old.meta.asr_choices, "{name}");
            assert_eq!(new.needs_key, old.meta.needs_key, "{name}");
            assert_eq!(new.default_keys.len(), old.meta.default_keys.0.len());
            assert_eq!(new.route.polish.as_deref(), Some(old.route.polish.as_str()));
            assert_eq!(
                new.route.translate.as_deref(),
                Some(old.route.translate.as_str())
            );
            assert_eq!(new.services.len(), old.services.len(), "{name}");
            for (a, b) in new.services.iter().zip(&old.services) {
                assert_eq!(
                    (&a.id, a.kind, &a.name, &a.base_url),
                    (&b.id, b.kind, &b.name, &b.base_url)
                );
                assert_eq!(
                    (&a.asr_model, &a.llm_model, a.asr_timeout_ms),
                    (&b.asr_model, &b.llm_model, b.asr_timeout_ms)
                );
            }
        }
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
            // Default key path: no typed key, and Debug of the parsed preset hides values.
            let rd_dir = TempDir::new("logs-default");
            let mut r = resolved(RD);
            assert!(!format!("{r:?}").contains(DEFAULT));
            apply_resolved(
                &rd_dir.config(),
                &store,
                &mut r,
                "rd-breeze-asr",
                &Keys::default(),
            )
            .unwrap();
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
        assert!(!logs.contains(DEFAULT), "{logs}");
    }
}
