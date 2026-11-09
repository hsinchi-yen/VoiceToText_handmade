//! Local model files (T1.11, `tasks/plan.md` section 5.1): manifest, status, resumable
//! download with SHA-256 verification, SenseVoice archive extraction, and Breeze import.
//!
//! - Manifest: `models.json` bundled into the binary, overridable by
//!   `%LOCALAPPDATA%\v2c\models.json` (an invalid override is ignored and reported).
//! - Downloads go to `<models>\<file>.part` and resume with an HTTP `Range` request. The finished
//!   file is hashed; a mismatch deletes it (`hash_mismatch`). Only then is it moved into place.
//! - HTTP: a client separate from the cloud client (`CloudHttp`, which never follows
//!   redirects). GitHub and Hugging Face serve assets through redirects to CDN hosts, so this
//!   one follows up to [`MAX_REDIRECTS`] redirects, **only to https**. It never carries
//!   credentials: no API keys, no cookies, no `Referer`. Plain http is refused for the first
//!   URL as well.
//! - Breeze import: a `.bin` produced by `tools/convert-breeze` is checked for the whisper.cpp
//!   GGML magic and its quantisation (q5_0 / q8_0, read from the header), then copied in.
//!
//! Progress goes to the main window only as [`MODEL_PROGRESS_EVENT`]. Logs carry model ids,
//! byte counts, and error codes. Every command refuses callers other than the main window.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tracing::{info, warn};
use v2c_core::config::BreezeQuant;

use crate::app_state::AppState;
use crate::paths::{self, Paths};

/// Main-window event with a [`Progress`] payload.
pub const MODEL_PROGRESS_EVENT: &str = "v2c:model-progress";
pub const MAX_REDIRECTS: usize = 10;
const MAIN_WINDOW: &str = "main";
const BUILTIN_MANIFEST: &str = include_str!("../../models.json");
const OVERRIDE_FILE: &str = "models.json";
/// Download folder for the SenseVoice archive, inside the models folder.
const SENSEVOICE_DL_DIR: &str = "dl-sensevoice";
const SENSEVOICE_ARCHIVE: &str = "sv.tar.bz2";
/// The files taken from the SenseVoice archive (everything else is skipped).
const SENSEVOICE_FILES: [&str; 2] = [
    v2c_asr_local::sensevoice::MODEL_INT8_FILE,
    v2c_asr_local::sensevoice::TOKENS_FILE,
];

// ---- manifest ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    /// sherpa-onnx `.tar.bz2` release archive; `model.int8.onnx` + `tokens.txt` are extracted.
    SensevoiceArchive,
    /// whisper.cpp GGML `.bin` for Breeze-ASR-25.
    BreezeGgml,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub kind: ModelKind,
    #[serde(default)]
    pub quant: Option<BreezeQuant>,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    /// Not hosted yet: shown, but not downloadable.
    #[serde(default)]
    pub placeholder: bool,
}

