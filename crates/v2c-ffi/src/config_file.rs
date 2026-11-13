//! Portable configuration file (import preview and export) for the Android app, plan 14.3.
//!
//! All rules live in `v2c_core::portable`; this module only maps the phone's model onto it:
//! one service per provider (fixed ids `cloud-gemini`, `cloud-openai`, `cloud-claude`,
//! `cloud-custom`), HTTPS endpoints only, one service for recognition and one for translation,
//! no polish and no vocabulary. Nothing here logs; keys never enter or leave (only whether a key
//! is stored), and text, ids and URLs of the file are never put into errors.

use v2c_core::config::{CloudKind, CloudService, Config};
use v2c_core::portable::{self, DefaultKeys, PortableFile, PortableRoute};
use v2c_core::route::suggested_models;
use v2c_core::vocab::Vocab;

use crate::services::{lan_has, CloudProvider, CloudServiceConfig, CloudUse};

/// Ids of the built-in TN / RD LAN services; always "already built in" even before the preset
/// files are loaded.
const BUILTIN_IDS: &[&str] = &[
    "tn-tea-asr",
    "tn-breeze-asr",
    "tn-qwen",
    "rd-tea-asr",
    "rd-breeze-asr",
    "rd-qwen",
];

/// Why a service of the file is not imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SkipReason {
    /// Same id as a built-in TN / RD LAN service.
    Builtin,
    /// `open_ai_compat` endpoint that is not HTTPS (phone cloud services are HTTPS only).
    HttpEndpoint,
    /// An official service with its own `base_url` (the phone cannot show or edit one).
    OfficialBaseUrl,
    /// The phone keeps one service per provider and another service of the same provider was
    /// taken instead (the one used by `route.asr`, then `route.translate`, else the first).
    DuplicateProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ImportStatus {
    Added,
    Updated,
    Unchanged,
}

/// A file service that the phone can take.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ImportedService {
    /// Id in the file (for display); `config.id` is the phone's id for the provider.
    pub source_id: String,
    /// Models are filled with the provider's suggested ones when the file leaves them out.
    /// `uses` holds only what the file's `route` asks for; merge it with the existing uses.
    pub config: CloudServiceConfig,
    pub status: ImportStatus,
    /// No key stored (or it will be dropped) and the file brings none: ask the user.
    pub needs_key: bool,
    /// The endpoint changed: delete the stored key of `config.id` before saving.
    pub drop_old_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SkippedService {
    /// Id in the file.
    pub id: String,
    pub reason: SkipReason,
}

/// Result of [`crate::parse_config_file`]. When `error_code` is set nothing else is filled.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConfigFilePreview {
    pub error_code: Option<String>,
    pub error_line: Option<u32>,
    pub title: Option<String>,
    pub exported_from: Option<String>,
    pub services: Vec<ImportedService>,
    pub skipped: Vec<SkippedService>,
    /// Phone id that gets the recognition use / the translation use, if the file names one.
    pub asr_service: Option<String>,
    pub translate_service: Option<String>,
    /// Parts of the file not used: sections and unknown fields (names only), plus
    /// `route.polish`, `route.asr` / `route.translate` when their service was skipped,
    /// `preset.default_keys` and `vocabulary`.
    pub ignored: Vec<String>,
}

impl ConfigFilePreview {
    fn failed(code: &str, line: Option<u32>) -> Self {
        ConfigFilePreview {
            error_code: Some(code.to_owned()),
            error_line: line,
            title: None,
            exported_from: None,
            services: vec![],
            skipped: vec![],
            asr_service: None,
            translate_service: None,
            ignored: vec![],
        }
    }
}

/// Export result: the file text and the phone ids that had to be left out.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConfigFileExport {
    pub text: String,
    pub skipped: Vec<String>,
}

fn phone_id(kind: CloudKind) -> &'static str {
    match kind {
        CloudKind::Gemini => "cloud-gemini",
        CloudKind::OpenAi => "cloud-openai",
        CloudKind::Claude => "cloud-claude",
        CloudKind::OpenAiCompat => "cloud-custom",
    }
}

fn provider_of(kind: CloudKind) -> CloudProvider {
    match kind {
        CloudKind::Gemini => CloudProvider::Gemini,
        CloudKind::OpenAi => CloudProvider::Openai,
        CloudKind::Claude => CloudProvider::Claude,
        CloudKind::OpenAiCompat => CloudProvider::Custom,
    }
}

