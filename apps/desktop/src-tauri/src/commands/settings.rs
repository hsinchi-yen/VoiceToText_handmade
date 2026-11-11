//! Settings page commands (T1.11, `tasks/plan.md` section 7).
//!
//! The page collects changes ([`Change`]) as a draft (T1.14): [`settings_preview`] validates
//! them and returns the settings as they would be, without writing anything. 「儲存」 sends the
//! whole draft to [`settings_apply`], which applies it to the current `config.toml` through
//! [`config_edit`] (only the changed keys are rewritten, comments survive) and emits
//! `v2c:config-changed` so the running engine reloads. Cloud services are not edited here
//! (雲端服務 page). Start with Windows uses `tauri-plugin-autostart` from Rust only (HKCU `Run`
//! key; no webview permission).
//!
//! [`settings_runtime_status`] reports what the engine actually uses (「目前使用中」), and
//! [`settings_test_local_asr`] runs the local engine on the built-in test clip.
//!
//! Every command refuses callers other than the main window (`forbidden`). Logs carry the
//! changed field names, engine names, timings and error codes only.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tracing::{info, warn};
use v2c_asr_local::{gpu_engine, GuardReason, LocalAsrKind, PickReason};
use v2c_core::config::{
    AppRule, BreezeQuant, CloudKind, Config, GpuProfile, KeepLoaded, LocalAsr, MicMode, LOCAL,
};

use crate::app_state::{self, AppState};
use crate::commands::models;
use crate::config_edit;
use crate::controller::StatusEvent;
use crate::engine::{ConfigDiff, CpuReason, LocalStatus, LocalTestRun, PurposeUse, RouteStatus};
use crate::tray::CONFIG_CHANGED_EVENT;

const MAIN_WINDOW: &str = "main";
/// Trigger keys `v2c_platform::windows::hotkey::WindowsHotkey` understands. Other keys would
/// be saved but never fire, so they are refused.
pub const DICTATE_KEYS: [&str; 1] = ["RightCtrl"];
pub const TRANSLATE_KEYS: [&str; 1] = ["RightCtrl+Shift"];

/// Launch argument the autostart entry passes; the main window then starts hidden.
pub const AUTOSTART_ARG: &str = "--autostart";

const MAX_APP_RULES: usize = 200;
const MAX_EXE_LEN: usize = 128;

/// One setting change from the settings page.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "snake_case")]
pub enum Change {
    PolishEnabled(bool),
    RouteAsr(String),
    RoutePolish(String),
    RouteTranslate(String),
    CloudFallbackToLocal(bool),
    HotkeyDictate(String),
    HotkeyTranslate(String),
    HoldThresholdMs(u64),
    DoubleTapWindowMs(u64),
    HandsFreeMaxSecs(u64),
    LocalAsr(LocalAsr),
    BreezeQuant(BreezeQuant),
    LlmBaseUrl(String),
    LlmModel(Option<String>),
    MicMode(MicMode),
    MicDevice(Option<String>),
    Apps(Vec<AppRule>),
    HistoryEnabled(bool),
    HistoryRetentionDays(u32),
    HistoryExcludeApps(Vec<String>),
    GpuProfile(GpuProfile),
    GpuReserveMb(u64),
    GpuKeepLoaded(KeepLoaded),
    GpuIdleUnloadMinutes(u64),
    GpuYieldOnFullscreen(bool),
    GpuHeavyApps(Vec<String>),
}

impl Change {
    /// Field name for logs (never the value).
    fn name(&self) -> &'static str {
        match self {
            Change::PolishEnabled(_) => "polish_enabled",
            Change::RouteAsr(_) => "route_asr",
            Change::RoutePolish(_) => "route_polish",
            Change::RouteTranslate(_) => "route_translate",
            Change::CloudFallbackToLocal(_) => "cloud_fallback_to_local",
            Change::HotkeyDictate(_) => "hotkey_dictate",
            Change::HotkeyTranslate(_) => "hotkey_translate",
            Change::HoldThresholdMs(_) => "hold_threshold_ms",
            Change::DoubleTapWindowMs(_) => "double_tap_window_ms",
            Change::HandsFreeMaxSecs(_) => "hands_free_max_secs",
            Change::LocalAsr(_) => "local_asr",
            Change::BreezeQuant(_) => "breeze_quant",
            Change::LlmBaseUrl(_) => "llm_base_url",
            Change::LlmModel(_) => "llm_model",
            Change::MicMode(_) => "mic_mode",
            Change::MicDevice(_) => "mic_device",
            Change::Apps(_) => "apps",
            Change::HistoryEnabled(_) => "history_enabled",
            Change::HistoryRetentionDays(_) => "history_retention_days",
            Change::HistoryExcludeApps(_) => "history_exclude_apps",
            Change::GpuProfile(_) => "gpu_profile",
            Change::GpuReserveMb(_) => "gpu_reserve_mb",
            Change::GpuKeepLoaded(_) => "gpu_keep_loaded",
            Change::GpuIdleUnloadMinutes(_) => "gpu_idle_unload_minutes",
            Change::GpuYieldOnFullscreen(_) => "gpu_yield_on_fullscreen",
            Change::GpuHeavyApps(_) => "gpu_heavy_apps",
        }
    }
}

fn in_range<T: PartialOrd>(v: T, lo: T, hi: T) -> Result<T, &'static str> {
    if v >= lo && v <= hi {
        Ok(v)
    } else {
        Err("out_of_range")
    }
}

/// A route target: `local` or the id of a saved cloud service (that can do ASR, for ASR).
fn route_target(cfg: &Config, id: &str, asr: bool) -> Result<String, &'static str> {
    let id = id.trim();
    if id == LOCAL {
        return Ok(LOCAL.to_owned());
    }
    let svc = cfg.cloud_service(id).ok_or("route_unknown_service")?;
    if asr && !svc.kind.supports_asr() {
        return Err("route_no_asr");
    }
    Ok(svc.id.clone())
}

/// An executable name such as `LINE.exe` (no path).
fn exe_name(s: &str) -> Result<String, &'static str> {
    let s = s.trim();
    let ok = !s.is_empty()
        && s.len() <= MAX_EXE_LEN
        && s.to_ascii_lowercase().ends_with(".exe")
        && s.len() > 4
        && !s.contains(['\\', '/', ':', '"', '<', '>', '|', '?', '*'])
        && !s.chars().any(char::is_control);
    if ok {
        Ok(s.to_owned())
    } else {
        Err("invalid_exe")
    }
}

/// Validated, trimmed list of executables without case-insensitive duplicates.
fn exe_list(list: &[String]) -> Result<Vec<String>, &'static str> {
    if list.len() > MAX_APP_RULES {
        return Err("too_many");
    }
    let mut out: Vec<String> = Vec::new();
    for s in list {
        let exe = exe_name(s)?;
        if !out.iter().any(|e| e.eq_ignore_ascii_case(&exe)) {
            out.push(exe);
        }
    }
    Ok(out)
}