impl ModelEntry {
    /// Downloadable: hosted (not a placeholder) and pinned to a SHA-256.
    pub fn downloadable(&self) -> bool {
        !self.placeholder && self.sha256.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub models: Vec<ModelEntry>,
}

impl Manifest {
    /// Parses and validates a manifest. Error codes: `manifest_invalid`.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let m: Manifest = serde_json::from_str(text).map_err(|_| "manifest_invalid")?;
        if m.version != 1 {
            return Err("manifest_invalid");
        }
        let mut seen = std::collections::HashSet::new();
        for e in &m.models {
            let id_ok = !e.id.is_empty()
                && e.id.len() <= 64
                && e.id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            let sha_ok = e
                .sha256
                .as_deref()
                .is_none_or(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()));
            let url_ok = reqwest::Url::parse(&e.url).is_ok_and(|u| u.scheme() == "https");
            let quant_ok = (e.kind == ModelKind::BreezeGgml) == e.quant.is_some();
            // A hosted file needs its size (progress, resume, and a check before hashing).
            let size_ok = e.placeholder || e.size.is_some_and(|s| s > 0);
            if !id_ok || !sha_ok || !url_ok || !quant_ok || !size_ok || !seen.insert(e.id.clone()) {
                return Err("manifest_invalid");
            }
        }
        Ok(m)
    }

    pub fn entry(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|e| e.id == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestSource {
    Builtin,
    Override,
    /// The override file exists but is invalid; the built-in manifest is used.
    OverrideInvalid,
}

/// The override manifest if valid, else the built-in one.
pub fn load_manifest(paths: &Paths) -> (Manifest, ManifestSource) {
    let builtin = || Manifest::parse(BUILTIN_MANIFEST).expect("bundled models.json is valid");
    match std::fs::read_to_string(paths.local_data_dir.join(OVERRIDE_FILE)) {
        Ok(text) => match Manifest::parse(&text) {
            Ok(m) => (m, ManifestSource::Override),
            Err(_) => {
                warn!("models.json override is invalid; using the built-in manifest");
                (builtin(), ManifestSource::OverrideInvalid)
            }
        },
        Err(_) => (builtin(), ManifestSource::Builtin),
    }
}

// ---- installed models ----------------------------------------------------------------------

pub fn breeze_present(paths: &Paths, quant: BreezeQuant) -> bool {
    paths::find_model(paths::breeze_model_file(quant), &paths.model_search_dirs()).is_some()
}

/// A SenseVoice directory with both model files.
pub fn sensevoice_present(paths: &Paths) -> bool {
    paths
        .model_search_dirs()
        .iter()
        .map(|d| d.join(paths::SENSEVOICE_DIR))
        .any(|d| v2c_asr_local::sensevoice::model_files(&d).is_ok())
}

/// Any local speech recognition model is installed (else onboarding is shown).
pub fn any_asr_model(paths: &Paths) -> bool {
    sensevoice_present(paths)
        || breeze_present(paths, BreezeQuant::Q5_0)
        || breeze_present(paths, BreezeQuant::Q8_0)
}

fn installed(paths: &Paths, e: &ModelEntry) -> bool {
    match (e.kind, e.quant) {
        (ModelKind::SensevoiceArchive, _) => sensevoice_present(paths),
        (ModelKind::BreezeGgml, Some(q)) => breeze_present(paths, q),
        (ModelKind::BreezeGgml, None) => false,
    }
}

/// Where a download of `e` is written while incomplete.
fn part_path(models: &Path, e: &ModelEntry) -> PathBuf {
    match (e.kind, e.quant) {
        (ModelKind::BreezeGgml, Some(q)) => {
            models.join(format!("{}.part", paths::breeze_model_file(q)))
        }
        _ => models
            .join(SENSEVOICE_DL_DIR)
            .join(format!("{SENSEVOICE_ARCHIVE}.part")),
    }
}

// ---- HTTP download ---------------------------------------------------------------------------

/// Redirect rule: at most [`MAX_REDIRECTS`] hops, and only to https unless `allow_http`
/// (tests against a local mock server).
pub fn redirect_allowed(url: &reqwest::Url, previous_hops: usize, allow_http: bool) -> bool {
    previous_hops < MAX_REDIRECTS && (url.scheme() == "https" || allow_http)
}

/// Download client for public model files (never sends credentials).
pub struct Downloader {
    client: reqwest::Client,
    allow_http: bool,
}

/// What a finished download must be.
#[derive(Debug, Clone)]
pub struct Expect {
    pub sha256: String,
    pub size: Option<u64>,
}

impl Downloader {
    pub fn new() -> Result<Self, &'static str> {
        Self::build(false)
    }

    #[cfg(test)]
    fn for_local_tests() -> Self {
        Self::build(true).unwrap()
    }

    fn build(allow_http: bool) -> Result<Self, &'static str> {
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if redirect_allowed(attempt.url(), attempt.previous().len(), allow_http) {
                attempt.follow()
            } else {
                attempt.error("redirect refused")
            }
        });
        let client = reqwest::Client::builder()
            .redirect(policy)
            .referer(false)
            .https_only(!allow_http)
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .user_agent(concat!("v2c/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| "download_client_failed")?;
        Ok(Self { client, allow_http })
    }

    /// Downloads `url` into `part` (resuming what is there), then checks the SHA-256. On
    /// success `part` holds the verified file. `progress(done, total)` is called per chunk.
    /// Cancelling keeps `part` for a later resume.
    pub async fn download(
        &self,
        url: &str,
        part: &Path,
        expect: &Expect,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), &'static str> {
        let parsed = reqwest::Url::parse(url).map_err(|_| "download_url_invalid")?;
        if parsed.scheme() != "https" && !self.allow_http {
            return Err("insecure_url");
        }
        if let Some(dir) = part.parent() {
            std::fs::create_dir_all(dir).map_err(|_| "storage")?;
        }
        let mut existing = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        if expect.size.is_some_and(|s| existing > s) {
            existing = 0;
        }
        let complete = expect.size.is_some_and(|s| existing == s && s > 0);
        if !complete {
            self.fetch(parsed, part, existing, expect.size, progress, cancel)
                .await?;
        }
        verify_file(part, expect).await
    }

    async fn fetch(
        &self,
        url: reqwest::Url,
        part: &Path,
        mut existing: u64,
        size: Option<u64>,
        progress: &(dyn Fn(u64, Option<u64>) + Send + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), &'static str> {
        // At most one retry: a server that answers a range request oddly gets a full GET.
        for _ in 0..2 {
            let mut req = self.client.get(url.clone());
            if existing > 0 {
                req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
            }
            let mut resp = req.send().await.map_err(|e| {
                warn!(error = %e, "model download request failed");
                if e.is_redirect() {
                    "redirect_refused"
                } else {
                    "network"
                }
            })?;
            let status = resp.status();
            let append = match status.as_u16() {
                206 if content_range_start(&resp) == Some(existing) => true,
                206 => {
                    existing = 0;
                    continue;
                }
                200 => false,
                416 if existing > 0 => {
                    // Range beyond the end: start over (the hash decides what is valid).
                    existing = 0;
                    let _ = std::fs::remove_file(part);
                    continue;
                }
                _ => {
                    warn!(status = status.as_u16(), "model download HTTP error");
                    return Err("download_http_error");
                }
            };
            let total = size.or_else(|| {
                resp.content_length()
                    .map(|l| if append { l + existing } else { l })
            });
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(part)
                .map_err(|_| "storage")?;
            let mut done = if append { existing } else { 0 };
            progress(done, total);
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Err("cancelled");
                }
                let chunk = resp.chunk().await.map_err(|e| {
                    warn!(error = %e, done, "model download interrupted");
                    "network"
                })?;
                let Some(chunk) = chunk else { break };
                file.write_all(&chunk).map_err(|_| "storage")?;
                done += chunk.len() as u64;
                progress(done, total);
            }
            file.sync_all().map_err(|_| "storage")?;
            return Ok(());
        }
        Err("download_http_error")
    }
}

