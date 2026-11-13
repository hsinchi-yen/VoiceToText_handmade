//! Android bindings (uniffi proc-macro mode, Kotlin through library mode).
//!
//! All functions are synchronous; an internal tokio runtime drives the network calls, so
//! Kotlin must call them from `Dispatchers.IO`. Errors are codes only ([`FfiError`]); nothing
//! here logs or returns transcribed text in an error, and API keys never leave the crate.
//! `v2c-core`'s public interface is untouched (see `tasks/plan.md` sections 8.1 and 8.2).

mod config_file;
mod route;
mod services;

pub use config_file::{
    ConfigFileExport, ConfigFilePreview, ImportStatus, ImportedService, SkipReason, SkippedService,
};

pub use route::{choose_asr, lan_service_id, AsrHint, AsrPick, AsrPlan, AsrSource, Lan};
pub use services::{
    CloudProvider, CloudServiceConfig, CloudUse, FfiError, LanService, LanServices, TestResult,
};
pub use v2c_core::Mode;

uniffi::setup_scaffolding!();

/// Simplified -> Traditional (Taiwan phrasing). Text that is already Traditional is returned
/// unchanged.
#[uniffi::export]
pub fn to_zh_tw(text: String) -> String {
    v2c_core::postprocess::zh_tw::to_zh_tw(&text)
}

/// Parses the bundled TN and RD preset files (`[[cloud]]` and `[preset] default_keys`) and
/// remembers them for [`transcribe_pcm`] and [`translate`]. Read only: nothing is written.
/// Calling it again replaces the previous services.
#[uniffi::export]
pub fn load_lan_services(tn_toml: String, rd_toml: String) -> Result<LanServices, FfiError> {
    let registry = services::Registry::from_toml(&[&tn_toml, &rd_toml])?;
    let summary = registry.summary();
    services::set_registry(registry);
    Ok(summary)
}
/// Recognises 16-bit little-endian mono PCM (Kotlin `ByteArray`; odd length is `InvalidAudio`)
/// with a speech service: an OpenAI-compatible LAN service from [`load_lan_services`] (its own
/// timeout; Breeze uses its preset key) or a cloud one from [`set_cloud_services`] (Gemini,
/// OpenAI, custom; Claude answers `Unsupported`). Returns the raw transcript; run it through
/// [`to_zh_tw`] before typing it.
#[uniffi::export]
pub fn transcribe_pcm(pcm_le: Vec<u8>, rate: u32, service_id: String) -> Result<String, FfiError> {
    let registry = services::registry_for(&service_id)?;
    services::runtime()?.block_on(registry.transcribe(pcm_le, rate, &service_id))
}

/// Chinese -> English with the given LLM service (the LAN Qwen or a cloud one).
#[uniffi::export]
pub fn translate(text: String, service_id: String) -> Result<String, FfiError> {
    let registry = services::registry_for(&service_id)?;
    services::runtime()?.block_on(registry.translate(&text, &service_id))
}

/// Replaces the user's cloud services (the previous cloud set, never the LAN ones) and keeps
/// their API keys in memory only. `keys` maps service id to key; a missing key makes the
/// service answer `Unauthorized` (custom endpoints may run without one).
///
/// `Config` for: empty / duplicate id, an id with leading or trailing whitespace, an id that
/// is also a LAN service id, `Custom` without `base_url`, a `base_url` that is not HTTPS
/// (HTTP is accepted on loopback only), or `asr_timeout_ms` of 0. Nothing is changed when it
/// fails.
#[uniffi::export]
pub fn set_cloud_services(
    services: Vec<CloudServiceConfig>,
    keys: std::collections::HashMap<String, String>,
) -> Result<(), FfiError> {
    if services.iter().any(|s| services::lan_has(&s.id)) {
        return Err(FfiError::Config);
    }
    services::set_cloud(services::Registry::from_cloud(services, keys)?);
    Ok(())
}

/// Reads a portable configuration file (`docs/config-file.md`) for the phone and plans the merge
/// into `current` (the phone's cloud services; `stored_keys` are the ids that have a stored key).
/// Pure: nothing is applied and no state changes. A bad file comes back as `error_code` (and
/// `error_line`) in the preview, never as an exception, and never carries file content.
#[uniffi::export]
pub fn parse_config_file(
    text: String,
    current: Vec<CloudServiceConfig>,
    stored_keys: Vec<String>,
) -> ConfigFilePreview {
    config_file::preview(&text, &current, &stored_keys)
}

/// The phone's cloud services as portable file text (no keys, no vocabulary, no polish).
/// `asr_id` / `translate_id` name the services used for recognition / translation.
#[uniffi::export]
pub fn export_config_file(
    services: Vec<CloudServiceConfig>,
    asr_id: Option<String>,
    translate_id: Option<String>,
    stored_keys: Vec<String>,
) -> ConfigFileExport {
    config_file::export(
        &services,
        asr_id.as_deref(),
        translate_id.as_deref(),
        &stored_keys,
    )
}

