//! Cloud service commands (T1.7b): the 「新增雲端服務」 wizard and the list of connected
//! services (plan §6.2).
//!
//! Commands (all refuse callers other than the main window with `forbidden`):
//! - `cloud_kinds`: per-kind wizard info (capabilities, default endpoint, suggested models,
//!   official key page).
//! - `cloud_list`: connected services, which purposes route to each, key last 4 only.
//! - `cloud_models(draft)`: model ids from the service's `/models`, when it has one.
//! - `cloud_test_connection(draft)`: [`route::test_connection_with_llm`] with plain
//!   Traditional Chinese reasons per check.
//! - `cloud_save(draft, purposes)`: key → OS keyring, service + `[route]` → `config.toml`,
//!   then `v2c:config-changed`.
//! - `cloud_remove(id)`: deletes the key and the service; routes using it go back to local.
//! - `cloud_open_key_page(kind)`: opens the vendor's API-key page (fixed allow-list) in the
//!   default browser, so the webview needs no opener permission.
//!
//! Display names (T1.13): a compatible endpoint may carry a user-chosen `name` (e.g.
//! 「區網 Qwen」); [`display_name`] is the one display rule (name for compatible endpoints,
//! else `id`). Names are checked by [`normalize_name`] (`local` is reserved) and must not
//! clash with another service's name or id (case-insensitive); [`new_id`] skips ids already
//! used as names. The `id`, `[route]`, and keyring entry never change on rename, and logs
//! keep using the `id` only.
//!
//! Key hygiene: keys come in only inside [`CloudDraft`] (`Debug` redacted, zeroized on drop),
//! go out only to the keyring and the request header, and never appear in results, errors,
//! or logs. Results expose the last 4 characters at most. Errors are short codes.
//!
//! `config.toml` is edited with `toml_edit` so the user's comments and unknown keys survive.
//! TODO(dedupe): tray.rs has a similar toml_edit helper; merge the two into one shared
//! config-update module.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, Value};
use tracing::{info, warn};
use v2c_core::config::{CloudKind, CloudService, Config, TimeoutConfig, LOCAL};
use v2c_core::error::ProviderErrorKind;
use v2c_core::llm::{LlmProvider, OpenAiCompatLlm};
use v2c_core::route::{self, Check, CloudHttp, Probe, ProbeOptions};
use v2c_core::secrets::{ApiKey, MemorySecretStore, SecretStore};
use v2c_core::V2cError;
use zeroize::Zeroize;

use crate::app_state::AppState;
use crate::tray::CONFIG_CHANGED_EVENT;

/// The only webview allowed to use the cloud commands.
const MAIN_WINDOW: &str = "main";

/// Official API-key pages (verified 2026-11, see docs/cloud-setup.md).
const KEY_PAGE_GEMINI: &str = "https://aistudio.google.com/apikey";
const KEY_PAGE_OPENAI: &str = "https://platform.openai.com/api-keys";
const KEY_PAGE_CLAUDE: &str = "https://platform.claude.com/settings/keys";

/// Serialises this module's read-modify-write of `config.toml`.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------------------

/// What the wizard has filled in so far. `id` is set when editing an existing service.
/// `api_key` empty/absent means "use the stored key" (or none, for a compatible endpoint).
#[derive(Deserialize, Clone)]
pub struct CloudDraft {
    #[serde(default)]
    pub id: Option<String>,
    pub kind: CloudKind,
    /// Optional display name (compatible endpoints only; ignored for other kinds).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub asr_model: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
}

impl Drop for CloudDraft {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}

impl std::fmt::Debug for CloudDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudDraft")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("has_name", &self.name.is_some())
            .field("has_base_url", &self.base_url.is_some())
            .field(
                "api_key",
                &self
                    .api_key
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("none"),
            )
            .field("asr_model", &self.asr_model)
            .field("llm_model", &self.llm_model)
            .finish()
    }
}

/// Which purposes go to a service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Purposes {
    #[serde(default)]
    pub asr: bool,
    #[serde(default)]
    pub polish: bool,
    #[serde(default)]
    pub translate: bool,
}

/// Wizard step 1/2/4 info for one kind.
#[derive(Debug, Serialize)]
pub struct KindInfo {
    pub kind: CloudKind,
    pub label: &'static str,
    /// Plain description of the usable purposes.
    pub capabilities: &'static str,
    pub supports_asr: bool,
    pub key_required: bool,
    pub default_base_url: Option<&'static str>,
    pub suggested_asr_model: Option<&'static str>,
    pub suggested_llm_model: Option<&'static str>,
    pub key_page_url: Option<&'static str>,
}

/// One row of the connected-services list.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ServiceView {
    pub id: String,
    /// User-chosen name (compatible endpoints only).
    pub name: Option<String>,
    /// What to show for this service: `name`, else `id` (see [`display_name`]).
    pub display_name: String,
    pub kind: CloudKind,
    pub label: &'static str,
    /// Configured override, if any (`None` = official endpoint).
    pub base_url: Option<String>,
    /// The endpoint actually used (empty if invalid).
    pub effective_base_url: String,
    pub asr_model: Option<String>,
    pub llm_model: Option<String>,
    pub purposes: Purposes,
    pub supports_asr: bool,
    pub has_key: bool,
    /// Last 4 characters of the stored key (empty for short keys or none).
    pub key_last4: String,
}

/// One probe step, ready for display.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CheckView {
    /// `models`, `asr`, or `llm`.
    pub name: &'static str,
    pub label: &'static str,
    /// `passed`, `failed`, or `skipped`.
    pub status: &'static str,
    pub latency_ms: Option<u64>,
    /// Plain-language reason (failures and notable skips).
    pub reason: Option<&'static str>,
}

/// `cloud_test_connection` result. Never contains the key.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TestResult {
    pub ok: bool,
    pub latency_ms: u64,
    /// Headline for the wizard, e.g. 「連線成功」 or the first failure reason.
    pub summary: &'static str,
    pub checks: Vec<CheckView>,
    pub models: Option<Vec<String>>,
    /// Purposes that passed the test (step 6 enables only these).
    pub supports: Purposes,
}

/// `cloud_models` result.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ModelList {
    pub models: Option<Vec<String>>,
    pub reason: Option<&'static str>,
}

// ---------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------

pub fn kind_label(kind: CloudKind) -> &'static str {
    match kind {
        CloudKind::Gemini => "Google Gemini",
        CloudKind::OpenAi => "OpenAI",
        CloudKind::Claude => "Anthropic Claude",
        CloudKind::OpenAiCompat => "自訂相容端點",
    }
}

fn kind_toml(kind: CloudKind) -> &'static str {
    match kind {
        CloudKind::OpenAi => "open_ai",
        CloudKind::Gemini => "gemini",
        CloudKind::Claude => "claude",
        CloudKind::OpenAiCompat => "open_ai_compat",
    }
}

fn id_base(kind: CloudKind) -> &'static str {
    match kind {
        CloudKind::OpenAi => "openai",
        CloudKind::Gemini => "gemini",
        CloudKind::Claude => "claude",
        CloudKind::OpenAiCompat => "custom",
    }
}

pub fn key_page(kind: CloudKind) -> Option<&'static str> {
    match kind {
        CloudKind::Gemini => Some(KEY_PAGE_GEMINI),
        CloudKind::OpenAi => Some(KEY_PAGE_OPENAI),
        CloudKind::Claude => Some(KEY_PAGE_CLAUDE),
        CloudKind::OpenAiCompat => None,
    }
}

