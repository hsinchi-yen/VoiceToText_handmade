//! LAN services: loading the bundled TN / RD preset files and calling them.
//!
//! Read only: the preset files are parsed, never written or merged into any config. The
//! per-service default API keys (Breeze) are kept in memory and never leave this module:
//! no `Debug` output, error or return value carries them.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use v2c_core::asr::{AsrContext, AsrProvider};
use v2c_core::audio::AudioClip;
use v2c_core::config::{CloudKind, CloudService, TimeoutConfig};
use v2c_core::error::{ProviderErrorKind, Stage};
use v2c_core::llm::LlmProvider;
use v2c_core::postprocess::zh_tw::to_zh_tw;
use v2c_core::prompts;
use v2c_core::route::{build_cloud_asr, build_cloud_llm, cloud_http_client, CloudHttp};
use v2c_core::secrets::{ApiKey, MemorySecretStore, SecretStore};
use v2c_core::V2cError;

/// Chinese-to-English translation can take a while on the 27B model; fixed here because the
/// Android app has no settings. Deliberately longer than the desktop `llm_cloud_ms` default
/// of 3000 ms.
const TRANSLATE_TIMEOUT_MS: u64 = 8000;

/// Error codes only: never a message, never user text or keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// Preset file could not be parsed or is inconsistent.
    #[error("config")]
    Config,
    /// `load_lan_services` has not succeeded yet.
    #[error("not_loaded")]
    NotLoaded,
    #[error("unknown_service")]
    UnknownService,
    /// The service has no ASR model / no LLM model.
    #[error("unsupported")]
    Unsupported,
    #[error("unauthorized")]
    Unauthorized,
    #[error("rate_limited")]
    RateLimited,
    #[error("unreachable")]
    Unreachable,
    #[error("bad_response")]
    BadResponse,
    #[error("server")]
    Server,
    #[error("timeout")]
    Timeout,
    #[error("invalid_audio")]
    InvalidAudio,
    /// `translate` got empty or whitespace-only text; no request was made.
    #[error("empty_text")]
    EmptyText,
    #[error("internal")]
    Internal,
}

impl From<V2cError> for FfiError {
    fn from(e: V2cError) -> Self {
        match e {
            V2cError::Provider { kind, .. } => match kind {
                ProviderErrorKind::Unauthorized => FfiError::Unauthorized,
                ProviderErrorKind::RateLimited => FfiError::RateLimited,
                ProviderErrorKind::Unreachable => FfiError::Unreachable,
                ProviderErrorKind::Unsupported => FfiError::Unsupported,
                ProviderErrorKind::BadResponse => FfiError::BadResponse,
                ProviderErrorKind::Server => FfiError::Server,
            },
            V2cError::Timeout { .. } => FfiError::Timeout,
            V2cError::NotConfigured(_) => FfiError::Unsupported,
            V2cError::Config(_) => FfiError::Config,
            _ => FfiError::Internal,
        }
    }
}

/// One LAN service as shown to Kotlin. No keys.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LanService {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub has_asr: bool,
    pub has_llm: bool,
    /// Effective ASR timeout in ms (`asr_timeout_ms` or the global default).
    pub asr_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LanServices {
    pub services: Vec<LanService>,
}

/// Cloud provider kinds the Android settings offer. `Claude` has no speech recognition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CloudProvider {
    Gemini,
    Openai,
    Claude,
    /// Any OpenAI-compatible endpoint; needs `base_url`.
    Custom,
}

impl CloudProvider {
    fn kind(self) -> CloudKind {
        match self {
            CloudProvider::Gemini => CloudKind::Gemini,
            CloudProvider::Openai => CloudKind::OpenAi,
            CloudProvider::Claude => CloudKind::Claude,
            CloudProvider::Custom => CloudKind::OpenAiCompat,
        }
    }
}

/// What a cloud service is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CloudUse {
    Asr,
    Translate,
}

/// One user-configured cloud service. The API key travels separately in
/// `set_cloud_services` and is never part of this record.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CloudServiceConfig {
    /// Stable id Kotlin passes as `service_id`; must not clash with a LAN service id.
    pub id: String,
    pub provider: CloudProvider,
    /// Text model (translation).
    pub model: String,
    /// Speech recognition model; empty / `None` uses the provider's suggested one.
    pub asr_model: Option<String>,
    /// Required for `Custom`; optional override otherwise. HTTPS, or HTTP on loopback only.
    pub base_url: Option<String>,
    pub uses: Vec<CloudUse>,
}

