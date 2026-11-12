//! LAN services: loading the bundled TN / RD preset files and calling them.
//!
//! Read only: the preset files are parsed, never written or merged into any config. The
//! per-service default API keys (Breeze) are kept in memory and never leave this module:
//! no `Debug` output, error or return value carries them.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde::Deserialize;
use v2c_core::asr::{AsrContext, AsrProvider};
use v2c_core::audio::AudioClip;
use v2c_core::config::{CloudService, TimeoutConfig};
use v2c_core::error::{ProviderErrorKind, Stage};
use v2c_core::llm::LlmProvider;
use v2c_core::postprocess::zh_tw::to_zh_tw;
use v2c_core::prompts;
use v2c_core::route::{build_cloud_asr, build_cloud_llm, cloud_http_client, CloudHttp};
use v2c_core::secrets::{ApiKey, MemorySecretStore, SecretStore};
use v2c_core::V2cError;

/// Chinese-to-English translation can take a while on the 27B model; fixed here because the
/// Android app has no settings (the desktop default of 3 s is for polish).
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
        })
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

    #[cfg(test)]
    pub(crate) fn has_key(&self, id: &str) -> bool {
        matches!(self.keys.get(id), Ok(Some(_)))
    }

    pub(crate) async fn transcribe(
        &self,
        samples: Vec<i16>,
        rate: u32,
        service_id: &str,
    ) -> Result<String, FfiError> {
        if samples.is_empty() || rate == 0 {
            return Err(FfiError::InvalidAudio);
        }
        let svc = self.service(service_id)?;
        let timeout = svc.asr_timeout(&TimeoutConfig::default());
        let asr: Box<dyn AsrProvider> = build_cloud_asr(svc, &self.keys, &self.http, timeout)
            .map_err(FfiError::from)?
            .ok_or(FfiError::Unsupported)?;
        let clip = AudioClip {
            samples: samples.iter().map(|&s| f32::from(s) / 32768.0).collect(),
            sample_rate: rate,
        };
        let t = asr
            .transcribe(&clip, &AsrContext::default())
            .await
            .map_err(FfiError::from)?;
        Ok(t.text)
    }

    pub(crate) async fn translate(&self, text: &str, service_id: &str) -> Result<String, FfiError> {
        let svc = self.service(service_id)?;
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
    if let Ok(mut g) = REGISTRY.write() {
        *g = Some(Arc::new(r));
    }
}

pub(crate) fn registry() -> Result<Arc<Registry>, FfiError> {
    REGISTRY
        .read()
        .map_err(|_| FfiError::Internal)?
        .clone()
        .ok_or(FfiError::NotLoaded)
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
            assert!(!r.has_key(&format!("{p}-tea-asr")));
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
                .block_on(r.transcribe(vec![100; 1600], 16000, "t-asr"))
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
            let run = |samples: Vec<i16>, rate, id: &str| {
                runtime().unwrap().block_on(r.transcribe(samples, rate, id))
            };
            assert_eq!(
                run(vec![1; 160], 16000, "t-asr").unwrap_err(),
                FfiError::Unauthorized
            );
            assert_eq!(
                run(vec![1; 160], 16000, "nope").unwrap_err(),
                FfiError::UnknownService
            );
            assert_eq!(
                run(vec![], 16000, "t-asr").unwrap_err(),
                FfiError::InvalidAudio
            );
            assert_eq!(
                run(vec![1], 0, "t-asr").unwrap_err(),
                FfiError::InvalidAudio
            );
            // A service without an ASR model.
            assert_eq!(
                run(vec![1; 160], 16000, "t-llm").unwrap_err(),
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
}