/// Start offset of a `Content-Range: bytes S-E/T` header.
fn content_range_start(resp: &reqwest::Response) -> Option<u64> {
    let v = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    let range = v.strip_prefix("bytes ")?;
    range.split('-').next()?.trim().parse().ok()
}

/// Lower-case hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Checks size and SHA-256; a mismatching file is deleted.
async fn verify_file(path: &Path, expect: &Expect) -> Result<(), &'static str> {
    let p = path.to_path_buf();
    let expect = expect.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), &'static str> {
        let len = std::fs::metadata(&p).map_err(|_| "storage")?.len();
        if expect.size.is_some_and(|s| s != len) {
            let _ = std::fs::remove_file(&p);
            return Err("size_mismatch");
        }
        let got = sha256_file(&p).map_err(|_| "storage")?;
        if !got.eq_ignore_ascii_case(&expect.sha256) {
            let _ = std::fs::remove_file(&p);
            return Err("hash_mismatch");
        }
        Ok(())
    })
    .await
    .map_err(|_| "internal_error")?;
    if let Err(code) = result {
        warn!(code, "downloaded model failed verification");
    }
    result
}

// ---- SenseVoice archive -------------------------------------------------------------------------

/// Extracts `model.int8.onnx` and `tokens.txt` from the sherpa-onnx `.tar.bz2` into
/// `models/<SENSEVOICE_DIR>/`. Entry paths are never used as output paths (only the file
/// name is matched), so a crafted archive cannot write elsewhere.
pub fn extract_sensevoice(archive: &Path, models: &Path) -> Result<PathBuf, &'static str> {
    let target = models.join(paths::SENSEVOICE_DIR);
    let tmp = models.join(format!("{}.tmp", paths::SENSEVOICE_DIR));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|_| "storage")?;
    let result = (|| {
        let f = std::fs::File::open(archive).map_err(|_| "storage")?;
        let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(std::io::BufReader::new(f)));
        let mut found = 0;
        for entry in tar.entries().map_err(|_| "archive_invalid")? {
            let mut entry = entry.map_err(|_| "archive_invalid")?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().map_err(|_| "archive_invalid")?;
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(wanted) = SENSEVOICE_FILES.iter().find(|w| **w == name) else {
                continue;
            };
            let mut out = std::fs::File::create(tmp.join(wanted)).map_err(|_| "storage")?;
            std::io::copy(&mut entry, &mut out).map_err(|_| "archive_invalid")?;
            out.sync_all().map_err(|_| "storage")?;
            found += 1;
        }
        if found < SENSEVOICE_FILES.len() || v2c_asr_local::sensevoice::model_files(&tmp).is_err() {
            return Err("archive_invalid");
        }
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(|_| "storage")?;
        }
        std::fs::rename(&tmp, &target).map_err(|_| "storage")?;
        Ok(target.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result
}

// ---- GGML check ---------------------------------------------------------------------------------

/// whisper.cpp `GGML_FILE_MAGIC` (0x67676d6c, little-endian on disk).
const GGML_MAGIC: u32 = 0x6767_6d6c;
/// Header bytes needed: magic + 11 `i32` hyperparameters (the last is `ftype`).
pub const GGML_HEADER_LEN: usize = 4 + 11 * 4;

/// Reads the quantisation from a whisper.cpp GGML header. Errors: `not_ggml` (no magic or too
/// short), `ggml_unsupported_quant` (not q5_0 / q8_0, e.g. the f16 file).
pub fn ggml_quant(header: &[u8]) -> Result<BreezeQuant, &'static str> {
    if header.len() < GGML_HEADER_LEN {
        return Err("not_ggml");
    }
    let word =
        |i: usize| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
    if word(0) != GGML_MAGIC {
        return Err("not_ggml");
    }
    // hparams: n_vocab, n_audio_ctx, n_audio_state, n_audio_head, n_audio_layer, n_text_ctx,
    // n_text_state, n_text_head, n_text_layer, n_mels, ftype. Quantised files store
    // `ftype + 1000 * GGML_QNT_VERSION`.
    let ftype = word(4 + 10 * 4) % 1000;
    match ftype {
        7 => Ok(BreezeQuant::Q8_0),
        8 => Ok(BreezeQuant::Q5_0),
        _ => Err("ggml_unsupported_quant"),
    }
}

fn read_header(path: &Path) -> Result<Vec<u8>, &'static str> {
    let mut f = std::fs::File::open(path).map_err(|_| "storage")?;
    let mut buf = vec![0u8; GGML_HEADER_LEN];
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..]).map_err(|_| "storage")?;
        if r == 0 {
            break;
        }
        n += r;
    }
    buf.truncate(n);
    Ok(buf)
}

