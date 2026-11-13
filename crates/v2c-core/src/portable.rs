//! Portable configuration file (format version 1): parse, validate, merge plan, export.
//!
//! A portable file carries the cloud / LAN services (`[[cloud]]`), who handles each purpose
//! (`[route]`) and the vocabulary (`[vocabulary]`). It never carries API keys: `[preset]
//! needs_key` only names the services that need one, and a hand-written file may ship shared
//! keys in `[preset] default_keys` ([`export`] never writes them). The built-in TN / RD preset
//! files are valid portable files. Spec: `tasks/plan.md` section 14, user docs:
//! `docs/config-file.md`.
//!
//! - [`parse`]: syntax and rule validation, no current config needed. Any failure rejects the
//!   whole file.
//! - [`plan_merge`] (pure): what importing into the current config would add / update / leave,
//!   which old keys must go, route and vocabulary changes. Merge = by service id; other services
//!   and their keys are untouched.
//! - [`export`]: the current config as file text (fixed field order, a comment line before each
//!   section, no `default_keys`).
//!
//! Privacy: errors carry a code and a line number, never file content. `Debug` of every type
//! here that can hold keys, URLs or vocabulary shows counts or ids only. Nothing is logged.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use serde::Deserialize;

use crate::config::{CloudKind, CloudService, Config, LOCAL};
use crate::route::{effective_base_url, suggested_models};
use crate::vocab::{self, Vocab};

/// The only format version this build understands.
pub const FORMAT_VERSION: u32 = 1;
/// Largest file text [`parse`] accepts, in bytes.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// Longest display name of a compatible endpoint, in characters.
pub const NAME_MAX_CHARS: usize = 32;
/// Shortest and longest per-service speech recognition timeout, ms.
pub const ASR_TIMEOUT_MIN_MS: u64 = 1000;
pub const ASR_TIMEOUT_MAX_MS: u64 = 60_000;
/// Longest key accepted in `default_keys`, in characters.
pub const MAX_DEFAULT_KEY_CHARS: usize = 256;

/// Top-level sections that are read; anything else is ignored and listed in the preview.
const WHITELIST: &[&str] = &["v2c", "preset", "route", "cloud", "vocabulary"];
/// Base ids of the built-in service kinds plus `local`: an `id_prefix` must not start with one.
const RESERVED_BASES: &[&str] = &["openai", "gemini", "claude", "custom", "local"];
/// Longest `title` / `exported_from` / `exported_at` kept (display only).
const MAX_DISPLAY_CHARS: usize = 64;

// ---------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------

/// Why a file was rejected: a short stable code and the 1-based line, when known.
///
/// Never contains file content (ids, URLs, keys, words), also not in `Debug`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PortableError {
    pub code: &'static str,
    pub line: Option<u32>,
}

impl PortableError {
    fn new(code: &'static str, line: Option<usize>) -> Self {
        Self {
            code,
            line: line.map(|l| l as u32),
        }
    }

    /// A short fixed English description of `code` (for logs-free diagnostics and docs).
    pub fn message(&self) -> &'static str {
        match self.code {
            "too_large" => "file is too large",
            "syntax" => "not valid TOML",
            "invalid_value" => "a value has the wrong type or is not allowed",
            "missing_field" => "a required field is missing",
            "unknown_kind" => "unknown service kind",
            "format_missing" => "no [v2c] format version",
            "format_invalid" => "format version is not a valid number",
            "format_too_new" => "file was produced by a newer version",
            "id_invalid" => "service id must be letters, digits, - or _ with no spaces",
            "id_reserved" => "service id is reserved",
            "preset_duplicate_id" => "service id appears more than once",
            "preset_id_prefix" => {
                "id_prefix is empty or starts like a built-in id, or a service id lacks it"
            }
            "base_url_required" => "this kind needs base_url",
            "invalid_base_url" => "base_url is not allowed (official services need https)",
            "name_too_long" => "name is too long",
            "invalid_name" => "name has control characters",
            "name_reserved" => "name is reserved",
            "name_taken" => "name clashes with another service",
            "asr_timeout_range" => "asr_timeout_ms must be 1000-60000",
            "route_unknown_service" => "route points to a service that is not in the file",
            "route_no_asr" => "this service cannot do speech recognition",
            "route_no_model" => "this service has no model for that purpose",
            "preset_bad_reference" => {
                "needs_key, default_keys or asr_choices names a service that does not fit"
            }
            "invalid_key" => "default key is empty, too long or has control characters",
            _ => "invalid file",
        }
    }
}

impl fmt::Debug for PortableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortableError")
            .field("code", &self.code)
            .field("line", &self.line)
            .finish()
    }
}

impl fmt::Display for PortableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(l) => write!(f, "line {l}: {} ({})", self.code, self.message()),
            None => write!(f, "{} ({})", self.code, self.message()),
        }
    }
}

impl std::error::Error for PortableError {}

// ---------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------

/// Shared keys shipped in a hand-written file, by service id. `Debug` shows ids only.
#[derive(Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(transparent)]
pub struct DefaultKeys(BTreeMap<String, String>);

impl DefaultKeys {
    /// The key for `id` (trimmed, validated by [`parse`]).
    pub fn get(&self, id: &str) -> Option<&str> {
        self.0.get(id).map(String::as_str)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.0.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The service ids that have a default key.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

impl fmt::Debug for DefaultKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

/// `[route]` of a file: only the fields that were written (the rest stay as they are).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortableRoute {
    pub asr: Option<String>,
    pub polish: Option<String>,
    pub translate: Option<String>,
    pub cloud_fallback_to_local: Option<bool>,
}

/// A validated, normalised file (names trimmed, blank models and URLs removed, trailing `/`
/// of URLs dropped).
#[derive(Clone, PartialEq, Eq)]
pub struct PortableFile {
    /// Format version (always [`FORMAT_VERSION`] today; legacy preset files count as 1).
    pub format: u32,
    /// `[v2c] exported_from` / `exported_at`, display only.
    pub exported_from: Option<String>,
    pub exported_at: Option<String>,
    /// `[preset] title`, display only.
    pub title: Option<String>,
    /// `[preset] id_prefix`: when set, every service id starts with it.
    pub id_prefix: Option<String>,
    /// `[preset] asr_choices`: services the user may pick as `route.asr` (preset files).
    pub asr_choices: Vec<String>,
    /// `[preset] needs_key`: services that need an API key.
    pub needs_key: Vec<String>,
    pub default_keys: DefaultKeys,
    /// `[preset] detect_llm_model`: ask the endpoint for its model list (preset files).
    pub detect_llm_model: bool,
    pub route: PortableRoute,
    pub services: Vec<CloudService>,
    /// Vocabulary after the `vocab` rules (at most `vocab::MAX_ENTRIES`).
    pub vocab: Vocab,
    /// Vocabulary entries the file rules dropped (too long, comment-like, over the limit).
    pub vocab_dropped: usize,
    /// Top-level sections that are not used (sorted).
    pub ignored: Vec<String>,
}

impl fmt::Debug for PortableFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortableFile")
            .field("format", &self.format)
            .field(
                "services",
                &self.services.iter().map(|s| &s.id).collect::<Vec<_>>(),
            )
            .field("needs_key", &self.needs_key)
            .field("default_keys", &self.default_keys)
            .field("vocab_len", &self.vocab.len())
            .field("ignored", &self.ignored)
            .finish()
    }
}

/// One `[route]` field that importing would change. Values are service ids, `local`, or
/// `true` / `false` for `cloud_fallback_to_local`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChange {
    /// `asr`, `polish`, `translate` or `cloud_fallback_to_local`.
    pub field: &'static str,
    pub old: String,
    pub new: String,
}

/// What importing a file into the current config would do. Pure data for the preview.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct MergePlan {
    /// Services that do not exist yet (ids, in file order).
    pub added: Vec<String>,
    /// Services that exist and differ.
    pub updated: Vec<String>,
    /// Services that exist and are identical.
    pub unchanged: Vec<String>,
    /// Updated services whose kind or endpoint changed: their old key must be deleted unless a
    /// new one is given (the key must not be sent to a new host).
    pub drop_old_key: Vec<String>,
    /// `needs_key` services that would have no key after the import: no default key, and no
    /// stored key that survives (new services, a changed endpoint, or `has_key` says none).
    /// A key typed in the preview can still fill them.
    pub key_missing: Vec<String>,
    pub route: Vec<RouteChange>,
    /// Vocabulary entries that would be appended, in file order.
    pub vocab_new: Vec<String>,
    /// Entries that do not fit: over `vocab::MAX_ENTRIES` in total, plus those the file rules
    /// dropped.
    pub vocab_blocked: usize,
    /// Parts of the file that are not used: top-level sections (`hotkey`) and unknown fields
    /// (`cloud[2].asr_modle`, 1-based). Names only, never values.
    pub ignored: Vec<String>,
    /// Services of the file whose id or name clashes (ignoring case) with the id or name of a
    /// service the import does not touch. The import must not be applied when not empty.
    pub name_taken: Vec<String>,
}