fn app_rules(rules: &[AppRule]) -> Result<Vec<AppRule>, &'static str> {
    // An empty list would read back as the built-in rules (see `Config::from_toml_str`).
    if rules.is_empty() {
        return Err("app_rules_empty");
    }
    if rules.len() > MAX_APP_RULES {
        return Err("too_many");
    }
    let mut out: Vec<AppRule> = Vec::new();
    for r in rules {
        let exe = exe_name(&r.exe)?;
        if out.iter().any(|o| o.exe.eq_ignore_ascii_case(&exe)) {
            return Err("duplicate_app");
        }
        out.push(AppRule { exe, ..r.clone() });
    }
    Ok(out)
}

/// LM Studio address: `http(s)://host[:port]/path`, trailing slash removed.
fn base_url(s: &str) -> Result<String, &'static str> {
    let s = s.trim().trim_end_matches('/');
    let url = reqwest::Url::parse(s).map_err(|_| "invalid_url")?;
    let ok = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && s.len() <= 512;
    if ok {
        Ok(s.to_owned())
    } else {
        Err("invalid_url")
    }
}

fn optional_text(v: &Option<String>, max: usize) -> Result<Option<String>, &'static str> {
    match v.as_deref().map(str::trim) {
        None | Some("") => Ok(None),
        Some(s) if s.chars().count() <= max && !s.chars().any(char::is_control) => {
            Ok(Some(s.to_owned()))
        }
        Some(_) => Err("invalid_text"),
    }
}

/// Validates `change` and applies it to `cfg`.
pub fn apply_change(cfg: &mut Config, change: &Change) -> Result<(), &'static str> {
    match change {
        Change::PolishEnabled(v) => cfg.polish.enabled = *v,
        Change::RouteAsr(id) => cfg.route.asr = route_target(cfg, id, true)?,
        Change::RoutePolish(id) => cfg.route.polish = route_target(cfg, id, false)?,
        Change::RouteTranslate(id) => cfg.route.translate = route_target(cfg, id, false)?,
        Change::CloudFallbackToLocal(v) => cfg.route.cloud_fallback_to_local = *v,
        Change::HotkeyDictate(k) => {
            if !DICTATE_KEYS.contains(&k.as_str()) {
                return Err("hotkey_unsupported");
            }
            cfg.hotkey.dictate = k.clone();
        }
        Change::HotkeyTranslate(k) => {
            if !TRANSLATE_KEYS.contains(&k.as_str()) {
                return Err("hotkey_unsupported");
            }
            cfg.hotkey.translate = k.clone();
        }
        Change::HoldThresholdMs(v) => cfg.hotkey.hold_threshold_ms = in_range(*v, 150, 1500)?,
        Change::DoubleTapWindowMs(v) => {
            cfg.hotkey.double_tap_window_ms = in_range(*v, 150, 1000)?;
        }
        Change::HandsFreeMaxSecs(v) => cfg.hotkey.hands_free_max_secs = in_range(*v, 10, 3600)?,
        Change::LocalAsr(v) => cfg.local.asr = *v,
        Change::BreezeQuant(v) => cfg.local.breeze_quant = *v,
        Change::LlmBaseUrl(v) => cfg.local.llm_base_url = base_url(v)?,
        Change::LlmModel(v) => cfg.local.llm_model = optional_text(v, 200)?,
        Change::MicMode(v) => cfg.audio.mic_mode = *v,
        Change::MicDevice(v) => cfg.audio.device = optional_text(v, 256)?,
        Change::Apps(rules) => cfg.apps = app_rules(rules)?,
        Change::HistoryEnabled(v) => cfg.history.enabled = *v,
        Change::HistoryRetentionDays(v) => cfg.history.retention_days = in_range(*v, 1, 365)?,
        Change::HistoryExcludeApps(v) => cfg.history.exclude_apps = exe_list(v)?,
        Change::GpuProfile(v) => cfg.gpu.profile = *v,
        Change::GpuReserveMb(v) => cfg.gpu.vram_reserve_mb = in_range(*v, 0, 16384)?,
        Change::GpuKeepLoaded(v) => cfg.gpu.keep_loaded = *v,
        Change::GpuIdleUnloadMinutes(v) => cfg.gpu.idle_unload_minutes = in_range(*v, 1, 240)?,
        Change::GpuYieldOnFullscreen(v) => cfg.gpu.yield_on_fullscreen = *v,
        Change::GpuHeavyApps(v) => cfg.gpu.heavy_apps = exe_list(v)?,
    }
    Ok(())
}

fn check_caller(label: &str) -> Result<(), &'static str> {
    if label == MAIN_WINDOW {
        Ok(())
    } else {
        warn!(window = label, "settings command refused");
        Err("forbidden")
    }
}

#[derive(Debug, Serialize)]
pub struct ServiceRef {
    pub id: String,
    pub kind: CloudKind,
    pub supports_asr: bool,
}

#[derive(Debug, Serialize)]
pub struct HotkeyOptions {
    pub dictate: &'static [&'static str],
    pub translate: &'static [&'static str],
}

#[derive(Debug, Serialize)]
pub struct SettingsView {
    pub config: Config,
    /// `config.toml` could not be read; `config` is the running config.
    pub file_error: Option<&'static str>,
    pub cloud: Vec<ServiceRef>,
    pub hotkeys: HotkeyOptions,
    pub autostart: Option<bool>,
    pub cuda_build: bool,
    pub sensevoice: bool,
    pub breeze_q5_0: bool,
    pub breeze_q8_0: bool,
    /// The TEA-ASR GGUF is installed (T1.12).
    pub tea_asr: bool,
    /// This binary can run TEA-ASR (built with the `tea` and `cuda` features).
    pub tea_build: bool,
    /// Value of the 語音辨識 menu on 潤飾與用途: a cloud service id, or `local:<[local] asr>`.
    pub asr_choice: String,
}

/// Prefix of the local options of the 語音辨識 menu (`local:auto`, `local:breeze`, ...).
pub const LOCAL_CHOICE_PREFIX: &str = "local:";

/// The 語音辨識 menu value for `cfg`: the cloud service when ASR is routed to one, otherwise the
/// local option matching `[local] asr`.
pub fn asr_choice(cfg: &Config) -> String {
    if cfg.route.asr != LOCAL && cfg.cloud_service(&cfg.route.asr).is_some() {
        return cfg.route.asr.clone();
    }
    let engine = serde_json::to_value(cfg.local.asr)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    format!("{LOCAL_CHOICE_PREFIX}{engine}")
}