/// Result of [`crate::test_connection`]: `error` is `None` on success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TestResult {
    pub latency_ms: u64,
    pub error: Option<FfiError>,
}

const TEST_TONE_RATE: u32 = 16_000;
const TEST_SENTENCE: &str = "你好，這是連線測試。";

/// One second of a quiet 440 Hz tone as 16-bit little-endian mono PCM.
fn test_tone_pcm() -> Vec<u8> {
    (0..TEST_TONE_RATE)
        .flat_map(|i| {
            let t = i as f32 / TEST_TONE_RATE as f32;
            let v = (0.1 * (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 32767.0) as i16;
            v.to_le_bytes()
        })
        .collect()
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Cloud endpoints must be HTTPS; plain HTTP only for loopback (tests, local proxies).
fn url_allowed(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return true;
    }
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// `[preset]` as far as this crate cares: only the default keys.
#[derive(Default, Deserialize)]
#[serde(default)]
struct PresetSection {
    default_keys: HashMap<String, String>,
}

impl std::fmt::Debug for PresetSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresetSection")
            .field("default_keys", &self.default_keys.len())
            .finish()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PresetFile {
    preset: PresetSection,
    cloud: Vec<CloudService>,
}

/// Parsed services plus their in-memory keys and the shared HTTP client.
pub(crate) struct Registry {
    services: HashMap<String, CloudService>,
    order: Vec<String>,
    keys: MemorySecretStore,
    http: CloudHttp,
    /// Per-service purpose limits (`CloudServiceConfig::uses`). Services without an entry
    /// (the LAN ones) are unrestricted.
    uses: HashMap<String, Vec<CloudUse>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("services", &self.order)
            .field("keys", &self.keys)
            .finish()
    }
}

impl Registry {
    /// Reads `[[cloud]]` and `[preset] default_keys` from each file. Any parse problem, an
    /// empty service list or a duplicate id fails the whole load.
    pub(crate) fn from_toml(files: &[&str]) -> Result<Self, FfiError> {
        let keys = MemorySecretStore::new();
        let mut services = HashMap::new();
        let mut order = Vec::new();
        for text in files {
            let file: PresetFile = toml::from_str(text).map_err(|_| FfiError::Config)?;
            if file.cloud.is_empty() {
                return Err(FfiError::Config);
            }
            for svc in file.cloud {
                if svc.id.is_empty() || services.contains_key(&svc.id) {
                    return Err(FfiError::Config);
                }
                if let Some(key) = file.preset.default_keys.get(&svc.id) {
                    keys.set(&svc.id, &ApiKey::new(key.clone()))
                        .map_err(FfiError::from)?;
                }
                order.push(svc.id.clone());
                services.insert(svc.id.clone(), svc);
            }
        }
        Ok(Registry {
            services,
            order,
            keys,
            http: cloud_http_client().map_err(FfiError::from)?,
            uses: HashMap::new(),
        })
    }

    /// Builds the cloud registry from the user's settings. Keys stay in memory only; a key
    /// for an unknown id is ignored. Any bad entry fails the whole call.
    pub(crate) fn from_cloud(
        configs: Vec<CloudServiceConfig>,
        keys: HashMap<String, String>,
    ) -> Result<Self, FfiError> {
        let store = MemorySecretStore::new();
        let mut services = HashMap::new();
        let mut order = Vec::new();
        let mut uses = HashMap::new();
        for c in configs {
            let id = c.id.trim().to_owned();
            if id.is_empty() || services.contains_key(&id) {
                return Err(FfiError::Config);
            }
            let base_url = non_empty(c.base_url);
            match &base_url {
                None if c.provider == CloudProvider::Custom => return Err(FfiError::Config),
                Some(u) if !url_allowed(u) => return Err(FfiError::Config),
                _ => {}
            }
            let svc = CloudService {
                id: id.clone(),
                name: None,
                kind: c.provider.kind(),
                base_url,
                asr_model: non_empty(c.asr_model),
                llm_model: non_empty(Some(c.model)),
                asr_timeout_ms: None,
            };
            if let Some(key) = keys.get(&id).filter(|k| !k.is_empty()) {
                store
                    .set(&id, &ApiKey::new(key.clone()))
                    .map_err(FfiError::from)?;
            }
            uses.insert(id.clone(), c.uses);
            order.push(id.clone());
            services.insert(id, svc);
        }
        Ok(Registry {
            services,
            order,
            keys: store,
            http: cloud_http_client().map_err(FfiError::from)?,
            uses,
        })
    }

    fn has_service(&self, id: &str) -> bool {
        self.services.contains_key(id)
    }

    pub(crate) fn summary(&self) -> LanServices {
        let t = TimeoutConfig::default();
        LanServices {
            services: self
                .order
                .iter()
                .map(|id| {
                    let s = &self.services[id];
                    LanService {
                        id: s.id.clone(),
                        name: s.name.clone().unwrap_or_else(|| s.id.clone()),
                        base_url: s.base_url.clone().unwrap_or_default(),
                        has_asr: s.asr_model.is_some(),
                        has_llm: s.llm_model.is_some(),
                        asr_timeout_ms: s.asr_timeout(&t).as_millis() as u64,
                    }
                })
                .collect(),
        }
    }

    fn service(&self, id: &str) -> Result<&CloudService, FfiError> {
        self.services.get(id).ok_or(FfiError::UnknownService)
    }

    /// `Unsupported` when the service is not set up for this purpose or its kind cannot do
    /// it (Claude has no speech recognition); `Unauthorized` when an official API has no key.
    fn check_use(&self, svc: &CloudService, purpose: CloudUse) -> Result<(), FfiError> {
        if let Some(allowed) = self.uses.get(&svc.id) {
            if !allowed.contains(&purpose) {
                return Err(FfiError::Unsupported);
            }
        }
        if purpose == CloudUse::Asr && !svc.kind.supports_asr() {
            return Err(FfiError::Unsupported);
        }
        let has_key = matches!(self.keys.get(&svc.id), Ok(Some(k)) if !k.is_empty());
        if !has_key && svc.kind != CloudKind::OpenAiCompat {
            return Err(FfiError::Unauthorized);
        }
        Ok(())
    }

    /// Connection test: a short generated tone for recognition, a fixed sentence for
    /// translation. Returns the wall time in ms.
    pub(crate) async fn test(&self, service_id: &str, purpose: CloudUse) -> Result<u64, FfiError> {
        let started = Instant::now();
        match purpose {
            CloudUse::Asr => {
                self.transcribe(test_tone_pcm(), TEST_TONE_RATE, service_id)
                    .await?;
            }
            CloudUse::Translate => {
                self.translate(TEST_SENTENCE, service_id).await?;
            }
        }
        Ok(started.elapsed().as_millis() as u64)
    }

    #[cfg(test)]
    pub(crate) fn has_key(&self, id: &str) -> bool {
        matches!(self.keys.get(id), Ok(Some(_)))
    }

    pub(crate) async fn transcribe(
        &self,
        pcm_le: Vec<u8>,
        rate: u32,
        service_id: &str,
    ) -> Result<String, FfiError> {
        // 16-bit little-endian mono: an odd byte count is a broken buffer.
        if pcm_le.is_empty() || !pcm_le.len().is_multiple_of(2) || rate == 0 {
            return Err(FfiError::InvalidAudio);
        }
        let svc = self.service(service_id)?;
        self.check_use(svc, CloudUse::Asr)?;
        let timeout = svc.asr_timeout(&TimeoutConfig::default());
        let asr: Box<dyn AsrProvider> = build_cloud_asr(svc, &self.keys, &self.http, timeout)
            .map_err(FfiError::from)?
            .ok_or(FfiError::Unsupported)?;
        let clip = AudioClip {
            samples: pcm_le
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)
                .collect(),
            sample_rate: rate,
        };
        let t = asr
            .transcribe(&clip, &AsrContext::default())
            .await
            .map_err(FfiError::from)?;
        Ok(t.text)
    }

    pub(crate) async fn translate(&self, text: &str, service_id: &str) -> Result<String, FfiError> {
        if text.trim().is_empty() {
            return Err(FfiError::EmptyText);
        }
        let svc = self.service(service_id)?;
        self.check_use(svc, CloudUse::Translate)?;
        let llm: Box<dyn LlmProvider> = build_cloud_llm(
            svc,
            &self.keys,
            &self.http,
            Duration::from_millis(TRANSLATE_TIMEOUT_MS),
            Stage::Translate,
        )
        .map_err(FfiError::from)?
        .ok_or(FfiError::Unsupported)?;
        let out = llm
            .complete(&prompts::translate(&[]), &prompts::wrap_transcript(text))
            .await
            .map_err(FfiError::from)?;
        let out = to_zh_tw(out.trim());
        if out.is_empty() {
            return Err(FfiError::BadResponse);
        }
        Ok(out)
    }
}