impl MergePlan {
    /// True when applying would change nothing.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.updated.is_empty()
            && self.route.is_empty()
            && self.vocab_new.is_empty()
    }

    /// True when the plan can be applied (no name clashes with the current config).
    pub fn is_applicable(&self) -> bool {
        self.name_taken.is_empty()
    }
}

impl fmt::Debug for MergePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MergePlan")
            .field("added", &self.added)
            .field("updated", &self.updated)
            .field("unchanged", &self.unchanged)
            .field("drop_old_key", &self.drop_old_key)
            .field("key_missing", &self.key_missing)
            .field("route", &self.route)
            .field("vocab_new_len", &self.vocab_new.len())
            .field("vocab_blocked", &self.vocab_blocked)
            .field("ignored", &self.ignored)
            .field("name_taken", &self.name_taken)
            .finish()
    }
}

// ---------------------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawFile {
    v2c: Option<RawHeader>,
    preset: Option<RawPreset>,
    route: Option<RawRoute>,
    #[serde(default)]
    cloud: Vec<CloudService>,
    vocabulary: Option<RawVocab>,
}

#[derive(Deserialize)]
struct RawHeader {
    format: Option<i64>,
    exported_from: Option<String>,
    exported_at: Option<String>,
}

#[derive(Deserialize)]
struct RawPreset {
    title: Option<String>,
    id_prefix: Option<String>,
    #[serde(default)]
    asr_choices: Vec<String>,
    #[serde(default)]
    needs_key: Vec<String>,
    #[serde(default)]
    default_keys: DefaultKeys,
    #[serde(default)]
    detect_llm_model: bool,
}

#[derive(Deserialize)]
struct RawRoute {
    asr: Option<String>,
    polish: Option<String>,
    translate: Option<String>,
    cloud_fallback_to_local: Option<bool>,
}

#[derive(Deserialize)]
struct RawVocab {
    #[serde(default)]
    words: Vec<String>,
}

/// Finds lines in the source text so validation errors can point at them.
struct Lines<'a> {
    text: &'a str,
    /// Line index (0-based) of each `[[cloud]]` header.
    cloud: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let cloud = text
            .lines()
            .enumerate()
            .filter(|(_, l)| l.trim_start().starts_with("[[cloud]]"))
            .map(|(i, _)| i)
            .collect();
        Self { text, cloud }
    }

    /// 1-based line of `key = ...` inside the `n`-th `[[cloud]]` table (falls back to its
    /// header).
    fn cloud_key(&self, n: usize, key: &str) -> Option<usize> {
        let start = *self.cloud.get(n)?;
        let end = self.cloud.get(n + 1).copied().unwrap_or(usize::MAX);
        self.text
            .lines()
            .enumerate()
            .skip(start)
            .take_while(|(i, _)| *i < end)
            .find(|(_, l)| is_key_line(l, key))
            .map(|(i, _)| i + 1)
            .or(Some(start + 1))
    }

    /// 1-based line of the first `key = ...` anywhere after the `[section]` header.
    fn section_key(&self, section: &str, key: &str) -> Option<usize> {
        let header = format!("[{section}]");
        let mut in_section = false;
        let mut header_line = None;
        for (i, l) in self.text.lines().enumerate() {
            let t = l.trim_start();
            if t.starts_with('[') {
                in_section = t.starts_with(&header);
                if in_section {
                    header_line = Some(i + 1);
                }
                continue;
            }
            if in_section && is_key_line(l, key) {
                return Some(i + 1);
            }
        }
        header_line
    }
}