fn view(app: &AppHandle, config: Config, file_error: Option<&'static str>) -> SettingsView {
    use tauri_plugin_autostart::ManagerExt;

    let paths = &app.state::<AppState>().paths;
    SettingsView {
        cloud: config
            .cloud
            .iter()
            .map(|c| ServiceRef {
                id: c.id.clone(),
                kind: c.kind,
                supports_asr: c.kind.supports_asr(),
            })
            .collect(),
        file_error,
        hotkeys: HotkeyOptions {
            dictate: &DICTATE_KEYS,
            translate: &TRANSLATE_KEYS,
        },
        autostart: app.autolaunch().is_enabled().ok(),
        cuda_build: v2c_asr_local::breeze::cuda_enabled(),
        sensevoice: models::sensevoice_present(paths),
        breeze_q5_0: models::breeze_present(paths, BreezeQuant::Q5_0),
        breeze_q8_0: models::breeze_present(paths, BreezeQuant::Q8_0),
        tea_asr: models::tea_present(paths),
        tea_build: v2c_asr_local::tea::tea_enabled() && v2c_asr_local::breeze::cuda_enabled(),
        asr_choice: asr_choice(&config),
        config,
    }
}

#[tauri::command]
pub async fn settings_get(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<SettingsView, &'static str> {
    check_caller(window.label())?;
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = a.state::<AppState>();
        let (cfg, err) = match app_state::read_config(&state.paths.config_file()) {
            Ok(c) => (c, None),
            Err(code) => ((*state.engine.config()).clone(), Some(code)),
        };
        view(&a, cfg, err)
    })
    .await
    .map_err(|_| "internal_error")
}

#[derive(Debug, Serialize)]
pub struct Applied {
    pub settings: SettingsView,
    /// Microphone or history changes take effect after restarting v2c.
    pub restart_required: bool,
}

/// Validates and saves `changes` (all or nothing), then emits `v2c:config-changed`.
#[tauri::command]
pub async fn settings_apply(
    app: AppHandle,
    window: WebviewWindow,
    changes: Vec<Change>,
) -> Result<Applied, &'static str> {
    check_caller(window.label())?;
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = a.state::<AppState>();
        let path = state.paths.config_file();
        let before = app_state::read_config(&path).unwrap_or_else(|_| Config::with_defaults());
        let names: Vec<&str> = changes.iter().map(Change::name).collect();
        let cfg = config_edit::update_config_file(&path, |cfg| {
            for c in &changes {
                apply_change(cfg, c)?;
            }
            Ok(true)
        })
        .inspect_err(|code| warn!(code, ?names, "settings not saved"))?;
        info!(?names, "settings saved");
        let diff = ConfigDiff::between(&before, &cfg);
        let _ = a.emit(CONFIG_CHANGED_EVENT, ());
        Ok(Applied {
            restart_required: diff.audio || diff.history,
            settings: view(&a, cfg, None),
        })
    })
    .await
    .map_err(|_| "internal_error")?
}

/// Turns 「開機自動啟動」 on or off; returns the new state.
#[tauri::command]
pub fn settings_set_autostart(
    app: AppHandle,
    window: WebviewWindow,
    enabled: bool,
) -> Result<bool, &'static str> {
    use tauri_plugin_autostart::ManagerExt;

    check_caller(window.label())?;
    let al = app.autolaunch();
    let r = if enabled { al.enable() } else { al.disable() };
    if let Err(e) = r {
        warn!(error = %e, enabled, "autostart change failed");
        return Err("autostart_failed");
    }
    info!(enabled, "autostart changed");
    al.is_enabled().map_err(|_| "autostart_failed")
}

/// Input device names for the microphone list (the system default is "no device").
pub fn input_devices() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut names: Vec<String> = devices
        .filter_map(|d| d.description().ok().map(|desc| desc.name().to_owned()))
        .collect();
    names.dedup();
    names
}

#[tauri::command]
pub async fn settings_input_devices(window: WebviewWindow) -> Result<Vec<String>, &'static str> {
    check_caller(window.label())?;
    tauri::async_runtime::spawn_blocking(input_devices)
        .await
        .map_err(|_| "internal_error")
}

// ---- draft preview --------------------------------------------------------------------

/// Applies `changes` to a copy of `base`. Returns the result and the fields whose change
/// alone leaves `base` as it is (the page drops those from its draft).
pub fn preview_changes(
    base: &Config,
    changes: &[Change],
) -> Result<(Config, Vec<&'static str>), &'static str> {
    let mut cfg = base.clone();
    let mut unchanged = Vec::new();
    for c in changes {
        apply_change(&mut cfg, c)?;
        let mut alone = base.clone();
        apply_change(&mut alone, c)?;
        if alone == *base {
            unchanged.push(c.name());
        }
    }
    Ok((cfg, unchanged))
}

#[derive(Debug, Serialize)]
pub struct Preview {
    /// The settings as they would be after saving the draft.
    pub settings: SettingsView,
    /// Draft fields that equal the saved value.
    pub unchanged: Vec<&'static str>,
}

/// Validates the page's draft against the current `config.toml` without writing anything.
#[tauri::command]
pub async fn settings_preview(
    app: AppHandle,
    window: WebviewWindow,
    changes: Vec<Change>,
) -> Result<Preview, &'static str> {
    check_caller(window.label())?;
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = a.state::<AppState>();
        let (base, err) = match app_state::read_config(&state.paths.config_file()) {
            Ok(c) => (c, None),
            Err(code) => ((*state.engine.config()).clone(), Some(code)),
        };
        let (cfg, unchanged) = preview_changes(&base, &changes)?;
        Ok(Preview {
            settings: view(&a, cfg, err),
            unchanged,
        })
    })
    .await
    .map_err(|_| "internal_error")?
}

// ---- 目前使用中 ------------------------------------------------------------------------

/// One line of 「目前使用中」.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UseView {
    /// What is used, e.g. 「Breeze-ASR-25 q8_0・NVIDIA 顯示卡・已載入」.
    pub text: String,
    /// Why it differs from the settings, in one sentence (or a problem).
    pub note: Option<String>,
    /// `note` is a problem (red) rather than an explanation (grey).
    pub warn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VramView {
    pub free_mb: u64,
    pub total_mb: u64,
    pub reserve_mb: u64,
    /// VRAM the configured GPU model needs (name predates TEA-ASR).
    pub breeze_need_mb: u64,
    /// That GPU model, e.g. 「Breeze-ASR-25 q5_0」 or 「TEA-ASR-1.1」.
    pub gpu_engine: String,
}

/// `settings_runtime_status`: what the running engine uses (not what the file says).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeStatus {
    pub asr: UseView,
    pub polish: UseView,
    pub translate: UseView,
    pub polish_enabled: bool,
    /// `None` without an NVIDIA GPU / NVML.
    pub vram: Option<VramView>,
}

fn kind_label(kind: CloudKind) -> &'static str {
    match kind {
        CloudKind::OpenAi => "OpenAI",
        CloudKind::Gemini => "Gemini",
        CloudKind::Claude => "Claude",
        CloudKind::OpenAiCompat => "自訂端點",
    }
}

fn quant_label(q: BreezeQuant) -> &'static str {
    match q {
        BreezeQuant::Q5_0 => "q5_0",
        BreezeQuant::Q8_0 => "q8_0",
    }
}