static REGISTRY: RwLock<Option<Arc<Registry>>> = RwLock::new(None);

pub(crate) fn set_registry(r: Registry) {
    // A poisoned lock only means another thread panicked; the value is replaced anyway.
    let mut g = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    *g = Some(Arc::new(r));
}

/// The user's cloud services; replaced as a whole by `set_cloud_services`, independent of the
/// LAN registry above.
static CLOUD: RwLock<Option<Arc<Registry>>> = RwLock::new(None);

pub(crate) fn set_cloud(r: Registry) {
    let mut g = CLOUD.write().unwrap_or_else(|e| e.into_inner());
    *g = Some(Arc::new(r));
}

/// Whether a LAN service already uses `id`.
pub(crate) fn lan_has(id: &str) -> bool {
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|r| r.has_service(id))
}

/// The registry that owns `id` (LAN first, then cloud). `NotLoaded` while the LAN presets
/// are not loaded and the id is not a cloud one; `UnknownService` otherwise.
pub(crate) fn registry_for(id: &str) -> Result<Arc<Registry>, FfiError> {
    let lan = REGISTRY.read().unwrap_or_else(|e| e.into_inner()).clone();
    let cloud = CLOUD.read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(r) = lan.iter().chain(cloud.iter()).find(|r| r.has_service(id)) {
        return Ok(r.clone());
    }
    if lan.is_none() {
        Err(FfiError::NotLoaded)
    } else {
        Err(FfiError::UnknownService)
    }
}

