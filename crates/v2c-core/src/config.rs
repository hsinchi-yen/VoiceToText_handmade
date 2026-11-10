//! User configuration (`%APPDATA%\v2c\config.toml`).
//!
//! Every field has a default, so an empty or partial file is valid. Defaults are local-first:
//! nothing here requires an API key. API keys are NOT stored here; they live in the OS keyring
//! and are looked up by `CloudService::id`.

use serde::{Deserialize, Serialize};

use crate::{error::V2cError, Result};

/// Provider reference meaning "use the local model".
pub const LOCAL: &str = "local";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub route: RouteConfig,
    pub polish: PolishConfig,
    pub hotkey: HotkeyConfig,
    pub audio: AudioConfig,
    pub local: LocalConfig,
    pub gpu: GpuConfig,
    pub history: HistoryConfig,
    pub timeouts: TimeoutConfig,
    /// Per-app injection rules, matched by executable name (case-insensitive).
    pub apps: Vec<AppRule>,
    /// Cloud services the user connected through the setup wizard.
    pub cloud: Vec<CloudService>,
}

impl Config {
    pub fn from_toml_str(s: &str) -> Result<Self> {
        let mut cfg: Config = toml::from_str(s).map_err(|e| V2cError::Config(e.to_string()))?;
        if cfg.apps.is_empty() {
            cfg.apps = default_app_rules();
        }
        Ok(cfg)
    }

    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| V2cError::Config(e.to_string()))
    }

    /// Defaults including the built-in app rules.
    pub fn with_defaults() -> Self {
        Config {
            apps: default_app_rules(),
            ..Config::default()
        }
    }

    pub fn cloud_service(&self, id: &str) -> Option<&CloudService> {
        self.cloud.iter().find(|c| c.id == id)
    }
}

/// Which provider handles each purpose: `"local"` or a `CloudService::id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouteConfig {
    pub asr: String,
    pub polish: String,
    pub translate: String,
    /// If a cloud call fails, retry with the local provider.
    pub cloud_fallback_to_local: bool,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            asr: LOCAL.into(),
            polish: LOCAL.into(),
            translate: LOCAL.into(),
            cloud_fallback_to_local: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolishConfig {
    pub enabled: bool,
    /// Give up on the LLM after this long and fall back to rule-based filler removal.
    pub timeout_ms: u64,
}