/// 「Breeze-ASR-25 q5_0」 / 「TEA-ASR-1.1」 / 「SenseVoice-Small」.
pub fn engine_label(kind: LocalAsrKind, cfg: &Config) -> String {
    match kind {
        LocalAsrKind::Breeze => format!("Breeze-ASR-25 {}", quant_label(cfg.local.breeze_quant)),
        LocalAsrKind::TeaAsr => "TEA-ASR-1.1".to_owned(),
        LocalAsrKind::SenseVoice => "SenseVoice-Small".to_owned(),
    }
}

/// `engine` value of the test result (`breeze`, `tea_asr`, `sense_voice`).
fn engine_value(kind: LocalAsrKind) -> &'static str {
    match kind {
        LocalAsrKind::Breeze => "breeze",
        LocalAsrKind::TeaAsr => "tea_asr",
        LocalAsrKind::SenseVoice => "sense_voice",
    }
}

/// 「Gemini：gemini・gemini-3.6-flash」.
fn cloud_text(cfg: &Config, id: &str, asr: bool) -> String {
    let Some(svc) = cfg.cloud_service(id) else {
        return id.to_owned();
    };
    let suggested = v2c_core::route::suggested_models(svc.kind);
    let model = if asr {
        svc.asr_model.as_deref().or(suggested.asr)
    } else {
        svc.llm_model.as_deref().or(suggested.llm)
    };
    let mut text = format!(
        "{}：{}",
        kind_label(svc.kind),
        super::cloud::display_name(svc)
    );
    if let Some(m) = model {
        text.push('・');
        text.push_str(m);
    }
    text
}

/// One sentence (繁中) on why the configured GPU model (Breeze or TEA-ASR) is not running;
/// `None` when the user chose SenseVoice (or nothing needs explaining). `cuda_build`: this
/// binary can run the GPU model (CUDA, plus the TEA-ASR runtime for TEA-ASR).
pub fn cpu_reason_text(reason: CpuReason, cfg: &Config, cuda_build: bool) -> Option<String> {
    let gpu = engine_label(gpu_engine(cfg), cfg);
    let gpu_short = match gpu_engine(cfg) {
        LocalAsrKind::TeaAsr => "TEA-ASR",
        _ => "Breeze",
    };
    let low_vram = |free_mb: u64, needed_mb: u64| {
        format!("顯存不足（可用 {free_mb} MB，需要 {needed_mb} MB），自動改用 SenseVoice。")
    };
    let text = match reason {
        CpuReason::LoadFailed => {
            format!("{gpu_short} 載入失敗，改用 SenseVoice（原因見記錄檔）。")
        }
        CpuReason::Guard(GuardReason::Engine(p)) => match p {
            PickReason::Configured | PickReason::BreezeFits => return None,
            PickReason::GpuDisabled => {
                "設定檔關閉了顯示卡（[gpu] enabled = false），改用 SenseVoice。".to_owned()
            }
            PickReason::CpuOnlyProfile => "顯示卡方案是「不用顯卡」，改用 SenseVoice。".to_owned(),
            PickReason::BreezeModelMissing | PickReason::TeaModelMissing => {
                format!("{gpu} 模型未安裝，改用 SenseVoice。")
            }
            PickReason::NoCuda if !cuda_build => "此版本沒有 CUDA，改用 SenseVoice。".to_owned(),
            PickReason::NoCuda => "沒有偵測到可用的 NVIDIA 顯示卡，改用 SenseVoice。".to_owned(),
            PickReason::LowVram { free_mb, needed_mb } => low_vram(free_mb, needed_mb),
        },
        CpuReason::Guard(GuardReason::LowVram { free_mb, needed_mb }) => {
            low_vram(free_mb, needed_mb)
        }
        CpuReason::Guard(GuardReason::VramUnknown) => {
            "讀不到顯存用量，自動模式改用 SenseVoice；要強制使用請選 Breeze-ASR-25。".to_owned()
        }
        CpuReason::Guard(GuardReason::HeavyApp) => {
            "吃顯卡的程式在前景，暫時改用 SenseVoice。".to_owned()
        }
        CpuReason::Guard(GuardReason::Fullscreen) => {
            "全螢幕程式在前景，暫時改用 SenseVoice。".to_owned()
        }
        CpuReason::Guard(GuardReason::Idle) => {
            format!("閒置一段時間後已卸載 {gpu_short}，下次按熱鍵時重新載入。")
        }
        CpuReason::Guard(GuardReason::RestorePending) => {
            format!("顯存剛恢復，稍候會重新載入 {gpu_short}。")
        }
    };
    Some(text)
}

/// The binary can run `cfg`'s GPU model.
pub fn gpu_build(cfg: &Config) -> bool {
    let cuda = v2c_asr_local::breeze::cuda_enabled();
    match gpu_engine(cfg) {
        LocalAsrKind::TeaAsr => cuda && v2c_asr_local::tea::tea_enabled(),
        _ => cuda,
    }
}

fn local_asr_view(cfg: &Config, local: &LocalStatus, cuda_build: bool) -> UseView {
    let reason = || {
        local
            .cpu_reason
            .filter(|_| cfg.local.asr != LocalAsr::SenseVoice)
            .and_then(|r| cpu_reason_text(r, cfg, cuda_build))
    };
    match local.active {
        Some(kind @ (LocalAsrKind::Breeze | LocalAsrKind::TeaAsr)) => UseView {
            text: format!(
                "{}・{}・已載入",
                engine_label(kind, cfg),
                if local.on_gpu {
                    "NVIDIA 顯示卡"
                } else {
                    "CPU"
                }
            ),
            note: None,
            warn: false,
        },
        Some(LocalAsrKind::SenseVoice) => UseView {
            text: "SenseVoice-Small・CPU".to_owned(),
            note: reason(),
            warn: false,
        },
        None if local.switching => UseView {
            text: "切換模型中…".to_owned(),
            note: Some("同一時間只載入一個語音模型，舊模型卸載後才載入新的。".to_owned()),
            warn: false,
        },
        None if local.starting => UseView {
            text: "載入中…".to_owned(),
            note: None,
            warn: false,
        },
        None if local.unloaded => UseView {
            text: "目前沒有載入模型".to_owned(),
            note: reason().or_else(|| Some("按熱鍵說話時會自動載入。".to_owned())),
            warn: false,
        },
        None => UseView {
            text: "沒有可用的地端語音模型".to_owned(),
            note: Some(match local.error {
                Some("model_missing") | None => "請到「初次設定」下載或匯入模型。".to_owned(),
                Some(code) => format!("模型無法載入（{code}），原因見記錄檔。"),
            }),
            warn: true,
        },
    }
}

fn cloud_unusable(cfg: &Config, id: &str) -> String {
    let name = cfg.cloud_service(id).map_or(id.to_owned(), |s| {
        format!("{}：{}", kind_label(s.kind), super::cloud::display_name(s))
    });
    format!("雲端服務「{name}」無法使用（金鑰或設定有誤）")
}