fn is_key_line(line: &str, key: &str) -> bool {
    line.trim_start()
        .strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn offset_line(text: &str, offset: usize) -> usize {
    let end = offset.min(text.len());
    text.as_bytes()[..end]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

fn display_text(s: Option<String>) -> Option<String> {
    let s: String = s?
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DISPLAY_CHARS)
        .collect();
    let s = s.trim().to_owned();
    (!s.is_empty()).then_some(s)
}

fn trim_opt(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// A service in canonical form: trimmed model / URL, blank = unset, names only on compatible
/// endpoints.
fn normalize_service(svc: &CloudService) -> CloudService {
    let mut s = svc.clone();
    s.base_url = trim_opt(&s.base_url).map(|u| u.trim_end_matches('/').to_owned());
    s.asr_model = trim_opt(&s.asr_model);
    s.llm_model = trim_opt(&s.llm_model);
    s.name = if s.kind == CloudKind::OpenAiCompat {
        trim_opt(&s.name)
    } else {
        None
    };
    s
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn check_name(name: &str) -> Result<(), &'static str> {
    if name.chars().count() > NAME_MAX_CHARS {
        return Err("name_too_long");
    }
    if name.chars().any(char::is_control) {
        return Err("invalid_name");
    }
    if name.to_lowercase() == LOCAL {
        return Err("name_reserved");
    }
    Ok(())
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The shown name of a normalised service (compatible endpoints only).
fn shown(s: &CloudService) -> Option<&str> {
    s.name
        .as_deref()
        .filter(|_| s.kind == CloudKind::OpenAiCompat)
}

/// The service's name equals (ignoring case) the id or name of one of `others`, or the id of
/// the service equals the name of one of them. `others` must be normalised.
fn clashes(s: &CloudService, others: &[CloudService]) -> bool {
    others.iter().filter(|o| o.id != s.id).any(|o| {
        shown(s).is_some_and(|n| eq_ci(n, &o.id) || shown(o).is_some_and(|on| eq_ci(n, on)))
            || shown(o).is_some_and(|on| eq_ci(on, &s.id))
    })
}

/// Validates and normalises one service against the ones accepted before it. On failure:
/// the error code and the field to point the line number at.
fn validate_service(
    svc: &CloudService,
    accepted: &[CloudService],
) -> Result<CloudService, (&'static str, &'static str)> {
    if !valid_id(&svc.id) {
        return Err(("id_invalid", "id"));
    }
    if svc.id.to_lowercase() == LOCAL {
        return Err(("id_reserved", "id"));
    }
    if accepted.iter().any(|a| a.id == svc.id) {
        return Err(("preset_duplicate_id", "id"));
    }
    let s = normalize_service(svc);
    if s.kind == CloudKind::OpenAiCompat && s.base_url.is_none() {
        return Err(("base_url_required", "base_url"));
    }
    if effective_base_url(&s).is_err() {
        return Err(("invalid_base_url", "base_url"));
    }
    if let Some(name) = &s.name {
        check_name(name).map_err(|c| (c, "name"))?;
    }
    if clashes(&s, accepted) {
        return Err(("name_taken", "name"));
    }
    if let Some(ms) = s.asr_timeout_ms {
        if !(ASR_TIMEOUT_MIN_MS..=ASR_TIMEOUT_MAX_MS).contains(&ms) {
            return Err(("asr_timeout_range", "asr_timeout_ms"));
        }
    }
    Ok(s)
}

fn has_asr(svc: &CloudService) -> bool {
    svc.kind.supports_asr() && (svc.asr_model.is_some() || suggested_models(svc.kind).asr.is_some())
}

fn has_llm(svc: &CloudService) -> bool {
    svc.llm_model.is_some() || suggested_models(svc.kind).llm.is_some()
}

const KNOWN_FIELDS: &[(&str, &[&str])] = &[
    ("v2c", &["format", "exported_from", "exported_at"]),
    (
        "preset",
        &[
            "title",
            "id_prefix",
            "asr_choices",
            "needs_key",
            "default_keys",
            "detect_llm_model",
        ],
    ),
    (
        "route",
        &["asr", "polish", "translate", "cloud_fallback_to_local"],
    ),
    ("vocabulary", &["words"]),
];
const KNOWN_CLOUD_FIELDS: &[&str] = &[
    "id",
    "name",
    "kind",
    "base_url",
    "asr_model",
    "llm_model",
    "asr_timeout_ms",
];
/// At most this many unknown fields are listed.
const MAX_IGNORED_FIELDS: usize = 50;

/// A field name safe to show: only the name, never a value; odd names become `?`.
fn field_label(k: &str) -> String {
    if k.len() <= 32
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_owned()
    } else {
        "?".to_owned()
    }
}

/// Fields inside the known sections that nothing reads (typos, a hand-written `api_key`), as
/// `section.field` / `cloud[N].field` (N is 1-based). Values are never looked at.
fn unknown_fields(table: &toml::Table) -> Vec<String> {
    let mut out = Vec::new();
    for (section, known) in KNOWN_FIELDS {
        if let Some(t) = table.get(*section).and_then(toml::Value::as_table) {
            for k in t.keys().filter(|k| !known.contains(&k.as_str())) {
                out.push(format!("{section}.{}", field_label(k)));
            }
        }
    }
    if let Some(list) = table.get("cloud").and_then(toml::Value::as_array) {
        for (i, entry) in list.iter().enumerate() {
            let Some(t) = entry.as_table() else { continue };
            for k in t
                .keys()
                .filter(|k| !KNOWN_CLOUD_FIELDS.contains(&k.as_str()))
            {
                out.push(format!("cloud[{}].{}", i + 1, field_label(k)));
            }
        }
    }
    out.truncate(MAX_IGNORED_FIELDS);
    out
}

/// Parses and validates a portable configuration file.
///
/// Rejects the whole file on the first problem (`format` newer than 1, unknown `kind`, bad or
/// duplicate ids, missing or disallowed `base_url`, route / `needs_key` / `default_keys`
/// references, capabilities, name and timeout rules). Needs no current config.
pub fn parse(text: &str) -> Result<PortableFile, PortableError> {
    if text.len() > MAX_FILE_BYTES {
        return Err(PortableError::new("too_large", None));
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let raw: RawFile = toml::from_str(text).map_err(|e| {
        let line = e.span().map(|s| offset_line(text, s.start));
        let msg = e.message();
        let code = if msg.contains("unknown variant") {
            "unknown_kind"
        } else if msg.contains("missing field") {
            "missing_field"
        } else if msg.contains("invalid type") || msg.contains("invalid value") {
            "invalid_value"
        } else {
            "syntax"
        };
        PortableError::new(code, line)
    })?;
    let table: toml::Table =
        toml::from_str(text).map_err(|_| PortableError::new("syntax", None))?;
    let mut ignored: Vec<String> = table
        .keys()
        .filter(|k| !WHITELIST.contains(&k.as_str()))
        .map(|k| field_label(k))
        .collect();
    ignored.sort();
    ignored.extend(unknown_fields(&table));

    let lines = Lines::new(text);
    let err = |code, line| PortableError::new(code, line);

    // Format version: `[v2c] format`; a file with `[preset]` but no `[v2c]` is a legacy preset.
    let format = match (&raw.v2c, &raw.preset) {
        (Some(h), _) => match h.format {
            None => return Err(err("format_missing", lines.section_key("v2c", "format"))),
            Some(n) if n > i64::from(FORMAT_VERSION) => {
                return Err(err("format_too_new", lines.section_key("v2c", "format")))
            }
            Some(n) if n < 1 => {
                return Err(err("format_invalid", lines.section_key("v2c", "format")))
            }
            Some(_) => FORMAT_VERSION,
        },
        (None, Some(_)) => FORMAT_VERSION,
        (None, None) => return Err(err("format_missing", None)),
    };

    // Services.
    let mut services: Vec<CloudService> = Vec::new();
    for (n, svc) in raw.cloud.iter().enumerate() {
        let s = validate_service(svc, &services)
            .map_err(|(code, field)| err(code, lines.cloud_key(n, field)))?;
        services.push(s);
    }
    let by_id = |id: &str| services.iter().find(|s| s.id == id);
    let is_ref = |id: &str| id == LOCAL || by_id(id).is_some();

    // [preset].
    let mut title = None;
    let mut id_prefix = None;
    let mut asr_choices = Vec::new();
    let mut needs_key = Vec::new();
    let mut default_keys = DefaultKeys::default();
    let mut detect_llm_model = false;
    if let Some(p) = raw.preset {
        title = display_text(p.title);
        if let Some(prefix) = p.id_prefix {
            let prefix = prefix.trim().to_owned();
            let lower = prefix.to_lowercase();
            if prefix.is_empty() || RESERVED_BASES.iter().any(|b| lower.starts_with(b)) {
                return Err(err(
                    "preset_id_prefix",
                    lines.section_key("preset", "id_prefix"),
                ));
            }
            for (n, s) in services.iter().enumerate() {
                if !s.id.starts_with(&prefix) {
                    return Err(err("preset_id_prefix", lines.cloud_key(n, "id")));
                }
            }
            id_prefix = Some(prefix);
        }
        for choice in &p.asr_choices {
            let ok = choice == LOCAL || by_id(choice).is_some_and(has_asr);
            if !ok {
                return Err(err(
                    "preset_bad_reference",
                    lines.section_key("preset", "asr_choices"),
                ));
            }
        }
        for id in &p.needs_key {
            if by_id(id).is_none() {
                return Err(err(
                    "preset_bad_reference",
                    lines.section_key("preset", "needs_key"),
                ));
            }
        }
        let mut keys = p.default_keys;
        for (id, raw_key) in &mut keys.0 {
            if !p.needs_key.contains(id) {
                return Err(err(
                    "preset_bad_reference",
                    lines.section_key("preset", "default_keys"),
                ));
            }
            let k = raw_key.trim().to_owned();
            if k.is_empty()
                || k.chars().count() > MAX_DEFAULT_KEY_CHARS
                || k.chars().any(char::is_control)
            {
                return Err(err(
                    "invalid_key",
                    lines.section_key("preset", "default_keys"),
                ));
            }
            *raw_key = k;
        }
        asr_choices = p.asr_choices;
        needs_key = p.needs_key;
        default_keys = keys;
        detect_llm_model = p.detect_llm_model;
    }

    // [route].
    let mut route = PortableRoute::default();
    if let Some(r) = raw.route {
        for (key, value) in [
            ("asr", &r.asr),
            ("polish", &r.polish),
            ("translate", &r.translate),
        ] {
            let Some(v) = value else { continue };
            let line = lines.section_key("route", key);
            if !is_ref(v) {
                return Err(err("route_unknown_service", line));
            }
            if let Some(s) = by_id(v) {
                let ok = if key == "asr" { has_asr(s) } else { has_llm(s) };
                if !ok {
                    let code = match (key, s.kind.supports_asr()) {
                        ("asr", false) => "route_no_asr",
                        _ => "route_no_model",
                    };
                    return Err(err(code, line));
                }
            }
        }
        route = PortableRoute {
            asr: r.asr,
            polish: r.polish,
            translate: r.translate,
            cloud_fallback_to_local: r.cloud_fallback_to_local,
        };
    }

    // [vocabulary]: each entry goes through the `vocab` rules.
    let mut vocab_dropped = 0usize;
    let mut vocab_words = Vocab::default();
    if let Some(v) = raw.vocabulary {
        let mut kept = Vec::new();
        for w in &v.words {
            let w = w.split_whitespace().collect::<Vec<_>>().join(" ");
            if w.is_empty() {
                continue;
            }
            if w.starts_with('#') {
                vocab_dropped += 1;
                continue;
            }
            kept.push(w);
        }
        let checked = vocab::check(&kept.join("\n"));
        vocab_dropped += checked.too_long_lines.len() + checked.over_limit;
        vocab_words = checked.vocab;
    }

    Ok(PortableFile {
        format,
        exported_from: display_text(raw.v2c.as_ref().and_then(|h| h.exported_from.clone())),
        exported_at: display_text(raw.v2c.and_then(|h| h.exported_at)),
        title,
        id_prefix,
        asr_choices,
        needs_key,
        default_keys,
        detect_llm_model,
        route,
        services,
        vocab: vocab_words,
        vocab_dropped,
        ignored,
    })
}

// ---------------------------------------------------------------------------------------
// Merge plan
// ---------------------------------------------------------------------------------------

fn normalized_endpoint(svc: &CloudService) -> Option<String> {
    effective_base_url(svc)
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_ascii_lowercase())
}

/// Same kind and same (normalised) effective endpoint: the old key may stay.
fn same_endpoint(a: &CloudService, b: &CloudService) -> bool {
    a.kind == b.kind
        && normalized_endpoint(a).is_some()
        && normalized_endpoint(a) == normalized_endpoint(b)
}

/// What importing `file` into `current` (and `vocab`) would do. Pure; the caller shows it as a
/// preview and applies it only after the user confirms. `has_key(id)` tells whether a key is
/// stored for a current service (used for `key_missing`).
pub fn plan_merge(
    current: &Config,
    vocab: &Vocab,
    file: &PortableFile,
    has_key: &dyn Fn(&str) -> bool,
) -> MergePlan {
    let mut plan = MergePlan {
        ignored: file.ignored.clone(),
        ..MergePlan::default()
    };

    // Services, by id.
    for svc in &file.services {
        // Does a key exist after the import (before the user types one)?
        let mut key_kept = file.default_keys.contains(&svc.id);
        match current.cloud_service(&svc.id) {
            None => plan.added.push(svc.id.clone()),
            Some(existing) => {
                let endpoint_kept = same_endpoint(existing, svc);
                if normalize_service(existing) == *svc {
                    plan.unchanged.push(svc.id.clone());
                } else {
                    plan.updated.push(svc.id.clone());
                    if !endpoint_kept {
                        plan.drop_old_key.push(svc.id.clone());
                    }
                }
                key_kept |= endpoint_kept && has_key(&svc.id);
            }
        }
        if file.needs_key.contains(&svc.id) && !key_kept {
            plan.key_missing.push(svc.id.clone());
        }
    }

    // A file service must not take the id or name of a service the import leaves alone.
    let mut others: Vec<CloudService> = current
        .cloud
        .iter()
        .filter(|s| !file.services.iter().any(|f| f.id == s.id))
        .map(normalize_service)
        .collect();
    for svc in &file.services {
        if clashes(svc, &others) {
            plan.name_taken.push(svc.id.clone());
        }
        others.push(svc.clone());
    }

    // Route.
    let r = &current.route;
    for (field, new, old) in [
        ("asr", &file.route.asr, &r.asr),
        ("polish", &file.route.polish, &r.polish),
        ("translate", &file.route.translate, &r.translate),
    ] {
        if let Some(new) = new {
            if new != old {
                plan.route.push(RouteChange {
                    field,
                    old: old.clone(),
                    new: new.clone(),
                });
            }
        }
    }
    if let Some(fb) = file.route.cloud_fallback_to_local {
        if fb != r.cloud_fallback_to_local {
            plan.route.push(RouteChange {
                field: "cloud_fallback_to_local",
                old: r.cloud_fallback_to_local.to_string(),
                new: fb.to_string(),
            });
        }
    }

    // Vocabulary: union, current order first, duplicates (ignoring case) skipped.
    let mut known: HashSet<String> = vocab.words().iter().map(|w| w.to_lowercase()).collect();
    let mut total = vocab.len();
    plan.vocab_blocked = file.vocab_dropped;
    for w in file.vocab.words() {
        if !known.insert(w.to_lowercase()) {
            continue;
        }
        if total < vocab::MAX_ENTRIES {
            plan.vocab_new.push(w.clone());
            total += 1;
        } else {
            plan.vocab_blocked += 1;
        }
    }
    plan
}

// ---------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------

fn quoted(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

fn string_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| quoted(s)).collect();
    format!("[{}]", inner.join(", "))
}

/// UTC time as `YYYY-MM-DDTHH:MM:SSZ` (civil-from-days, no time-zone data needed).
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn kind_name(kind: CloudKind) -> &'static str {
    match kind {
        CloudKind::OpenAi => "open_ai",
        CloudKind::Gemini => "gemini",
        CloudKind::Claude => "claude",
        CloudKind::OpenAiCompat => "open_ai_compat",
    }
}

