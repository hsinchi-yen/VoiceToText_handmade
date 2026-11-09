//! Cloud routing: which provider serves each purpose, per-kind defaults, and the
//! setup-wizard connection test.
//!
//! - [`resolve`] (pure): `[route]` + connected services → [`RoutePlan`].
//! - [`default_base_url`] / [`suggested_models`]: wizard pre-fill values. These are
//!   suggestions only; the chosen values are saved in `CloudService` (config), and request
//!   code always takes the model from there.
//! - [`test_connection`]: wizard step 5 (plan §6.2) and Android settings.
//! - [`cloud_http_client`]: the only HTTP client cloud providers accept ([`CloudHttp`]).
//!
//! Building the pipeline's provider objects from a `RoutePlan` is done in the integration
//! step (after the OpenAI-compatible LLM from T1.5 lands).

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::asr::compat::CompatAsr;
use crate::asr::gemini::GeminiAsr;
pub use crate::asr::http::{cloud_http_client, CloudHttp};
use crate::asr::http::{join_url, CallSite};
use crate::asr::openai::OpenAiAsr;
use crate::asr::wav::probe_clip;
use crate::asr::{AsrContext, AsrProvider};
use crate::config::{CloudKind, CloudService, RouteConfig, TimeoutConfig, LOCAL};
use crate::error::{ProviderErrorKind, Stage, V2cError};
use crate::llm::claude::{ClaudeLlm, ANTHROPIC_VERSION};
use crate::llm::gemini::GeminiLlm;
use crate::llm::LlmProvider;
use crate::secrets::{ApiKey, SecretStore};
use crate::Result;

// ---------------------------------------------------------------------------------------
// Route resolution (pure)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Asr,
    Polish,
    Translate,
}

/// Where one purpose goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Local,
    /// A `CloudService::id` that exists and supports the purpose.
    Cloud {
        service_id: String,
        kind: CloudKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurposePlan {
    pub target: Target,
    /// Retry with the local provider if the cloud call fails. Always `false` for `Local`.
    pub fallback_to_local: bool,
}

impl PurposePlan {
    fn local() -> Self {
        Self {
            target: Target::Local,
            fallback_to_local: false,
        }
    }
}

/// A `[route]` entry that could not be honoured; the purpose falls back to `Local`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteIssue {
    /// No `CloudService` with this id.
    UnknownService {
        purpose: Purpose,
        service_id: String,
    },
    /// The service cannot do this purpose (Claude has no speech recognition).
    Unsupported {
        purpose: Purpose,
        service_id: String,
    },
    /// No model configured for this purpose and the kind has no suggested default
    /// (e.g. an OpenAI-compatible endpoint without `asr_model`).
    NoModel {
        purpose: Purpose,
        service_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePlan {
    pub asr: PurposePlan,
    pub polish: PurposePlan,
    pub translate: PurposePlan,
    /// Problems to show in settings; never fatal (local-first).
    pub issues: Vec<RouteIssue>,
}

impl RoutePlan {
    pub fn get(&self, purpose: Purpose) -> &PurposePlan {
        match purpose {
            Purpose::Asr => &self.asr,
            Purpose::Polish => &self.polish,
            Purpose::Translate => &self.translate,
        }
    }

    /// True when nothing leaves the machine.
    pub fn all_local(&self) -> bool {
        [&self.asr, &self.polish, &self.translate]
            .iter()
            .all(|p| p.target == Target::Local)
    }
}

/// Resolves `[route]` against the connected services. Unknown or unsuitable references
/// resolve to `Local` and are reported in `issues`.
pub fn resolve(route: &RouteConfig, services: &[CloudService]) -> RoutePlan {
    let mut issues = Vec::new();
    let mut one = |purpose: Purpose, reference: &str| -> PurposePlan {
        let reference = reference.trim();
        if reference.is_empty() || reference == LOCAL {
            return PurposePlan::local();
        }
        let Some(svc) = services.iter().find(|s| s.id == reference) else {
            issues.push(RouteIssue::UnknownService {
                purpose,
                service_id: reference.to_owned(),
            });
            return PurposePlan::local();
        };
        if purpose == Purpose::Asr && !svc.kind.supports_asr() {
            issues.push(RouteIssue::Unsupported {
                purpose,
                service_id: reference.to_owned(),
            });
            return PurposePlan::local();
        }
        let suggested = suggested_models(svc.kind);
        let model = match purpose {
            Purpose::Asr => model_for(&svc.asr_model, suggested.asr),
            Purpose::Polish | Purpose::Translate => model_for(&svc.llm_model, suggested.llm),
        };
        if model.is_none() {
            issues.push(RouteIssue::NoModel {
                purpose,
                service_id: reference.to_owned(),
            });
            return PurposePlan::local();
        }
        PurposePlan {
            target: Target::Cloud {
                service_id: svc.id.clone(),
                kind: svc.kind,
            },
            fallback_to_local: route.cloud_fallback_to_local,
        }
    };
    let asr = one(Purpose::Asr, &route.asr);
    let polish = one(Purpose::Polish, &route.polish);
    let translate = one(Purpose::Translate, &route.translate);
    RoutePlan {
        asr,
        polish,
        translate,
        issues,
    }
}

// ---------------------------------------------------------------------------------------
// Per-kind defaults (wizard suggestions)
// ---------------------------------------------------------------------------------------

/// Official endpoint for a kind; `None` for `OpenAiCompat` (the user must enter one).
/// TODO: move to config defaults once `config.rs` can be edited.
pub fn default_base_url(kind: CloudKind) -> Option<&'static str> {
    match kind {
        CloudKind::OpenAi => Some("https://api.openai.com/v1"),
        CloudKind::Gemini => Some("https://generativelanguage.googleapis.com/v1beta"),
        CloudKind::Claude => Some("https://api.anthropic.com/v1"),
        CloudKind::OpenAiCompat => None,
    }
}

/// Pre-filled model ids for the wizard (verified against vendor docs, 2026-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuggestedModels {
    pub asr: Option<&'static str>,
    pub llm: Option<&'static str>,
}