/// The runtime behind the synchronous FFI calls (Kotlin calls from `Dispatchers.IO`).
pub(crate) fn runtime() -> Result<&'static tokio::runtime::Runtime, FfiError> {
    static RT: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .ok()
    })
    .as_ref()
    .ok_or(FfiError::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TN: &str =
        include_str!("../../../apps/desktop/src-tauri/presets/TN_CUSTOM_LLM_ADDR.toml");
    const RD: &str =
        include_str!("../../../apps/desktop/src-tauri/presets/RD_CUSTOM_LLM_ADDR.toml");

    #[test]
    fn bundled_presets_load() {
        let r = Registry::from_toml(&[TN, RD]).unwrap();
        let s = r.summary();
        let ids: Vec<&str> = s.services.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "tn-tea-asr",
                "tn-breeze-asr",
                "tn-qwen",
                "rd-tea-asr",
                "rd-breeze-asr",
                "rd-qwen"
            ]
        );
        for p in ["tn", "rd"] {
            assert!(r.has_key(&format!("{p}-breeze-asr")), "{p} breeze key");
            assert!(r.has_key(&format!("{p}-tea-asr")), "{p} tea key");
        }
        let breeze = s.services.iter().find(|x| x.id == "tn-breeze-asr").unwrap();
        assert_eq!(breeze.asr_timeout_ms, 8000);
        assert!(breeze.has_asr && !breeze.has_llm);
        let tea = s.services.iter().find(|x| x.id == "tn-tea-asr").unwrap();
        assert_eq!(tea.asr_timeout_ms, TimeoutConfig::default().asr_cloud_ms);
        let qwen = s.services.iter().find(|x| x.id == "tn-qwen").unwrap();
        assert!(qwen.has_llm && !qwen.has_asr);
    }

    #[test]
    fn each_bundled_file_loads_alone() {
        assert_eq!(
            Registry::from_toml(&[TN]).unwrap().summary().services.len(),
            3
        );
        assert_eq!(
            Registry::from_toml(&[RD]).unwrap().summary().services.len(),
            3
        );
    }

    #[test]
    fn malformed_input_is_a_config_error() {
        for bad in [
            "this is [not toml",
            "[[cloud]]\nid = 5\nkind = \"open_ai_compat\"",
            "[[cloud]]\nid = \"x\"\nkind = \"nope\"",
            "[preset]\ndefault_keys = 3\n[[cloud]]\nid = \"x\"\nkind = \"open_ai_compat\"",
            "",
            "[preset]\ntitle = \"no services\"",
        ] {
            assert_eq!(
                Registry::from_toml(&[TN, bad]).unwrap_err(),
                FfiError::Config,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        assert_eq!(
            Registry::from_toml(&[TN, TN]).unwrap_err(),
            FfiError::Config
        );
    }

    #[test]
    fn debug_and_summary_never_contain_keys() {
        let key = toml::from_str::<PresetFile>(TN)
            .unwrap()
            .preset
            .default_keys
            .into_values()
            .next()
            .unwrap();
        assert!(!key.is_empty());
        let r = Registry::from_toml(&[TN, RD]).unwrap();
        let shown = format!("{r:?} {:?}", r.summary());
        assert!(!shown.contains(&key), "key leaked");
        let parsed: PresetFile = toml::from_str(TN).unwrap();
        assert!(!format!("{parsed:?}").contains(&key));
    }

    #[test]
    fn errors_are_codes_only() {
        for e in [FfiError::Config, FfiError::Unauthorized, FfiError::Internal] {
            let shown = format!("{e} {e:?}");
            assert!(shown.len() < 40, "{shown}");
        }
        let e: FfiError = V2cError::Config("secret text 你好".into()).into();
        assert_eq!(e, FfiError::Config);
        assert!(!format!("{e} {e:?}").contains("secret"));
    }

    fn mock_registry(uri: &str, key: Option<&str>) -> Registry {
        let keys = key
            .map(|k| format!("[preset]\ndefault_keys = {{ \"t-asr\" = \"{k}\" }}\n"))
            .unwrap_or_default();
        let toml = format!(
            "{keys}\n[[cloud]]\nid = \"t-asr\"\nkind = \"open_ai_compat\"\nbase_url = \"{uri}/v1\"\n\
             asr_model = \"m\"\nasr_timeout_ms = 500\n\n[[cloud]]\nid = \"t-llm\"\n\
             kind = \"open_ai_compat\"\nbase_url = \"{uri}/v1\"\nllm_model = \"q\"\n"
        );
        Registry::from_toml(&[&toml]).unwrap()
    }

    /// Mock servers live on their own runtime thread; the code under test uses its own.
    fn with_server<T>(f: impl FnOnce(&MockServer, &tokio::runtime::Runtime) -> T) -> T {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        f(&server, &rt)
    }

    #[test]
    fn transcribe_sends_default_key_and_returns_text() {
        with_server(|server, rt| {
            rt.block_on(
                Mock::given(method("POST"))
                    .and(path("/v1/audio/transcriptions"))
                    .and(wiremock::matchers::header("authorization", "Bearer k-123"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(serde_json::json!({ "text": "hello" })),
                    )
                    .expect(1)
                    .mount(server),
            );
            let r = mock_registry(&server.uri(), Some("k-123"));
            let out = runtime()
                .unwrap()
                .block_on(r.transcribe(vec![100; 3200], 16000, "t-asr"))
                .unwrap();
            assert_eq!(out, "hello");
        });
    }

    #[test]
    fn transcribe_error_codes() {
        with_server(|server, rt| {
            rt.block_on(
                Mock::given(method("POST"))
                    .respond_with(ResponseTemplate::new(401))
                    .mount(server),
            );
            let r = mock_registry(&server.uri(), None);
            let run = |pcm: Vec<u8>, rate, id: &str| {
                runtime().unwrap().block_on(r.transcribe(pcm, rate, id))
            };
            assert_eq!(
                run(vec![1; 320], 16000, "t-asr").unwrap_err(),
                FfiError::Unauthorized
            );
            assert_eq!(
                run(vec![1; 320], 16000, "nope").unwrap_err(),
                FfiError::UnknownService
            );
            // Odd byte length is not 16-bit PCM.
            assert_eq!(
                run(vec![1; 321], 16000, "t-asr").unwrap_err(),
                FfiError::InvalidAudio
            );
            assert_eq!(
                run(vec![], 16000, "t-asr").unwrap_err(),
                FfiError::InvalidAudio
            );
            assert_eq!(
                run(vec![1; 2], 0, "t-asr").unwrap_err(),
                FfiError::InvalidAudio
            );
            // A service without an ASR model.
            assert_eq!(
                run(vec![1; 320], 16000, "t-llm").unwrap_err(),
                FfiError::Unsupported
            );
        });
    }

    #[test]
    fn translate_returns_llm_text() {
        with_server(|server, rt| {
            rt.block_on(
                Mock::given(method("POST"))
                    .and(path("/v1/chat/completions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "choices": [{ "message": { "role": "assistant", "content": " Good morning. " } }]
                    })))
                    .mount(server),
            );
            let r = mock_registry(&server.uri(), None);
            let out = runtime()
                .unwrap()
                .block_on(r.translate("早安", "t-llm"))
                .unwrap();
            assert_eq!(out, "Good morning.");
        });
    }

    #[test]
    fn translate_rejects_blank_text_without_a_request() {
        with_server(|server, rt| {
            let r = mock_registry(&server.uri(), None);
            for blank in ["", "   ", "\n\t "] {
                let e = runtime()
                    .unwrap()
                    .block_on(r.translate(blank, "t-llm"))
                    .unwrap_err();
                assert_eq!(e, FfiError::EmptyText);
            }
            assert!(rt.block_on(server.received_requests()).unwrap().is_empty());
        });
    }

    #[test]
    fn transcribe_times_out_per_service() {
        with_server(|server, rt| {
            rt.block_on(
                Mock::given(method("POST"))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(serde_json::json!({ "text": "late" }))
                            .set_delay(Duration::from_secs(1)),
                    )
                    .mount(server),
            );
            // `t-asr` has asr_timeout_ms = 500.
            let r = mock_registry(&server.uri(), None);
            let e = runtime()
                .unwrap()
                .block_on(r.transcribe(vec![1; 320], 16000, "t-asr"))
                .unwrap_err();
            assert_eq!(e, FfiError::Timeout);
        });
    }
}

