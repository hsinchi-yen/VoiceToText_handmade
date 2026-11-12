//! Android bindings (uniffi proc-macro mode, Kotlin through library mode).
//!
//! All functions are synchronous; an internal tokio runtime drives the network calls, so
//! Kotlin must call them from `Dispatchers.IO`. Errors are codes only ([`FfiError`]); nothing
//! here logs or returns transcribed text in an error, and API keys never leave the crate.
//! `v2c-core`'s public interface is untouched (see `tasks/plan.md` sections 8.1 and 8.2).

mod route;
mod services;

pub use route::{choose_asr, lan_service_id, AsrHint, AsrPick, AsrPlan, AsrSource, Lan};
pub use services::{FfiError, LanService, LanServices};
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

/// Recognises 16-bit little-endian mono PCM (Kotlin `ByteArray`; odd length is `InvalidAudio`) with an OpenAI-compatible ASR service from
/// [`load_lan_services`] (its own timeout; Breeze uses its preset key). Returns the raw
/// transcript; run it through [`to_zh_tw`] before typing it.
#[uniffi::export]
pub fn transcribe_pcm(pcm_le: Vec<u8>, rate: u32, service_id: String) -> Result<String, FfiError> {
    let registry = services::registry()?;
    services::runtime()?.block_on(registry.transcribe(pcm_le, rate, &service_id))
}

/// Chinese -> English with the given LLM service (the LAN Qwen).
#[uniffi::export]
pub fn translate(text: String, service_id: String) -> Result<String, FfiError> {
    let registry = services::registry()?;
    services::runtime()?.block_on(registry.translate(&text, &service_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_zh_tw_converts_and_keeps_traditional() {
        assert_eq!(to_zh_tw("软件".into()), "軟體");
        assert_eq!(to_zh_tw("語音輸入 hello".into()), "語音輸入 hello");
    }

    // The only test touching the global registry.
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
    }
}