/// Result of [`export`]: the file text and what had to be left out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub text: String,
    /// Ids of services left out because they would not pass [`parse`] (bad id, name or timeout
    /// out of range, missing or disallowed URL, name clash, ...). Empty for a consistent config.
    pub skipped: Vec<String>,
    /// `[route]` fields written as `local` because their service was skipped, is missing, or
    /// cannot do that purpose (`asr`, `polish`, `translate`).
    pub route_reset: Vec<&'static str>,
}

/// The current settings as portable file text.
///
/// Writes `[v2c]`, `[preset] needs_key`, `[route]`, every `[[cloud]]` and `[vocabulary]` (when
/// `vocab` has entries). Never writes keys or `default_keys`. `has_key(id)` tells whether a key
/// is stored for a service; the official kinds are always listed in `needs_key`. `from` is the
/// platform label (`windows` / `linux` / `android`). Field order is fixed and each section has
/// a one-line comment before it.
///
/// The text always passes [`parse`], even for a hand-edited config: services that would fail
/// validation are left out (listed in `skipped`) and routes that would fail the capability check
/// become `local` (listed in `route_reset`).
pub fn export(
    cfg: &Config,
    vocab: Option<&Vocab>,
    has_key: &dyn Fn(&str) -> bool,
    from: &str,
) -> Exported {
    let mut services: Vec<CloudService> = Vec::new();
    let mut skipped = Vec::new();
    for svc in &cfg.cloud {
        match validate_service(svc, &services) {
            Ok(s) => services.push(s),
            Err(_) => skipped.push(svc.id.clone()),
        }
    }
    let mut route_reset = Vec::new();
    let mut out = String::new();

    out.push_str(
        "# v2c configuration file. Keys are never exported; paste them after importing.\n",
    );
    out.push_str("# Format reference: docs/config-file.md\n");
    out.push_str("[v2c]\n");
    out.push_str("# Format version (required).\n");
    out.push_str(&format!("format = {FORMAT_VERSION}\n"));
    out.push_str("# Where this file came from and when (display only).\n");
    out.push_str(&format!("exported_from = {}\n", quoted(from)));
    out.push_str(&format!("exported_at = {}\n", quoted(&utc_now())));

    let needs_key: Vec<String> = services
        .iter()
        .filter(|s| s.kind != CloudKind::OpenAiCompat || has_key(&s.id))
        .map(|s| s.id.clone())
        .collect();
    if !needs_key.is_empty() {
        out.push_str("\n# Services that need an API key; you are asked for it when importing.\n");
        out.push_str("[preset]\n");
        out.push_str(&format!("needs_key = {}\n", string_array(&needs_key)));
    }

    // A route that cannot be imported again (missing service, no such capability) is `local`.
    let mut target = |field: &'static str, id: &str, ok: fn(&CloudService) -> bool| {
        if id == LOCAL || services.iter().any(|s| s.id == id && ok(s)) {
            id.to_owned()
        } else {
            route_reset.push(field);
            LOCAL.to_owned()
        }
    };
    let (asr, polish, translate) = (
        target("asr", &cfg.route.asr, has_asr),
        target("polish", &cfg.route.polish, has_llm),
        target("translate", &cfg.route.translate, has_llm),
    );
    out.push_str("\n# Who handles each purpose: \"local\" or a service id below.\n");
    out.push_str("[route]\n");
    out.push_str(&format!("asr = {}\n", quoted(&asr)));
    out.push_str(&format!("polish = {}\n", quoted(&polish)));
    out.push_str(&format!("translate = {}\n", quoted(&translate)));
    out.push_str(&format!(
        "cloud_fallback_to_local = {}\n",
        cfg.route.cloud_fallback_to_local
    ));

    for s in &services {
        out.push_str("\n# One cloud or LAN service. Models left out use the suggested values.\n");
        out.push_str("[[cloud]]\n");
        out.push_str(&format!("id = {}\n", quoted(&s.id)));
        if let Some(name) = &s.name {
            out.push_str(&format!("name = {}\n", quoted(name)));
        }
        out.push_str(&format!("kind = {}\n", quoted(kind_name(s.kind))));
        if let Some(v) = &s.asr_model {
            out.push_str(&format!("asr_model = {}\n", quoted(v)));
        }
        if let Some(v) = &s.llm_model {
            out.push_str(&format!("llm_model = {}\n", quoted(v)));
        }
        if let Some(v) = &s.base_url {
            out.push_str(&format!("base_url = {}\n", quoted(v)));
        }
        if let Some(ms) = s.asr_timeout_ms {
            out.push_str(&format!("asr_timeout_ms = {ms}\n"));
        }
    }

    if let Some(v) = vocab.filter(|v| !v.is_empty()) {
        out.push_str("\n# Personal vocabulary, one entry per item (added to the existing list).\n");
        out.push_str("[vocabulary]\n");
        out.push_str(&format!("words = {}\n", string_array(v.words())));
    }
    Exported {
        text: out,
        skipped,
        route_reset,
    }
}