/// TODO: move to config defaults once `config.rs` can be edited.
pub fn suggested_models(kind: CloudKind) -> SuggestedModels {
    match kind {
        CloudKind::OpenAi => SuggestedModels {
            asr: Some("gpt-transcribe"),
            llm: Some("gpt-6-luna"),
        },
        CloudKind::Gemini => SuggestedModels {
            asr: Some("gemini-3.6-flash"),
            llm: Some("gemini-3.6-flash"),
        },
        CloudKind::Claude => SuggestedModels {
            asr: None,
            llm: Some("claude-haiku-5-5"),
        },
        CloudKind::OpenAiCompat => SuggestedModels {
            asr: None,
            llm: None,
        },
    }
}

/// `CloudService::base_url`, else the kind's official endpoint.
///
/// Official APIs (OpenAI, Gemini, Claude) carry an API key, so an override must be
/// `https://`; plain `http://` is accepted only for loopback (local proxies, tests).
/// `OpenAiCompat` may use `http://` (LM Studio on another PC in the LAN).
pub fn effective_base_url(service: &CloudService) -> Result<String> {
    let url = service
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .or_else(|| default_base_url(service.kind))
        .ok_or(V2cError::NotConfigured("cloud service base_url"))?;
    let parsed = reqwest::Url::parse(url).map_err(|_| V2cError::NotConfigured("valid base_url"))?;
    let ok = match parsed.scheme() {
        "https" => true,
        "http" => service.kind == CloudKind::OpenAiCompat || is_loopback(&parsed),
        _ => false,
    };
    if !ok {
        return Err(V2cError::NotConfigured("https base_url"));
    }
    Ok(url.to_owned())
}

fn is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn model_for(configured: &Option<String>, suggested: Option<&'static str>) -> Option<String> {
    configured
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
        .or_else(|| suggested.map(str::to_owned))
}

/// Key for `service`: required for official APIs, optional for `OpenAiCompat`.
fn key_for(service: &CloudService, secrets: &dyn SecretStore) -> Result<Option<ApiKey>> {
    let key = secrets.get(&service.id)?.filter(|k| !k.is_empty());
    if key.is_none() && service.kind != CloudKind::OpenAiCompat {
        return Err(V2cError::NotConfigured("cloud service api key"));
    }
    Ok(key)
}