pub fn kinds() -> Vec<KindInfo> {
    [
        CloudKind::Gemini,
        CloudKind::OpenAi,
        CloudKind::Claude,
        CloudKind::OpenAiCompat,
    ]
    .into_iter()
    .map(|kind| {
        let s = route::suggested_models(kind);
        KindInfo {
            kind,
            label: kind_label(kind),
            capabilities: match kind {
                CloudKind::Claude => "只能潤飾與中翻英（不提供語音辨識）",
                CloudKind::OpenAiCompat => "依端點而定：語音辨識、潤飾、中翻英",
                _ => "語音辨識、潤飾、中翻英",
            },
            supports_asr: kind.supports_asr(),
            key_required: kind != CloudKind::OpenAiCompat,
            default_base_url: route::default_base_url(kind),
            suggested_asr_model: s.asr,
            suggested_llm_model: s.llm,
            key_page_url: key_page(kind),
        }
    })
    .collect()
}

/// Longest display name, in Unicode characters.
pub const NAME_MAX_CHARS: usize = 32;

/// The one display rule: the user's name if set, else the id (`custom`, `custom-2`, ...).
pub fn display_name(svc: &CloudService) -> &str {
    shown_name(svc).unwrap_or(&svc.id)
}

/// The name that counts: compatible endpoints only (a hand-edited `name` on another kind is
/// ignored), trimmed, non-empty.
fn shown_name(svc: &CloudService) -> Option<&str> {
    if svc.kind != CloudKind::OpenAiCompat {
        return None;
    }
    svc.name.as_deref().map(str::trim).filter(|n| !n.is_empty())
}

/// Trims a display name; empty means "no name". Only compatible endpoints keep one.
fn normalize_name(kind: CloudKind, name: &Option<String>) -> Result<Option<String>, &'static str> {
    if kind != CloudKind::OpenAiCompat {
        return Ok(None);
    }
    let Some(n) = trimmed(name) else {
        return Ok(None);
    };
    if n.chars().count() > NAME_MAX_CHARS {
        return Err("name_too_long");
    }
    if n.chars().any(char::is_control) {
        return Err("invalid_name");
    }
    if n.to_lowercase() == LOCAL {
        return Err("name_reserved");
    }
    Ok(Some(n))
}

/// `name` must differ (case-insensitively) from every other service's name and id.
fn check_name_unique(name: &str, id: &str, services: &[CloudService]) -> Result<(), &'static str> {
    let clash = |other: &str| other.trim().to_lowercase() == name.to_lowercase();
    let taken = services
        .iter()
        .filter(|s| s.id != id)
        .any(|s| clash(&s.id) || shown_name(s).is_some_and(clash));
    if taken {
        Err("name_taken")
    } else {
        Ok(())
    }
}

fn trimmed(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

impl CloudDraft {
    /// The `CloudService` this draft describes, validated (endpoint scheme, required URL,
    /// name length). Name uniqueness is checked on save.
    fn to_service(&self, id: &str) -> Result<CloudService, &'static str> {
        let mut base_url = trimmed(&self.base_url).map(|u| u.trim_end_matches('/').to_owned());
        if base_url.as_deref() == route::default_base_url(self.kind) {
            base_url = None;
        }
        let svc = CloudService {
            id: id.to_owned(),
            name: normalize_name(self.kind, &self.name)?,
            kind: self.kind,
            base_url,
            asr_model: if self.kind.supports_asr() {
                trimmed(&self.asr_model)
            } else {
                None
            },
            llm_model: trimmed(&self.llm_model),
        };
        if self.kind == CloudKind::OpenAiCompat && svc.base_url.is_none() {
            return Err("base_url_required");
        }
        route::effective_base_url(&svc).map_err(|_| "invalid_base_url")?;
        Ok(svc)
    }

    /// The pasted key, validated; `None` when nothing was pasted.
    fn pasted_key(&self) -> Result<Option<ApiKey>, &'static str> {
        match self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
        {
            Some(k) if k.chars().any(char::is_control) => Err("invalid_key"),
            Some(k) => Ok(Some(ApiKey::new(k))),
            None => Ok(None),
        }
    }

    /// The pasted key, else the stored one, else none. The stored key is reused only when
    /// `svc` (this draft) has the same kind and endpoint as the `saved` service, so editing
    /// the URL never sends the old key to a new host.
    fn key(
        &self,
        stored: &dyn SecretStore,
        saved: Option<&CloudService>,
        svc: &CloudService,
    ) -> Result<Option<ApiKey>, &'static str> {
        if let Some(k) = self.pasted_key()? {
            return Ok(Some(k));
        }
        match saved.filter(|prev| same_endpoint(prev, svc)) {
            Some(prev) => stored
                .get(&prev.id)
                .map(|k| k.filter(|k| !k.is_empty()))
                .map_err(|_| "keyring_failed"),
            None => Ok(None),
        }
    }
}

fn normalized_endpoint(svc: &CloudService) -> Option<String> {
    route::effective_base_url(svc)
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_ascii_lowercase())
}

/// Same kind and same (normalised) effective endpoint.
fn same_endpoint(a: &CloudService, b: &CloudService) -> bool {
    a.kind == b.kind
        && normalized_endpoint(a).is_some()
        && normalized_endpoint(a) == normalized_endpoint(b)
}

fn config_error_code(e: &V2cError) -> &'static str {
    match e {
        V2cError::NotConfigured(what) if what.contains("key") => "key_missing",
        V2cError::NotConfigured(what) if what.contains("base_url") => "invalid_base_url",
        V2cError::Storage(_) => "keyring_failed",
        _ => "test_failed",
    }
}

fn failure_reason(kind: ProviderErrorKind, step: &str) -> &'static str {
    match kind {
        ProviderErrorKind::Unauthorized => "金鑰錯誤（或這把金鑰沒有權限）",
        ProviderErrorKind::RateLimited => "額度不足或請求太頻繁",
        ProviderErrorKind::Unreachable => "網路不通（連不到這個服務）",
        ProviderErrorKind::Unsupported if step == "asr" => "這個端點不支援語音辨識",
        ProviderErrorKind::Unsupported => "這個端點不支援文字模型（請確認模型名稱）",
        ProviderErrorKind::Server => "伺服器錯誤，請稍後再試",
        ProviderErrorKind::BadResponse => "服務回應無法辨認（請確認網址與模型名稱）",
    }
}

const TIMED_OUT: &str = "逾時：服務回應太慢（請檢查網路，或稍後再試）";

fn check_view(name: &'static str, check: Check, kind: CloudKind) -> CheckView {
    let label = match name {
        "models" => "讀取模型清單",
        "asr" => "語音辨識（3 秒測試音檔）",
        _ => "文字模型（潤飾／中翻英）",
    };
    let (status, latency_ms, reason) = match check {
        Check::Passed { latency_ms } => ("passed", Some(latency_ms), None),
        Check::Failed(k) => ("failed", None, Some(failure_reason(k, name))),
        Check::TimedOut => ("failed", None, Some(TIMED_OUT)),
        Check::Skipped => (
            "skipped",
            None,
            Some(match name {
                "models" => "這個服務沒有提供模型清單（不影響使用）",
                "asr" if !kind.supports_asr() => "這個服務不提供語音辨識",
                "asr" => "沒有填語音辨識模型，未測試",
                _ => "沒有填文字模型，未測試",
            }),
        ),
    };
    CheckView {
        name,
        label,
        status,
        latency_ms,
        reason,
    }
}

fn to_result(kind: CloudKind, p: &Probe) -> TestResult {
    let checks = vec![
        check_view("models", p.models_check, kind),
        check_view("asr", p.asr, kind),
        check_view("llm", p.llm, kind),
    ];
    let summary = if p.ok {
        "連線成功"
    } else if let Some(c) = checks.iter().find(|c| c.status == "failed") {
        c.reason.unwrap_or("連線失敗")
    } else {
        "沒有可以測試的項目：請填入模型名稱"
    };
    TestResult {
        ok: p.ok,
        latency_ms: p.latency_ms,
        summary,
        checks,
        models: p.models.clone(),
        supports: Purposes {
            asr: p.supports_asr(),
            polish: p.supports_llm(),
            translate: p.supports_llm(),
        },
    }
}