#[cfg(test)]
mod cloud_tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg(id: &str, provider: CloudProvider, base: Option<&str>) -> CloudServiceConfig {
        CloudServiceConfig {
            id: id.into(),
            provider,
            model: "m".into(),
            asr_model: Some("am".into()),
            base_url: base.map(str::to_owned),
            uses: vec![CloudUse::Asr, CloudUse::Translate],
        }
    }

    fn keys(id: &str, key: &str) -> HashMap<String, String> {
        HashMap::from([(id.to_owned(), key.to_owned())])
    }

    fn with_server<T>(f: impl FnOnce(&MockServer, &tokio::runtime::Runtime) -> T) -> T {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        f(&server, &rt)
    }

    fn run<T>(f: impl std::future::Future<Output = T>) -> T {
        runtime().unwrap().block_on(f)
    }

    const PCM: usize = 3200;

    #[test]
    fn config_validation() {
        let ok = |c: CloudServiceConfig| Registry::from_cloud(vec![c], HashMap::new());
        // Custom needs a base_url.
        assert_eq!(
            ok(cfg("c", CloudProvider::Custom, None)).unwrap_err(),
            FfiError::Config
        );
        assert_eq!(
            ok(cfg("c", CloudProvider::Custom, Some("  "))).unwrap_err(),
            FfiError::Config
        );
        // Plain http is refused for every provider unless the host is loopback.
        for p in [
            CloudProvider::Gemini,
            CloudProvider::Openai,
            CloudProvider::Claude,
            CloudProvider::Custom,
        ] {
            for bad in [
                "http://example.com/v1",
                "http://192.168.1.5:1234/v1",
                "http://127.0.0.1.evil.com/v1",
                "http://localhost@evil.com/v1",
                "ftp://example.com",
                "example.com/v1",
            ] {
                assert_eq!(
                    ok(cfg("c", p, Some(bad))).unwrap_err(),
                    FfiError::Config,
                    "{p:?} {bad}"
                );
            }
            for good in [
                "https://example.com/v1",
                "HTTPS://example.com",
                "http://127.0.0.1:8080/v1",
                "http://localhost:9/v1",
                "http://[::1]:9/v1",
            ] {
                assert!(ok(cfg("c", p, Some(good))).is_ok(), "{p:?} {good}");
            }
        }
        // Official providers need no base_url.
        assert!(ok(cfg("c", CloudProvider::Gemini, None)).is_ok());
        // Empty and duplicate ids.
        assert_eq!(
            ok(cfg(" ", CloudProvider::Gemini, None)).unwrap_err(),
            FfiError::Config
        );
        assert_eq!(
            Registry::from_cloud(
                vec![
                    cfg("c", CloudProvider::Gemini, None),
                    cfg("c", CloudProvider::Openai, None)
                ],
                HashMap::new()
            )
            .unwrap_err(),
            FfiError::Config
        );
        // An empty list is valid (clears the cloud services).
        assert!(Registry::from_cloud(vec![], HashMap::new()).is_ok());
    }

    #[test]
    fn keys_stay_out_of_debug_output() {
        let r = Registry::from_cloud(
            vec![cfg("c", CloudProvider::Openai, None)],
            keys("c", "sk-SECRET-123"),
        )
        .unwrap();
        assert!(r.has_key("c"));
        assert!(!format!("{r:?}").contains("sk-SECRET"));
    }

    #[test]
    fn openai_asr_and_translate() {
        with_server(|server, rt| {
            rt.block_on(async {
                Mock::given(method("POST"))
                    .and(path("/v1/audio/transcriptions"))
                    .and(header("authorization", "Bearer sk-1"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "hi"})))
                    .expect(1)
                    .mount(server)
                    .await;
                Mock::given(method("POST"))
                    .and(path("/v1/chat/completions"))
                    .and(header("authorization", "Bearer sk-1"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "choices": [{"message": {"role": "assistant", "content": "Hello."}}]
                    })))
                    .expect(1)
                    .mount(server)
                    .await;
            });
            let base = format!("{}/v1", server.uri());
            let r = Registry::from_cloud(
                vec![cfg("o", CloudProvider::Openai, Some(&base))],
                keys("o", "sk-1"),
            )
            .unwrap();
            assert_eq!(run(r.transcribe(vec![9; PCM], 16000, "o")).unwrap(), "hi");
            assert_eq!(run(r.translate("你好", "o")).unwrap(), "Hello.");
        });
    }

    #[test]
    fn gemini_asr_and_translate() {
        with_server(|server, rt| {
            rt.block_on(
                Mock::given(method("POST"))
                    .and(path("/v1beta/models/am:generateContent"))
                    .and(header("x-goog-api-key", "AIza-1"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "candidates": [{"content": {"parts": [{"text": "from gemini"}]},
                                        "finishReason": "STOP"}]
                    })))
                    .expect(1)
                    .mount(server),
            );
            rt.block_on(
                Mock::given(method("POST"))
                    .and(path("/v1beta/models/m:generateContent"))
                    .and(header("x-goog-api-key", "AIza-1"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "candidates": [{"content": {"parts": [{"text": "Good."}]},
                                        "finishReason": "STOP"}]
                    })))
                    .expect(1)
                    .mount(server),
            );
            let base = format!("{}/v1beta", server.uri());
            let r = Registry::from_cloud(
                vec![cfg("g", CloudProvider::Gemini, Some(&base))],
                keys("g", "AIza-1"),
            )
            .unwrap();
            assert_eq!(
                run(r.transcribe(vec![9; PCM], 16000, "g")).unwrap(),
                "from gemini"
            );
            assert_eq!(run(r.translate("好", "g")).unwrap(), "Good.");
        });
    }

    #[test]
    fn claude_translates_but_cannot_transcribe() {
        with_server(|server, rt| {
            rt.block_on(
                Mock::given(method("POST"))
                    .and(path("/v1/messages"))
                    .and(header("x-api-key", "sk-ant-1"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "content": [{"type": "text", "text": "Hi there."}],
                        "stop_reason": "end_turn"
                    })))
                    .expect(1)
                    .mount(server),
            );
            let base = format!("{}/v1", server.uri());
            let r = Registry::from_cloud(
                vec![cfg("a", CloudProvider::Claude, Some(&base))],
                keys("a", "sk-ant-1"),
            )
            .unwrap();
            assert_eq!(run(r.translate("嗨", "a")).unwrap(), "Hi there.");
            assert_eq!(
                run(r.transcribe(vec![9; PCM], 16000, "a")).unwrap_err(),
                FfiError::Unsupported
            );
            assert_eq!(
                run(r.test("a", CloudUse::Asr)).unwrap_err(),
                FfiError::Unsupported
            );
            // Only the translate call reached the server.
            assert_eq!(rt.block_on(server.received_requests()).unwrap().len(), 1);
        });
    }

    #[test]
    fn custom_endpoint_works_without_a_key() {
        with_server(|server, rt| {
            rt.block_on(
                Mock::given(method("POST"))
                    .and(path("/x/audio/transcriptions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "ok"})))
                    .mount(server),
            );
            let base = format!("{}/x", server.uri());
            let r = Registry::from_cloud(
                vec![cfg("u", CloudProvider::Custom, Some(&base))],
                HashMap::new(),
            )
            .unwrap();
            assert_eq!(run(r.transcribe(vec![9; PCM], 16000, "u")).unwrap(), "ok");
        });
    }

    #[test]
    fn missing_key_and_purpose_limits() {
        let mut only_translate = cfg("t", CloudProvider::Openai, None);
        only_translate.uses = vec![CloudUse::Translate];
        let r = Registry::from_cloud(
            vec![only_translate, cfg("nokey", CloudProvider::Gemini, None)],
            keys("t", "sk-1"),
        )
        .unwrap();
        // Not set up for recognition.
        assert_eq!(
            run(r.transcribe(vec![9; PCM], 16000, "t")).unwrap_err(),
            FfiError::Unsupported
        );
        // Official API without a key: no request is made.
        assert_eq!(
            run(r.translate("你好", "nokey")).unwrap_err(),
            FfiError::Unauthorized
        );
        assert_eq!(
            run(r.transcribe(vec![9; PCM], 16000, "nokey")).unwrap_err(),
            FfiError::Unauthorized
        );
        assert_eq!(
            run(r.translate("你好", "other")).unwrap_err(),
            FfiError::UnknownService
        );
    }

    #[test]
    fn test_connection_reports_latency_and_error_codes() {
        with_server(|server, rt| {
            let base = format!("{}/v1", server.uri());
            let r = Registry::from_cloud(
                vec![cfg("o", CloudProvider::Openai, Some(&base))],
                keys("o", "sk-1"),
            )
            .unwrap();
            // Success on both purposes; the recognition test uploads generated audio.
            rt.block_on(async {
                Mock::given(method("POST"))
                    .and(path("/v1/audio/transcriptions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": ""})))
                    .mount(server)
                    .await;
                Mock::given(method("POST"))
                    .and(path("/v1/chat/completions"))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                        "choices": [{"message": {"role": "assistant", "content": "Hello."}}]
                    })))
                    .mount(server)
                    .await;
            });
            assert!(run(r.test("o", CloudUse::Asr)).is_ok());
            assert!(run(r.test("o", CloudUse::Translate)).is_ok());
            let reqs = rt.block_on(server.received_requests()).unwrap();
            assert!(reqs[0].body.len() > 32_000, "about 1 s of 16 kHz audio");
            assert_eq!(
                run(r.test("nope", CloudUse::Asr)).unwrap_err(),
                FfiError::UnknownService
            );

            // Error codes, per HTTP status.
            for (status, code) in [
                (401, FfiError::Unauthorized),
                (403, FfiError::Unauthorized),
                (429, FfiError::RateLimited),
                (404, FfiError::Unsupported),
                (500, FfiError::Server),
                (302, FfiError::BadResponse),
            ] {
                rt.block_on(server.reset());
                rt.block_on(
                    Mock::given(method("POST"))
                        .respond_with(ResponseTemplate::new(status))
                        .mount(server),
                );
                for purpose in [CloudUse::Asr, CloudUse::Translate] {
                    assert_eq!(
                        run(r.test("o", purpose)).unwrap_err(),
                        code,
                        "{status} {purpose:?}"
                    );
                }
            }
        });
    }

    #[test]
    fn unreachable_endpoint_and_no_redirect() {
        // A port that was just released: nothing listens there.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dead = format!("http://127.0.0.1:{port}");
        let r = Registry::from_cloud(
            vec![cfg("d", CloudProvider::Custom, Some(&format!("{dead}/v1")))],
            HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            run(r.test("d", CloudUse::Translate)).unwrap_err(),
            FfiError::Unreachable
        );
        assert_eq!(
            run(r.test("d", CloudUse::Asr)).unwrap_err(),
            FfiError::Unreachable
        );

        // A redirect is never followed (the key header would go to the other host).
        with_server(|attacker, rt| {
            with_server(|server, rt2| {
                rt2.block_on(
                    Mock::given(method("POST"))
                        .respond_with(ResponseTemplate::new(307).insert_header(
                            "location",
                            format!("{}/steal", attacker.uri()).as_str(),
                        ))
                        .mount(server),
                );
                let base = format!("{}/v1", server.uri());
                let r = Registry::from_cloud(
                    vec![cfg("o", CloudProvider::Openai, Some(&base))],
                    keys("o", "sk-1"),
                )
                .unwrap();
                assert_eq!(
                    run(r.translate("你好", "o")).unwrap_err(),
                    FfiError::BadResponse
                );
            });
            assert!(rt
                .block_on(attacker.received_requests())
                .unwrap()
                .is_empty());
        });
    }

    #[test]
    fn errors_carry_no_text_or_keys() {
        with_server(|server, rt| {
            rt.block_on(
                Mock::given(method("POST"))
                    .respond_with(ResponseTemplate::new(401).set_body_string("sk-LEAK 你好"))
                    .mount(server),
            );
            let base = format!("{}/v1", server.uri());
            let r = Registry::from_cloud(
                vec![cfg("o", CloudProvider::Openai, Some(&base))],
                keys("o", "sk-SECRET"),
            )
            .unwrap();
            let e = run(r.translate("機密句子", "o")).unwrap_err();
            let shown = format!("{e} {e:?}");
            assert!(!shown.contains("sk-") && !shown.contains("機密"), "{shown}");
        });
    }
}