/// Builds the ASR provider for a service, or `None` if the kind has no ASR or no model is
/// set (compat endpoints have no default model).
pub(crate) fn build_asr(
    service: &CloudService,
    key: Option<ApiKey>,
    http: &CloudHttp,
    timeout: Duration,
) -> Result<Option<Box<dyn AsrProvider>>> {
    let base = effective_base_url(service)?;
    let Some(model) = model_for(&service.asr_model, suggested_models(service.kind).asr) else {
        return Ok(None);
    };
    let http = http.clone();
    let p: Box<dyn AsrProvider> = match service.kind {
        CloudKind::OpenAi => {
            Box::new(OpenAiAsr::new(http, base, key, model, timeout).with_name(&service.id))
        }
        CloudKind::OpenAiCompat => {
            Box::new(CompatAsr::new(http, base, key, model, timeout).with_name(&service.id))
        }
        CloudKind::Gemini => Box::new(
            GeminiAsr::new(
                http,
                base,
                key.unwrap_or_else(|| ApiKey::new("")),
                model,
                timeout,
            )
            .with_name(&service.id),
        ),
        CloudKind::Claude => return Ok(None),
    };
    Ok(Some(p))
}

/// Builds Gemini / Claude LLM providers. OpenAI and OpenAI-compatible LLMs use T1.5's
/// `OpenAiCompatLlm` and are built by the integration step, so they return `None` here.
pub(crate) fn build_native_llm(
    service: &CloudService,
    key: Option<ApiKey>,
    http: &CloudHttp,
    timeout: Duration,
) -> Result<Option<Box<dyn LlmProvider>>> {
    let base = effective_base_url(service)?;
    let Some(model) = model_for(&service.llm_model, suggested_models(service.kind).llm) else {
        return Ok(None);
    };
    let key = key.unwrap_or_else(|| ApiKey::new(""));
    let http = http.clone();
    let p: Box<dyn LlmProvider> = match service.kind {
        CloudKind::Gemini => {
            Box::new(GeminiLlm::new(http, base, key, model, timeout).with_name(&service.id))
        }
        CloudKind::Claude => {
            Box::new(ClaudeLlm::new(http, base, key, model, timeout).with_name(&service.id))
        }
        CloudKind::OpenAi | CloudKind::OpenAiCompat => return Ok(None),
    };
    Ok(Some(p))
}

// ---------------------------------------------------------------------------------------
// Connection test
// ---------------------------------------------------------------------------------------

/// Length of the generated probe clip (plan §6.2: a built-in 3-second test clip).
pub const PROBE_CLIP_MS: u64 = 3000;
const PROBE_SYSTEM: &str = "Repeat the user's message exactly.";
const PROBE_USER: &str = "This is a connection test.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOptions {
    /// Test speech recognition (ignored for kinds without ASR).
    pub asr: bool,
    /// Test the text model.
    pub llm: bool,
    pub asr_timeout: Duration,
    pub llm_timeout: Duration,
}

impl ProbeOptions {
    /// Test every purpose the kind supports, with the configured cloud timeouts.
    pub fn from_timeouts(t: &TimeoutConfig) -> Self {
        Self {
            asr: true,
            llm: true,
            asr_timeout: Duration::from_millis(t.asr_cloud_ms),
            llm_timeout: Duration::from_millis(t.llm_cloud_ms),
        }
    }
}

/// Outcome of one probe step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Not requested, not supported by this kind, or no model configured.
    Skipped,
    Passed {
        latency_ms: u64,
    },
    Failed(ProviderErrorKind),
    TimedOut,
}

impl Check {
    fn from_result<T>(r: &Result<T>, started: Instant) -> Self {
        match r {
            Ok(_) => Check::Passed {
                latency_ms: started.elapsed().as_millis() as u64,
            },
            Err(V2cError::Timeout { .. }) => Check::TimedOut,
            Err(V2cError::Provider { kind, .. }) => Check::Failed(*kind),
            Err(_) => Check::Failed(ProviderErrorKind::BadResponse),
        }
    }

    pub fn passed(&self) -> bool {
        matches!(self, Check::Passed { .. })
    }

    fn is_failure(&self) -> bool {
        matches!(self, Check::Failed(_) | Check::TimedOut)
    }
}

/// Result of [`test_connection`].
///
/// - `models`: the service's model list (`GET /models`) for the wizard's drop-down, `None`
///   when the endpoint has no list or it failed (`models_check` says which).
/// - `asr`: transcribed the generated 3 s tone. `llm`: answered one fixed sentence.
/// - `ok`: at least one step passed and none failed (a missing model list alone is not a
///   failure, since many self-hosted servers lack `/models`).
/// - `latency_ms`: wall time of the whole test.
///
/// When the model list already shows the key is rejected or the host is unreachable, the
/// ASR/LLM steps are not sent (no point uploading audio) and report the same failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub ok: bool,
    pub latency_ms: u64,
    pub models_check: Check,
    pub models: Option<Vec<String>>,
    pub asr: Check,
    pub llm: Check,
}