/// `OpenAiCompatLlm` for OpenAI / compatible endpoints (Gemini and Claude are built inside
/// `route`). `None` when no model is known (compat endpoints have no default).
fn openai_llm(
    svc: &CloudService,
    key: Option<&ApiKey>,
    http: &CloudHttp,
    timeout: Duration,
) -> Option<OpenAiCompatLlm> {
    if !matches!(svc.kind, CloudKind::OpenAi | CloudKind::OpenAiCompat) {
        return None;
    }
    let model = svc
        .llm_model
        .clone()
        .or_else(|| route::suggested_models(svc.kind).llm.map(str::to_owned))?;
    let base = route::effective_base_url(svc).ok()?;
    Some(
        OpenAiCompatLlm::new(
            http.client().clone(),
            base,
            key.map(|k| k.expose().to_owned()),
            Some(model),
            timeout,
        )
        .with_name(svc.id.clone()),
    )
}

/// Runs the wizard's connection test for `draft`. The key comes from the draft, else from
/// `stored` when the draft edits one of the `saved` services without changing its endpoint.
pub async fn test_draft(
    draft: &CloudDraft,
    stored: &dyn SecretStore,
    saved: &[CloudService],
    http: &CloudHttp,
    timeouts: &TimeoutConfig,
    asr: bool,
    llm: bool,
) -> Result<(TestResult, Probe), &'static str> {
    let id = draft
        .id
        .clone()
        .unwrap_or_else(|| id_base(draft.kind).into());
    let svc = draft.to_service(&id)?;
    let prev = draft
        .id
        .as_deref()
        .and_then(|id| saved.iter().find(|s| s.id == id));
    let key = draft.key(stored, prev, &svc)?;
    let mem = MemorySecretStore::new();
    if let Some(k) = &key {
        mem.set(&id, k).map_err(|_| "keyring_failed")?;
    }
    let opts = ProbeOptions {
        asr,
        llm,
        ..ProbeOptions::from_timeouts(timeouts)
    };
    let compat_llm = openai_llm(&svc, key.as_ref(), http, opts.llm_timeout);
    drop(key);
    let probe = route::test_connection_with_llm(
        &svc,
        &mem,
        http,
        &opts,
        compat_llm.as_ref().map(|l| l as &dyn LlmProvider),
    )
    .await
    .map_err(|e| config_error_code(&e))?;
    let result = to_result(svc.kind, &probe);
    info!(
        service = %id,
        kind = kind_toml(svc.kind),
        ok = result.ok,
        latency_ms = result.latency_ms,
        "cloud connection test"
    );
    Ok((result, probe))
}

/// Unique id for a new service of `kind` (`gemini`, `gemini-2`, ...). An id equal to an
/// existing service's display name (case-insensitive) is also skipped, so a new service
/// never shows up looking like a named one. Existing ids never change.
fn new_id(kind: CloudKind, cfg: &Config) -> String {
    let base = id_base(kind);
    let taken = |id: &str| {
        id == LOCAL
            || cfg.cloud_service(id).is_some()
            || cfg
                .cloud
                .iter()
                .any(|s| shown_name(s).is_some_and(|n| n.to_lowercase() == id.to_lowercase()))
    };
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .unwrap_or_else(|| base.to_owned())
}

fn view(svc: &CloudService, cfg: &Config, stored: &dyn SecretStore) -> ServiceView {
    let key = match stored.get(&svc.id) {
        Ok(k) => k.filter(|k| !k.is_empty()),
        Err(e) => {
            warn!(service = %svc.id, error = %e, "cloud: could not read key");
            None
        }
    };
    ServiceView {
        id: svc.id.clone(),
        name: shown_name(svc).map(str::to_owned),
        display_name: display_name(svc).to_owned(),
        kind: svc.kind,
        label: kind_label(svc.kind),
        base_url: svc.base_url.clone(),
        effective_base_url: route::effective_base_url(svc).unwrap_or_default(),
        asr_model: svc.asr_model.clone(),
        llm_model: svc.llm_model.clone(),
        purposes: Purposes {
            asr: cfg.route.asr == svc.id,
            polish: cfg.route.polish == svc.id,
            translate: cfg.route.translate == svc.id,
        },
        supports_asr: svc.kind.supports_asr(),
        has_key: key.is_some(),
        key_last4: key.map(|k| k.last4()).unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------------------
// Config file (comment-preserving)
// ---------------------------------------------------------------------------------------

fn read_config_text(path: &Path) -> Result<String, &'static str> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::with_defaults()
            .to_toml_string()
            .map_err(|_| "config_save_failed"),
        Err(_) => Err("config_unreadable"),
    }
}

pub fn load_config(path: &Path) -> Result<Config, &'static str> {
    Config::from_toml_str(&read_config_text(path)?).map_err(|_| "config_invalid")
}

/// Reads `config.toml` (defaults if missing), lets `edit` change the document, checks the
/// result still parses, and writes it atomically. An invalid file is left untouched.
/// Callers hold [`CONFIG_LOCK`] for the whole read-modify-write.
fn update_config(
    path: &Path,
    edit: impl FnOnce(&mut DocumentMut, &Config) -> Result<(), &'static str>,
) -> Result<Config, &'static str> {
    let text = read_config_text(path)?;
    let current = Config::from_toml_str(&text).map_err(|_| "config_invalid")?;
    let mut doc: DocumentMut = text.parse().map_err(|_| "config_invalid")?;
    edit(&mut doc, &current)?;
    let out = doc.to_string();
    let updated = Config::from_toml_str(&out).map_err(|_| "config_invalid")?;
    crate::app_state::save_config_text(path, &out).map_err(|e| {
        warn!(error = %e, "could not save config.toml");
        "config_save_failed"
    })?;
    Ok(updated)
}

fn config_lock() -> std::sync::MutexGuard<'static, ()> {
    CONFIG_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Sets `key = "val"` keeping the old value's comments/whitespace.
fn set_str(table: &mut dyn TableLike, key: &str, val: &str) {
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            let mut v = Value::from(val);
            *v.decor_mut() = decor;
            *old = v;
        }
        _ => {
            table.insert(key, toml_edit::value(val));
        }
    }
}

fn set_opt(table: &mut dyn TableLike, key: &str, val: &Option<String>) {
    match val {
        Some(v) => set_str(table, key, v),
        None => {
            table.remove(key);
        }
    }
}

/// `[[cloud]]` as an array of tables (converting `cloud = []` written by the serializer).
fn cloud_tables(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables, &'static str> {
    let item = doc.entry("cloud").or_insert(Item::None);
    let taken = std::mem::take(item);
    *item = match taken {
        Item::None => Item::ArrayOfTables(ArrayOfTables::new()),
        Item::Value(Value::Array(a)) if a.is_empty() => Item::ArrayOfTables(ArrayOfTables::new()),
        other => match other.into_array_of_tables() {
            Ok(aot) => Item::ArrayOfTables(aot),
            Err(_) => return Err("config_invalid"),
        },
    };
    item.as_array_of_tables_mut().ok_or("config_invalid")
}

fn table_id(t: &Table) -> Option<&str> {
    t.get("id").and_then(Item::as_str)
}