/// Copies a Breeze GGML file into `models` under its standard name (via a `.part` file).
pub fn import_breeze_file(
    src: &Path,
    models: &Path,
    progress: &dyn Fn(u64, Option<u64>),
) -> Result<BreezeQuant, &'static str> {
    let quant = ggml_quant(&read_header(src)?)?;
    std::fs::create_dir_all(models).map_err(|_| "storage")?;
    let dest = models.join(paths::breeze_model_file(quant));
    if same_file(src, &dest) {
        return Ok(quant);
    }
    // Not the download's <file>.bin.part: an import and a download of the same quant never
    // write the same temporary file.
    let part = dest.with_extension("bin.import.part");
    let result = (|| {
        let total = std::fs::metadata(src).map_err(|_| "storage")?.len();
        let mut from = std::fs::File::open(src).map_err(|_| "storage")?;
        let mut to = std::fs::File::create(&part).map_err(|_| "storage")?;
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            let n = from.read(&mut buf).map_err(|_| "storage")?;
            if n == 0 {
                break;
            }
            to.write_all(&buf[..n]).map_err(|_| "storage")?;
            done += n as u64;
            progress(done, Some(total));
        }
        to.sync_all().map_err(|_| "storage")?;
        drop(to);
        std::fs::rename(&part, &dest).map_err(|_| "storage")
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map(|()| quant)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

// ---- Tauri commands -----------------------------------------------------------------------------

/// Running downloads / imports and their cancel flags, by model id.
static ACTIVE: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn check_caller(label: &str) -> Result<(), &'static str> {
    if label == MAIN_WINDOW {
        Ok(())
    } else {
        warn!(window = label, "models command refused");
        Err("forbidden")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub id: String,
    /// `download`, `verify`, `extract`, `copy`, `done`, `error`, `cancelled`.
    pub phase: &'static str,
    pub done: u64,
    pub total: Option<u64>,
    pub code: Option<&'static str>,
}

fn emit(app: &AppHandle, id: &str, phase: &'static str, done: u64, total: Option<u64>) {
    let _ = app.emit_to(
        MAIN_WINDOW,
        MODEL_PROGRESS_EVENT,
        Progress {
            id: id.to_owned(),
            phase,
            done,
            total,
            code: None,
        },
    );
}

/// Throttled progress callback (at most ~10 events per second, plus the last one).
fn progress_emitter(
    app: AppHandle,
    id: String,
    phase: &'static str,
) -> impl Fn(u64, Option<u64>) + Send + Sync + 'static {
    let last = Mutex::new(std::time::Instant::now() - Duration::from_secs(1));
    move |done, total| {
        let mut l = last.lock().unwrap_or_else(|p| p.into_inner());
        if l.elapsed() >= Duration::from_millis(100) || total == Some(done) {
            *l = std::time::Instant::now();
            emit(&app, &id, phase, done, total);
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EntryView {
    pub id: String,
    pub kind: ModelKind,
    pub quant: Option<BreezeQuant>,
    pub size: Option<u64>,
    pub placeholder: bool,
    pub downloadable: bool,
    pub installed: bool,
    /// Bytes already downloaded (resumable).
    pub partial_bytes: u64,
    pub running: bool,
}

#[derive(Debug, Serialize)]
pub struct ModelsStatus {
    pub models_dir: String,
    pub any_asr: bool,
    pub sensevoice: bool,
    pub breeze_q5_0: bool,
    pub breeze_q8_0: bool,
    pub manifest: ManifestSource,
    pub entries: Vec<EntryView>,
}

pub fn status(paths: &Paths) -> ModelsStatus {
    let (manifest, source) = load_manifest(paths);
    let models = paths.models_dir();
    let active = ACTIVE.lock().unwrap_or_else(|p| p.into_inner());
    let entries = manifest
        .models
        .iter()
        .map(|e| EntryView {
            id: e.id.clone(),
            kind: e.kind,
            quant: e.quant,
            size: e.size,
            placeholder: e.placeholder,
            downloadable: e.downloadable(),
            installed: installed(paths, e),
            partial_bytes: std::fs::metadata(part_path(&models, e)).map_or(0, |m| m.len()),
            running: active.contains_key(&e.id),
        })
        .collect();
    ModelsStatus {
        models_dir: models.display().to_string(),
        any_asr: any_asr_model(paths),
        sensevoice: sensevoice_present(paths),
        breeze_q5_0: breeze_present(paths, BreezeQuant::Q5_0),
        breeze_q8_0: breeze_present(paths, BreezeQuant::Q8_0),
        manifest: source,
        entries,
    }
}

#[tauri::command]
pub async fn models_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<ModelsStatus, &'static str> {
    check_caller(window.label())?;
    let paths = state.paths.clone();
    tauri::async_runtime::spawn_blocking(move || status(&paths))
        .await
        .map_err(|_| "internal_error")
}

/// Marks `id` as running; `None` if it already is.
fn begin(id: &str) -> Option<ActiveGuard> {
    let mut active = ACTIVE.lock().unwrap_or_else(|p| p.into_inner());
    if active.contains_key(id) {
        return None;
    }
    let flag = Arc::new(AtomicBool::new(false));
    active.insert(id.to_owned(), Arc::clone(&flag));
    Some(ActiveGuard {
        id: id.to_owned(),
        cancel: flag,
    })
}

struct ActiveGuard {
    id: String,
    cancel: Arc<AtomicBool>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.id);
    }
}