fn kind_of(p: CloudProvider) -> CloudKind {
    match p {
        CloudProvider::Gemini => CloudKind::Gemini,
        CloudProvider::Openai => CloudKind::OpenAi,
        CloudProvider::Claude => CloudKind::Claude,
        CloudProvider::Custom => CloudKind::OpenAiCompat,
    }
}

fn is_builtin(id: &str) -> bool {
    BUILTIN_IDS.contains(&id) || lan_has(id)
}

fn is_https(url: &str) -> bool {
    url.trim().to_ascii_lowercase().starts_with("https://")
}

fn non_blank(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// The phone's service as a core service (no name: the phone has none).
fn to_core(c: &CloudServiceConfig) -> CloudService {
    CloudService {
        id: c.id.clone(),
        name: None,
        kind: kind_of(c.provider),
        base_url: c.base_url.as_deref().and_then(non_blank),
        asr_model: c.asr_model.as_deref().and_then(non_blank),
        llm_model: non_blank(&c.model),
        asr_timeout_ms: c.asr_timeout_ms,
    }
}

/// Fill models the file left out with the provider's suggested ones (what the phone shows).
fn with_suggested(mut s: CloudService) -> CloudService {
    let sug = suggested_models(s.kind);
    if s.asr_model.is_none() {
        s.asr_model = sug.asr.map(str::to_owned);
    }
    if s.llm_model.is_none() {
        s.llm_model = sug.llm.map(str::to_owned);
    }
    s
}

/// Classifies the services of a portable file for the phone and plans the merge into `current`
/// (the phone's services). `stored_keys` are the ids of `current` that have a stored key.
pub fn preview(
    text: &str,
    current: &[CloudServiceConfig],
    stored_keys: &[String],
) -> ConfigFilePreview {
    let file = match portable::parse(text) {
        Ok(f) => f,
        Err(e) => return ConfigFilePreview::failed(e.code, e.line),
    };
    let mut ignored = file.ignored.clone();
    let mut skipped = Vec::new();

    // 1. Services the phone cannot take.
    let mut candidates: Vec<&CloudService> = Vec::new();
    for svc in &file.services {
        let reason = if is_builtin(&svc.id) {
            Some(SkipReason::Builtin)
        } else if svc.kind == CloudKind::OpenAiCompat {
            (!svc.base_url.as_deref().is_some_and(is_https)).then_some(SkipReason::HttpEndpoint)
        } else {
            svc.base_url
                .is_some()
                .then_some(SkipReason::OfficialBaseUrl)
        };
        match reason {
            Some(reason) => skipped.push(SkippedService {
                id: svc.id.clone(),
                reason,
            }),
            None => candidates.push(svc),
        }
    }

    // 2. One service per provider: the one used by route.asr, then route.translate, else first.
    let rank = |s: &CloudService| {
        if file.route.asr.as_deref() == Some(&s.id) {
            0
        } else if file.route.translate.as_deref() == Some(&s.id) {
            1
        } else {
            2
        }
    };
    let mut chosen: Vec<&CloudService> = Vec::new();
    for svc in &candidates {
        let best = candidates
            .iter()
            .filter(|o| o.kind == svc.kind)
            .min_by_key(|o| rank(o));
        if best.is_some_and(|b| b.id == svc.id) {
            chosen.push(svc);
        } else {
            skipped.push(SkippedService {
                id: svc.id.clone(),
                reason: SkipReason::DuplicateProvider,
            });
        }
    }

    // 3. Re-key onto the phone's ids and plan the merge with the core rules.
    let source_of = |phone: &str| {
        chosen
            .iter()
            .find(|s| phone_id(s.kind) == phone)
            .map(|s| s.id.clone())
            .unwrap_or_default()
    };
    let remapped: Vec<CloudService> = chosen
        .iter()
        .map(|s| {
            let mut c = with_suggested((*s).clone());
            c.id = phone_id(s.kind).to_owned();
            c.name = None;
            c
        })
        .collect();
    let target = |wanted: &Option<String>, field: &str, ignored: &mut Vec<String>| {
        let id = wanted.as_deref()?;
        if id == v2c_core::config::LOCAL {
            return None;
        }
        match chosen.iter().find(|s| s.id == id) {
            Some(s) => Some(phone_id(s.kind).to_owned()),
            None => {
                ignored.push(field.to_owned());
                None
            }
        }
    };
    let asr_service = target(&file.route.asr, "route.asr", &mut ignored);
    let translate_service = target(&file.route.translate, "route.translate", &mut ignored);
    if file
        .route
        .polish
        .as_deref()
        .is_some_and(|p| p != v2c_core::config::LOCAL)
    {
        ignored.push("route.polish".into());
    }
    if !file.default_keys.is_empty() {
        ignored.push("preset.default_keys".into());
    }
    if !file.vocab.is_empty() || file.vocab_dropped > 0 {
        ignored.push("vocabulary".into());
    }

    // Official providers always need a key; a custom endpoint only when the file says so.
    let file2 = PortableFile {
        needs_key: remapped
            .iter()
            .filter(|s| {
                s.kind != CloudKind::OpenAiCompat || file.needs_key.contains(&source_of(&s.id))
            })
            .map(|s| s.id.clone())
            .collect(),
        default_keys: DefaultKeys::default(),
        route: PortableRoute::default(),
        services: remapped.clone(),
        vocab: Vocab::default(),
        vocab_dropped: 0,
        ignored: vec![],
        ..file.clone()
    };
    let cfg = Config {
        cloud: current.iter().map(to_core).collect(),
        ..Config::default()
    };
    let has_key = |id: &str| stored_keys.iter().any(|k| k == id);
    let plan = portable::plan_merge(&cfg, &Vocab::default(), &file2, &has_key);
    ignored.extend(plan.ignored.iter().cloned());

    let services = remapped
        .iter()
        .map(|s| {
            let status = if plan.added.contains(&s.id) {
                ImportStatus::Added
            } else if plan.updated.contains(&s.id) {
                ImportStatus::Updated
            } else {
                ImportStatus::Unchanged
            };
            let mut uses = Vec::new();
            if asr_service.as_deref() == Some(&s.id) {
                uses.push(CloudUse::Asr);
            }
            if translate_service.as_deref() == Some(&s.id) {
                uses.push(CloudUse::Translate);
            }
            ImportedService {
                source_id: source_of(&s.id),
                config: CloudServiceConfig {
                    id: s.id.clone(),
                    provider: provider_of(s.kind),
                    model: s.llm_model.clone().unwrap_or_default(),
                    asr_model: s.asr_model.clone(),
                    base_url: s.base_url.clone(),
                    uses,
                    asr_timeout_ms: s.asr_timeout_ms,
                },
                status,
                needs_key: plan.key_missing.contains(&s.id),
                drop_old_key: plan.drop_old_key.contains(&s.id),
            }
        })
        .collect();

    ConfigFilePreview {
        error_code: None,
        error_line: None,
        title: file.title.clone(),
        exported_from: file.exported_from.clone(),
        services,
        skipped,
        asr_service,
        translate_service,
        ignored,
    }
}

/// Drops the `[route]` lines the phone has nothing to say about (`polish`, the fallback flag and
/// purposes that stay `local`), so importing the file on the desktop does not reset those
/// settings. An emptied `[route]` section is removed with its comment.
fn strip_idle_route(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut in_route = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_route = t == "[route]";
        }
        let idle = in_route
            && (t.starts_with("polish =")
                || t.starts_with("cloud_fallback_to_local =")
                || t == "asr = \"local\""
                || t == "translate = \"local\"");
        if !idle {
            kept.push(line);
        }
    }
    let mut res: Vec<&str> = Vec::new();
    for (i, line) in kept.iter().enumerate() {
        if line.trim() == "[route]" {
            let next = kept.get(i + 1).map_or("", |l| l.trim());
            if next.is_empty() || next.starts_with('[') || next.starts_with('#') {
                while res.last().is_some_and(|l| l.trim_start().starts_with('#')) {
                    res.pop();
                }
                continue;
            }
        }
        res.push(line);
    }
    let mut s = res.join("\n");
    s.push('\n');
    s
}

/// The phone's cloud services as portable file text. No keys, no vocabulary, no polish.
/// `asr_id` / `translate_id` name the services used for each purpose. `stored_keys` are the ids
/// that have a stored key (a Custom service is listed under `needs_key` only then).
pub fn export(
    services: &[CloudServiceConfig],
    asr_id: Option<&str>,
    translate_id: Option<&str>,
    stored_keys: &[String],
) -> ConfigFileExport {
    let mut cfg = Config {
        cloud: services.iter().map(to_core).collect(),
        ..Config::default()
    };
    cfg.route.asr = asr_id.unwrap_or(v2c_core::config::LOCAL).to_owned();
    cfg.route.translate = translate_id.unwrap_or(v2c_core::config::LOCAL).to_owned();
    let has_key = |id: &str| stored_keys.iter().any(|k| k == id);
    let out = portable::export(&cfg, None, &has_key, "android");
    ConfigFileExport {
        text: strip_idle_route(&out.text),
        skipped: out.skipped,
    }
}

#[cfg(test)]
mod tests;