fn upsert_service(doc: &mut DocumentMut, svc: &CloudService) -> Result<(), &'static str> {
    let aot = cloud_tables(doc)?;
    let idx = aot
        .iter()
        .position(|t| table_id(t) == Some(svc.id.as_str()));
    let table = match idx {
        Some(i) => aot.get_mut(i).ok_or("config_invalid")?,
        None => {
            let mut t = Table::new();
            t.insert("id", toml_edit::value(svc.id.as_str()));
            aot.push(t);
            let last = aot.len() - 1;
            aot.get_mut(last).ok_or("config_invalid")?
        }
    };
    set_str(table, "kind", kind_toml(svc.kind));
    set_opt(table, "name", &svc.name);
    set_opt(table, "base_url", &svc.base_url);
    set_opt(table, "asr_model", &svc.asr_model);
    set_opt(table, "llm_model", &svc.llm_model);
    Ok(())
}

fn remove_service_table(doc: &mut DocumentMut, id: &str) -> Result<(), &'static str> {
    cloud_tables(doc)?.retain(|t| table_id(t) != Some(id));
    Ok(())
}

/// Points each purpose at `id` (checked) or back to local (unchecked and currently `id`).
fn set_routes(
    doc: &mut DocumentMut,
    current: &Config,
    id: &str,
    purposes: Option<Purposes>,
) -> Result<(), &'static str> {
    let route = doc
        .entry("route")
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or("config_invalid")?;
    let r = &current.route;
    for (key, now, want) in [
        ("asr", &r.asr, purposes.map(|p| p.asr)),
        ("polish", &r.polish, purposes.map(|p| p.polish)),
        ("translate", &r.translate, purposes.map(|p| p.translate)),
    ] {
        match want {
            Some(true) => set_str(route, key, id),
            _ if now.trim() == id => set_str(route, key, LOCAL),
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// Command logic (testable without Tauri)
// ---------------------------------------------------------------------------------------

pub fn list_services(
    config_path: &Path,
    stored: &dyn SecretStore,
) -> Result<Vec<ServiceView>, &'static str> {
    let cfg = load_config(config_path)?;
    Ok(cfg.cloud.iter().map(|s| view(s, &cfg, stored)).collect())
}

/// Saves the draft (new or edit) and routes `purposes` to it. Returns the saved row.
pub fn save_service(
    config_path: &Path,
    stored: &dyn SecretStore,
    draft: &CloudDraft,
    purposes: Purposes,
) -> Result<ServiceView, &'static str> {
    let _guard = config_lock();
    let cfg = load_config(config_path)?;
    let (id, is_new) = match &draft.id {
        Some(id) if cfg.cloud_service(id).is_some() => (id.clone(), false),
        Some(_) => return Err("not_found"),
        None => (new_id(draft.kind, &cfg), true),
    };
    let svc = draft.to_service(&id)?;
    if let Some(name) = &svc.name {
        check_name_unique(name, &id, &cfg.cloud)?;
    }
    if purposes.asr && !svc.kind.supports_asr() {
        return Err("unsupported_purpose");
    }
    let suggested = route::suggested_models(svc.kind);
    if purposes.asr && svc.asr_model.is_none() && suggested.asr.is_none() {
        return Err("model_required");
    }
    if (purposes.polish || purposes.translate) && svc.llm_model.is_none() && suggested.llm.is_none()
    {
        return Err("model_required");
    }
    let prev = cfg.cloud_service(&id);
    let pasted = draft.pasted_key()?;
    let reusable = prev.is_some_and(|p| same_endpoint(p, &svc));
    let key = draft.key(stored, prev, &svc)?;
    if key.is_none() && svc.kind != CloudKind::OpenAiCompat {
        return Err("key_missing");
    }
    drop(key);
    // The keyring entry may change: a pasted key replaces it; an endpoint change without a new
    // key clears it (a compatible endpoint's old key must not go to the new host). The old
    // entry is kept so a failed config write can put it back.
    let old_key = stored.get(&id).map_err(|_| "keyring_failed")?;
    let key_changed = match &pasted {
        Some(k) => {
            stored.set(&id, k).map_err(|e| {
                warn!(service = %id, error = %e, "cloud: could not store key");
                "keyring_failed"
            })?;
            true
        }
        None if !reusable && old_key.is_some() => {
            stored.delete(&id).map_err(|_| "keyring_failed")?;
            true
        }
        None => false,
    };
    drop(pasted);
    let purposes = Purposes {
        asr: purposes.asr && svc.kind.supports_asr(),
        ..purposes
    };
    let saved = update_config(config_path, |doc, current| {
        upsert_service(doc, &svc)?;
        set_routes(doc, current, &id, Some(purposes))
    });
    let cfg = match saved {
        Ok(cfg) => cfg,
        Err(code) => {
            if key_changed {
                let restored = match &old_key {
                    Some(k) => stored.set(&id, k),
                    None => stored.delete(&id),
                };
                if let Err(e) = restored {
                    warn!(service = %id, error = %e, "cloud: could not restore key");
                }
            }
            return Err(code);
        }
    };
    drop(old_key);
    info!(
        service = %id,
        kind = kind_toml(svc.kind),
        new = is_new,
        asr = purposes.asr,
        polish = purposes.polish,
        translate = purposes.translate,
        "cloud service saved"
    );
    let saved = cfg.cloud_service(&id).ok_or("config_invalid")?;
    Ok(view(saved, &cfg, stored))
}

/// `cloud_remove` result.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RemoveResult {
    /// `false` when the service was removed but its keyring entry could not be deleted
    /// (the UI says so; the key is no longer used by anything).
    pub key_deleted: bool,
}

/// Removes the service and its key; routes that used it go back to local.
pub fn remove_service(
    config_path: &Path,
    stored: &dyn SecretStore,
    id: &str,
) -> Result<RemoveResult, &'static str> {
    let _guard = config_lock();
    let cfg = load_config(config_path)?;
    if cfg.cloud_service(id).is_none() {
        return Err("not_found");
    }
    update_config(config_path, |doc, current| {
        set_routes(doc, current, id, None)?;
        remove_service_table(doc, id)
    })?;
    let key_deleted = match stored.delete(id) {
        Ok(()) => true,
        Err(e) => {
            warn!(service = %id, error = %e, "cloud: service removed but key not deleted");
            false
        }
    };
    info!(service = %id, key_deleted, "cloud service removed");
    Ok(RemoveResult { key_deleted })
}

/// Only the main window may use the cloud commands.
fn check_caller(label: &str) -> Result<(), &'static str> {
    if label == MAIN_WINDOW {
        Ok(())
    } else {
        warn!(window = label, "cloud command refused");
        Err("forbidden")
    }
}

// ---------------------------------------------------------------------------------------
// Platform glue
// ---------------------------------------------------------------------------------------

/// The OS keyring. Installs the Windows Credential Manager store on first use if the app
/// has not installed a default store yet. main.rs installs it at startup, so this is only a
/// fallback (e.g. if that failed transiently).
fn secret_store() -> Result<v2c_core::secrets::KeyringSecretStore, &'static str> {
    if keyring_core::get_default_store().is_none() {
        install_default_store()?;
    }
    Ok(v2c_core::secrets::KeyringSecretStore::new())
}

#[cfg(windows)]
fn install_default_store() -> Result<(), &'static str> {
    let store = windows_native_keyring_store::Store::new().map_err(|_| {
        warn!("cloud: Windows credential store unavailable");
        "keyring_failed"
    })?;
    keyring_core::set_default_store(store);
    info!("cloud: installed Windows credential store");
    Ok(())
}

#[cfg(not(windows))]
fn install_default_store() -> Result<(), &'static str> {
    Err("keyring_failed")
}

/// Opens a fixed vendor URL in the default browser.
/// TODO: move to v2c-platform (shell open) once that crate can be edited.
fn open_in_browser(url: &'static str) -> Result<(), &'static str> {
    #[cfg(windows)]
    let cmd = std::process::Command::new("explorer.exe").arg(url).spawn();
    #[cfg(not(windows))]
    let cmd = std::process::Command::new("xdg-open").arg(url).spawn();
    cmd.map(|_| ()).map_err(|_| "open_failed")
}

fn config_path(app: &tauri::AppHandle) -> std::path::PathBuf {
    app.state::<AppState>().paths.config_file()
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
) -> Result<T, &'static str> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|_| "internal_error")?
}