/// Downloads, verifies, and installs model `id` from the manifest. Errors are plain codes
/// (also sent as a `phase: "error"` progress event); the app keeps running either way.
#[tauri::command]
pub async fn models_download(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
) -> Result<(), &'static str> {
    check_caller(window.label())?;
    let paths = app.state::<AppState>().paths.clone();
    let (manifest, _) = load_manifest(&paths);
    let entry = manifest.entry(&id).cloned().ok_or("not_found")?;
    if entry.placeholder {
        return Err("model_not_hosted");
    }
    let sha256 = entry.sha256.clone().ok_or("model_not_hosted")?;
    let guard = begin(&id).ok_or("already_running")?;
    info!(id, size = ?entry.size, "model download started");
    let result = install(&app, &paths, &entry, sha256, &guard.cancel).await;
    let (phase, code) = match result {
        Ok(()) => ("done", None),
        Err("cancelled") => ("cancelled", Some("cancelled")),
        Err(c) => ("error", Some(c)),
    };
    match code {
        None => info!(id, "model installed"),
        Some(c) => warn!(id, code = c, "model install did not finish"),
    }
    let _ = app.emit_to(
        MAIN_WINDOW,
        MODEL_PROGRESS_EVENT,
        Progress {
            id: id.clone(),
            phase,
            done: 0,
            total: None,
            code,
        },
    );
    result
}

async fn install(
    app: &AppHandle,
    paths: &Paths,
    e: &ModelEntry,
    sha256: String,
    cancel: &AtomicBool,
) -> Result<(), &'static str> {
    let models = paths.models_dir();
    let expect = Expect {
        sha256,
        size: e.size,
    };
    let dl = Downloader::new()?;
    match (e.kind, e.quant) {
        (ModelKind::SensevoiceArchive, _) => {
            let dir = models.join(SENSEVOICE_DL_DIR);
            let archive = dir.join(SENSEVOICE_ARCHIVE);
            // An archive left by an earlier run (or a manual download) is reused if it verifies.
            let reuse = archive.is_file() && verify_file(&archive, &expect).await.is_ok();
            if !reuse {
                let part = part_path(&models, e);
                let progress = progress_emitter(app.clone(), e.id.clone(), "download");
                dl.download(&e.url, &part, &expect, &progress, cancel)
                    .await?;
                std::fs::rename(&part, &archive).map_err(|_| "storage")?;
            }
            emit(app, &e.id, "extract", 0, None);
            let models2 = models.clone();
            let archive2 = archive.clone();
            tokio::task::spawn_blocking(move || extract_sensevoice(&archive2, &models2))
                .await
                .map_err(|_| "internal_error")??;
            // The archive is no longer needed (163 MB).
            let _ = std::fs::remove_dir_all(&dir);
            Ok(())
        }
        (ModelKind::BreezeGgml, Some(q)) => {
            let part = part_path(&models, e);
            let progress = progress_emitter(app.clone(), e.id.clone(), "download");
            dl.download(&e.url, &part, &expect, &progress, cancel)
                .await?;
            let header = read_header(&part)?;
            if ggml_quant(&header) != Ok(q) {
                let _ = std::fs::remove_file(&part);
                return Err("not_ggml");
            }
            std::fs::rename(&part, models.join(paths::breeze_model_file(q))).map_err(|_| "storage")
        }
        (ModelKind::BreezeGgml, None) => Err("manifest_invalid"),
    }
}

/// Cancels a running download (the partial file is kept for resuming).
#[tauri::command]
pub fn models_cancel(window: WebviewWindow, id: String) -> Result<(), &'static str> {
    check_caller(window.label())?;
    match ACTIVE.lock().unwrap_or_else(|p| p.into_inner()).get(&id) {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        }
        None => Err("not_running"),
    }
}

#[derive(Debug, Serialize)]
pub struct Imported {
    pub quant: BreezeQuant,
}