impl Probe {
    pub fn supports_asr(&self) -> bool {
        self.asr.passed()
    }

    pub fn supports_llm(&self) -> bool {
        self.llm.passed()
    }

    /// The first failure, for the wizard's error message.
    pub fn first_failure(&self) -> Option<Check> {
        [self.models_check, self.asr, self.llm]
            .into_iter()
            .find(Check::is_failure)
    }
}

/// Tests a service the way the wizard needs: list models, transcribe a generated clip
/// (kinds with ASR), and run one short LLM request (Gemini / Claude).
///
/// OpenAI and OpenAI-compatible LLM checks need T1.5's `OpenAiCompatLlm`; until the
/// integration step passes one via [`test_connection_with_llm`], `llm` is `Skipped` for
/// those kinds (the model list still verifies the key and endpoint).
///
/// `Err` only for configuration problems (missing key for an official API, missing
/// `base_url` for a compatible endpoint, keyring failure); network/API failures are
/// reported inside `Probe`. `http` comes from [`cloud_http_client`] (no redirects).
pub async fn test_connection(
    service: &CloudService,
    secrets: &dyn SecretStore,
    http: &CloudHttp,
    opts: &ProbeOptions,
) -> Result<Probe> {
    test_connection_with_llm(service, secrets, http, opts, None).await
}

/// Like [`test_connection`], but tests `llm` (e.g. an `OpenAiCompatLlm` built for this
/// service) instead of the built-in Gemini/Claude providers.
pub async fn test_connection_with_llm(
    service: &CloudService,
    secrets: &dyn SecretStore,
    http: &CloudHttp,
    opts: &ProbeOptions,
    llm: Option<&dyn LlmProvider>,
) -> Result<Probe> {
    let started = Instant::now();
    let key = key_for(service, secrets)?;
    let base = effective_base_url(service)?;

    let t = Instant::now();
    let listed = list_models(service.kind, &base, key.as_ref(), http, opts.llm_timeout).await;
    let models_check = match &listed {
        // Missing list endpoint is informational, not a failure.
        Err(V2cError::Provider {
            kind: ProviderErrorKind::Unsupported,
            ..
        }) => Check::Skipped,
        r => Check::from_result(r, t),
    };
    let models = listed.ok();
    let blocking = match models_check {
        Check::Failed(k @ (ProviderErrorKind::Unauthorized | ProviderErrorKind::Unreachable)) => {
            Some(Check::Failed(k))
        }
        _ => None,
    };

    let asr = match (opts.asr, blocking) {
        (false, _) => Check::Skipped,
        (true, Some(b)) if service.kind.supports_asr() => b,
        (true, _) => match build_asr(service, key.clone(), http, opts.asr_timeout)? {
            None => Check::Skipped,
            Some(p) => {
                let t = Instant::now();
                let r = p
                    .transcribe(&probe_clip(PROBE_CLIP_MS), &AsrContext::default())
                    .await;
                Check::from_result(&r, t)
            }
        },
    };

    let built;
    let llm_provider: Option<&dyn LlmProvider> = match llm {
        Some(p) => Some(p),
        None => {
            built = build_native_llm(service, key.clone(), http, opts.llm_timeout)?;
            built.as_deref()
        }
    };
    let llm = match (opts.llm, blocking, llm_provider) {
        (false, _, _) | (true, _, None) => Check::Skipped,
        (true, Some(b), Some(_)) => b,
        (true, None, Some(p)) => {
            let t = Instant::now();
            let r = p.complete(PROBE_SYSTEM, PROBE_USER).await;
            Check::from_result(&r, t)
        }
    };

    let checks = [models_check, asr, llm];
    let ok = checks.iter().any(Check::passed) && !checks.iter().any(Check::is_failure);
    Ok(Probe {
        ok,
        latency_ms: started.elapsed().as_millis() as u64,
        models_check,
        models,
        asr,
        llm,
    })
}