impl Default for PolishConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub dictate: String,
    pub translate: String,
    /// Held at least this long = push-to-talk; shorter = a tap.
    pub hold_threshold_ms: u64,
    /// Second tap within this window enters hands-free mode.
    pub double_tap_window_ms: u64,
    pub hands_free_max_secs: u64,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            dictate: "RightCtrl".into(),
            translate: "RightCtrl+Shift".into(),
            hold_threshold_ms: 350,
            double_tap_window_ms: 300,
            hands_free_max_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MicMode {
    /// Stream stays open; the last `preroll_ms` are kept in memory so the first word isn't lost.
    #[default]
    AlwaysOn,
    /// Open the microphone only while the hotkey is active.
    OnPress,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub mic_mode: MicMode,
    pub preroll_ms: u64,
    /// Input device name; `None` = system default.
    pub device: Option<String>,
    /// Clips shorter than this are discarded.
    pub min_clip_ms: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            mic_mode: MicMode::AlwaysOn,
            preroll_ms: 500,
            device: None,
            min_clip_ms: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LocalAsr {
    /// Breeze on GPU when CUDA and the model are available, otherwise SenseVoice on CPU.
    #[default]
    Auto,
    Breeze,
    SenseVoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BreezeQuant {
    #[default]
    Q5_0,
    Q8_0,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub asr: LocalAsr,
    pub breeze_quant: BreezeQuant,
    /// OpenAI-compatible endpoint of the user's own LM Studio.
    pub llm_base_url: String,
    /// Model id as shown by LM Studio; `None` = whatever model is loaded.
    pub llm_model: Option<String>,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            asr: LocalAsr::Auto,
            breeze_quant: BreezeQuant::Q5_0,
            llm_base_url: "http://localhost:1234/v1".into(),
            llm_model: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GpuProfile {
    #[default]
    Balanced,
    LowVram,
    CpuOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeepLoaded {
    Always,
    /// Unload when idle or when free VRAM drops below the reserve.
    #[default]
    Auto,
    OnDemand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuConfig {
    pub enabled: bool,
    pub profile: GpuProfile,
    /// VRAM kept free for display output and other apps.
    pub vram_reserve_mb: u64,
    pub keep_loaded: KeepLoaded,
    pub idle_unload_minutes: u64,
    pub yield_on_fullscreen: bool,
    /// Executables that make v2c switch to CPU while they are in the foreground.
    pub heavy_apps: Vec<String>,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            profile: GpuProfile::Balanced,
            vram_reserve_mb: 600,
            keep_loaded: KeepLoaded::Auto,
            idle_unload_minutes: 15,
            yield_on_fullscreen: true,
            heavy_apps: vec!["blender.exe".into(), "Adobe Premiere Pro.exe".into()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub retention_days: u32,
    pub max_entries: u32,
    /// Never record history while these executables are in the foreground.
    pub exclude_apps: Vec<String>,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 7,
            max_entries: 200,
            exclude_apps: vec![
                "1Password.exe".into(),
                "Bitwarden.exe".into(),
                "KeePassXC.exe".into(),
                "KeePass.exe".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeoutConfig {
    pub asr_cloud_ms: u64,
    pub llm_cloud_ms: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            asr_cloud_ms: 5000,
            llm_cloud_ms: 3000,
        }
    }
}

/// How a line break is typed into an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NewlineKey {
    #[default]
    Enter,
    /// Chat apps send the message on Enter, so line breaks use Shift+Enter.
    ShiftEnter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRule {
    /// Executable name, e.g. `LINE.exe`. Matched case-insensitively.
    pub exe: String,
    #[serde(default)]
    pub newline: NewlineKey,
    /// Always use clipboard paste instead of simulated typing.
    #[serde(default)]
    pub always_paste: bool,
}

/// Built-in rules: chat apps and browsers use Shift+Enter for line breaks; VM consoles
/// always paste because they forward scan codes and drop `KEYEVENTF_UNICODE` (VK_PACKET)
/// input (pasting there needs the guest's clipboard sharing enabled).
pub fn default_app_rules() -> Vec<AppRule> {
    let chat = [
        "LINE.exe",
        "ms-teams.exe",
        "Teams.exe",
        "slack.exe",
        "Discord.exe",
        "Telegram.exe",
        "chrome.exe",
        "msedge.exe",
        "firefox.exe",
    ]
    .into_iter()
    .map(|exe| AppRule {
        exe: exe.into(),
        newline: NewlineKey::ShiftEnter,
        always_paste: false,
    });
    let vm_consoles = [
        "vmware.exe",       // VMware Workstation
        "vmplayer.exe",     // VMware Workstation Player
        "vmware-view.exe",  // VMware / Omnissa Horizon Client
        "vmrc.exe",         // VMware Remote Console
        "VirtualBoxVM.exe", // VirtualBox VM window
    ]
    .into_iter()
    .map(|exe| AppRule {
        exe: exe.into(),
        newline: NewlineKey::Enter,
        always_paste: true,
    });
    chat.chain(vm_consoles).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudKind {
    OpenAi,
    Gemini,
    /// Text only: polish and translate, no speech recognition.
    Claude,
    /// Any OpenAI-compatible endpoint (self-hosted service, LM Studio on another PC).
    OpenAiCompat,
}

impl CloudKind {
    pub fn supports_asr(self) -> bool {
        !matches!(self, CloudKind::Claude)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudService {
    /// Stable id used in `[route]` and as the keyring entry name.
    pub id: String,
    /// Display name chosen by the user (`OpenAiCompat` only). Shown instead of `id` when set;
    /// never used for routing, the keyring, or logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: CloudKind,
    /// Required for `OpenAiCompat`; optional override for the others.
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub asr_model: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_local_first_defaults() {
        let cfg = Config::from_toml_str("").unwrap();
        assert_eq!(cfg.route.asr, LOCAL);
        assert_eq!(cfg.route.polish, LOCAL);
        assert_eq!(cfg.route.translate, LOCAL);
        assert!(cfg.route.cloud_fallback_to_local);
        assert!(cfg.cloud.is_empty());
        assert_eq!(cfg.apps, default_app_rules());
    }

    #[test]
    fn defaults_match_plan() {
        let c = Config::with_defaults();
        assert_eq!(c.polish.timeout_ms, 1000);
        assert_eq!(c.hotkey.dictate, "RightCtrl");
        assert_eq!(c.hotkey.translate, "RightCtrl+Shift");
        assert_eq!(c.hotkey.hold_threshold_ms, 350);
        assert_eq!(c.hotkey.double_tap_window_ms, 300);
        assert_eq!(c.hotkey.hands_free_max_secs, 300);
        assert_eq!(c.audio.mic_mode, MicMode::AlwaysOn);
        assert_eq!(c.audio.preroll_ms, 500);
        assert_eq!(c.audio.min_clip_ms, 300);
        assert_eq!(c.local.llm_base_url, "http://localhost:1234/v1");
        assert_eq!(c.local.breeze_quant, BreezeQuant::Q5_0);
        assert_eq!(c.gpu.vram_reserve_mb, 600);
        assert_eq!(c.gpu.idle_unload_minutes, 15);
        assert_eq!(c.gpu.keep_loaded, KeepLoaded::Auto);
        assert_eq!(c.history.retention_days, 7);
        assert_eq!(c.history.max_entries, 200);
        assert_eq!(c.timeouts.asr_cloud_ms, 5000);
        assert_eq!(c.timeouts.llm_cloud_ms, 3000);
    }

    #[test]
    fn default_app_rules_paste_only_into_vm_consoles() {
        let rules = default_app_rules();
        let paste: Vec<&str> = rules
            .iter()
            .filter(|r| r.always_paste)
            .map(|r| r.exe.as_str())
            .collect();
        assert_eq!(
            paste,
            [
                "vmware.exe",
                "vmplayer.exe",
                "vmware-view.exe",
                "vmrc.exe",
                "VirtualBoxVM.exe"
            ]
        );
        assert!(rules
            .iter()
            .filter(|r| r.exe == "LINE.exe" || r.exe == "chrome.exe")
            .all(|r| !r.always_paste && r.newline == NewlineKey::ShiftEnter));
    }

    #[test]
    fn roundtrip_toml() {
        let mut c = Config::with_defaults();
        c.cloud.push(CloudService {
            id: "my-gemini".into(),
            name: None,
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: Some("gemini-flash".into()),
            llm_model: None,
        });
        c.route.asr = "my-gemini".into();
        let s = c.to_toml_string().unwrap();
        let back = Config::from_toml_str(&s).unwrap();
        assert_eq!(back, c);
        assert_eq!(
            back.cloud_service("my-gemini").unwrap().kind,
            CloudKind::Gemini
        );
    }

    #[test]
    fn cloud_name_is_optional_and_omitted_when_unset() {
        let old = "[[cloud]]
id = \"custom\"
kind = \"open_ai_compat\"
base_url = \"http://h/v1\"
";
        let cfg = Config::from_toml_str(old).unwrap();
        assert_eq!(cfg.cloud_service("custom").unwrap().name, None);
        assert!(!cfg.to_toml_string().unwrap().contains("name"));

        let named = format!(
            "{old}name = \"區網 Qwen\"
"
        );
        let cfg = Config::from_toml_str(&named).unwrap();
        assert_eq!(
            cfg.cloud_service("custom").unwrap().name.as_deref(),
            Some("區網 Qwen")
        );
        let back = Config::from_toml_str(&cfg.to_toml_string().unwrap()).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let cfg = Config::from_toml_str("[gpu]\nprofile = \"cpu_only\"\n").unwrap();
        assert_eq!(cfg.gpu.profile, GpuProfile::CpuOnly);
        assert_eq!(cfg.gpu.vram_reserve_mb, 600);
        assert_eq!(cfg.audio.mic_mode, MicMode::AlwaysOn);
    }

    #[test]
    fn claude_has_no_asr() {
        assert!(!CloudKind::Claude.supports_asr());
        assert!(CloudKind::Gemini.supports_asr());
    }

    #[test]
    fn invalid_toml_is_config_error() {
        assert!(matches!(
            Config::from_toml_str("route = 5"),
            Err(V2cError::Config(_))
        ));
    }
}
