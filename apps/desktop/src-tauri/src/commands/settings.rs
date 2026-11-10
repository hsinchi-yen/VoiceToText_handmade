//! Settings page commands (T1.11, `tasks/plan.md` section 7).
//!
//! The page changes one setting at a time ([`Change`]); [`settings_apply`] validates every
//! change, applies it to the current `config.toml` through [`config_edit`] (only the changed
//! keys are rewritten, comments survive), and emits `v2c:config-changed` so the running
//! engine reloads. Cloud services are not edited here (雲端服務 page). Start with Windows uses
//! `tauri-plugin-autostart` from Rust only (HKCU `Run` key; no webview permission).
//!
//! Every command refuses callers other than the main window (`forbidden`). Logs carry the
//! changed field names only.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tracing::{info, warn};
use v2c_core::config::{
    AppRule, BreezeQuant, CloudKind, Config, GpuProfile, KeepLoaded, LocalAsr, MicMode, LOCAL,
};

use crate::app_state::{self, AppState};
use crate::commands::models;
use crate::config_edit;
use crate::engine::ConfigDiff;
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
        config,
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

#[derive(Debug, Serialize, PartialEq)]
pub struct LmTest {
    pub latency_ms: u64,
    /// Model ids LM Studio reports (for the model list).
    pub models: Vec<String>,
}

/// `GET {base_url}/models` on the given LM Studio address. Errors: `invalid_url`,
/// `unreachable`, `timeout`, `http_error`, `bad_response`.
pub async fn test_lm(client: &reqwest::Client, base: &str) -> Result<LmTest, &'static str> {
    #[derive(Deserialize)]
    struct Models {
        data: Vec<ModelId>,
    }
    #[derive(Deserialize)]
    struct ModelId {
        id: String,
    }
    let base = base_url(base)?;
    let t = Instant::now();
    let resp = client
        .get(format!("{base}/models"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "timeout"
            } else {
                "unreachable"
            }
        })?;
    if !resp.status().is_success() {
        return Err("http_error");
    }
    let m: Models = resp.json().await.map_err(|_| "bad_response")?;
    Ok(LmTest {
        latency_ms: t.elapsed().as_millis() as u64,
        models: m.data.into_iter().map(|d| d.id).take(100).collect(),
    })
}

/// 「測試連線」 for LM Studio.
#[tauri::command]
pub async fn settings_test_lm(
    window: WebviewWindow,
    base_url: String,
) -> Result<LmTest, &'static str> {
    check_caller(window.label())?;
    // The cloud client never follows redirects; LM Studio does not need any.
    let http = v2c_core::route::cloud_http_client().map_err(|_| "internal_error")?;
    let r = test_lm(http.client(), &base_url).await;
    match &r {
        Ok(t) => info!(
            latency_ms = t.latency_ms,
            models = t.models.len(),
            "LM Studio test ok"
        ),
        Err(code) => info!(code, "LM Studio test failed"),
    }
    r
}

#[cfg(test)]
mod tests {
    use v2c_core::config::{CloudService, NewlineKey};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn with_services() -> Config {
        let mut c = Config::with_defaults();
        for (id, kind) in [("g", CloudKind::Gemini), ("c", CloudKind::Claude)] {
            c.cloud.push(CloudService {
                id: id.into(),
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

    #[tokio::test]
    async fn lm_studio_test_lists_models() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "qwen2.5-3b-instruct"}, {"id": "gemma-3-4b"}]
            })))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let r = test_lm(&client, &format!("{}/v1/", server.uri()))
            .await
            .unwrap();
        assert_eq!(r.models, ["qwen2.5-3b-instruct", "gemma-3-4b"]);
        assert_eq!(
            test_lm(&client, &format!("{}/nope", server.uri())).await,
            Err("http_error")
        );
        assert_eq!(
            test_lm(&client, "http://127.0.0.1:9/v1").await,
            Err("unreachable")
        );
        assert_eq!(test_lm(&client, "nonsense").await, Err("invalid_url"));
    }

    #[test]
    fn only_the_main_window_may_call() {
        assert_eq!(check_caller("main"), Ok(()));
        assert_eq!(check_caller("overlay"), Err("forbidden"));
    }
}