/// The running config (reloaded on `v2c:config-changed`), for timeouts and saved services.
fn live_config(app: &tauri::AppHandle) -> Arc<Config> {
    app.state::<AppState>().engine.config()
}

// ---------------------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------------------

#[tauri::command]
pub fn cloud_kinds(window: tauri::WebviewWindow) -> Result<Vec<KindInfo>, &'static str> {
    check_caller(window.label())?;
    Ok(kinds())
}

#[tauri::command]
pub async fn cloud_list(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Vec<ServiceView>, &'static str> {
    check_caller(window.label())?;
    let path = config_path(&app);
    blocking(move || list_services(&path, &secret_store()?)).await
}

#[tauri::command]
pub async fn cloud_test_connection(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    draft: CloudDraft,
) -> Result<TestResult, &'static str> {
    check_caller(window.label())?;
    let store = blocking(secret_store).await?;
    let http = route::cloud_http_client().map_err(|_| "test_failed")?;
    let cfg = live_config(&app);
    test_draft(&draft, &store, &cfg.cloud, &http, &cfg.timeouts, true, true)
        .await
        .map(|(r, _)| r)
}

#[tauri::command]
pub async fn cloud_models(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    draft: CloudDraft,
) -> Result<ModelList, &'static str> {
    check_caller(window.label())?;
    let store = blocking(secret_store).await?;
    let http = route::cloud_http_client().map_err(|_| "test_failed")?;
    let cfg = live_config(&app);
    let (r, _) = test_draft(
        &draft,
        &store,
        &cfg.cloud,
        &http,
        &cfg.timeouts,
        false,
        false,
    )
    .await?;
    let reason = r
        .checks
        .iter()
        .find(|c| c.name == "models" && c.status != "passed")
        .and_then(|c| c.reason);
    Ok(ModelList {
        models: r.models,
        reason,
    })
}

#[tauri::command]
pub async fn cloud_save(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    draft: CloudDraft,
    purposes: Purposes,
) -> Result<ServiceView, &'static str> {
    check_caller(window.label())?;
    let path = config_path(&app);
    let row = blocking(move || save_service(&path, &secret_store()?, &draft, purposes)).await?;
    let _ = app.emit(CONFIG_CHANGED_EVENT, ());
    Ok(row)
}

#[tauri::command]
pub async fn cloud_remove(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    id: String,
) -> Result<RemoveResult, &'static str> {
    check_caller(window.label())?;
    let path = config_path(&app);
    let result = blocking(move || remove_service(&path, &secret_store()?, &id)).await?;
    let _ = app.emit(CONFIG_CHANGED_EVENT, ());
    Ok(result)
}

#[tauri::command]
pub fn cloud_open_key_page(
    window: tauri::WebviewWindow,
    kind: CloudKind,
) -> Result<(), &'static str> {
    check_caller(window.label())?;
    open_in_browser(key_page(kind).ok_or("not_found")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "sk-TESTSECRET-abcdefgh-WXYZ";

    fn draft(kind: CloudKind, base: Option<String>, key: Option<&str>) -> CloudDraft {
        CloudDraft {
            id: None,
            kind,
            name: None,
            base_url: base,
            api_key: key.map(str::to_owned),
            asr_model: None,
            llm_model: None,
        }
    }

    fn timeouts() -> TimeoutConfig {
        TimeoutConfig {
            asr_cloud_ms: 2000,
            llm_cloud_ms: 2000,
        }
    }

    fn http() -> CloudHttp {
        route::cloud_http_client().unwrap()
    }

    /// `config.toml` in a fresh temp folder that is deleted when the test ends.
    struct TempConfig {
        dir: std::path::PathBuf,
        path: std::path::PathBuf,
    }

    impl std::ops::Deref for TempConfig {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.path
        }
    }

    impl AsRef<Path> for TempConfig {
        fn as_ref(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn temp_config(tag: &str) -> TempConfig {
        let dir = std::env::temp_dir().join(format!("v2c-cloud-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        TempConfig { dir, path }
    }

    fn assert_no_key<T: Serialize + std::fmt::Debug>(v: &T) {
        let s = serde_json::to_string(v).unwrap();
        assert!(!s.contains("TESTSECRET"), "{s}");
        assert!(!format!("{v:?}").contains("TESTSECRET"));
    }

    async fn run(d: &CloudDraft) -> TestResult {
        let (r, _) = test_draft(
            d,
            &MemorySecretStore::new(),
            &[],
            &http(),
            &timeouts(),
            true,
            true,
        )
        .await
        .unwrap();
        assert_no_key(&r);
        r
    }

    fn status<'a>(r: &'a TestResult, name: &str) -> &'a CheckView {
        r.checks.iter().find(|c| c.name == name).unwrap()
    }

    // ---- test connection ----

    #[tokio::test]
    async fn gemini_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1beta/models"))
            .and(header("x-goog-api-key", KEY))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [
                {"name": "models/g-flash", "supportedGenerationMethods": ["generateContent"]}
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1beta/models/g-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{"content": {"parts": [{"text": "ok"}]}}]
            })))
            .expect(2)
            .mount(&server)
            .await;
        let mut d = draft(
            CloudKind::Gemini,
            Some(format!("{}/v1beta", server.uri())),
            Some(KEY),
        );
        d.asr_model = Some("g-flash".into());
        d.llm_model = Some("g-flash".into());
        let r = run(&d).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(r.summary, "連線成功");
        assert_eq!(r.models, Some(vec!["g-flash".to_string()]));
        assert!(r.supports.asr && r.supports.polish && r.supports.translate);
        assert!(status(&r, "asr").latency_ms.is_some());
    }

    #[tokio::test]
    async fn openai_success_tests_chat_via_compat_llm() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "gpt-x"}]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header("authorization", format!("Bearer {KEY}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "hi"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "This is a connection test."}, "finish_reason": "stop"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let d = draft(
            CloudKind::OpenAi,
            Some(format!("{}/v1", server.uri())),
            Some(KEY),
        );
        let r = run(&d).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(status(&r, "llm").status, "passed");
        assert!(r.supports.asr && r.supports.polish);
    }

    #[tokio::test]
    async fn claude_success_without_asr() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", KEY))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": [{"id": "claude-haiku-5-5"}]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "This is a connection test."}]
            })))
            .mount(&server)
            .await;
        let d = draft(
            CloudKind::Claude,
            Some(format!("{}/v1", server.uri())),
            Some(KEY),
        );
        let r = run(&d).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(status(&r, "asr").status, "skipped");
        assert_eq!(status(&r, "asr").reason, Some("這個服務不提供語音辨識"));
        assert!(!r.supports.asr && r.supports.polish && r.supports.translate);
    }

    #[tokio::test]
    async fn bad_key_is_explained() {
        for kind in [CloudKind::OpenAi, CloudKind::Gemini, CloudKind::Claude] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200))
                .expect(0)
                .mount(&server)
                .await;
            let d = draft(kind, Some(server.uri()), Some(KEY));
            let r = run(&d).await;
            assert!(!r.ok);
            assert!(r.summary.starts_with("金鑰錯誤"), "{kind:?} {r:?}");
            assert_eq!(r.supports, Purposes::default());
        }
    }

    #[tokio::test]
    async fn compat_without_audio_is_unsupported() {
        let server = MockServer::start().await; // every path 404s
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}]
            })))
            .mount(&server)
            .await;
        let mut d = draft(
            CloudKind::OpenAiCompat,
            Some(format!("{}/v1", server.uri())),
            None,
        );
        d.asr_model = Some("whisper".into());
        d.llm_model = Some("qwen".into());
        let r = run(&d).await;
        assert!(!r.ok);
        assert_eq!(status(&r, "asr").reason, Some("這個端點不支援語音辨識"));
        assert_eq!(r.summary, "這個端點不支援語音辨識");
        assert_eq!(status(&r, "models").status, "skipped");
        assert!(!r.supports.asr && r.supports.polish);
    }

    #[tokio::test]
    async fn rate_limit_and_unreachable_reasons() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let r = run(&draft(CloudKind::Gemini, Some(server.uri()), Some(KEY))).await;
        assert_eq!(r.summary, "額度不足或請求太頻繁");

        // Nothing listens on port 9 of loopback. Windows retries a refused connect for
        // about 2 s, so give it a longer timeout than the other tests.
        let slow = TimeoutConfig {
            asr_cloud_ms: 15_000,
            llm_cloud_ms: 15_000,
        };
        let d = draft(
            CloudKind::OpenAi,
            Some("http://127.0.0.1:9/v1".into()),
            Some(KEY),
        );
        let (r, _) = test_draft(
            &d,
            &MemorySecretStore::new(),
            &[],
            &http(),
            &slow,
            true,
            true,
        )
        .await
        .unwrap();
        assert!(r.summary.starts_with("網路不通"), "{r:?}");
    }

    #[tokio::test]
    async fn config_problems_are_codes() {
        let s = MemorySecretStore::new();
        let t = timeouts();
        let missing_key = draft(CloudKind::OpenAi, None, None);
        assert_eq!(
            test_draft(&missing_key, &s, &[], &http(), &t, true, true)
                .await
                .unwrap_err(),
            "key_missing"
        );
        let no_url = draft(CloudKind::OpenAiCompat, None, None);
        assert_eq!(
            test_draft(&no_url, &s, &[], &http(), &t, true, true)
                .await
                .unwrap_err(),
            "base_url_required"
        );
        let plain_http = draft(CloudKind::Claude, Some("http://proxy/v1".into()), Some(KEY));
        assert_eq!(
            test_draft(&plain_http, &s, &[], &http(), &t, true, true)
                .await
                .unwrap_err(),
            "invalid_base_url"
        );
    }

    #[tokio::test]
    async fn stored_key_is_used_when_editing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("x-api-key", KEY))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;
        let stored = MemorySecretStore::new();
        stored.set("claude", &ApiKey::new(KEY)).unwrap();
        let saved = [CloudService {
            id: "claude".into(),
            name: None,
            kind: CloudKind::Claude,
            base_url: Some(server.uri()),
            asr_model: None,
            llm_model: None,
        }];
        let mut d = draft(CloudKind::Claude, Some(format!("{}/", server.uri())), None);
        d.id = Some("claude".into());
        let (r, _) = test_draft(&d, &stored, &saved, &http(), &timeouts(), false, false)
            .await
            .unwrap();
        assert!(r.ok, "{r:?}");
        assert_eq!(r.models, Some(vec![]));

        // A different endpoint (or kind) never gets the stored key.
        let other = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .expect(0)
            .mount(&other)
            .await;
        d.base_url = Some(other.uri());
        let err = test_draft(&d, &stored, &saved, &http(), &timeouts(), false, false)
            .await
            .unwrap_err();
        assert_eq!(err, "key_missing");
        let mut wrong_kind = draft(CloudKind::OpenAi, Some(server.uri()), None);
        wrong_kind.id = Some("claude".into());
        let err = test_draft(
            &wrong_kind,
            &stored,
            &saved,
            &http(),
            &timeouts(),
            false,
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "key_missing");
        // Unknown id: no stored key either.
        d.base_url = Some(server.uri());
        let err = test_draft(&d, &stored, &[], &http(), &timeouts(), false, false)
            .await
            .unwrap_err();
        assert_eq!(err, "key_missing");
    }

    // ---- save / list / remove ----

    const USER_CONFIG: &str = "\
# my settings
[route]
asr = \"local\"   # keep this comment
polish = \"local\"
translate = \"local\"
cloud_fallback_to_local = false