// ---------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, RouteConfig};

    const TN: &str =
        include_str!("../../../apps/desktop/src-tauri/presets/TN_CUSTOM_LLM_ADDR.toml");
    const RD: &str =
        include_str!("../../../apps/desktop/src-tauri/presets/RD_CUSTOM_LLM_ADDR.toml");
    const EXAMPLE: &str = include_str!("../../../docs/examples/v2c-config.example.toml");
    const DOC: &str = include_str!("../../../docs/config-file.md");

    /// Strings that must never appear in an error message.
    const SECRET_KEY: &str = "SECRET-KEY-9f3a";
    const SECRET_WORD: &str = "SECRET-WORD-7b2c";
    const SECRET_HOST: &str = "secret-host.invalid";

    fn fixture(name: &str) -> &'static str {
        match name {
            "valid-gemini-claude" => {
                include_str!("../../../tests/fixtures/config/valid-gemini-claude.toml")
            }
            "valid-lan" => include_str!("../../../tests/fixtures/config/valid-lan.toml"),
            "valid-vocab-only" => {
                include_str!("../../../tests/fixtures/config/valid-vocab-only.toml")
            }
            "valid-extra-sections" => {
                include_str!("../../../tests/fixtures/config/valid-extra-sections.toml")
            }
            "reject-unknown-kind" => {
                include_str!("../../../tests/fixtures/config/reject-unknown-kind.toml")
            }
            "reject-format-2" => {
                include_str!("../../../tests/fixtures/config/reject-format-2.toml")
            }
            "reject-id-space" => {
                include_str!("../../../tests/fixtures/config/reject-id-space.toml")
            }
            "reject-duplicate-id" => {
                include_str!("../../../tests/fixtures/config/reject-duplicate-id.toml")
            }
            "reject-route-unknown" => {
                include_str!("../../../tests/fixtures/config/reject-route-unknown.toml")
            }
            "reject-claude-asr" => {
                include_str!("../../../tests/fixtures/config/reject-claude-asr.toml")
            }
            "reject-compat-no-url" => {
                include_str!("../../../tests/fixtures/config/reject-compat-no-url.toml")
            }
            "reject-official-http" => {
                include_str!("../../../tests/fixtures/config/reject-official-http.toml")
            }
            "reject-default-key-unlisted" => {
                include_str!("../../../tests/fixtures/config/reject-default-key-unlisted.toml")
            }
            other => panic!("no fixture {other}"),
        }
    }

    fn code(text: &str) -> (&'static str, Option<u32>) {
        let e = parse(text).expect_err("must be rejected");
        (e.code, e.line)
    }

    fn cfg_with(services: Vec<CloudService>) -> Config {
        Config {
            cloud: services,
            ..Config::default()
        }
    }

    fn gemini(id: &str) -> CloudService {
        CloudService {
            id: id.into(),
            name: None,
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: None,
            llm_model: None,
            asr_timeout_ms: None,
        }
    }

    fn compat(id: &str, url: &str) -> CloudService {
        CloudService {
            kind: CloudKind::OpenAiCompat,
            base_url: Some(url.into()),
            ..gemini(id)
        }
    }

    // --- accepted files ------------------------------------------------------------------

    #[test]
    fn builtin_presets_parse() {
        for (text, prefix, n_asr) in [(TN, "tn-", 2), (RD, "rd-", 2)] {
            let f = parse(text).unwrap();
            assert_eq!(f.format, 1);
            assert_eq!(f.id_prefix.as_deref(), Some(prefix));
            assert_eq!(f.services.len(), 3);
            assert_eq!(f.asr_choices.len(), n_asr);
            assert_eq!(f.needs_key.len(), 2);
            assert_eq!(f.default_keys.len(), 2);
            assert!(f.detect_llm_model);
            assert_eq!(
                f.route.polish.as_deref(),
                Some(format!("{prefix}qwen").as_str())
            );
            assert_eq!(f.route.asr, None);
            let breeze = f
                .services
                .iter()
                .find(|s| s.id.ends_with("breeze-asr"))
                .unwrap();
            assert_eq!(breeze.asr_timeout_ms, Some(8000));
            assert!(f.ignored.is_empty());
        }
    }

    #[test]
    fn builtin_preset_merge_matches_what_preset_apply_writes() {
        let f = parse(TN).unwrap();
        let cur = Config::default();
        let plan = plan_merge(&cur, &Vocab::default(), &f, &|_| false);
        assert_eq!(plan.added, ["tn-tea-asr", "tn-breeze-asr", "tn-qwen"]);
        assert!(plan.updated.is_empty() && plan.unchanged.is_empty());
        // Both ASR services have a default key, so nothing is missing.
        assert!(plan.key_missing.is_empty());
        // route.asr is not in the file: unchanged; polish / translate move to the LAN LLM.
        let fields: Vec<_> = plan
            .route
            .iter()
            .map(|c| (c.field, c.new.as_str()))
            .collect();
        assert_eq!(fields, [("polish", "tn-qwen"), ("translate", "tn-qwen")]);
        // Applying twice changes nothing; the other group's services stay out of the plan.
        let mut applied = cur.clone();
        applied.cloud = f.services.clone();
        applied.route.polish = "tn-qwen".into();
        applied.route.translate = "tn-qwen".into();
        let again = plan_merge(&applied, &Vocab::default(), &f, &|_| false);
        assert!(again.is_empty());
        assert_eq!(again.unchanged.len(), 3);
        let rd = parse(RD).unwrap();
        let both = plan_merge(&applied, &Vocab::default(), &rd, &|_| false);
        assert_eq!(both.added.len(), 3);
        assert!(both.drop_old_key.is_empty());
    }

    #[test]
    fn valid_fixtures_parse() {
        for name in [
            "valid-gemini-claude",
            "valid-lan",
            "valid-vocab-only",
            "valid-extra-sections",
        ] {
            parse(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let f = parse(fixture("valid-gemini-claude")).unwrap();
        assert_eq!(f.route.asr.as_deref(), Some("my-gemini"));
        assert_eq!(f.route.translate.as_deref(), Some("my-claude"));
        assert_eq!(f.needs_key, ["my-gemini", "my-claude"]);
        let lan = parse(fixture("valid-lan")).unwrap();
        assert_eq!(lan.default_keys.get("lan-asr"), Some("shared-key"));
        let v = parse(fixture("valid-vocab-only")).unwrap();
        assert!(v.services.is_empty());
        assert_eq!(v.vocab.words(), ["Breeze-ASR", "台積電", "v2c"]);
    }

    #[test]
    fn example_and_doc_snippets_parse() {
        parse(EXAMPLE).unwrap();
        let mut n = 0;
        let doc = DOC.replace("\r\n", "\n");
        let mut rest = doc.as_str();
        while let Some(i) = rest.find("```toml\n") {
            let body = &rest[i + 8..];
            let end = body.find("```").expect("unclosed code block");
            parse(&body[..end]).unwrap_or_else(|e| panic!("doc example {n}: {e}"));
            n += 1;
            rest = &body[end + 3..];
        }
        assert!(n >= 3, "docs must hold the 3 full examples, found {n}");
    }

    #[test]
    fn extra_sections_are_ignored_and_listed() {
        let f = parse(fixture("valid-extra-sections")).unwrap();
        assert_eq!(f.ignored, ["audio", "hotkey"]);
        let plan = plan_merge(&Config::default(), &Vocab::default(), &f, &|_| false);
        assert_eq!(plan.ignored, ["audio", "hotkey"]);
    }

    #[test]
    fn odd_top_level_section_names_are_not_echoed() {
        // A top-level key before any table header is a root key, not a field of `[v2c]`.
        let f = parse("\"sk-SECRET value\" = 1\n[v2c]\nformat = 1\n").unwrap();
        assert_eq!(f.ignored, ["?"]);
        assert!(!format!("{f:?}").contains("SECRET"));
    }

    #[test]
    fn legacy_preset_without_v2c_is_format_1_but_nothing_at_all_is_not() {
        assert_eq!(parse("[preset]\nneeds_key = []\n").unwrap().format, 1);
        assert_eq!(code("[route]\nasr = \"local\"\n").0, "format_missing");
        assert_eq!(code("[v2c]\nexported_from = \"x\"\n").0, "format_missing");
        assert_eq!(code("[v2c]\nformat = 0\n").0, "format_invalid");
    }

    // --- one fixture per rejection rule -----------------------------------------------------

    #[test]
    fn each_rule_has_a_rejecting_fixture() {
        let cases = [
            ("reject-unknown-kind", "unknown_kind"),
            ("reject-format-2", "format_too_new"),
            ("reject-id-space", "id_invalid"),
            ("reject-duplicate-id", "preset_duplicate_id"),
            ("reject-route-unknown", "route_unknown_service"),
            ("reject-claude-asr", "route_no_asr"),
            ("reject-compat-no-url", "base_url_required"),
            ("reject-official-http", "invalid_base_url"),
            ("reject-default-key-unlisted", "preset_bad_reference"),
        ];
        for (name, want) in cases {
            let (got, line) = code(fixture(name));
            assert_eq!(got, want, "{name}");
            assert!(line.is_some(), "{name} should name a line");
        }
    }

    #[test]
    fn rejection_lines_point_at_the_offending_field() {
        assert_eq!(
            code(fixture("reject-format-2")),
            ("format_too_new", Some(2))
        );
        let (_, line) = code(fixture("reject-duplicate-id"));
        let text = fixture("reject-duplicate-id");
        let l = text.lines().nth(line.unwrap() as usize - 1).unwrap();
        assert!(l.trim_start().starts_with("id"), "{l}");
        let (_, line) = code(fixture("reject-unknown-kind"));
        let l = fixture("reject-unknown-kind")
            .lines()
            .nth(line.unwrap() as usize - 1)
            .unwrap();
        assert!(l.contains("kind"), "{l}");
    }

    #[test]
    fn more_rejections() {
        let svc = |extra: &str| {
            format!("[v2c]\nformat = 1\n[[cloud]]\nid = \"a\"\nkind = \"open_ai_compat\"\nbase_url = \"http://h/v1\"\n{extra}")
        };
        // Timeout range.
        assert_eq!(code(&svc("asr_timeout_ms = 999\n")).0, "asr_timeout_range");
        assert_eq!(
            code(&svc("asr_timeout_ms = 60001\n")).0,
            "asr_timeout_range"
        );
        parse(&svc("asr_timeout_ms = 60000\n")).unwrap();
        // Names.
        assert_eq!(
            code(&svc(&format!("name = \"{}\"\n", "x".repeat(33)))).0,
            "name_too_long"
        );
        assert_eq!(code(&svc("name = \"LOCAL\"\n")).0, "name_reserved");
        assert_eq!(code(&svc("name = \"a\\u0007b\"\n")).0, "invalid_name");
        // Ids.
        for bad in ["", " a", "a ", "a b", "a/b", "a.b", "é"] {
            let t = format!("[v2c]\nformat = 1\n[[cloud]]\nid = \"{bad}\"\nkind = \"gemini\"\n");
            assert_eq!(code(&t).0, "id_invalid", "id {bad:?}");
        }
        let t = "[v2c]\nformat = 1\n[[cloud]]\nid = \"Local\"\nkind = \"gemini\"\n";
        assert_eq!(code(t).0, "id_reserved");
        // Names clash with another id or name, ignoring case.
        let two = "[v2c]\nformat = 1\n[[cloud]]\nid = \"a\"\nkind = \"open_ai_compat\"\nbase_url = \"http://h/v1\"\nname = \"B\"\n[[cloud]]\nid = \"b\"\nkind = \"gemini\"\n";
        assert_eq!(code(two).0, "name_taken");
        // Non-compat names are dropped, not an error.
        let named = "[v2c]\nformat = 1\n[[cloud]]\nid = \"g\"\nkind = \"gemini\"\nname = \"x\"\n";
        assert_eq!(parse(named).unwrap().services[0].name, None);
        // Unlisted needs_key, bad choices, bad default keys.
        assert_eq!(
            code("[v2c]\nformat = 1\n[preset]\nneeds_key = [\"zz\"]\n").0,
            "preset_bad_reference"
        );
        assert_eq!(
            code("[v2c]\nformat = 1\n[preset]\nasr_choices = [\"zz\"]\n").0,
            "preset_bad_reference"
        );
        let dk = |v: &str| {
            format!("[v2c]\nformat = 1\n[preset]\nneeds_key = [\"g\"]\ndefault_keys = {{ g = {v} }}\n[[cloud]]\nid = \"g\"\nkind = \"gemini\"\n")
        };
        assert_eq!(code(&dk("\"  \"")).0, "invalid_key");
        assert_eq!(
            code(&dk(&format!("\"{}\"", "k".repeat(257)))).0,
            "invalid_key"
        );
        assert_eq!(code(&dk("\"a\\u0007\"")).0, "invalid_key");
        assert_eq!(
            parse(&dk("\"  ok  \"")).unwrap().default_keys.get("g"),
            Some("ok")
        );
        // id_prefix.
        let pre = |p: &str, id: &str| {
            format!("[preset]\nid_prefix = \"{p}\"\n[[cloud]]\nid = \"{id}\"\nkind = \"gemini\"\n")
        };
        assert_eq!(code(&pre("gemini-", "gemini-x")).0, "preset_id_prefix");
        assert_eq!(code(&pre("", "x")).0, "preset_id_prefix");
        assert_eq!(code(&pre("tn-", "rd-x")).0, "preset_id_prefix");
        parse(&pre("tn-", "tn-x")).unwrap();
        // Route capability: a compat endpoint with no text model cannot polish.
        let r = "[v2c]\nformat = 1\n[route]\npolish = \"a\"\n[[cloud]]\nid = \"a\"\nkind = \"open_ai_compat\"\nbase_url = \"http://h/v1\"\nasr_model = \"m\"\n";
        assert_eq!(code(r).0, "route_no_model");
        // Official https override is fine, loopback http too.
        parse("[v2c]\nformat = 1\n[[cloud]]\nid = \"g\"\nkind = \"gemini\"\nbase_url = \"https://proxy/v1beta/\"\n").unwrap();
        parse("[v2c]\nformat = 1\n[[cloud]]\nid = \"g\"\nkind = \"claude\"\nbase_url = \"http://localhost:9/v1\"\n").unwrap();
        // Syntax / type errors carry a line.
        assert_eq!(code("[v2c]\nformat = 1\n[route\n"), ("syntax", Some(3)));
        assert_eq!(code("[v2c]\nformat = \"1\"\n").0, "invalid_value");
        assert_eq!(
            code("[v2c]\nformat = 1\n[[cloud]]\nkind = \"gemini\"\n").0,
            "missing_field"
        );
        // Size cap.
        assert_eq!(code(&"#".repeat(MAX_FILE_BYTES + 1)), ("too_large", None));
    }

    #[test]
    fn errors_never_contain_file_content() {
        let secret_files = [
            format!("[v2c]\nformat = 1\n[preset]\nneeds_key = [\"g\"]\ndefault_keys = {{ g = \"{SECRET_KEY}\\u0007\" }}\n[[cloud]]\nid = \"g\"\nkind = \"gemini\"\n[vocabulary]\nwords = [\"{SECRET_WORD}\"]\n"),
            format!("[v2c]\nformat = 1\n[[cloud]]\nid = \"{SECRET_KEY} x\"\nkind = \"gemini\"\n"),
            format!("[v2c]\nformat = 1\n[[cloud]]\nid = \"a\"\nkind = \"{SECRET_KEY}\"\n"),
            format!("[v2c]\nformat = 1\n[[cloud]]\nid = \"a\"\nkind = \"gemini\"\nbase_url = \"ftp://{SECRET_HOST}/{SECRET_KEY}\"\n"),
            format!("[v2c]\nformat = 1\n[route]\nasr = \"{SECRET_KEY}\"\n[vocabulary]\nwords = [\"{SECRET_WORD}\"]\n"),
            format!("[v2c]\nformat = 2\n[vocabulary]\nwords = [\"{SECRET_WORD}\"]\n"),
            format!("[v2c]\nformat = 1\n[vocabulary]\nwords = [\"{SECRET_WORD}\", 5]\n"),
            format!("[v2c\nformat = 1\nx = \"{SECRET_KEY}\"\n"),
            format!("[v2c]\nformat = 1\n[[cloud]]\nid = \"a\"\nkind = \"open_ai_compat\"\nbase_url = \"http://{SECRET_HOST}/v1\"\nname = \"{SECRET_WORD}{SECRET_WORD}{SECRET_WORD}\"\n"),
        ];
        for text in &secret_files {
            let e = parse(text).expect_err("rejected");
            for shown in [format!("{e}"), format!("{e:?}"), e.message().to_owned()] {
                for s in [SECRET_KEY, SECRET_WORD, SECRET_HOST] {
                    assert!(!shown.contains(s), "{shown}");
                }
            }
        }
        // Accepted files: Debug of the parsed file and of the plan hide keys, URLs and words.
        let ok = format!("[v2c]\nformat = 1\n[preset]\nneeds_key = [\"g\"]\ndefault_keys = {{ g = \"{SECRET_KEY}\" }}\n[[cloud]]\nid = \"g\"\nkind = \"open_ai_compat\"\nbase_url = \"http://{SECRET_HOST}/v1\"\n[vocabulary]\nwords = [\"{SECRET_WORD}\"]\n");
        let f = parse(&ok).unwrap();
        let plan = plan_merge(&Config::default(), &Vocab::default(), &f, &|_| false);
        for shown in [
            format!("{f:?}"),
            format!("{plan:?}"),
            format!("{:?}", f.default_keys),
        ] {
            for s in [SECRET_KEY, SECRET_WORD, SECRET_HOST] {
                assert!(!shown.contains(s), "{shown}");
            }
        }
    }

    // --- merge plan ------------------------------------------------------------------------

    #[test]
    fn plan_classifies_added_updated_unchanged_and_drops_keys_on_new_host() {
        let cur = cfg_with(vec![
            compat("same", "http://h/v1"),
            compat("moved", "http://old/v1"),
            compat("edited", "http://h/v1"),
            gemini("keep-me"),
        ]);
        let file = parse(
            "[v2c]\nformat = 1\n[preset]\nneeds_key = [\"moved\", \"fresh\"]\n\
             [[cloud]]\nid = \"same\"\nkind = \"open_ai_compat\"\nbase_url = \"http://h/v1/\"\n\
             [[cloud]]\nid = \"moved\"\nkind = \"open_ai_compat\"\nbase_url = \"http://new/v1\"\n\
             [[cloud]]\nid = \"edited\"\nkind = \"open_ai_compat\"\nbase_url = \"http://H/v1\"\nllm_model = \"m\"\n\
             [[cloud]]\nid = \"fresh\"\nkind = \"gemini\"\n",
        )
        .unwrap();
        let plan = plan_merge(&cur, &Vocab::default(), &file, &|_| false);
        assert_eq!(plan.added, ["fresh"]);
        assert_eq!(plan.updated, ["moved", "edited"]);
        assert_eq!(plan.unchanged, ["same"]);
        // Only the changed host loses its key; a model edit on the same host keeps it.
        assert_eq!(plan.drop_old_key, ["moved"]);
        assert_eq!(plan.key_missing, ["moved", "fresh"]);
        assert!(!plan.updated.contains(&"keep-me".to_owned()));
        assert!(plan.is_applicable() && !plan.is_empty());
    }

    #[test]
    fn default_key_means_not_missing() {
        let file = parse(fixture("valid-lan")).unwrap();
        let plan = plan_merge(&Config::default(), &Vocab::default(), &file, &|_| false);
        assert!(plan.key_missing.is_empty());
        assert!(file.default_keys.contains("lan-asr"));
    }

    #[test]
    fn route_changes_list_only_real_differences() {
        let mut cur = cfg_with(vec![gemini("g")]);
        cur.route = RouteConfig {
            asr: "g".into(),
            ..RouteConfig::default()
        };
        let file = parse(
            "[v2c]\nformat = 1\n[route]\nasr = \"g\"\ntranslate = \"g\"\ncloud_fallback_to_local = false\n[[cloud]]\nid = \"g\"\nkind = \"gemini\"\n",
        )
        .unwrap();
        let plan = plan_merge(&cur, &Vocab::default(), &file, &|_| false);
        let got: Vec<_> = plan
            .route
            .iter()
            .map(|c| (c.field, c.old.as_str(), c.new.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("translate", "local", "g"),
                ("cloud_fallback_to_local", "true", "false")
            ]
        );
    }

    #[test]
    fn name_clash_with_existing_service_is_a_problem() {
        let mut other = compat("other", "http://h/v1");
        other.name = Some("Mine".into());
        let cur = cfg_with(vec![other]);
        let file = parse("[v2c]\nformat = 1\n[[cloud]]\nid = \"new\"\nkind = \"open_ai_compat\"\nbase_url = \"http://x/v1\"\nname = \"mine\"\n").unwrap();
        let plan = plan_merge(&cur, &Vocab::default(), &file, &|_| false);
        assert!(!plan.is_applicable());
        assert_eq!(plan.name_taken, ["new"]);
        // An id that equals an outside service's name is refused too (see cloud.rs new_id).
        let file =
            parse("[v2c]\nformat = 1\n[[cloud]]\nid = \"MINE\"\nkind = \"gemini\"\n").unwrap();
        let plan = plan_merge(&cur, &Vocab::default(), &file, &|_| false);
        assert_eq!(plan.name_taken, ["MINE"]);
        // The service that owns the name may be updated by the file itself.
        let file = parse("[v2c]\nformat = 1\n[[cloud]]\nid = \"other\"\nkind = \"open_ai_compat\"\nbase_url = \"http://h/v1\"\nname = \"Mine\"\n").unwrap();
        assert!(plan_merge(&cur, &Vocab::default(), &file, &|_| false).is_applicable());
    }

    #[test]
    fn vocabulary_merges_as_union_with_limit() {
        let cur_vocab = Vocab::from_words(&["Alpha", "beta"]);
        let file = parse("[v2c]\nformat = 1\n[vocabulary]\nwords = [\"ALPHA\", \"Gamma\", \"  Del  ta \", \"gamma\", \"\", \"# x\"]\n").unwrap();
        let plan = plan_merge(&Config::default(), &cur_vocab, &file, &|_| false);
        assert_eq!(plan.vocab_new, ["Gamma", "Del ta"]);
        assert_eq!(plan.vocab_blocked, 1); // the comment-like entry
                                           // Limit: current list nearly full.
        let words: Vec<String> = (0..vocab::MAX_ENTRIES - 1)
            .map(|i| format!("w{i}"))
            .collect();
        let full = Vocab::from_words(&words);
        let file =
            parse("[v2c]\nformat = 1\n[vocabulary]\nwords = [\"n1\", \"n2\", \"n3\"]\n").unwrap();
        let plan = plan_merge(&Config::default(), &full, &file, &|_| false);
        assert_eq!(plan.vocab_new, ["n1"]);
        assert_eq!(plan.vocab_blocked, 2);
        // Too long entries are dropped and counted.
        let long = "x".repeat(vocab::MAX_ENTRY_CHARS + 1);
        let file = parse(&format!(
            "[v2c]\nformat = 1\n[vocabulary]\nwords = [\"{long}\", \"ok\"]\n"
        ))
        .unwrap();
        assert_eq!(file.vocab.words(), ["ok"]);
        assert_eq!(file.vocab_dropped, 1);
    }

    // --- export ----------------------------------------------------------------------------

    fn rich_config() -> Config {
        let mut lan = compat("lan", "http://10.0.0.5:8002/v1");
        lan.name = Some("My \"LAN\" ASR".into());
        lan.asr_model = Some("breeze-asr-25".into());
        lan.asr_timeout_ms = Some(8000);
        let mut g = gemini("my-gemini");
        g.asr_model = Some("gemini-3.6-flash".into());
        g.llm_model = Some("gemini-3.6-flash".into());
        let mut c = cfg_with(vec![
            g,
            lan,
            CloudService {
                kind: CloudKind::Claude,
                llm_model: Some("claude-haiku-5-5".into()),
                ..gemini("my-claude")
            },
        ]);
        c.route = RouteConfig {
            asr: "lan".into(),
            polish: "my-claude".into(),
            translate: "my-gemini".into(),
            cloud_fallback_to_local: false,
        };
        c
    }

    #[test]
    fn export_round_trips_to_all_unchanged() {
        let cfg = rich_config();
        let vocab = Vocab::from_words(&["Breeze-ASR", "台積電", "He said \"hi\""]);
        let text = export(&cfg, Some(&vocab), &|id| id == "lan", "windows").text;
        let file = parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(file.format, 1);
        assert_eq!(file.exported_from.as_deref(), Some("windows"));
        assert!(file.exported_at.is_some());
        assert_eq!(file.needs_key, ["my-gemini", "lan", "my-claude"]);
        assert!(file.default_keys.is_empty());
        let plan = plan_merge(&cfg, &vocab, &file, &|_| false);
        assert!(plan.is_empty(), "{plan:?}");
        assert_eq!(plan.unchanged, ["my-gemini", "lan", "my-claude"]);
        assert!(plan.vocab_new.is_empty() && plan.vocab_blocked == 0 && plan.name_taken.is_empty());
        // Importing into an empty config reproduces it.
        let fresh = plan_merge(&Config::default(), &Vocab::default(), &file, &|_| false);
        assert_eq!(fresh.added.len(), 3);
        assert_eq!(fresh.vocab_new.len(), 3);
        assert_eq!(file.services, cfg.cloud);
    }

    #[test]
    fn export_never_writes_keys_and_lists_needs_key_by_rule() {
        let cfg = rich_config();
        let text = export(&cfg, None, &|_| true, "linux").text;
        assert!(!text.contains("default_keys"));
        assert!(!text.contains("[vocabulary]"));
        // No stored key for the compat service: only the official kinds need one.
        let text = export(&cfg, None, &|_| false, "linux").text;
        let f = parse(&text).unwrap();
        assert_eq!(f.needs_key, ["my-gemini", "my-claude"]);
        // Nothing needs a key: no [preset] section at all.
        let none = export(
            &cfg_with(vec![compat("c", "http://h/v1")]),
            None,
            &|_| false,
            "android",
        )
        .text;
        assert!(!none.contains("[preset]"));
        parse(&none).unwrap();
    }

    #[test]
    fn export_layout_is_fixed_with_a_comment_before_each_section() {
        let cfg = rich_config();
        let vocab = Vocab::from_words(&["x"]);
        let text = export(&cfg, Some(&vocab), &|_| false, "windows").text;
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.starts_with('[') && *l != "[v2c]" {
                assert!(lines[i - 1].starts_with('#'), "no comment before {l}");
            }
        }
        let order: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| l.starts_with('['))
            .collect();
        assert_eq!(
            order,
            [
                "[v2c]",
                "[preset]",
                "[route]",
                "[[cloud]]",
                "[[cloud]]",
                "[[cloud]]",
                "[vocabulary]"
            ]
        );
        // Field order inside [[cloud]] is fixed.
        let lan = text.split("[[cloud]]").nth(2).unwrap();
        let keys: Vec<&str> = lan
            .lines()
            .filter_map(|l| l.split(" = ").next())
            .filter(|k| !k.starts_with('#') && !k.is_empty())
            .collect();
        assert_eq!(
            keys,
            [
                "id",
                "name",
                "kind",
                "asr_model",
                "base_url",
                "asr_timeout_ms"
            ]
        );
    }

    #[test]
    fn export_writes_dangling_route_as_local() {
        let mut cfg = cfg_with(vec![gemini("g")]);
        cfg.route.asr = "gone".into();
        let f = parse(&export(&cfg, None, &|_| false, "windows").text).unwrap();
        assert_eq!(f.route.asr.as_deref(), Some("local"));
    }

    #[test]
    fn utc_now_has_rfc3339_shape() {
        let t = utc_now();
        assert_eq!(t.len(), 20);
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T');
    }

    // --- review fixes ----------------------------------------------------------------------

    #[test]
    fn crlf_input_parses_like_lf_with_the_same_lines() {
        for name in ["valid-gemini-claude", "valid-lan", "valid-vocab-only"] {
            let lf = fixture(name).replace("\r\n", "\n");
            let crlf = lf.replace('\n', "\r\n");
            assert_eq!(parse(&crlf).unwrap(), parse(&lf).unwrap(), "{name}");
        }
        for name in [
            "reject-unknown-kind",
            "reject-duplicate-id",
            "reject-id-space",
        ] {
            let lf = fixture(name).replace("\r\n", "\n");
            let crlf = lf.replace('\n', "\r\n");
            assert_eq!(code(&crlf), code(&lf), "{name}");
        }
        parse(&EXAMPLE.replace("\r\n", "\n").replace('\n', "\r\n")).unwrap();
    }

    #[test]
    fn key_missing_covers_every_needs_key_service_without_a_surviving_key() {
        let text = "[v2c]\nformat = 1\n[preset]\nneeds_key = [\"kept\", \"nokey\", \"moved\", \"dflt\", \"new\"]\ndefault_keys = { dflt = \"k\" }\n\
            [[cloud]]\nid = \"kept\"\nkind = \"gemini\"\n\
            [[cloud]]\nid = \"nokey\"\nkind = \"claude\"\n\
            [[cloud]]\nid = \"moved\"\nkind = \"open_ai_compat\"\nbase_url = \"http://new/v1\"\n\
            [[cloud]]\nid = \"dflt\"\nkind = \"gemini\"\n\
            [[cloud]]\nid = \"new\"\nkind = \"open_ai\"\n";
        let file = parse(text).unwrap();
        let cur = cfg_with(vec![
            gemini("kept"),
            CloudService {
                kind: CloudKind::Claude,
                ..gemini("nokey")
            },
            compat("moved", "http://old/v1"),
        ]);
        let plan = plan_merge(&cur, &Vocab::default(), &file, &|id| {
            id == "kept" || id == "moved"
        });
        // kept: stored key survives. nokey: none stored. moved: old key dropped. dflt: default.
        assert_eq!(plan.key_missing, ["nokey", "moved", "new"]);
    }

    #[test]
    fn unknown_fields_are_listed_by_name_and_never_echoed() {
        let text = format!(
            "[v2c]\nformat = 1\nnote = \"{SECRET_WORD}\"\n[route]\nasr_x = \"local\"\n\
             [[cloud]]\nid = \"a\"\nkind = \"gemini\"\n\
             [[cloud]]\nid = \"b\"\nkind = \"gemini\"\nasr_modle = \"m\"\napi_key = \"{SECRET_KEY}\"\n\
             [hotkey]\ndictate = \"x\"\n"
        );
        let f = parse(&text).unwrap();
        assert_eq!(
            f.ignored,
            [
                "hotkey",
                "v2c.note",
                "route.asr_x",
                "cloud[2].api_key",
                "cloud[2].asr_modle"
            ]
        );
        let plan = plan_merge(&Config::default(), &Vocab::default(), &f, &|_| false);
        let shown = format!("{f:?}{plan:?}");
        assert!(!shown.contains(SECRET_KEY) && !shown.contains(SECRET_WORD));
        // The key is not stored anywhere in the parsed services.
        assert!(!format!("{:?}", f.services).contains(SECRET_KEY));
        // Odd field names are not echoed.
        let odd = format!("[v2c]\nformat = 1\n[route]\n\"{SECRET_KEY} with space\" = 1\n");
        assert_eq!(parse(&odd).unwrap().ignored, ["route.?"]);
    }

    #[test]
    fn export_of_an_inconsistent_config_still_parses() {
        let mut long = compat("long-name", "http://h/v1");
        long.name = Some("x".repeat(NAME_MAX_CHARS + 1));
        let mut slow = compat("slow", "http://h/v1");
        slow.asr_timeout_ms = Some(5);
        let mut bad_id = gemini("bad id é");
        bad_id.asr_model = Some("m".into());
        let mut no_url = compat("no-url", "");
        no_url.base_url = None;
        let mut dup = gemini("dup");
        dup.llm_model = Some("m".into());
        let mut named = compat("named", "http://h/v1");
        named.name = Some("Dup".into()); // clashes with the id `dup`
        let mut lan = compat("lan-asr", "http://h/v1");
        lan.asr_model = Some("m".into()); // no text model
        let mut cfg = cfg_with(vec![
            long,
            slow,
            bad_id,
            no_url,
            dup.clone(),
            dup,
            named,
            lan,
            CloudService {
                kind: CloudKind::Claude,
                ..gemini("claude")
            },
        ]);
        cfg.route = RouteConfig {
            asr: "claude".into(),     // Claude cannot do speech recognition
            polish: "lan-asr".into(), // no text model
            translate: "slow".into(), // skipped service
            cloud_fallback_to_local: true,
        };
        let out = export(&cfg, None, &|_| false, "windows");
        let file = parse(&out.text).unwrap_or_else(|e| panic!("{e}\n{}", out.text));
        assert_eq!(
            out.skipped,
            ["long-name", "slow", "bad id é", "no-url", "dup", "named"].map(String::from)
        );
        assert_eq!(out.route_reset, ["asr", "polish", "translate"]);
        assert_eq!(file.route.asr.as_deref(), Some("local"));
        assert_eq!(
            file.services
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["dup", "lan-asr", "claude"]
        );
        // A consistent config skips nothing.
        let ok = export(&rich_config(), None, &|_| false, "linux");
        assert!(ok.skipped.is_empty() && ok.route_reset.is_empty());
    }
}
