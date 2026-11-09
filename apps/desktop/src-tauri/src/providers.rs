//! Which provider serves each purpose (`tasks/plan.md` sections 6 and 6.1).
//!
//! - [`Providers::build`]: from `[route]` + `[[cloud]]` (via `route::resolve`) and the keyring,
//!   builds the cloud providers and the local LM Studio clients. Rebuilt on config reload.
//! - [`select`] (pure): primary + optional local fallback for one purpose.
//! - [`FallbackAsr`] / [`FallbackLlm`]: retry with the local provider when the cloud call
//!   fails (`cloud_fallback_to_local`), and record that it happened.
//! - [`LmHealth`]: whether LM Studio answered the last `GET /models` check.
//! - [`notice_codes`] (pure): which status codes to show after an utterance.
//!
//! Logs carry provider names, purposes, and error codes only (AGENTS.md rule 1).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::warn;
use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
use v2c_core::audio::AudioClip;
use v2c_core::config::Config;
use v2c_core::error::Stage;
use v2c_core::llm::{LlmProvider, OpenAiCompatLlm};
use v2c_core::pipeline::Warning;
use v2c_core::route::{self, CloudHttp, Purpose, PurposePlan, RoutePlan, Target};
use v2c_core::secrets::SecretStore;

/// Request timeout for LM Studio polish / translate calls. Polish has its own, shorter budget
/// in the pipeline (`polish.timeout_ms`, scaled with length up to this value). Not in config.
pub const LOCAL_LLM_TIMEOUT: Duration = Duration::from_secs(8);
/// Timeout of the LM Studio liveness check (`GET /models`).
pub const LM_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
/// How often LM Studio's liveness is checked.
pub const LM_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Error code when a purpose is routed to a cloud service that cannot be used (missing key,
/// bad URL, no model) and `cloud_fallback_to_local = false`.
pub const CLOUD_NOT_CONFIGURED: &str = "cloud_not_configured";

/// Providers built from one config.
#[derive(Default)]
pub struct Providers {
    pub plan: Option<RoutePlan>,
    /// LM Studio (`[local] llm_base_url`) for polish and for translate (they differ only in
    /// the error stage).
    pub local_polish: Option<Arc<dyn LlmProvider>>,
    pub local_translate: Option<Arc<dyn LlmProvider>>,
    /// Short-timeout client for the liveness check.
    pub lm_probe: Option<OpenAiCompatLlm>,
    pub cloud_asr: Option<Arc<dyn AsrProvider>>,
    pub cloud_polish: Option<Arc<dyn LlmProvider>>,
    pub cloud_translate: Option<Arc<dyn LlmProvider>>,
    /// Timeout of the cloud LLM calls, ms (`[timeouts] llm_cloud_ms`).
    pub cloud_llm_timeout_ms: u64,
}

impl Providers {
    /// Builds everything `cfg` routes to. Never fails: a cloud provider that cannot be built is
    /// logged and left out (the purpose then falls back to local, or reports
    /// [`CLOUD_NOT_CONFIGURED`]).
    pub fn build(cfg: &Config, secrets: &dyn SecretStore, http: Option<&CloudHttp>) -> Self {
        let plan = route::resolve(&cfg.route, &cfg.cloud);
        for issue in &plan.issues {
            warn!(?issue, "route entry ignored; using local");
        }
        let mut p = Providers {
            cloud_llm_timeout_ms: cfg.timeouts.llm_cloud_ms,
            ..Default::default()
        };
        let Some(http) = http else {
            warn!("no HTTP client; LM Studio and cloud services are unavailable");
            p.plan = Some(plan);
            return p;
        };
        let local = |stage, timeout| {
            OpenAiCompatLlm::new(
                http.client().clone(),
                cfg.local.llm_base_url.clone(),
                None,
                cfg.local.llm_model.clone(),
                timeout,
            )
            .with_stage(stage)
        };
        p.local_polish = Some(Arc::new(local(Stage::Polish, LOCAL_LLM_TIMEOUT)));
        p.local_translate = Some(Arc::new(local(Stage::Translate, LOCAL_LLM_TIMEOUT)));
        p.lm_probe = Some(local(Stage::Polish, LM_CHECK_TIMEOUT));

        let asr_timeout = Duration::from_millis(cfg.timeouts.asr_cloud_ms);
        let llm_timeout = Duration::from_millis(cfg.timeouts.llm_cloud_ms);
        let service = |purpose: Purpose| match &plan.get(purpose).target {
            Target::Cloud { service_id, .. } => cfg.cloud_service(service_id),
            Target::Local => None,
        };
        if let Some(svc) = service(Purpose::Asr) {
            p.cloud_asr = log_build(
                Purpose::Asr,
                &svc.id,
                route::build_cloud_asr(svc, secrets, http, asr_timeout),
            )
            .map(Arc::from);
        }
        for (purpose, stage) in [
            (Purpose::Polish, Stage::Polish),
            (Purpose::Translate, Stage::Translate),
        ] {
            if let Some(svc) = service(purpose) {
                let built = log_build(
                    purpose,
                    &svc.id,
                    route::build_cloud_llm(svc, secrets, http, llm_timeout, stage),
                )
                .map(Arc::from);
                match purpose {
                    Purpose::Polish => p.cloud_polish = built,
                    _ => p.cloud_translate = built,
                }
            }
        }
        p.plan = Some(plan);
        p
    }

    pub fn purpose(&self, purpose: Purpose) -> PurposePlan {
        self.plan.as_ref().map_or(
            PurposePlan {
                target: Target::Local,
                fallback_to_local: false,
            },
            |p| p.get(purpose).clone(),
        )
    }
}

fn log_build<T>(
    purpose: Purpose,
    service_id: &str,
    r: v2c_core::Result<Option<Box<T>>>,
) -> Option<Box<T>>
where
    T: ?Sized,
{
    match r {
        Ok(Some(p)) => Some(p),
        Ok(None) => {
            warn!(
                ?purpose,
                service = service_id,
                "cloud service has no model for this purpose"
            );
            None
        }
        Err(e) => {
            // Errors never contain the key (secrets.rs / route.rs).
            warn!(?purpose, service = service_id, error = %e, "cloud provider unavailable");
            None
        }
    }
}

/// The provider(s) chosen for one purpose.
#[derive(Clone)]
pub struct Selected<T: ?Sized> {
    /// `None`: nothing can serve this purpose.
    pub primary: Option<Arc<T>>,
    /// Local provider to retry with if the cloud call fails.
    pub fallback: Option<Arc<T>>,
    /// The cloud provider could not be built and the local one is used instead.
    pub fell_back_upfront: bool,
    /// The purpose is routed to a cloud service that cannot be used, without fallback.
    pub cloud_missing: bool,
}

/// Primary and fallback for a purpose from its route, the built cloud provider, and the
/// local provider (if available).
pub fn select<T: ?Sized>(
    plan: &PurposePlan,
    cloud: Option<Arc<T>>,
    local: Option<Arc<T>>,
) -> Selected<T> {
    match (&plan.target, cloud) {
        (Target::Local, _) => Selected {
            primary: local,
            fallback: None,
            fell_back_upfront: false,
            cloud_missing: false,
        },
        (Target::Cloud { .. }, Some(cloud)) => Selected {
            primary: Some(cloud),
            fallback: if plan.fallback_to_local { local } else { None },
            fell_back_upfront: false,
            cloud_missing: false,
        },
        (Target::Cloud { .. }, None) if plan.fallback_to_local => Selected {
            fell_back_upfront: local.is_some(),
            primary: local,
            fallback: None,
            cloud_missing: false,
        },
        (Target::Cloud { .. }, None) => Selected {
            primary: None,
            fallback: None,
            fell_back_upfront: false,
            cloud_missing: true,
        },
    }
}

/// ASR that retries with `fallback` when `primary` fails.
pub struct FallbackAsr {
    pub primary: Arc<dyn AsrProvider>,
    pub fallback: Arc<dyn AsrProvider>,
    pub fell_back: Arc<AtomicBool>,
}

#[async_trait]
impl AsrProvider for FallbackAsr {
    fn name(&self) -> &str {
        self.primary.name()
    }

    async fn transcribe(&self, clip: &AudioClip, ctx: &AsrContext) -> v2c_core::Result<Transcript> {
        match self.primary.transcribe(clip, ctx).await {
            Ok(t) => Ok(t),
            Err(e) => {
                warn!(
                    provider = self.primary.name(),
                    fallback = self.fallback.name(),
                    error = %e,
                    "cloud ASR failed; using local"
                );
                self.fell_back.store(true, Ordering::SeqCst);
                self.fallback.transcribe(clip, ctx).await
            }
        }
    }
}

/// LLM that retries with `fallback` when `primary` fails.
pub struct FallbackLlm {
    pub primary: Arc<dyn LlmProvider>,
    pub fallback: Arc<dyn LlmProvider>,
    pub fell_back: Arc<AtomicBool>,
}

#[async_trait]
impl LlmProvider for FallbackLlm {
    fn name(&self) -> &str {
        self.primary.name()
    }

    async fn complete(&self, system: &str, user: &str) -> v2c_core::Result<String> {
        match self.primary.complete(system, user).await {
            Ok(t) => Ok(t),
            Err(e) => {
                warn!(
                    provider = self.primary.name(),
                    fallback = self.fallback.name(),
                    error = %e,
                    "cloud LLM failed; using local"
                );
                self.fell_back.store(true, Ordering::SeqCst);
                self.fallback.complete(system, user).await
            }
        }
    }
}

/// Wraps a selection into one ASR provider.
pub fn wrap_asr(
    s: &Selected<dyn AsrProvider>,
    flag: &Arc<AtomicBool>,
) -> Option<Arc<dyn AsrProvider>> {
    let primary = s.primary.clone()?;
    Some(match &s.fallback {
        Some(fb) => Arc::new(FallbackAsr {
            primary,
            fallback: Arc::clone(fb),
            fell_back: Arc::clone(flag),
        }),
        None => primary,
    })
}

/// Wraps a selection into one LLM provider.
pub fn wrap_llm(
    s: &Selected<dyn LlmProvider>,
    flag: &Arc<AtomicBool>,
) -> Option<Arc<dyn LlmProvider>> {
    let primary = s.primary.clone()?;
    Some(match &s.fallback {
        Some(fb) => Arc::new(FallbackLlm {
            primary,
            fallback: Arc::clone(fb),
            fell_back: Arc::clone(flag),
        }),
        None => primary,
    })
}

/// LM Studio liveness, updated by the periodic check.
pub struct LmHealth {
    up: AtomicBool,
    /// The "LM Studio is not running" notice has not been shown in the current outage.
    notice_pending: AtomicBool,
}

impl Default for LmHealth {
    fn default() -> Self {
        Self {
            up: AtomicBool::new(false),
            notice_pending: AtomicBool::new(true),
        }
    }
}

impl LmHealth {
    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::SeqCst)
    }

    /// Records a check result; returns the new state if it changed.
    pub fn set(&self, up: bool) -> Option<bool> {
        let was = self.up.swap(up, Ordering::SeqCst);
        if was && !up {
            // A new outage: show the notice once again.
            self.notice_pending.store(true, Ordering::SeqCst);
        }
        (was != up).then_some(up)
    }

    /// `true` once per outage (the first time polish runs without LM Studio).
    pub fn take_offline_notice(&self) -> bool {
        !self.is_up() && self.notice_pending.swap(false, Ordering::SeqCst)
    }
}