/// 「語音辨識」 line.
pub fn asr_view(
    cfg: &Config,
    route: &PurposeUse,
    local: &LocalStatus,
    cuda_build: bool,
) -> UseView {
    match route {
        PurposeUse::Cloud { service_id } => UseView {
            text: cloud_text(cfg, service_id, true),
            note: None,
            warn: false,
        },
        PurposeUse::Unavailable { service_id } => UseView {
            text: "無法辨識".to_owned(),
            note: Some(format!(
                "{}，且沒有開啟「雲端失敗時自動改用地端」。",
                cloud_unusable(cfg, service_id)
            )),
            warn: true,
        },
        PurposeUse::Local => local_asr_view(cfg, local, cuda_build),
        PurposeUse::LocalInsteadOfCloud { service_id } => {
            let mut v = local_asr_view(cfg, local, cuda_build);
            let lead = format!("{}，改用地端。", cloud_unusable(cfg, service_id));
            v.note = Some(match v.note {
                Some(n) => format!("{lead}{n}"),
                None => lead,
            });
            v.warn = true;
            v
        }
    }
}

/// 「潤飾」 (`translate = false`) or 「中翻英」 line.
pub fn llm_view(cfg: &Config, route: &PurposeUse, lm_up: bool, translate: bool) -> UseView {
    if !translate && !cfg.polish.enabled {
        return UseView {
            text: "已關閉".to_owned(),
            note: None,
            warn: false,
        };
    }
    let without = if translate {
        "中翻英無法使用"
    } else {
        "潤飾改用規則式去贅字"
    };
    let local = |lead: Option<String>| {
        let model = cfg
            .local
            .llm_model
            .as_deref()
            .unwrap_or("LM Studio 目前載入的模型");
        let down = (!lm_up).then(|| {
            format!(
                "連不到 LM Studio（{}），{without}。",
                cfg.local.llm_base_url
            )
        });
        let warn = lead.is_some() || down.is_some();
        let note = match (lead, down) {
            (Some(l), Some(d)) => Some(format!("{l}{d}")),
            (l, d) => l.or(d),
        };
        UseView {
            text: format!("本機 LM Studio・{model}"),
            note,
            warn,
        }
    };
    match route {
        PurposeUse::Cloud { service_id } => UseView {
            text: cloud_text(cfg, service_id, false),
            note: None,
            warn: false,
        },
        PurposeUse::Unavailable { service_id } => UseView {
            text: "無法使用".to_owned(),
            note: Some(format!(
                "{}，且沒有開啟「雲端失敗時自動改用地端」；{without}。",
                cloud_unusable(cfg, service_id)
            )),
            warn: true,
        },
        PurposeUse::Local => local(None),
        PurposeUse::LocalInsteadOfCloud { service_id } => local(Some(format!(
            "{}，改用地端。",
            cloud_unusable(cfg, service_id)
        ))),
    }
}

/// Builds 「目前使用中」 (pure; tested).
pub fn runtime_status(
    cfg: &Config,
    route: &RouteStatus,
    local: &LocalStatus,
    vram: Option<VramView>,
    cuda_build: bool,
) -> RuntimeStatus {
    RuntimeStatus {
        asr: asr_view(cfg, &route.asr, local, cuda_build),
        polish: llm_view(cfg, &route.polish, route.lm_up, false),
        translate: llm_view(cfg, &route.translate, route.lm_up, true),
        polish_enabled: cfg.polish.enabled,
        vram,
    }
}

/// 「目前使用中」: the running engine's state.
#[tauri::command]
pub async fn settings_runtime_status(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<RuntimeStatus, &'static str> {
    check_caller(window.label())?;
    let engine = Arc::clone(&app.state::<AppState>().engine);
    tauri::async_runtime::spawn_blocking(move || {
        let cfg = engine.config();
        let vram = engine.vram().map(|s| VramView {
            free_mb: s.free_mb,
            total_mb: s.total_mb,
            reserve_mb: cfg.gpu.vram_reserve_mb,
            breeze_need_mb: engine.gpu_need_mb(),
            gpu_engine: engine_label(gpu_engine(&cfg), &cfg),
        });
        runtime_status(
            &cfg,
            &engine.route_status(),
            &engine.local_status(),
            vram,
            gpu_build(&cfg),
        )
    })
    .await
    .map_err(|_| "internal_error")
}

// ---- 地端語音模型「測試」 ------------------------------------------------------------------

/// One step of the test, shown like the cloud wizard's step 5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepView {
    pub label: &'static str,
    /// `passed`, `failed`, or `skipped`.
    pub status: &'static str,
    pub latency_ms: Option<u64>,
    pub detail: Option<String>,
}

/// `settings_test_local_asr` result. Never contains the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalTestView {
    pub ok: bool,
    pub summary: String,
    /// `breeze`, `tea_asr`, or `sense_voice`.
    pub engine: &'static str,
    pub on_gpu: bool,
    pub fallback_reason: Option<String>,
    pub load_ms: Option<u64>,
    pub transcribe_ms: Option<u64>,
    pub vram_mb: Option<u64>,
    pub steps: Vec<StepView>,
}

/// Builds the test result view (pure; tested).
pub fn local_test_view(run: &LocalTestRun, cfg: &Config, cuda_build: bool) -> LocalTestView {
    let label = engine_label(run.engine, cfg);
    let place = if run.on_gpu {
        "NVIDIA 顯示卡"
    } else {
        "CPU"
    };
    let reason = run
        .cpu_reason
        .filter(|_| cfg.local.asr != LocalAsr::SenseVoice)
        .and_then(|r| cpu_reason_text(r, cfg, cuda_build));
    let mut chosen = format!("{label}・{place}");
    if let Some(mb) = run.vram_mb {
        chosen.push_str(&format!("，約使用 {mb} MB 顯存"));
    }
    if let Some(r) = &reason {
        chosen.push('。');
        chosen.push_str(r);
    }
    const LOAD: &str = "載入模型";
    let load = match (run.load_ms, run.cpu_reason) {
        // The GPU model failed; SenseVoice may have been loaded instead (`load_ms`).
        (ms, Some(CpuReason::LoadFailed)) => StepView {
            label: LOAD,
            status: "failed",
            latency_ms: ms,
            detail: Some(format!(
                "{} 載入失敗，改用 SenseVoice",
                engine_label(gpu_engine(cfg), cfg)
            )),
        },
        (Some(ms), _) => StepView {
            label: LOAD,
            status: "passed",
            latency_ms: Some(ms),
            detail: Some("這次才載入".to_owned()),
        },
        (None, _) => StepView {
            label: LOAD,
            status: "skipped",
            latency_ms: None,
            detail: Some("已在記憶體中".to_owned()),
        },
    };
    const TRANSCRIBE: &str = "辨識 3 秒測試音";
    let transcribe = match run.transcribe {
        Ok(ms) => StepView {
            label: TRANSCRIBE,
            status: "passed",
            latency_ms: Some(ms),
            detail: None,
        },
        Err(_) => StepView {
            label: TRANSCRIBE,
            status: "failed",
            latency_ms: None,
            detail: Some("辨識失敗，原因見記錄檔".to_owned()),
        },
    };
    let ok = run.transcribe.is_ok();
    LocalTestView {
        ok,
        summary: if ok {
            format!("{label}・{place} 可以正常辨識")
        } else {
            format!("{label} 辨識失敗")
        },
        engine: engine_value(run.engine),
        on_gpu: run.on_gpu,
        fallback_reason: reason,
        load_ms: run.load_ms,
        transcribe_ms: run.transcribe.ok(),
        vram_mb: run.vram_mb,
        steps: vec![
            StepView {
                label: "選擇引擎",
                status: "passed",
                latency_ms: None,
                detail: Some(chosen),
            },
            load,
            transcribe,
        ],
    }
}