/// Checks one configured service: recognition sends a generated 1 s tone, translation a fixed
/// sentence. On success `error` is `None` and `latency_ms` is the round trip; on failure
/// `error` is the code (`Unauthorized` bad key, `RateLimited` quota / rate limit,
/// `Unreachable` network, `Unsupported` endpoint without audio, `Timeout`).
#[uniffi::export]
pub fn test_connection(service_id: String, purpose: CloudUse) -> TestResult {
    let run = || -> Result<u64, FfiError> {
        let registry = services::registry_for(&service_id)?;
        services::runtime()?.block_on(registry.test(&service_id, purpose))
    };
    match run() {
        Ok(latency_ms) => TestResult {
            latency_ms,
            error: None,
        },
        Err(e) => TestResult {
            latency_ms: 0,
            error: Some(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn to_zh_tw_converts_and_keeps_traditional() {
        assert_eq!(to_zh_tw("软件".into()), "軟體");
        assert_eq!(to_zh_tw("語音輸入 hello".into()), "語音輸入 hello");
    }

    // The only test touching the global registries (LAN and cloud), run in one sequence.
    #[test]
    fn load_then_call_reports_codes() {
        assert_eq!(
            translate("你好".into(), "tn-qwen".into()).unwrap_err(),
            FfiError::NotLoaded
        );
        let tn = include_str!("../../../apps/desktop/src-tauri/presets/TN_CUSTOM_LLM_ADDR.toml");
        let rd = include_str!("../../../apps/desktop/src-tauri/presets/RD_CUSTOM_LLM_ADDR.toml");
        assert_eq!(
            load_lan_services("bad [".into(), rd.into()).unwrap_err(),
            FfiError::Config
        );
        let s = load_lan_services(tn.into(), rd.into()).unwrap();
        assert_eq!(s.services.len(), 6);
        assert_eq!(
            translate("你好".into(), "missing".into()).unwrap_err(),
            FfiError::UnknownService
        );
        assert_eq!(
            transcribe_pcm(vec![], 16000, "tn-tea-asr".into()).unwrap_err(),
            FfiError::InvalidAudio
        );

        cloud_through_the_ffi_functions();
    }

    fn cloud_config(id: &str, base: &str) -> CloudServiceConfig {
        CloudServiceConfig {
            id: id.into(),
            provider: CloudProvider::Custom,
            model: "m".into(),
            asr_model: Some("am".into()),
            base_url: Some(base.into()),
            uses: vec![CloudUse::Asr, CloudUse::Translate],
            asr_timeout_ms: None,
        }
    }

    /// Runs after the LAN services are loaded (same test: both registries are process-wide).
    fn cloud_through_the_ffi_functions() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        rt.block_on(
            Mock::given(method("POST"))
                .and(path("/v1/audio/transcriptions"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "cloud"})),
                )
                .mount(&server),
        );
        rt.block_on(
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "choices": [{"message": {"role": "assistant", "content": "Hi."}}]
                })))
                .mount(&server),
        );
        let base = format!("{}/v1", server.uri());

        // A cloud id is unknown until set.
        assert_eq!(
            translate("你好".into(), "cloud-1".into()).unwrap_err(),
            FfiError::UnknownService
        );
        set_cloud_services(vec![cloud_config("cloud-1", &base)], HashMap::new()).unwrap();
        assert_eq!(
            transcribe_pcm(vec![1; 3200], 16000, "cloud-1".into()).unwrap(),
            "cloud"
        );
        assert_eq!(translate("你好".into(), "cloud-1".into()).unwrap(), "Hi.");
        let t = test_connection("cloud-1".into(), CloudUse::Translate);
        assert_eq!(t.error, None);
        assert_eq!(
            test_connection("nope".into(), CloudUse::Asr).error,
            Some(FfiError::UnknownService)
        );

        // A LAN id cannot be reused, and a failed call changes nothing.
        assert_eq!(
            set_cloud_services(vec![cloud_config("tn-qwen", &base)], HashMap::new()).unwrap_err(),
            FfiError::Config
        );
        let mut bad = cloud_config("cloud-2", "http://example.com/v1");
        assert_eq!(
            set_cloud_services(vec![bad.clone()], HashMap::new()).unwrap_err(),
            FfiError::Config
        );
        bad.base_url = None;
        assert_eq!(
            set_cloud_services(vec![bad], HashMap::new()).unwrap_err(),
            FfiError::Config
        );
        assert_eq!(translate("你好".into(), "cloud-1".into()).unwrap(), "Hi.");

        // A new set replaces the old cloud services and leaves the LAN ones alone.
        set_cloud_services(vec![cloud_config("cloud-2", &base)], HashMap::new()).unwrap();
        assert_eq!(
            translate("你好".into(), "cloud-1".into()).unwrap_err(),
            FfiError::UnknownService
        );
        assert_eq!(translate("你好".into(), "cloud-2".into()).unwrap(), "Hi.");
        assert_eq!(
            translate("  ".into(), "tn-qwen".into()).unwrap_err(),
            FfiError::EmptyText
        );
        assert_eq!(
            transcribe_pcm(vec![], 16000, "rd-breeze-asr".into()).unwrap_err(),
            FfiError::InvalidAudio
        );
        set_cloud_services(vec![], HashMap::new()).unwrap();
        assert_eq!(
            translate("你好".into(), "cloud-2".into()).unwrap_err(),
            FfiError::UnknownService
        );
        assert_eq!(
            translate("  ".into(), "tn-qwen".into()).unwrap_err(),
            FfiError::EmptyText
        );
    }
}