/// Polish failed or timed out while an LLM was available (rule-based cleanup was used).
pub const POLISH_TIMED_OUT: &str = "polish_timed_out";

/// Status codes to show after a successful utterance, in order, without duplicates.
/// `cloud_fell_back`: a cloud call failed (or could not be built) and local was used;
/// `polish_offline`: polish ran without LM Studio (first time in this outage) ->
/// `polish_fell_back`. A polish error / timeout while an LLM was available ->
/// [`POLISH_TIMED_OUT`].
pub fn notice_codes(
    warnings: &[Warning],
    cloud_fell_back: bool,
    polish_offline: bool,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    if cloud_fell_back || warnings.contains(&Warning::CloudFellBackToLocal) {
        out.push("cloud_fell_back");
    }
    if polish_offline {
        out.push("polish_fell_back");
    } else if warnings.contains(&Warning::PolishFellBack) {
        out.push(POLISH_TIMED_OUT);
    }
    out
}

#[cfg(test)]
mod tests {
    use v2c_core::config::{CloudKind, CloudService, LOCAL};
    use v2c_core::error::ProviderErrorKind;
    use v2c_core::secrets::{ApiKey, MemorySecretStore};
    use v2c_core::{Lang, V2cError};

    use super::*;

    struct Asr(&'static str, bool);

    #[async_trait]
    impl AsrProvider for Asr {
        fn name(&self) -> &str {
            self.0
        }
        async fn transcribe(&self, _: &AudioClip, _: &AsrContext) -> v2c_core::Result<Transcript> {
            if self.1 {
                Ok(Transcript {
                    text: self.0.into(),
                    lang: Lang::Zh,
                })
            } else {
                Err(V2cError::Provider {
                    stage: Stage::Asr,
                    provider: self.0.into(),
                    kind: ProviderErrorKind::Unreachable,
                })
            }
        }
    }

    struct Llm(&'static str, bool);

    #[async_trait]
    impl LlmProvider for Llm {
        fn name(&self) -> &str {
            self.0
        }
        async fn complete(&self, _: &str, _: &str) -> v2c_core::Result<String> {
            if self.1 {
                Ok(self.0.into())
            } else {
                Err(V2cError::Timeout {
                    stage: Stage::Polish,
                    ms: 1,
                })
            }
        }
    }

    fn cloud(fallback: bool) -> PurposePlan {
        PurposePlan {
            target: Target::Cloud {
                service_id: "g".into(),
                kind: CloudKind::Gemini,
            },
            fallback_to_local: fallback,
        }
    }

    fn local() -> PurposePlan {
        PurposePlan {
            target: Target::Local,
            fallback_to_local: false,
        }
    }

    fn names(s: &Selected<dyn AsrProvider>) -> (Option<String>, Option<String>) {
        (
            s.primary.as_ref().map(|a| a.name().to_owned()),
            s.fallback.as_ref().map(|a| a.name().to_owned()),
        )
    }

    #[test]
    fn selection_follows_route_and_fallback_flag() {
        let c: Arc<dyn AsrProvider> = Arc::new(Asr("cloud", true));
        let l: Arc<dyn AsrProvider> = Arc::new(Asr("local", true));

        let s = select(&local(), Some(c.clone()), Some(l.clone()));
        assert_eq!(names(&s), (Some("local".into()), None));

        let s = select(&cloud(true), Some(c.clone()), Some(l.clone()));
        assert_eq!(names(&s), (Some("cloud".into()), Some("local".into())));

        let s = select(&cloud(false), Some(c.clone()), Some(l.clone()));
        assert_eq!(names(&s), (Some("cloud".into()), None));

        // Cloud could not be built (e.g. no key).
        let s = select(&cloud(true), None, Some(l.clone()));
        assert_eq!(names(&s), (Some("local".into()), None));
        assert!(s.fell_back_upfront && !s.cloud_missing);

        let s = select(&cloud(false), None, Some(l));
        assert!(s.primary.is_none() && s.cloud_missing);

        // Nothing local either (LM Studio down): no provider, not "cloud missing".
        let s = select::<dyn AsrProvider>(&cloud(true), None, None);
        assert!(s.primary.is_none() && !s.cloud_missing && !s.fell_back_upfront);
        let s = select::<dyn AsrProvider>(&local(), Some(c), None);
        assert!(s.primary.is_none());
    }

    #[tokio::test]
    async fn fallback_asr_retries_locally_and_flags_it() {
        let flag = Arc::new(AtomicBool::new(false));
        let s = select::<dyn AsrProvider>(
            &cloud(true),
            Some(Arc::new(Asr("cloud", false))),
            Some(Arc::new(Asr("local", true))),
        );
        let asr = wrap_asr(&s, &flag).unwrap();
        let t = asr
            .transcribe(&AudioClip::new(vec![0.0; 10]), &AsrContext::default())
            .await
            .unwrap();
        assert_eq!(t.text, "local");
        assert!(flag.load(Ordering::SeqCst));

        // A working cloud call does not fall back.
        let flag = Arc::new(AtomicBool::new(false));
        let s = select::<dyn AsrProvider>(
            &cloud(true),
            Some(Arc::new(Asr("cloud", true))),
            Some(Arc::new(Asr("local", true))),
        );
        let t = wrap_asr(&s, &flag)
            .unwrap()
            .transcribe(&AudioClip::new(vec![0.0; 10]), &AsrContext::default())
            .await
            .unwrap();
        assert_eq!(t.text, "cloud");
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn fallback_llm_retries_locally_and_without_fallback_errors() {
        let flag = Arc::new(AtomicBool::new(false));
        let s = select::<dyn LlmProvider>(
            &cloud(true),
            Some(Arc::new(Llm("cloud", false))),
            Some(Arc::new(Llm("local", true))),
        );
        assert_eq!(
            wrap_llm(&s, &flag)
                .unwrap()
                .complete("s", "u")
                .await
                .unwrap(),
            "local"
        );
        assert!(flag.load(Ordering::SeqCst));

        let flag = Arc::new(AtomicBool::new(false));
        let s = select::<dyn LlmProvider>(
            &cloud(false),
            Some(Arc::new(Llm("cloud", false))),
            Some(Arc::new(Llm("local", true))),
        );
        assert!(wrap_llm(&s, &flag)
            .unwrap()
            .complete("s", "u")
            .await
            .is_err());
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn lm_health_notice_once_per_outage() {
        let h = LmHealth::default();
        assert!(!h.is_up());
        assert!(h.take_offline_notice());
        assert!(!h.take_offline_notice());
        assert_eq!(h.set(false), None);
        assert_eq!(h.set(true), Some(true));
        assert!(!h.take_offline_notice(), "no notice while up");
        assert_eq!(h.set(false), Some(false));
        assert!(h.take_offline_notice());
        assert!(!h.take_offline_notice());
    }

    #[test]
    fn notice_codes_map_warnings() {
        assert!(notice_codes(&[], false, false).is_empty());
        assert_eq!(
            notice_codes(&[Warning::PolishFellBack], false, false),
            vec![POLISH_TIMED_OUT]
        );
        assert_eq!(notice_codes(&[], false, true), vec!["polish_fell_back"]);
        assert_eq!(
            notice_codes(&[Warning::PolishFellBack], true, true),
            vec!["cloud_fell_back", "polish_fell_back"]
        );
        assert_eq!(
            notice_codes(&[Warning::CloudFellBackToLocal], false, false),
            vec!["cloud_fell_back"]
        );
    }

    #[test]
    fn build_routes_local_by_default_and_cloud_with_key() {
        let http = route::cloud_http_client().unwrap();
        let secrets = MemorySecretStore::new();
        let mut cfg = Config::with_defaults();
        let p = Providers::build(&cfg, &secrets, Some(&http));
        assert!(p.local_polish.is_some() && p.local_translate.is_some() && p.lm_probe.is_some());
        assert!(p.cloud_asr.is_none() && p.cloud_polish.is_none());
        assert_eq!(p.purpose(Purpose::Asr).target, Target::Local);

        cfg.cloud.push(CloudService {
            id: "gem".into(),
            kind: CloudKind::Gemini,
            base_url: None,
            asr_model: None,
            llm_model: None,
        });
        cfg.route.asr = "gem".into();
        cfg.route.translate = "gem".into();
        // No key yet: routed, but the providers cannot be built.
        let p = Providers::build(&cfg, &secrets, Some(&http));
        assert!(matches!(
            p.purpose(Purpose::Asr).target,
            Target::Cloud { .. }
        ));
        assert!(p.cloud_asr.is_none() && p.cloud_translate.is_none());
        secrets.set("gem", &ApiKey::new("AIza-TEST")).unwrap();
        let p = Providers::build(&cfg, &secrets, Some(&http));
        assert_eq!(p.cloud_asr.as_ref().unwrap().name(), "gem");
        assert_eq!(p.cloud_translate.as_ref().unwrap().name(), "gem");
        assert!(p.cloud_polish.is_none());
        assert_eq!(cfg.route.polish, LOCAL);
    }
}