/// `GET {base}/models` in each vendor's format; returns model ids.
async fn list_models(
    kind: CloudKind,
    base: &str,
    key: Option<&ApiKey>,
    http: &CloudHttp,
    timeout: Duration,
) -> Result<Vec<String>> {
    let call = CallSite {
        stage: Stage::Polish,
        provider: "models".into(),
        timeout,
    };
    let url = join_url(base, "models");
    let mut req = match kind {
        CloudKind::Gemini => http.client().get(url).query(&[("pageSize", "1000")]),
        CloudKind::Claude => http
            .client()
            .get(url)
            .query(&[("limit", "1000")])
            .header("anthropic-version", ANTHROPIC_VERSION),
        CloudKind::OpenAi | CloudKind::OpenAiCompat => http.client().get(url),
    };
    if let Some(k) = key {
        req = match kind {
            CloudKind::Gemini => req.header("x-goog-api-key", k.expose()),
            CloudKind::Claude => req.header("x-api-key", k.expose()),
            CloudKind::OpenAi | CloudKind::OpenAiCompat => req.bearer_auth(k.expose()),
        };
    }
    let v = call.send_json(req).await?;
    parse_models(kind, &v).ok_or_else(|| call.error(ProviderErrorKind::BadResponse))
}

fn parse_models(kind: CloudKind, v: &Value) -> Option<Vec<String>> {
    let ids = match kind {
        CloudKind::Gemini => v
            .get("models")?
            .as_array()?
            .iter()
            .filter(|m| {
                // Keep models usable with generateContent when the field is present.
                m.get("supportedGenerationMethods")
                    .and_then(Value::as_array)
                    .is_none_or(|ms| ms.iter().any(|x| x == "generateContent"))
            })
            .filter_map(|m| m.get("name").and_then(Value::as_str))
            .map(|n| n.trim_start_matches("models/").to_owned())
            .collect(),
        _ => v
            .get("data")?
            .as_array()?
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect(),
    };
    Some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::http::cloud_http_client;
    use crate::secrets::MemorySecretStore;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn svc(id: &str, kind: CloudKind) -> CloudService {
        CloudService {
            id: id.into(),
            kind,
            base_url: None,
            asr_model: None,
            llm_model: None,
        }
    }

    fn route(asr: &str, polish: &str, translate: &str, fallback: bool) -> RouteConfig {
        RouteConfig {
            asr: asr.into(),
            polish: polish.into(),
            translate: translate.into(),
            cloud_fallback_to_local: fallback,
        }
    }

    // ---- resolve ----

    #[test]
    fn default_route_is_all_local() {
        let plan = resolve(&RouteConfig::default(), &[]);
        assert!(plan.all_local());
        assert!(plan.issues.is_empty());
        assert!(!plan.asr.fallback_to_local);
    }

    #[test]
    fn cloud_purposes_get_fallback_flag() {
        let services = [svc("g", CloudKind::Gemini), svc("c", CloudKind::Claude)];
        let plan = resolve(&route("g", "c", "local", true), &services);
        assert_eq!(
            plan.asr.target,
            Target::Cloud {
                service_id: "g".into(),
                kind: CloudKind::Gemini
            }
        );
        assert!(plan.asr.fallback_to_local);
        assert_eq!(
            plan.get(Purpose::Polish).target,
            Target::Cloud {
                service_id: "c".into(),
                kind: CloudKind::Claude
            }
        );
        assert_eq!(plan.translate, PurposePlan::local());
        assert!(!plan.all_local());

        let no_fb = resolve(&route("g", "c", "c", false), &services);
        assert!(!no_fb.asr.fallback_to_local && !no_fb.translate.fallback_to_local);
    }

    #[test]
    fn unknown_or_unsupported_falls_back_to_local_with_issue() {
        let services = [svc("c", CloudKind::Claude)];
        let plan = resolve(&route("c", "missing", "c", true), &services);
        assert_eq!(plan.asr, PurposePlan::local());
        assert_eq!(plan.polish, PurposePlan::local());
        assert!(matches!(plan.translate.target, Target::Cloud { .. }));
        assert_eq!(
            plan.issues,
            vec![
                RouteIssue::Unsupported {
                    purpose: Purpose::Asr,
                    service_id: "c".into()
                },
                RouteIssue::UnknownService {
                    purpose: Purpose::Polish,
                    service_id: "missing".into()
                },
            ]
        );
    }

    #[test]
    fn empty_reference_means_local() {
        let plan = resolve(&route("", " ", "local", true), &[]);
        assert!(plan.all_local() && plan.issues.is_empty());
    }

    // ---- defaults ----

    #[test]
    fn defaults_per_kind() {
        assert!(default_base_url(CloudKind::OpenAiCompat).is_none());
        assert_eq!(
            suggested_models(CloudKind::Claude).llm,
            Some("claude-haiku-5-5")
        );
        assert!(suggested_models(CloudKind::Claude).asr.is_none());
        let compat = svc("x", CloudKind::OpenAiCompat);
        assert!(matches!(
            effective_base_url(&compat),
            Err(V2cError::NotConfigured(_))
        ));
        let mut g = svc("g", CloudKind::Gemini);
        assert!(effective_base_url(&g).unwrap().starts_with("https://"));
        g.base_url = Some("https://proxy/v1beta".into());
        assert_eq!(effective_base_url(&g).unwrap(), "https://proxy/v1beta");
    }

    #[test]
    fn official_apis_require_https_except_loopback() {
        for kind in [CloudKind::OpenAi, CloudKind::Gemini, CloudKind::Claude] {
            let mut s = svc("x", kind);
            for bad in [
                "http://proxy/v1",
                "http://192.168.1.5/v1",
                "ftp://h/v1",
                "nonsense",
            ] {
                s.base_url = Some(bad.into());
                assert!(
                    matches!(effective_base_url(&s), Err(V2cError::NotConfigured(_))),
                    "{kind:?} {bad}"
                );
            }
            for ok in [
                "http://127.0.0.1:8080/v1",
                "http://localhost:1234/v1",
                "http://[::1]:9/v1",
            ] {
                s.base_url = Some(ok.into());
                assert_eq!(effective_base_url(&s).unwrap(), ok);
            }
        }
        let mut lan = svc("lan", CloudKind::OpenAiCompat);
        lan.base_url = Some("http://192.168.1.5:1234/v1".into());
        assert!(effective_base_url(&lan).is_ok());
        lan.base_url = Some("file:///etc/passwd".into());
        assert!(effective_base_url(&lan).is_err());
    }

    #[test]
    fn missing_model_falls_back_to_local_with_issue() {
        let mut lan = svc("lan", CloudKind::OpenAiCompat);
        lan.base_url = Some("http://192.168.1.5:1234/v1".into());
        lan.llm_model = Some("qwen".into());
        let plan = resolve(&route("lan", "lan", "lan", true), &[lan]);
        assert_eq!(plan.asr, PurposePlan::local());
        assert!(matches!(plan.polish.target, Target::Cloud { .. }));
        assert_eq!(
            plan.issues,
            vec![RouteIssue::NoModel {
                purpose: Purpose::Asr,
                service_id: "lan".into()
            }]
        );
    }

    // ---- test_connection ----

    fn opts() -> ProbeOptions {
        ProbeOptions {
            asr: true,
            llm: true,
            asr_timeout: Duration::from_millis(500),
            llm_timeout: Duration::from_millis(500),
        }
    }

    fn secrets(id: &str, key: &str) -> MemorySecretStore {
        let s = MemorySecretStore::new();
        s.set(id, &ApiKey::new(key)).unwrap();
        s
    }

    #[tokio::test]
    async fn gemini_probe_lists_models_and_tests_both() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1beta/models"))
            .and(header("x-goog-api-key", "AIza-TEST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [
                {"name": "models/g-flash", "supportedGenerationMethods": ["generateContent"]},
                {"name": "models/g-embed", "supportedGenerationMethods": ["embedContent"]}
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
        let mut s = svc("my-gemini", CloudKind::Gemini);
        s.base_url = Some(format!("{}/v1beta", server.uri()));
        s.asr_model = Some("g-flash".into());
        s.llm_model = Some("g-flash".into());
        let p = test_connection(
            &s,
            &secrets("my-gemini", "AIza-TEST"),
            &cloud_http_client().unwrap(),
            &opts(),
        )
        .await
        .unwrap();
        assert!(p.ok, "{p:?}");
        assert_eq!(p.models, Some(vec!["g-flash".to_string()]));
        assert!(p.supports_asr() && p.supports_llm());
        assert!(p.first_failure().is_none());
    }

    #[tokio::test]
    async fn claude_probe_has_no_asr() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-ant-TEST"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
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
            .expect(1)
            .mount(&server)
            .await;
        let mut s = svc("claude", CloudKind::Claude);
        s.base_url = Some(format!("{}/v1", server.uri()));
        let p = test_connection(
            &s,
            &secrets("claude", "sk-ant-TEST"),
            &cloud_http_client().unwrap(),
            &opts(),
        )
        .await
        .unwrap();
        assert!(p.ok);
        assert_eq!(p.asr, Check::Skipped);
        assert!(p.supports_llm());
        assert_eq!(p.models, Some(vec!["claude-haiku-5-5".to_string()]));
    }

    #[tokio::test]
    async fn bad_key_stops_before_uploading_audio() {
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
        let mut s = svc("oa", CloudKind::OpenAi);
        s.base_url = Some(format!("{}/v1", server.uri()));
        let p = test_connection(
            &s,
            &secrets("oa", "sk-BAD"),
            &cloud_http_client().unwrap(),
            &opts(),
        )
        .await
        .unwrap();
        assert!(!p.ok);
        assert_eq!(p.asr, Check::Failed(ProviderErrorKind::Unauthorized));
        assert_eq!(
            p.first_failure(),
            Some(Check::Failed(ProviderErrorKind::Unauthorized))
        );
        // OpenAI LLM needs OpenAiCompatLlm (integration step).
        assert_eq!(p.llm, Check::Skipped);
    }

    #[tokio::test]
    async fn compat_without_models_or_audio_reports_unsupported_asr() {
        let server = MockServer::start().await; // every path 404s
        let mut s = svc("lan", CloudKind::OpenAiCompat);
        s.base_url = Some(format!("{}/v1", server.uri()));
        s.asr_model = Some("whisper".into());
        let p = test_connection(
            &s,
            &MemorySecretStore::new(),
            &cloud_http_client().unwrap(),
            &opts(),
        )
        .await
        .unwrap();
        assert_eq!(p.models_check, Check::Skipped);
        assert_eq!(p.models, None);
        assert_eq!(p.asr, Check::Failed(ProviderErrorKind::Unsupported));
        assert!(!p.ok);
        assert!(!p.supports_asr());
    }

    #[tokio::test]
    async fn rate_limit_and_timeout_are_reported() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": []})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let mut s = svc("g", CloudKind::Gemini);
        s.base_url = Some(server.uri());
        let p = test_connection(
            &s,
            &secrets("g", "k"),
            &cloud_http_client().unwrap(),
            &opts(),
        )
        .await
        .unwrap();
        assert_eq!(p.asr, Check::Failed(ProviderErrorKind::RateLimited));
        assert_eq!(p.llm, Check::Failed(ProviderErrorKind::RateLimited));

        let slow = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"models": []}))
                    .set_delay(Duration::from_secs(2)),
            )
            .mount(&slow)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
            .mount(&slow)
            .await;
        s.base_url = Some(slow.uri());
        let p = test_connection(
            &s,
            &secrets("g", "k"),
            &cloud_http_client().unwrap(),
            &opts(),
        )
        .await
        .unwrap();
        assert_eq!(p.models_check, Check::TimedOut);
        assert_eq!(p.asr, Check::TimedOut);
        assert!(!p.ok);
    }

    #[tokio::test]
    async fn missing_key_is_config_error() {
        let s = svc("oa", CloudKind::OpenAi);
        let err = test_connection(
            &s,
            &MemorySecretStore::new(),
            &cloud_http_client().unwrap(),
            &opts(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, V2cError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn injected_llm_is_used() {
        struct Echo;
        #[async_trait::async_trait]
        impl LlmProvider for Echo {
            fn name(&self) -> &str {
                "echo"
            }
            async fn complete(&self, _s: &str, u: &str) -> Result<String> {
                Ok(u.to_owned())
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .mount(&server)
            .await;
        let mut s = svc("oa", CloudKind::OpenAi);
        s.base_url = Some(server.uri());
        let o = ProbeOptions {
            asr: false,
            ..opts()
        };
        let p = test_connection_with_llm(
            &s,
            &secrets("oa", "k"),
            &cloud_http_client().unwrap(),
            &o,
            Some(&Echo),
        )
        .await
        .unwrap();
        assert!(p.ok && p.supports_llm());
        assert_eq!(p.asr, Check::Skipped);
    }
}