/// Dictation in progress (from the latest status event).
fn dictating(last: Option<&StatusEvent>) -> bool {
    matches!(
        last,
        Some(StatusEvent::Recording { .. } | StatusEvent::Transcribing | StatusEvent::Polishing)
    )
}

/// 「測試」 on 地端語音模型: runs the built-in 3 s clip through the local engine the saved
/// settings use. Errors: `busy` while dictating, `already_running`, local engine errors.
#[tauri::command]
pub async fn settings_test_local_asr(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<LocalTestView, &'static str> {
    check_caller(window.label())?;
    let state = app.state::<AppState>();
    let last = state
        .last_status
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    if dictating(last.as_ref()) {
        return Err("busy");
    }
    let engine = Arc::clone(&state.engine);
    let run = engine
        .test_local_asr()
        .await
        .inspect_err(|code| info!(code, "local ASR test refused"))?;
    let cfg = engine.config();
    Ok(local_test_view(&run, &cfg, gpu_build(&cfg)))
}

#[cfg(test)]
mod tests {
    use v2c_core::config::{CloudService, NewlineKey};

    use super::*;

    fn with_services() -> Config {
        let mut c = Config::with_defaults();
        for (id, kind) in [("g", CloudKind::Gemini), ("c", CloudKind::Claude)] {
            c.cloud.push(CloudService {
                id: id.into(),
                name: None,
                kind,
                base_url: None,
                asr_model: None,
                llm_model: None,
            });
        }
        c
    }