[polish]
enabled = true
future_option = 42  # unknown key
";

    #[test]
    fn save_writes_config_key_and_routes_preserving_comments() {
        let path = temp_config("save");
        std::fs::write(&path, USER_CONFIG).unwrap();
        let store = MemorySecretStore::new();
        let mut d = draft(CloudKind::Gemini, None, Some(KEY));
        d.asr_model = Some("gemini-3.6-flash".into());
        d.llm_model = Some("gemini-3.6-flash".into());
        let row = save_service(
            &path,
            &store,
            &d,
            Purposes {
                asr: true,
                polish: true,
                translate: false,
            },
        )
        .unwrap();
        assert_eq!(row.id, "gemini");
        assert_eq!(row.key_last4, "WXYZ");
        assert!(row.has_key && row.purposes.asr && row.purposes.polish && !row.purposes.translate);
        assert_no_key(&row);
        assert_eq!(store.get("gemini").unwrap().unwrap().expose(), KEY);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my settings"));
        assert!(text.contains("# keep this comment"));
        assert!(text.contains("future_option = 42"));
        assert!(!text.contains("TESTSECRET"));
        let cfg = Config::from_toml_str(&text).unwrap();
        assert_eq!(cfg.route.asr, "gemini");
        assert_eq!(cfg.route.polish, "gemini");
        assert_eq!(cfg.route.translate, LOCAL);
        assert!(!cfg.route.cloud_fallback_to_local);
        let svc = cfg.cloud_service("gemini").unwrap();
        assert_eq!(svc.kind, CloudKind::Gemini);
        assert_eq!(svc.base_url, None);

        // A second Gemini gets a new id; editing the first keeps its key.
        let second = save_service(&path, &store, &d, Purposes::default()).unwrap();
        assert_eq!(second.id, "gemini-2");
        let mut edit = draft(CloudKind::Gemini, None, None);
        edit.id = Some("gemini".into());
        edit.llm_model = Some("gemini-other".into());
        let row = save_service(
            &path,
            &store,
            &edit,
            Purposes {
                asr: false,
                polish: true,
                translate: true,
            },
        )
        .unwrap();
        assert_eq!(row.llm_model.as_deref(), Some("gemini-other"));
        assert_eq!(row.asr_model, None);
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.route.asr, LOCAL);
        assert_eq!(cfg.route.translate, "gemini");
        assert_eq!(store.get("gemini").unwrap().unwrap().expose(), KEY);
        assert_eq!(list_services(&path, &store).unwrap().len(), 2);
    }

    #[test]
    fn save_into_default_config_and_compat_without_key() {
        let path = temp_config("default");
        // Missing file: starts from defaults (`cloud = []` as written by the serializer).
        let store = MemorySecretStore::new();
        let mut d = draft(
            CloudKind::OpenAiCompat,
            Some("http://192.168.1.5:1234/v1/".into()),
            None,
        );
        d.llm_model = Some("qwen".into());
        let row = save_service(
            &path,
            &store,
            &d,
            Purposes {
                asr: false,
                polish: true,
                translate: true,
            },
        )
        .unwrap();
        assert_eq!(row.id, "custom");
        assert!(!row.has_key);
        let cfg = load_config(&path).unwrap();
        assert_eq!(
            cfg.cloud_service("custom").unwrap().base_url.as_deref(),
            Some("http://192.168.1.5:1234/v1")
        );
        assert_eq!(cfg.route.polish, "custom");
        let again = save_service(&path, &store, &d, Purposes::default()).unwrap();
        assert_eq!(again.id, "custom-2");
    }

    fn compat(url: &str, name: Option<&str>) -> CloudDraft {
        let mut d = draft(CloudKind::OpenAiCompat, Some(url.into()), None);
        d.llm_model = Some("qwen".into());
        d.name = name.map(str::to_owned);
        d
    }

    #[test]
    fn name_is_trimmed_limited_and_compat_only() {
        let n = |kind, v: &str| normalize_name(kind, &Some(v.to_owned()));
        let compat = CloudKind::OpenAiCompat;
        assert_eq!(n(compat, "  區網 Qwen  "), Ok(Some("區網 Qwen".into())));
        assert_eq!(n(compat, "   "), Ok(None));
        assert_eq!(n(compat, ""), Ok(None));
        assert_eq!(normalize_name(compat, &None), Ok(None));
        // 32 characters (not bytes) is the limit; CJK counts one per character.
        assert_eq!(n(compat, &"字".repeat(32)), Ok(Some("字".repeat(32))));
        assert_eq!(n(compat, &"字".repeat(33)), Err("name_too_long"));
        assert_eq!(n(compat, &"a".repeat(33)), Err("name_too_long"));
        assert_eq!(n(compat, "a\nb"), Err("invalid_name"));
        assert_eq!(n(compat, " Local "), Err("name_reserved"));
        assert_eq!(n(compat, "local LLM"), Ok(Some("local LLM".into())));
        for kind in [CloudKind::Gemini, CloudKind::OpenAi, CloudKind::Claude] {
            assert_eq!(n(kind, "My Gemini"), Ok(None));
        }

        let mut svc = compat_service("custom-2");
        assert_eq!(display_name(&svc), "custom-2");
        svc.name = Some("區網 Qwen".into());
        assert_eq!(display_name(&svc), "區網 Qwen");
        svc.name = Some("  ".into());
        assert_eq!(display_name(&svc), "custom-2");
        // A name on another kind (hand-edited config) is never shown.
        let mut cfg = Config::with_defaults();
        cfg.cloud.push(CloudService {
            id: "gemini".into(),
            name: Some("Mine".into()),
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: None,
            llm_model: None,
        });
        assert_eq!(display_name(&cfg.cloud[0]), "gemini");
        let row = view(&cfg.cloud[0], &cfg, &MemorySecretStore::new());
        assert_eq!((row.name, row.display_name.as_str()), (None, "gemini"));
    }

    fn compat_service(id: &str) -> CloudService {
        CloudService {
            id: id.into(),
            name: None,
            kind: CloudKind::OpenAiCompat,
            base_url: Some("http://h/v1".into()),
            asr_model: None,
            llm_model: None,
        }
    }

    #[test]
    fn duplicate_names_are_refused_case_insensitively() {
        let mut a = compat_service("custom");
        a.name = Some("LAN Qwen".into());
        let b = compat_service("custom-2");
        let all = [a, b];
        assert_eq!(
            check_name_unique("lan qwen", "custom-2", &all),
            Err("name_taken")
        );
        assert_eq!(
            check_name_unique("CUSTOM", "custom-2", &all),
            Err("name_taken")
        );
        // A service may keep its own name or use its own id.
        assert_eq!(check_name_unique("LAN QWEN", "custom", &all), Ok(()));
        assert_eq!(check_name_unique("custom-2", "custom-2", &all), Ok(()));
        assert_eq!(check_name_unique("Speech", "custom-2", &all), Ok(()));
    }

    #[test]
    fn name_is_saved_renamed_and_removed_without_touching_id_route_or_key() {
        let path = temp_config("name");
        std::fs::write(&path, USER_CONFIG).unwrap();
        let store = MemorySecretStore::new();
        let text_only = Purposes {
            asr: false,
            polish: true,
            translate: true,
        };
        // Unnamed: shows the id.
        let first = save_service(
            &path,
            &store,
            &compat("http://10.0.0.5:8002/v1", None),
            Purposes::default(),
        )
        .unwrap();
        assert_eq!(
            (first.id.as_str(), first.display_name.as_str()),
            ("custom", "custom")
        );
        assert_eq!(first.name, None);

        // Named: id is still custom-N; the name is written with toml_edit.
        let mut d = compat("http://10.0.0.5:8000/v1", Some("  區網 Qwen "));
        d.api_key = Some(KEY.into());
        let row = save_service(&path, &store, &d, text_only).unwrap();
        assert_eq!(row.id, "custom-2");
        assert_eq!(row.name.as_deref(), Some("區網 Qwen"));
        assert_eq!(row.display_name, "區網 Qwen");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("name = \"區網 Qwen\""), "{text}");
        assert!(text.contains("# keep this comment") && text.contains("future_option = 42"));
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.route.polish, "custom-2");

        // Same name (any case) or another service's id is refused, and nothing changes.
        let dup = compat("http://10.0.0.6:8000/v1", Some("區網 qwen"));
        assert_eq!(
            save_service(&path, &store, &dup, Purposes::default()),
            Err("name_taken")
        );
        let mut clash = compat("http://10.0.0.5:8002/v1", Some("Custom-2"));
        clash.id = Some("custom".into());
        assert_eq!(
            save_service(&path, &store, &clash, Purposes::default()),
            Err("name_taken")
        );
        let long = compat("http://10.0.0.6:8000/v1", Some(&"x".repeat(33)));
        assert_eq!(
            save_service(&path, &store, &long, Purposes::default()),
            Err("name_too_long")
        );
        assert_eq!(load_config(&path).unwrap().cloud.len(), 2);

        // Rename: same id, routes and key unchanged.
        let mut rename = compat("http://10.0.0.5:8000/v1", Some("Office LLM"));
        rename.id = Some("custom-2".into());
        let row = save_service(&path, &store, &rename, text_only).unwrap();
        assert_eq!(
            (row.id.as_str(), row.display_name.as_str()),
            ("custom-2", "Office LLM")
        );
        assert!(row.has_key);
        assert_eq!(store.get("custom-2").unwrap().unwrap().expose(), KEY);
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.route.polish, "custom-2");
        assert_eq!(cfg.route.translate, "custom-2");

        // Blank name removes the key from config.toml.
        rename.name = Some("   ".into());
        let row = save_service(&path, &store, &rename, text_only).unwrap();
        assert_eq!((row.name, row.display_name.as_str()), (None, "custom-2"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("name ="), "{text}");
        assert!(text.contains("# keep this comment"));
    }

    #[test]
    fn new_id_skips_ids_used_as_names() {
        let mut cfg = Config::with_defaults();
        let mut named = compat_service("custom");
        named.name = Some("Custom-2".into());
        cfg.cloud.push(named);
        assert_eq!(new_id(CloudKind::OpenAiCompat, &cfg), "custom-3");
        // Existing ids are untouched; other kinds are unaffected.
        assert_eq!(cfg.cloud[0].id, "custom");
        assert_eq!(new_id(CloudKind::Gemini, &cfg), "gemini");
    }

    #[test]
    fn names_on_official_services_are_ignored() {
        let path = temp_config("name-official");
        let store = MemorySecretStore::new();
        let mut d = draft(CloudKind::Gemini, None, Some(KEY));
        d.name = Some("My Gemini".into());
        let row = save_service(&path, &store, &d, Purposes::default()).unwrap();
        assert_eq!((row.name, row.display_name.as_str()), (None, "gemini"));
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("My Gemini"));
    }

    #[test]
    fn old_config_without_names_lists_ids() {
        let path = temp_config("name-old");
        std::fs::write(
            &path,
            "[[cloud]]
id = \"custom\"
kind = \"open_ai_compat\"
base_url = \"http://h:8000/v1\"
llm_model = \"qwen\"
",
        )
        .unwrap();
        let rows = list_services(&path, &MemorySecretStore::new()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].name.as_ref(), rows[0].display_name.as_str()),
            (None, "custom")
        );
    }

    #[test]
    fn save_rejects_bad_drafts() {
        let path = temp_config("reject");
        let store = MemorySecretStore::new();
        let all = Purposes {
            asr: true,
            polish: true,
            translate: true,
        };
        assert_eq!(
            save_service(
                &path,
                &store,
                &draft(CloudKind::Claude, None, Some(KEY)),
                all
            ),
            Err("unsupported_purpose")
        );
        assert_eq!(
            save_service(
                &path,
                &store,
                &draft(CloudKind::OpenAi, None, None),
                Purposes::default()
            ),
            Err("key_missing")
        );
        let compat = draft(
            CloudKind::OpenAiCompat,
            Some("http://localhost:1234/v1".into()),
            None,
        );
        assert_eq!(
            save_service(&path, &store, &compat, all),
            Err("model_required")
        );
        let mut ghost = draft(CloudKind::Gemini, None, Some(KEY));
        ghost.id = Some("nope".into());
        assert_eq!(
            save_service(&path, &store, &ghost, Purposes::default()),
            Err("not_found")
        );
        std::fs::write(&path, "route = 5").unwrap();
        assert_eq!(
            save_service(
                &path,
                &store,
                &draft(CloudKind::Gemini, None, Some(KEY)),
                Purposes::default()
            ),
            Err("config_invalid")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "route = 5");
        assert!(store.get("gemini").unwrap().is_none());
    }

    #[test]
    fn remove_resets_routes_and_deletes_key() {
        let path = temp_config("remove");
        std::fs::write(&path, USER_CONFIG).unwrap();
        let store = MemorySecretStore::new();
        let all = Purposes {
            asr: false,
            polish: true,
            translate: true,
        };
        save_service(
            &path,
            &store,
            &draft(CloudKind::Claude, None, Some(KEY)),
            all,
        )
        .unwrap();
        save_service(
            &path,
            &store,
            &draft(CloudKind::Gemini, None, Some(KEY)),
            Purposes {
                asr: true,
                ..Purposes::default()
            },
        )
        .unwrap();
        assert_eq!(
            remove_service(&path, &store, "claude"),
            Ok(RemoveResult { key_deleted: true })
        );
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.route.polish, LOCAL);
        assert_eq!(cfg.route.translate, LOCAL);
        assert_eq!(cfg.route.asr, "gemini");
        assert!(cfg.cloud_service("claude").is_none());
        assert!(store.get("claude").unwrap().is_none());
        assert!(store.get("gemini").unwrap().is_some());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep this comment"));
        assert_eq!(remove_service(&path, &store, "claude"), Err("not_found"));
    }

    const NEW_KEY: &str = "sk-TESTSECRET-NEWKEY-0000";

    #[test]
    fn failed_config_write_restores_the_keyring() {
        let path = temp_config("rollback");
        let store = MemorySecretStore::new();
        save_service(
            &path,
            &store,
            &draft(CloudKind::Gemini, None, Some(KEY)),
            Purposes::default(),
        )
        .unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        // A folder where the temporary file goes makes the config write fail.
        std::fs::create_dir_all(path.with_extension("toml.tmp")).unwrap();

        let mut edit = draft(CloudKind::Gemini, None, Some(NEW_KEY));
        edit.id = Some("gemini".into());
        assert_eq!(
            save_service(&path, &store, &edit, Purposes::default()),
            Err("config_save_failed")
        );
        assert_eq!(store.get("gemini").unwrap().unwrap().expose(), KEY);

        let fresh = draft(CloudKind::OpenAi, None, Some(NEW_KEY));
        assert_eq!(
            save_service(&path, &store, &fresh, Purposes::default()),
            Err("config_save_failed")
        );
        assert!(store.get("openai").unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn endpoint_change_needs_a_new_key() {
        let path = temp_config("endpoint");
        let store = MemorySecretStore::new();
        // Official service: moving to another endpoint without a key is refused.
        save_service(
            &path,
            &store,
            &draft(CloudKind::OpenAi, None, Some(KEY)),
            Purposes::default(),
        )
        .unwrap();
        let mut moved = draft(
            CloudKind::OpenAi,
            Some("https://proxy.example/v1".into()),
            None,
        );
        moved.id = Some("openai".into());
        assert_eq!(
            save_service(&path, &store, &moved, Purposes::default()),
            Err("key_missing")
        );
        assert_eq!(store.get("openai").unwrap().unwrap().expose(), KEY);
        // Same endpoint written differently keeps the key.
        let mut same = draft(
            CloudKind::OpenAi,
            Some("HTTPS://api.openai.com/v1/".into()),
            None,
        );
        same.id = Some("openai".into());
        assert!(
            save_service(&path, &store, &same, Purposes::default())
                .unwrap()
                .has_key
        );

        // Compatible endpoint: the old key is dropped, not sent to the new host.
        let mut lan = draft(
            CloudKind::OpenAiCompat,
            Some("http://192.168.1.5:1234/v1".into()),
            Some(KEY),
        );
        lan.llm_model = Some("qwen".into());
        save_service(&path, &store, &lan, Purposes::default()).unwrap();
        lan.id = Some("custom".into());
        lan.api_key = None;
        lan.base_url = Some("http://192.168.1.99:1234/v1".into());
        let row = save_service(&path, &store, &lan, Purposes::default()).unwrap();
        assert!(!row.has_key);
        assert!(store.get("custom").unwrap().is_none());
    }

    /// A store whose deletes fail (locked keyring).
    struct NoDelete(MemorySecretStore);

    impl SecretStore for NoDelete {
        fn get(&self, id: &str) -> v2c_core::Result<Option<ApiKey>> {
            self.0.get(id)
        }
        fn set(&self, id: &str, key: &ApiKey) -> v2c_core::Result<()> {
            self.0.set(id, key)
        }
        fn delete(&self, _id: &str) -> v2c_core::Result<()> {
            Err(V2cError::Storage("locked".into()))
        }
    }

    #[test]
    fn remove_succeeds_when_only_the_key_delete_fails() {
        let path = temp_config("nodelete");
        let store = NoDelete(MemorySecretStore::new());
        save_service(
            &path,
            &store,
            &draft(CloudKind::Claude, None, Some(KEY)),
            Purposes {
                asr: false,
                polish: true,
                translate: false,
            },
        )
        .unwrap();
        assert_eq!(
            remove_service(&path, &store, "claude"),
            Ok(RemoveResult { key_deleted: false })
        );
        let cfg = load_config(&path).unwrap();
        assert!(cfg.cloud_service("claude").is_none());
        assert_eq!(cfg.route.polish, LOCAL);
    }

    #[test]
    fn draft_debug_is_redacted_and_kinds_are_complete() {
        let d = draft(CloudKind::OpenAi, None, Some(KEY));
        let dbg = format!("{d:?}");
        assert!(!dbg.contains("TESTSECRET") && dbg.contains("<redacted>"));
        let parsed: CloudDraft = serde_json::from_value(json!({
            "kind": "open_ai_compat", "base_url": "http://localhost:1234/v1", "api_key": KEY
        }))
        .unwrap();
        assert_eq!(parsed.kind, CloudKind::OpenAiCompat);
        assert!(!format!("{parsed:?}").contains("TESTSECRET"));

        let k = kinds();
        assert_eq!(k.len(), 4);
        let claude = k.iter().find(|i| i.kind == CloudKind::Claude).unwrap();
        assert!(!claude.supports_asr && claude.capabilities.contains("只能潤飾與中翻英"));
        for i in &k {
            assert!(i.key_page_url.is_none_or(|u| u.starts_with("https://")));
        }
    }

    #[test]
    fn only_main_window_may_call() {
        assert_eq!(check_caller("main"), Ok(()));
        assert_eq!(check_caller("overlay"), Err("forbidden"));
    }
}