/// 「從本機檔案匯入」: picks a Breeze `.bin` (native file dialog), checks it, copies it in.
/// `Ok(None)` when the user closed the dialog.
#[tauri::command]
pub async fn models_import_breeze(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Option<Imported>, &'static str> {
    use tauri_plugin_dialog::DialogExt;

    check_caller(window.label())?;
    let guard = begin("breeze-import").ok_or("already_running")?;
    let picker = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        picker
            .dialog()
            .file()
            .set_title("選擇 Breeze-ASR-25 GGML 模型檔")
            .add_filter("GGML 模型", &["bin"])
            .blocking_pick_file()
    })
    .await
    .map_err(|_| "internal_error")?;
    let Some(src) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let models = app.state::<AppState>().paths.models_dir();
    let progress = progress_emitter(app.clone(), guard.id.clone(), "copy");
    let result =
        tauri::async_runtime::spawn_blocking(move || import_breeze_file(&src, &models, &progress))
            .await
            .map_err(|_| "internal_error")?;
    match result {
        Ok(quant) => {
            info!(?quant, "Breeze model imported");
            emit(&app, &guard.id, "done", 0, None);
            Ok(Some(Imported { quant }))
        }
        Err(code) => {
            warn!(code, "Breeze import failed");
            Err(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("v2c-models-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    fn noop(_: u64, _: Option<u64>) {}

    #[test]
    fn builtin_manifest_is_valid_and_breeze_is_a_placeholder() {
        let m = Manifest::parse(BUILTIN_MANIFEST).unwrap();
        let sv = m.entry("sensevoice").unwrap();
        assert_eq!(sv.kind, ModelKind::SensevoiceArchive);
        assert!(sv.downloadable());
        assert_eq!(sv.size, Some(163_002_883));
        for id in ["breeze-q5_0", "breeze-q8_0"] {
            let b = m.entry(id).unwrap();
            assert!(b.placeholder && !b.downloadable(), "{id}");
            assert!(b.quant.is_some());
        }
    }

    #[test]
    fn manifest_validation() {
        let ok = r#"{"version":1,"models":[{"id":"a","kind":"sensevoice_archive","url":"https://x.test/a","sha256":null,"size":5}]}"#;
        assert!(Manifest::parse(ok).is_ok());
        // Placeholders may omit the size.
        let ph = r#"{"version":1,"models":[{"id":"a","kind":"sensevoice_archive","url":"https://x.test/a","placeholder":true}]}"#;
        assert!(Manifest::parse(ph).is_ok());
        assert!(Manifest::parse(ok).is_ok());
        for bad in [
            "not json",
            r#"{"version":2,"models":[]}"#,
            // http is refused
            r#"{"version":1,"models":[{"id":"a","kind":"sensevoice_archive","url":"http://x.test/a"}]}"#,
            // bad hash
            r#"{"version":1,"models":[{"id":"a","kind":"sensevoice_archive","url":"https://x.test/a","sha256":"zz"}]}"#,
            // Breeze needs a quant
            r#"{"version":1,"models":[{"id":"a","kind":"breeze_ggml","url":"https://x.test/a"}]}"#,
            // duplicate id
            r#"{"version":1,"models":[{"id":"a","kind":"sensevoice_archive","url":"https://x.test/a"},{"id":"a","kind":"sensevoice_archive","url":"https://x.test/b"}]}"#,
            // hosted entry without a size
            r#"{"version":1,"models":[{"id":"a","kind":"sensevoice_archive","url":"https://x.test/a","sha256":null}]}"#,
            r#"{"version":1,"models":[{"id":"a","kind":"sensevoice_archive","url":"https://x.test/a","size":0}]}"#,
            // id with a path separator
            r#"{"version":1,"models":[{"id":"../a","kind":"sensevoice_archive","url":"https://x.test/a"}]}"#,
        ] {
            assert_eq!(Manifest::parse(bad), Err("manifest_invalid"), "{bad}");
        }
    }

    #[test]
    fn override_manifest_is_used_when_valid() {
        let dir = temp_dir("manifest");
        let paths = Paths {
            data_dir: dir.clone(),
            local_data_dir: dir.clone(),
        };
        assert_eq!(load_manifest(&paths).1, ManifestSource::Builtin);
        std::fs::write(dir.join(OVERRIDE_FILE), "{").unwrap();
        assert_eq!(load_manifest(&paths).1, ManifestSource::OverrideInvalid);
        std::fs::write(
            dir.join(OVERRIDE_FILE),
            r#"{"version":1,"models":[{"id":"breeze-q5_0","kind":"breeze_ggml","quant":"q5_0","url":"https://hf.test/q5.bin","sha256":"00000000000000000000000000000000000000000000000000000000000000aa","size":10}]}"#,
        )
        .unwrap();
        let (m, src) = load_manifest(&paths);
        assert_eq!(src, ManifestSource::Override);
        assert!(m.entry("breeze-q5_0").unwrap().downloadable());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_of_a_file() {
        let dir = temp_dir("sha");
        let f = dir.join("x");
        std::fs::write(&f, b"abc").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redirects_only_to_https_and_bounded() {
        let https = reqwest::Url::parse("https://cdn.test/f").unwrap();
        let http = reqwest::Url::parse("http://cdn.test/f").unwrap();
        assert!(redirect_allowed(&https, 0, false));
        assert!(!redirect_allowed(&http, 0, false));
        assert!(redirect_allowed(&http, 0, true));
        assert!(!redirect_allowed(&https, MAX_REDIRECTS, false));
    }

    #[tokio::test]
    async fn plain_http_is_refused_in_production() {
        let dir = temp_dir("http");
        let dl = Downloader::new().unwrap();
        let expect = Expect {
            sha256: sha(b""),
            size: None,
        };
        let r = dl
            .download(
                "http://127.0.0.1:9/x",
                &dir.join("x.part"),
                &expect,
                &noop,
                &AtomicBool::new(false),
            )
            .await;
        assert_eq!(r, Err("insecure_url"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn full_download_follows_redirect_and_verifies() {
        let server = MockServer::start().await;
        let data = body();
        Mock::given(method("GET"))
            .and(path("/asset"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/cdn/blob", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/cdn/blob"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
            .mount(&server)
            .await;
        let dir = temp_dir("full");
        let part = dir.join("m.part");
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let last = Mutex::new((0u64, None));
        let progress = |d: u64, t: Option<u64>| {
            calls.fetch_add(1, Ordering::SeqCst);
            *last.lock().unwrap() = (d, t);
        };
        let expect = Expect {
            sha256: sha(&data),
            size: Some(data.len() as u64),
        };
        Downloader::for_local_tests()
            .download(
                &format!("{}/asset", server.uri()),
                &part,
                &expect,
                &progress,
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), data);
        assert!(calls.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            *last.lock().unwrap(),
            (data.len() as u64, Some(data.len() as u64))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resumes_with_a_range_request() {
        let server = MockServer::start().await;
        let data = body();
        let half = 70_000usize;
        Mock::given(method("GET"))
            .and(path("/m"))
            .and(header("range", format!("bytes={half}-").as_str()))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header(
                        "content-range",
                        format!("bytes {half}-{}/{}", data.len() - 1, data.len()).as_str(),
                    )
                    .set_body_bytes(data[half..].to_vec()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = temp_dir("resume");
        let part = dir.join("m.part");
        std::fs::write(&part, &data[..half]).unwrap();
        let expect = Expect {
            sha256: sha(&data),
            size: Some(data.len() as u64),
        };
        Downloader::for_local_tests()
            .download(
                &format!("{}/m", server.uri()),
                &part,
                &expect,
                &noop,
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A server without range support answers 200 with the whole file: start over.
    #[tokio::test]
    async fn server_ignoring_range_restarts_the_file() {
        let server = MockServer::start().await;
        let data = body();
        Mock::given(method("GET"))
            .and(path("/m"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
            .mount(&server)
            .await;
        let dir = temp_dir("norange");
        let part = dir.join("m.part");
        std::fs::write(&part, b"garbage-prefix").unwrap();
        let expect = Expect {
            sha256: sha(&data),
            size: None,
        };
        Downloader::for_local_tests()
            .download(
                &format!("{}/m", server.uri()),
                &part,
                &expect,
                &noop,
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn hash_mismatch_deletes_the_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"tampered".to_vec()))
            .mount(&server)
            .await;
        let dir = temp_dir("mismatch");
        let part = dir.join("m.part");
        let expect = Expect {
            sha256: sha(b"original"),
            size: None,
        };
        let r = Downloader::for_local_tests()
            .download(
                &format!("{}/m", server.uri()),
                &part,
                &expect,
                &noop,
                &AtomicBool::new(false),
            )
            .await;
        assert_eq!(r, Err("hash_mismatch"));
        assert!(!part.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn http_errors_and_cancel() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ok"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body()))
            .mount(&server)
            .await;
        let dir = temp_dir("errors");
        let expect = Expect {
            sha256: sha(&body()),
            size: None,
        };
        let dl = Downloader::for_local_tests();
        let r = dl
            .download(
                &format!("{}/missing", server.uri()),
                &dir.join("a.part"),
                &expect,
                &noop,
                &AtomicBool::new(false),
            )
            .await;
        assert_eq!(r, Err("download_http_error"));
        let r = dl
            .download(
                &format!("{}/ok", server.uri()),
                &dir.join("b.part"),
                &expect,
                &noop,
                &AtomicBool::new(true),
            )
            .await;
        assert_eq!(r, Err("cancelled"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Redirect loops end with an error, not a hang.
    #[tokio::test]
    async fn redirect_loop_is_refused() {
        struct Loop(String);
        impl Respond for Loop {
            fn respond(&self, _: &Request) -> ResponseTemplate {
                ResponseTemplate::new(302).insert_header("location", self.0.as_str())
            }
        }
        let server = MockServer::start().await;
        let url = format!("{}/loop", server.uri());
        Mock::given(method("GET"))
            .respond_with(Loop(url.clone()))
            .mount(&server)
            .await;
        let dir = temp_dir("loop");
        let expect = Expect {
            sha256: sha(b""),
            size: None,
        };
        let r = Downloader::for_local_tests()
            .download(
                &url,
                &dir.join("x.part"),
                &expect,
                &noop,
                &AtomicBool::new(false),
            )
            .await;
        assert_eq!(r, Err("redirect_refused"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn ggml_header(ftype: i32) -> Vec<u8> {
        let mut h = GGML_MAGIC.to_le_bytes().to_vec();
        for v in [51866i32, 1500, 1280, 20, 32, 448, 1280, 20, 32, 128, ftype] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        h
    }

    #[test]
    fn ggml_magic_and_quant() {
        assert_eq!(ggml_quant(&ggml_header(2008)), Ok(BreezeQuant::Q5_0));
        assert_eq!(ggml_quant(&ggml_header(2007)), Ok(BreezeQuant::Q8_0));
        assert_eq!(ggml_quant(&ggml_header(8)), Ok(BreezeQuant::Q5_0));
        assert_eq!(ggml_quant(&ggml_header(1)), Err("ggml_unsupported_quant"));
        assert_eq!(ggml_quant(b"GGUF\x03\0\0\0"), Err("not_ggml"));
        let mut bad = ggml_header(8);
        bad[0] = 0;
        assert_eq!(ggml_quant(&bad), Err("not_ggml"));
    }

    #[test]
    fn import_copies_a_valid_file_under_its_standard_name() {
        let dir = temp_dir("import");
        let src = dir.join("my-breeze.bin");
        let mut data = ggml_header(2008);
        data.extend(std::iter::repeat_n(7u8, 4096));
        std::fs::write(&src, &data).unwrap();
        let models = dir.join("models");
        let q = import_breeze_file(&src, &models, &noop).unwrap();
        assert_eq!(q, BreezeQuant::Q5_0);
        let dest = models.join(paths::breeze_model_file(q));
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!dest.with_extension("bin.import.part").exists());
        // Importing the installed file again is a no-op.
        assert_eq!(import_breeze_file(&dest, &models, &noop), Ok(q));

        let junk = dir.join("junk.bin");
        std::fs::write(&junk, b"not a model at all, definitely not ggml data....").unwrap();
        assert_eq!(import_breeze_file(&junk, &models, &noop), Err("not_ggml"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An import and a download of the same quant use different temporary files, so a running
    /// download's partial file survives an import.
    #[test]
    fn import_does_not_touch_a_download_part_file() {
        let dir = temp_dir("import-part");
        let models = dir.join("models");
        std::fs::create_dir_all(&models).unwrap();
        let entry = ModelEntry {
            id: "breeze-q5_0".into(),
            kind: ModelKind::BreezeGgml,
            quant: Some(BreezeQuant::Q5_0),
            url: "https://hf.test/q5.bin".into(),
            sha256: None,
            size: Some(1),
            placeholder: false,
        };
        let download_part = part_path(&models, &entry);
        std::fs::write(&download_part, b"partial download").unwrap();
        let src = dir.join("q5.bin");
        let mut data = ggml_header(2008);
        data.extend([1u8; 64]);
        std::fs::write(&src, &data).unwrap();
        assert_eq!(
            import_breeze_file(&src, &models, &noop),
            Ok(BreezeQuant::Q5_0)
        );
        assert_eq!(std::fs::read(&download_part).unwrap(), b"partial download");
        let dest = models.join(paths::breeze_model_file(BreezeQuant::Q5_0));
        assert_ne!(dest.with_extension("bin.import.part"), download_part);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_archive(path: &Path, entries: &[(&str, &[u8])]) {
        let f = std::fs::File::create(path).unwrap();
        let enc = bzip2::write::BzEncoder::new(f, bzip2::Compression::fast());
        let mut b = tar::Builder::new(enc);
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            // `append_data` would reject `..`; write the raw name like a crafted archive.
            h.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn sensevoice_archive_extracts_only_the_model_files() {
        let dir = temp_dir("extract");
        let archive = dir.join("sv.tar.bz2");
        let d = paths::SENSEVOICE_DIR;
        make_archive(
            &archive,
            &[
                (&format!("{d}/model.int8.onnx"), b"onnx"),
                (&format!("{d}/model.onnx"), b"big"),
                (&format!("{d}/tokens.txt"), b"tok"),
                (&format!("{d}/test_wavs/zh.wav"), b"wav"),
                ("../evil.txt", b"evil"),
            ],
        );
        let models = dir.join("models");
        std::fs::create_dir_all(&models).unwrap();
        let out = extract_sensevoice(&archive, &models).unwrap();
        assert_eq!(out, models.join(d));
        let mut names: Vec<String> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["model.int8.onnx", "tokens.txt"]);
        assert_eq!(std::fs::read(out.join("tokens.txt")).unwrap(), b"tok");
        assert!(!dir.join("evil.txt").exists() && !models.join("evil.txt").exists());
        assert!(v2c_asr_local::sensevoice::model_files(&out).is_ok());

        // An archive without the model files leaves nothing behind.
        let bad = dir.join("bad.tar.bz2");
        make_archive(&bad, &[("x/readme.md", b"hi")]);
        let models2 = dir.join("models2");
        std::fs::create_dir_all(&models2).unwrap();
        assert_eq!(extract_sensevoice(&bad, &models2), Err("archive_invalid"));
        assert_eq!(std::fs::read_dir(&models2).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presence_checks_and_partial_bytes() {
        let dir = temp_dir("presence");
        let paths = Paths {
            data_dir: dir.clone(),
            local_data_dir: dir.clone(),
        };
        let models = paths.models_dir();
        std::fs::create_dir_all(models.join(paths::SENSEVOICE_DIR)).unwrap();
        // An empty SenseVoice folder does not count.
        assert!(!sensevoice_present(&paths));
        std::fs::write(
            models.join(paths::SENSEVOICE_DIR).join("model.int8.onnx"),
            b"x",
        )
        .unwrap();
        std::fs::write(models.join(paths::SENSEVOICE_DIR).join("tokens.txt"), b"x").unwrap();
        assert!(sensevoice_present(&paths));
        assert!(any_asr_model(&paths));

        let (m, _) = load_manifest(&paths);
        let e = m.entry("sensevoice").unwrap();
        let part = part_path(&models, e);
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, [0u8; 123]).unwrap();
        let st = status(&paths);
        let v = st.entries.iter().find(|v| v.id == "sensevoice").unwrap();
        assert!(v.installed);
        assert_eq!(v.partial_bytes, 123);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_main_window_may_call() {
        assert_eq!(check_caller("main"), Ok(()));
        assert_eq!(check_caller("overlay"), Err("forbidden"));
    }
}