    #[test]
    fn changes_deserialize_from_the_page_format() {
        let c: Change =
            serde_json::from_str(r#"{"field":"gpu_profile","value":"low_vram"}"#).unwrap();
        assert_eq!(c, Change::GpuProfile(GpuProfile::LowVram));
        let c: Change = serde_json::from_str(r#"{"field":"llm_model","value":null}"#).unwrap();
        assert_eq!(c, Change::LlmModel(None));
        let c: Change = serde_json::from_str(
            r#"{"field":"apps","value":[{"exe":"LINE.exe","newline":"shift_enter","always_paste":false}]}"#,
        )
        .unwrap();
        assert!(matches!(c, Change::Apps(ref r) if r[0].newline == NewlineKey::ShiftEnter));
        assert!(serde_json::from_str::<Change>(r#"{"field":"cloud","value":[]}"#).is_err());
    }

    #[test]
    fn hotkeys_only_accept_supported_keys() {
        let mut c = Config::with_defaults();
        assert!(apply_change(&mut c, &Change::HotkeyDictate("RightCtrl".into())).is_ok());
        assert_eq!(
            apply_change(&mut c, &Change::HotkeyDictate("F9".into())),
            Err("hotkey_unsupported")
        );
        assert_eq!(
            apply_change(&mut c, &Change::HotkeyTranslate("RightCtrl".into())),
            Err("hotkey_unsupported")
        );
        assert_eq!(
            apply_change(&mut c, &Change::HoldThresholdMs(10)),
            Err("out_of_range")
        );
        apply_change(&mut c, &Change::HoldThresholdMs(400)).unwrap();
        assert_eq!(c.hotkey.hold_threshold_ms, 400);
        assert_eq!(c.hotkey.dictate, "RightCtrl");
    }

    #[test]
    fn routes_must_name_a_saved_service_that_fits() {
        let mut c = with_services();
        apply_change(&mut c, &Change::RouteAsr("g".into())).unwrap();
        assert_eq!(c.route.asr, "g");
        // Claude has no speech recognition.
        assert_eq!(
            apply_change(&mut c, &Change::RouteAsr("c".into())),
            Err("route_no_asr")
        );
        apply_change(&mut c, &Change::RoutePolish(" c ".into())).unwrap();
        assert_eq!(c.route.polish, "c");
        assert_eq!(
            apply_change(&mut c, &Change::RouteTranslate("nope".into())),
            Err("route_unknown_service")
        );
        apply_change(&mut c, &Change::RouteAsr("local".into())).unwrap();
        assert_eq!(c.route.asr, LOCAL);
    }

    #[test]
    fn app_rules_are_validated() {
        let rule = |exe: &str| AppRule {
            exe: exe.into(),
            newline: NewlineKey::Enter,
            always_paste: false,
        };
        let mut c = Config::with_defaults();
        apply_change(
            &mut c,
            &Change::Apps(vec![rule(" Code.exe "), rule("LINE.exe")]),
        )
        .unwrap();
        assert_eq!(c.apps[0].exe, "Code.exe");
        for (rules, code) in [
            (vec![], "app_rules_empty"),
            (vec![rule("C:\\x\\a.exe")], "invalid_exe"),
            (vec![rule("notepad")], "invalid_exe"),
            (vec![rule(".exe")], "invalid_exe"),
            (vec![rule("a.exe"), rule("A.EXE")], "duplicate_app"),
        ] {
            assert_eq!(apply_change(&mut c, &Change::Apps(rules)), Err(code));
        }
        apply_change(
            &mut c,
            &Change::HistoryExcludeApps(vec!["a.exe".into(), "A.exe".into(), "b.exe".into()]),
        )
        .unwrap();
        assert_eq!(c.history.exclude_apps, ["a.exe", "b.exe"]);
        apply_change(&mut c, &Change::GpuHeavyApps(vec![])).unwrap();
        assert!(c.gpu.heavy_apps.is_empty());
    }

    #[test]
    fn urls_text_and_ranges() {
        let mut c = Config::with_defaults();
        apply_change(
            &mut c,
            &Change::LlmBaseUrl(" http://127.0.0.1:1234/v1/ ".into()),
        )
        .unwrap();
        assert_eq!(c.local.llm_base_url, "http://127.0.0.1:1234/v1");
        for bad in [
            "ftp://x/v1",
            "localhost:1234",
            "http://user:pw@h/v1",
            "http://h/v1?x=1",
        ] {
            assert_eq!(
                apply_change(&mut c, &Change::LlmBaseUrl(bad.into())),
                Err("invalid_url"),
                "{bad}"
            );
        }
        apply_change(&mut c, &Change::LlmModel(Some("  ".into()))).unwrap();
        assert_eq!(c.local.llm_model, None);
        apply_change(&mut c, &Change::MicDevice(Some("USB Mic".into()))).unwrap();
        assert_eq!(c.audio.device.as_deref(), Some("USB Mic"));
        assert_eq!(
            apply_change(&mut c, &Change::MicDevice(Some("a\nb".into()))),
            Err("invalid_text")
        );
        assert_eq!(
            apply_change(&mut c, &Change::HistoryRetentionDays(0)),
            Err("out_of_range")
        );
        assert_eq!(
            apply_change(&mut c, &Change::GpuReserveMb(100_000)),
            Err("out_of_range")
        );
        apply_change(&mut c, &Change::GpuReserveMb(3072)).unwrap();
        assert_eq!(c.gpu.vram_reserve_mb, 3072);
    }

    #[test]
    fn saving_changes_keeps_the_rest_of_the_file() {
        let dir = std::env::temp_dir().join(format!("v2c-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# mine\n[gpu]\nvram_reserve_mb = 2048 # keep\n").unwrap();
        let changes = [
            Change::GpuReserveMb(3000),
            Change::MicMode(MicMode::OnPress),
        ];
        let cfg = config_edit::update_config_file(&path, |cfg| {
            for c in &changes {
                apply_change(cfg, c)?;
            }
            Ok(true)
        })
        .unwrap();
        assert_eq!(cfg.gpu.vram_reserve_mb, 3000);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine") && text.contains("vram_reserve_mb = 3000 # keep"));
        assert!(text.contains("mic_mode = \"on_press\""), "{text}");
        // One invalid change: nothing is written.
        let r = config_edit::update_config_file(&path, |cfg| {
            apply_change(cfg, &Change::GpuReserveMb(1000))?;
            apply_change(cfg, &Change::HotkeyDictate("F1".into()))?;
            Ok(true)
        });
        assert_eq!(r, Err("hotkey_unsupported"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asr_menu_value_follows_route_and_local_engine() {
        let mut c = with_services();
        assert_eq!(asr_choice(&c), "local:auto");
        // 「地端：Breeze-ASR-25」 saves both fields in one go.
        let (cfg, _) = preview_changes(
            &c,
            &[
                Change::RouteAsr("local".into()),
                Change::LocalAsr(LocalAsr::Breeze),
            ],
        )
        .unwrap();
        assert_eq!(
            (cfg.route.asr.as_str(), cfg.local.asr),
            (LOCAL, LocalAsr::Breeze)
        );
        assert_eq!(asr_choice(&cfg), "local:breeze");
        // A cloud option only changes the route; the local engine is kept for later.
        c.local.asr = LocalAsr::SenseVoice;
        apply_change(&mut c, &Change::RouteAsr("g".into())).unwrap();
        assert_eq!(asr_choice(&c), "g");
        assert_eq!(c.local.asr, LocalAsr::SenseVoice);
        // Back to local.
        apply_change(&mut c, &Change::RouteAsr("local".into())).unwrap();
        assert_eq!(asr_choice(&c), "local:sense_voice");
        // A route to a service that no longer exists shows the local engine.
        c.route.asr = "gone".into();
        assert_eq!(asr_choice(&c), "local:sense_voice");
    }

    #[test]
    fn preview_reports_unchanged_fields_and_refuses_bad_ones() {
        let base = with_services();
        let (cfg, unchanged) = preview_changes(
            &base,
            &[
                Change::RouteAsr("local".into()),
                Change::LocalAsr(LocalAsr::Breeze),
                Change::PolishEnabled(true),
                Change::GpuReserveMb(800),
            ],
        )
        .unwrap();
        assert_eq!(unchanged, ["route_asr", "polish_enabled"]);
        assert_eq!(cfg.gpu.vram_reserve_mb, 800);
        assert_eq!(
            preview_changes(
                &base,
                &[Change::GpuReserveMb(1), Change::RouteAsr("c".into())]
            )
            .err(),
            Some("route_no_asr")
        );
    }

    fn local(active: Option<LocalAsrKind>, reason: Option<CpuReason>) -> LocalStatus {
        LocalStatus {
            active,
            on_gpu: matches!(active, Some(LocalAsrKind::Breeze | LocalAsrKind::TeaAsr)),
            gpu_engine: LocalAsrKind::Breeze,
            switching: false,
            starting: false,
            unloaded: false,
            error: None,
            cpu_reason: reason,
        }
    }

    #[test]
    fn runtime_status_reports_tea_switching_and_idle() {
        let mut c = Config::with_defaults();
        c.local.asr = LocalAsr::TeaAsr;
        let r = routes(true);
        let tea = LocalStatus {
            gpu_engine: LocalAsrKind::TeaAsr,
            ..local(Some(LocalAsrKind::TeaAsr), None)
        };
        let s = runtime_status(&c, &r, &tea, None, true);
        assert_eq!(s.asr.text, "TEA-ASR-1.1・NVIDIA 顯示卡・已載入");
        // Stand-in while a fullscreen app runs.
        let sv = LocalStatus {
            gpu_engine: LocalAsrKind::TeaAsr,
            ..local(
                Some(LocalAsrKind::SenseVoice),
                Some(CpuReason::Guard(GuardReason::Fullscreen)),
            )
        };
        let s = runtime_status(&c, &r, &sv, None, true);
        assert_eq!(s.asr.text, "SenseVoice-Small・CPU");
        assert!(s.asr.note.unwrap().contains("全螢幕"));
        // Missing model / no TEA runtime.
        let missing = CpuReason::Guard(GuardReason::Engine(PickReason::TeaModelMissing));
        assert_eq!(
            cpu_reason_text(missing, &c, true).unwrap(),
            "TEA-ASR-1.1 模型未安裝，改用 SenseVoice。"
        );
        let no_cuda = CpuReason::Guard(GuardReason::Engine(PickReason::NoCuda));
        assert!(cpu_reason_text(no_cuda, &c, false)
            .unwrap()
            .contains("此版本沒有 CUDA"));
        assert!(cpu_reason_text(CpuReason::LoadFailed, &c, true)
            .unwrap()
            .starts_with("TEA-ASR 載入失敗"));
        // Switching: nothing loaded on purpose, not an error.
        let switching = LocalStatus {
            switching: true,
            ..local(None, None)
        };
        let s = runtime_status(&c, &r, &switching, None, true);
        assert_eq!(s.asr.text, "切換模型中…");
        assert!(!s.asr.warn);
        // Idle unload.
        let idle = LocalStatus {
            unloaded: true,
            ..local(None, Some(CpuReason::Guard(GuardReason::Idle)))
        };
        let s = runtime_status(&c, &r, &idle, None, true);
        assert_eq!(s.asr.text, "目前沒有載入模型");
        assert!(s.asr.note.unwrap().contains("TEA-ASR"));
        assert!(!s.asr.warn);
        assert!(!gpu_build(&c) || v2c_asr_local::tea::tea_enabled());
    }

    fn routes(lm_up: bool) -> RouteStatus {
        RouteStatus {
            asr: PurposeUse::Local,
            polish: PurposeUse::Local,
            translate: PurposeUse::Local,
            lm_up,
        }
    }

    #[test]
    fn runtime_status_shows_breeze_on_the_gpu() {
        let mut c = Config::with_defaults();
        c.local.breeze_quant = BreezeQuant::Q8_0;
        c.local.llm_model = Some("qwen3-4b".into());
        let s = runtime_status(
            &c,
            &routes(true),
            &local(Some(LocalAsrKind::Breeze), None),
            None,
            true,
        );
        assert_eq!(s.asr.text, "Breeze-ASR-25 q8_0・NVIDIA 顯示卡・已載入");
        assert_eq!(s.asr.note, None);
        assert_eq!(s.polish.text, "本機 LM Studio・qwen3-4b");
        assert_eq!(s.translate.note, None);
        assert!(s.polish_enabled);
    }

    #[test]
    fn runtime_status_explains_why_sensevoice_runs() {
        let c = Config::with_defaults();
        let low = CpuReason::Guard(GuardReason::Engine(PickReason::LowVram {
            free_mb: 1200,
            needed_mb: 2100,
        }));
        let s = runtime_status(
            &c,
            &routes(false),
            &local(Some(LocalAsrKind::SenseVoice), Some(low)),
            None,
            true,
        );
        assert_eq!(s.asr.text, "SenseVoice-Small・CPU");
        let note = s.asr.note.unwrap();
        assert!(note.contains("顯存不足") && note.contains("1200"), "{note}");
        // LM Studio down: polish degrades, translate is unavailable.
        assert!(s.polish.warn && s.polish.note.unwrap().contains("規則式去贅字"));
        assert!(s.translate.note.unwrap().contains("中翻英無法使用"));

        // The user chose SenseVoice: nothing to explain.
        let mut chosen = c.clone();
        chosen.local.asr = LocalAsr::SenseVoice;
        let reason = CpuReason::Guard(GuardReason::Engine(PickReason::Configured));
        let s = runtime_status(
            &chosen,
            &routes(true),
            &local(Some(LocalAsrKind::SenseVoice), Some(reason)),
            None,
            true,
        );
        assert_eq!(s.asr.note, None);
        // No CUDA in this build.
        let no_cuda = CpuReason::Guard(GuardReason::Engine(PickReason::NoCuda));
        assert_eq!(
            cpu_reason_text(no_cuda, &c, false).as_deref(),
            Some("此版本沒有 CUDA，改用 SenseVoice。")
        );
    }

    #[test]
    fn runtime_status_for_cloud_routes() {
        let mut c = with_services();
        c.route.polish = "c".into();
        let r = RouteStatus {
            asr: PurposeUse::LocalInsteadOfCloud {
                service_id: "g".into(),
            },
            polish: PurposeUse::Cloud {
                service_id: "c".into(),
            },
            translate: PurposeUse::Unavailable {
                service_id: "g".into(),
            },
            lm_up: true,
        };
        let s = runtime_status(&c, &r, &local(None, None), None, true);
        assert_eq!(s.polish.text, "Claude：c・claude-haiku-5-5");
        assert!(s.asr.warn && s.asr.note.as_deref().unwrap().contains("Gemini：g"));
        assert_eq!(s.asr.text, "沒有可用的地端語音模型");
        assert_eq!(s.translate.text, "無法使用");
        // Polish off.
        c.polish.enabled = false;
        let s = runtime_status(&c, &r, &local(None, None), None, true);
        assert_eq!(s.polish.text, "已關閉");
    }

    #[test]
    fn local_test_view_reports_steps_without_text() {
        let c = Config::with_defaults();
        let run = LocalTestRun {
            engine: LocalAsrKind::Breeze,
            on_gpu: true,
            cpu_reason: None,
            load_ms: Some(2300),
            transcribe: Ok(410),
            vram_mb: Some(1700),
        };
        let v = local_test_view(&run, &c, true);
        assert!(v.ok);
        assert_eq!(v.engine, "breeze");
        assert_eq!(v.summary, "Breeze-ASR-25 q5_0・NVIDIA 顯示卡 可以正常辨識");
        let status: Vec<_> = v.steps.iter().map(|s| (s.status, s.latency_ms)).collect();
        assert_eq!(
            status,
            [
                ("passed", None),
                ("passed", Some(2300)),
                ("passed", Some(410))
            ]
        );
        assert!(v.steps[0].detail.as_deref().unwrap().contains("1700 MB"));

        let run = LocalTestRun {
            engine: LocalAsrKind::SenseVoice,
            on_gpu: false,
            cpu_reason: Some(CpuReason::Guard(GuardReason::LowVram {
                free_mb: 900,
                needed_mb: 2400,
            })),
            load_ms: None,
            transcribe: Err("asr_failed"),
            vram_mb: None,
        };
        let v = local_test_view(&run, &c, true);
        assert!(!v.ok);
        assert!(v.fallback_reason.as_deref().unwrap().contains("顯存不足"));
        assert_eq!(v.steps[1].status, "skipped");
        assert_eq!(v.steps[2].status, "failed");
        let failed = LocalTestRun {
            cpu_reason: Some(CpuReason::LoadFailed),
            transcribe: Ok(900),
            ..run
        };
        assert_eq!(local_test_view(&failed, &c, true).steps[1].status, "failed");
        // TEA-ASR.
        let mut t = Config::with_defaults();
        t.local.asr = LocalAsr::TeaAsr;
        let tea = LocalTestRun {
            engine: LocalAsrKind::TeaAsr,
            on_gpu: true,
            cpu_reason: None,
            load_ms: Some(9600),
            transcribe: Ok(240),
            vram_mb: Some(2600),
        };
        let v = local_test_view(&tea, &t, true);
        assert_eq!(v.engine, "tea_asr");
        assert_eq!(v.summary, "TEA-ASR-1.1・NVIDIA 顯示卡 可以正常辨識");
        let tea_failed = LocalTestRun {
            engine: LocalAsrKind::SenseVoice,
            on_gpu: false,
            cpu_reason: Some(CpuReason::LoadFailed),
            ..tea
        };
        let v = local_test_view(&tea_failed, &t, true);
        assert_eq!(v.engine, "sense_voice");
        assert_eq!(
            v.steps[1].detail.as_deref(),
            Some("TEA-ASR-1.1 載入失敗，改用 SenseVoice")
        );
    }

    #[test]
    fn test_is_refused_while_dictating() {
        assert!(dictating(Some(&StatusEvent::Transcribing)));
        assert!(dictating(Some(&StatusEvent::Recording {
            rms: 0.1,
            hands_free: false
        })));
        assert!(!dictating(Some(&StatusEvent::Done {
            chars: 3,
            latency_ms: 10
        })));
        assert!(!dictating(None));
    }

    #[test]
    fn only_the_main_window_may_call() {
        assert_eq!(check_caller("main"), Ok(()));
        assert_eq!(check_caller("overlay"), Err("forbidden"));
    }
}
