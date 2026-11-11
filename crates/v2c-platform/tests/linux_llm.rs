//! T3.3: polish / translate against an existing OpenAI-compatible LLM server (on the Linux dev
//! host: llama.cpp), with the provider built from `Config` the same way the desktop app does
//! (`apps/desktop/src-tauri/src/providers.rs`): `[local] llm_base_url` / `llm_model`, or a
//! `[[cloud]]` `open_ai_compat` service routed through `route::resolve` + `build_cloud_llm`.
//!
//! Hermetic by default (wiremock). `llm_real_*` tests are `#[ignore]` and need
//! `V2C_TEST_LLM_BASE_URL`; they send only a synthetic sentence and print lengths/timings.

#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
use v2c_core::audio::AudioClip;
use v2c_core::config::{CloudKind, Config};
use v2c_core::error::{ProviderErrorKind, Stage};
use v2c_core::llm::{LlmProvider, OpenAiCompatLlm};
use v2c_core::pipeline::{Pipeline, PipelineConfig, Warning};
use v2c_core::postprocess::fillers::strip_fillers;
use v2c_core::route::{self, cloud_http_client, Purpose, Target};
use v2c_core::secrets::{ApiKey, MemorySecretStore, SecretStore};
use v2c_core::{prompts, Lang, Mode, V2cError};

/// Desktop `providers::LOCAL_LLM_TIMEOUT` (not in config; the desktop crate is not a
/// dependency of this one, so the value is mirrored here).
const LOCAL_LLM_TIMEOUT: Duration = Duration::from_secs(8);

const SENTENCE: &str = "嗯，那個，我們明天早上十點開會";
const POLISHED: &str = "我們明天早上十點開會。";
const TRANSLATED: &str = "We have a meeting at ten tomorrow morning.";

// ----------------------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------------------

/// `[local]` pointing at `base_url` (e.g. a LAN llama.cpp), as a user would write it.
fn local_config(base_url: &str, model: &str) -> Config {
    Config::from_toml_str(&format!(
        "[local]\nllm_base_url = \"{base_url}\"\nllm_model = \"{model}\"\n\n[gpu]\nenabled = false\n"
    ))
    .unwrap()
}

/// `[[cloud]]` custom OpenAI-compatible endpoint used for polish and translate.
fn cloud_config(base_url: &str, model: &str, llm_cloud_ms: u64) -> Config {
    Config::from_toml_str(&format!(
        r#"
[route]
polish = "lan-llm"
translate = "lan-llm"

[timeouts]
llm_cloud_ms = {llm_cloud_ms}

[gpu]
enabled = false

[[cloud]]
id = "lan-llm"
kind = "open_ai_compat"
base_url = "{base_url}"
llm_model = "{model}"
"#
    ))
    .unwrap()
}

/// Same construction as `Providers::build` for the local LLM.
fn local_llm(cfg: &Config, stage: Stage, timeout: Duration) -> OpenAiCompatLlm {
    let http = cloud_http_client().unwrap();
    OpenAiCompatLlm::new(
        http.client().clone(),
        cfg.local.llm_base_url.clone(),
        None,
        cfg.local.llm_model.clone(),
        timeout,
    )
    .with_stage(stage)
}

/// Same construction as `Providers::build` for a routed cloud LLM.
fn cloud_llm(cfg: &Config, secrets: &dyn SecretStore, purpose: Purpose) -> Box<dyn LlmProvider> {
    let plan = route::resolve(&cfg.route, &cfg.cloud);
    assert!(plan.issues.is_empty(), "{:?}", plan.issues);
    let Target::Cloud { service_id, kind } = &plan.get(purpose).target else {
        panic!("{purpose:?} not routed to the cloud service");
    };
    assert_eq!(*kind, CloudKind::OpenAiCompat);
    let svc = cfg.cloud_service(service_id).unwrap();
    let stage = match purpose {
        Purpose::Translate => Stage::Translate,
        _ => Stage::Polish,
    };
    route::build_cloud_llm(
        svc,
        secrets,
        &cloud_http_client().unwrap(),
        Duration::from_millis(cfg.timeouts.llm_cloud_ms),
        stage,
    )
    .unwrap()
    .expect("model configured")
}

/// Mirrors desktop `app_state::pipeline_config`.
fn pipeline_config(cfg: &Config) -> PipelineConfig {
    PipelineConfig {
        polish_enabled: cfg.polish.enabled,
        polish_timeout_ms: cfg.polish.timeout_ms,
        min_clip_ms: cfg.audio.min_clip_ms,
        vocabulary: Vec::new(),
    }
}

fn chat(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }]
    }))
}

/// llama.cpp with `--reasoning-format` (default): reasoning in its own field, clean content.
fn llama_cpp_reply(content: &str, reasoning: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "id": "chatcmpl-x",
        "object": "chat.completion",
        "model": "Qwen3.8-27B-UD-Q6_K_L",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "reasoning_content": reasoning,
                "content": content
            }
        }],
        "usage": { "prompt_tokens": 120, "completion_tokens": 40, "total_tokens": 160 },
        "timings": { "prompt_ms": 30.0, "predicted_ms": 200.0 }
    }))
}

async fn mount_chat(server: &MockServer, template: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(template)
        .mount(server)
        .await;
}

fn provider_kind(e: &V2cError) -> (Stage, ProviderErrorKind) {
    match e {
        V2cError::Provider { stage, kind, .. } => (*stage, *kind),
        other => panic!("expected provider error, got {other:?}"),
    }
}

/// A local port with nothing listening.
fn refused_base_url() -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    format!("http://127.0.0.1:{port}/v1")
}

struct FixedAsr(&'static str);

#[async_trait]
impl AsrProvider for FixedAsr {
    fn name(&self) -> &str {
        "mock-asr"
    }
    async fn transcribe(&self, _: &AudioClip, _: &AsrContext) -> v2c_core::Result<Transcript> {
        Ok(Transcript {
            text: self.0.into(),
            lang: Lang::Zh,
        })
    }
}

/// 1 s, 220 Hz, clearly above the silence threshold.
fn speech_like_clip() -> AudioClip {
    AudioClip::new(
        (0..16_000)
            .map(|i| 0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / 16_000.0).sin())
            .collect(),
    )
}

fn body(req: &Request) -> Value {
    serde_json::from_slice(&req.body).unwrap()
}

// ----------------------------------------------------------------------------------------
// Config-driven construction
// ----------------------------------------------------------------------------------------

#[tokio::test]
async fn local_config_sets_base_url_and_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat(POLISHED))
        .expect(1)
        .mount(&server)
        .await;
    // Trailing slash as users often paste it.
    let cfg = local_config(&format!("{}/v1/", server.uri()), "Qwen3.8-27B-UD-Q6_K_L");
    assert!(!cfg.gpu.enabled);
    let llm = local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT);
    assert_eq!(llm.name(), "lmstudio");
    assert_eq!(llm.base_url(), format!("{}/v1", server.uri()));

    let out = llm
        .complete(&prompts::polish(&[]), &prompts::wrap_transcript(SENTENCE))
        .await
        .unwrap();
    assert_eq!(out, POLISHED);
    let reqs = server.received_requests().await.unwrap();
    let b = body(&reqs[0]);
    assert_eq!(b["model"], json!("Qwen3.8-27B-UD-Q6_K_L"));
    assert_eq!(b["stream"], json!(false));
    assert!(reqs[0].headers.get("authorization").is_none());
}

#[tokio::test]
async fn local_config_without_model_omits_it() {
    let server = MockServer::start().await;
    mount_chat(&server, chat("ok")).await;
    let cfg = Config::from_toml_str(&format!(
        "[local]\nllm_base_url = \"{}/v1\"\n",
        server.uri()
    ))
    .unwrap();
    assert_eq!(cfg.local.llm_model, None);
    local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT)
        .complete("s", "u")
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert!(body(&reqs[0]).get("model").is_none());
}

#[tokio::test]
async fn cloud_compat_route_sets_name_model_key_and_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer lan-secret"))
        .respond_with(chat(TRANSLATED))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = cloud_config(
        &format!("{}/v1", server.uri()),
        "Qwen3.8-27B-UD-Q6_K_L",
        3000,
    );
    let plan = route::resolve(&cfg.route, &cfg.cloud);
    assert_eq!(plan.asr.target, Target::Local, "ASR stays local");
    assert!(plan.polish.fallback_to_local && plan.translate.fallback_to_local);

    let secrets = MemorySecretStore::new();
    secrets.set("lan-llm", &ApiKey::new("lan-secret")).unwrap();
    let llm = cloud_llm(&cfg, &secrets, Purpose::Translate);
    assert_eq!(llm.name(), "lan-llm");
    let out = llm.complete("s", "你好").await.unwrap();
    assert_eq!(out, TRANSLATED);
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(body(&reqs[0])["model"], json!("Qwen3.8-27B-UD-Q6_K_L"));
}

#[tokio::test]
async fn cloud_compat_route_without_key_sends_no_auth() {
    let server = MockServer::start().await;
    mount_chat(&server, chat("ok")).await;
    let cfg = cloud_config(&format!("{}/v1", server.uri()), "m", 3000);
    cloud_llm(&cfg, &MemorySecretStore::new(), Purpose::Polish)
        .complete("s", "u")
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert!(reqs[0].headers.get("authorization").is_none());
}

#[tokio::test]
async fn cloud_timeout_comes_from_config() {
    let server = MockServer::start().await;
    mount_chat(
        &server,
        chat("late").set_delay(Duration::from_millis(2_000)),
    )
    .await;
    let cfg = cloud_config(&format!("{}/v1", server.uri()), "m", 150);
    let llm = cloud_llm(&cfg, &MemorySecretStore::new(), Purpose::Translate);
    let started = Instant::now();
    let e = llm.complete("s", "u").await.unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert!(
        matches!(
            e,
            V2cError::Timeout {
                stage: Stage::Translate,
                ms: 150
            }
        ),
        "{e:?}"
    );
}

// ----------------------------------------------------------------------------------------
// Failure modes through the config-built provider
// ----------------------------------------------------------------------------------------

#[tokio::test]
async fn connection_refused_is_unreachable() {
    let cfg = local_config(&refused_base_url(), "m");
    // Long timeout: Windows retries a refused localhost connect for about 2 s.
    let llm = local_llm(&cfg, Stage::Polish, Duration::from_secs(10));
    let e = llm.complete("s", "u").await.unwrap_err();
    assert_eq!(
        provider_kind(&e),
        (Stage::Polish, ProviderErrorKind::Unreachable)
    );
    let cfg = cloud_config(&refused_base_url(), "m", 10_000);
    let e = cloud_llm(&cfg, &MemorySecretStore::new(), Purpose::Translate)
        .complete("s", "u")
        .await
        .unwrap_err();
    assert_eq!(
        provider_kind(&e),
        (Stage::Translate, ProviderErrorKind::Unreachable)
    );
}

#[tokio::test]
async fn http_status_codes_map_to_kinds() {
    for (code, want) in [
        (401, ProviderErrorKind::Unauthorized),
        (500, ProviderErrorKind::Server),
        // llama.cpp answers 501 for endpoints it does not implement.
        (501, ProviderErrorKind::Unsupported),
    ] {
        let server = MockServer::start().await;
        mount_chat(
            &server,
            ResponseTemplate::new(code).set_body_json(json!({
                "error": { "code": code, "message": "x", "type": "not_supported_error" }
            })),
        )
        .await;
        let cfg = local_config(&format!("{}/v1", server.uri()), "m");
        let e = local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT)
            .complete("s", SENTENCE)
            .await
            .unwrap_err();
        assert_eq!(provider_kind(&e), (Stage::Polish, want), "HTTP {code}");
        assert!(!e.to_string().contains("明天"), "{e}");
    }
}

#[tokio::test]
async fn truncated_malformed_and_empty_answers_are_bad_response() {
    for template in [
        // finish_reason=length: part of the sentence would be lost.
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "我們明天" },
                          "finish_reason": "length" }]
        })),
        // Malformed JSON (cut off mid-body).
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_string("{\"choices\":[{\"message\":{\"content\":\"我們"),
        // Thinking used the whole `max_tokens` budget (seen on the dev host's llama.cpp with
        // thinking on): reasoning only, empty content, finish_reason=length.
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "content": "",
                                       "reasoning_content": "The user wants me to..." },
                          "finish_reason": "length" }]
        })),
        // Same, but the server reports `stop`.
        llama_cpp_reply("", "The user wants me to polish..."),
        // No content field at all.
        ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{ "message": { "role": "assistant", "reasoning_content": "x" },
                          "finish_reason": "stop" }]
        })),
    ] {
        let server = MockServer::start().await;
        mount_chat(&server, template).await;
        let cfg = local_config(&format!("{}/v1", server.uri()), "m");
        let e = local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT)
            .complete("s", "u")
            .await
            .unwrap_err();
        assert_eq!(
            provider_kind(&e),
            (Stage::Polish, ProviderErrorKind::BadResponse)
        );
    }
}

// ----------------------------------------------------------------------------------------
// Response parsing (llama.cpp shapes)
// ----------------------------------------------------------------------------------------

#[tokio::test]
async fn llama_cpp_reasoning_content_is_ignored() {
    let server = MockServer::start().await;
    mount_chat(
        &server,
        llama_cpp_reply(POLISHED, "使用者說了贅字「嗯，那個」，應該刪掉。"),
    )
    .await;
    let cfg = local_config(&format!("{}/v1", server.uri()), "Qwen3.8-27B-UD-Q6_K_L");
    let out = local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT)
        .complete(&prompts::polish(&[]), &prompts::wrap_transcript(SENTENCE))
        .await
        .unwrap();
    assert_eq!(out, POLISHED);
}

#[tokio::test]
async fn think_block_in_content_is_stripped() {
    // llama.cpp started with `--reasoning-format none`: reasoning stays inline.
    let server = MockServer::start().await;
    mount_chat(
        &server,
        chat("<think>\nThe user wants a polished sentence.\n</think>\n\n我們明天早上十點開會。"),
    )
    .await;
    let cfg = local_config(&format!("{}/v1", server.uri()), "m");
    let out = local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT)
        .complete("s", SENTENCE)
        .await
        .unwrap();
    assert_eq!(out, POLISHED);
    assert!(!out.contains("think"));
}

// ----------------------------------------------------------------------------------------
// Pipeline behaviour on LLM failure
// ----------------------------------------------------------------------------------------

fn pipeline_with(cfg: &Config, llm: Arc<dyn LlmProvider>) -> Pipeline {
    Pipeline::new(
        Arc::new(FixedAsr(SENTENCE)),
        Some(llm),
        pipeline_config(cfg),
    )
    .with_polish_timeout_cap_ms(LOCAL_LLM_TIMEOUT.as_millis() as u64)
}

#[tokio::test]
async fn polish_server_error_falls_back_to_filler_removal() {
    let server = MockServer::start().await;
    mount_chat(&server, ResponseTemplate::new(500)).await;
    let cfg = local_config(&format!("{}/v1", server.uri()), "m");
    let llm = Arc::new(local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT));
    let out = pipeline_with(&cfg, llm)
        .run(speech_like_clip(), Mode::Dictate)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.warnings, vec![Warning::PolishFellBack]);
    assert_eq!(out.text, strip_fillers(SENTENCE));
    assert!(!out.text.starts_with('嗯'), "fillers removed");
}

#[tokio::test]
async fn polish_unreachable_falls_back_to_filler_removal() {
    let cfg = local_config(&refused_base_url(), "m");
    let llm = Arc::new(local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT));
    let out = pipeline_with(&cfg, llm)
        .run(speech_like_clip(), Mode::Dictate)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.warnings, vec![Warning::PolishFellBack]);
    assert_eq!(out.text, strip_fillers(SENTENCE));
}

#[tokio::test]
async fn polish_slower_than_config_timeout_falls_back() {
    // A thinking model (about 2.7 s on the dev host's llama.cpp) against `[polish] timeout_ms`.
    let server = MockServer::start().await;
    mount_chat(
        &server,
        llama_cpp_reply(POLISHED, "...").set_delay(Duration::from_millis(1_500)),
    )
    .await;
    let mut cfg = local_config(&format!("{}/v1", server.uri()), "m");
    cfg.polish.timeout_ms = 200;
    let llm = Arc::new(local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT));
    let started = Instant::now();
    let out = pipeline_with(&cfg, llm)
        .run(speech_like_clip(), Mode::Dictate)
        .await
        .unwrap()
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(1_200));
    assert_eq!(out.warnings, vec![Warning::PolishFellBack]);
    assert_eq!(out.text, strip_fillers(SENTENCE));
}

#[tokio::test]
async fn polish_success_uses_llm_output() {
    let server = MockServer::start().await;
    mount_chat(&server, llama_cpp_reply(POLISHED, "...")).await;
    let cfg = local_config(&format!("{}/v1", server.uri()), "m");
    let llm = Arc::new(local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT));
    let out = pipeline_with(&cfg, llm)
        .run(speech_like_clip(), Mode::Dictate)
        .await
        .unwrap()
        .unwrap();
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(out.text, POLISHED);
    assert!(out.timings.llm_ms.is_some());
}

#[tokio::test]
async fn translate_failure_is_an_error() {
    for template in [
        ResponseTemplate::new(500),
        ResponseTemplate::new(501),
        ResponseTemplate::new(200).set_body_string("not json"),
    ] {
        let server = MockServer::start().await;
        mount_chat(&server, template).await;
        let cfg = cloud_config(&format!("{}/v1", server.uri()), "m", 3000);
        let llm: Arc<dyn LlmProvider> = Arc::from(cloud_llm(
            &cfg,
            &MemorySecretStore::new(),
            Purpose::Translate,
        ));
        let e = pipeline_with(&cfg, llm)
            .run(speech_like_clip(), Mode::TranslateZhToEn)
            .await
            .unwrap_err();
        let (stage, _) = provider_kind(&e);
        assert_eq!(stage, Stage::Translate);
    }
    // Unreachable translate server: also an error, never untranslated Chinese.
    let cfg = local_config(&refused_base_url(), "m");
    let llm = Arc::new(local_llm(&cfg, Stage::Translate, Duration::from_secs(10)));
    let e = pipeline_with(&cfg, llm)
        .run(speech_like_clip(), Mode::TranslateZhToEn)
        .await
        .unwrap_err();
    assert_eq!(
        provider_kind(&e),
        (Stage::Translate, ProviderErrorKind::Unreachable)
    );
}

#[tokio::test]
async fn translate_success_outputs_english() {
    let server = MockServer::start().await;
    mount_chat(&server, llama_cpp_reply(TRANSLATED, "...")).await;
    let cfg = local_config(&format!("{}/v1", server.uri()), "m");
    let llm = Arc::new(local_llm(&cfg, Stage::Translate, LOCAL_LLM_TIMEOUT));
    let out = pipeline_with(&cfg, llm)
        .run(speech_like_clip(), Mode::TranslateZhToEn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.text, TRANSLATED);
    assert_eq!(out.lang, Lang::En);
}

// ----------------------------------------------------------------------------------------
// Real endpoint (opt-in)
// ----------------------------------------------------------------------------------------

/// Synthetic test sentence (not user speech).
const REAL_SENTENCE: &str = "嗯，這是一個連線測試，我們下週三下午兩點開會";

struct RealEnv {
    base_url: String,
    model: Option<String>,
    timeout: Duration,
}

fn real_env() -> Option<RealEnv> {
    let base_url = std::env::var("V2C_TEST_LLM_BASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())?;
    let model = std::env::var("V2C_TEST_LLM_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let timeout_ms = std::env::var("V2C_TEST_LLM_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15_000);
    Some(RealEnv {
        base_url,
        model,
        timeout: Duration::from_millis(timeout_ms),
    })
}

fn real_config(env: &RealEnv) -> Config {
    let mut cfg = Config::with_defaults();
    cfg.local.llm_base_url = env.base_url.clone();
    cfg.local.llm_model = env.model.clone();
    cfg.gpu.enabled = false;
    cfg
}

/// One direct provider call with a generous timeout; prints only length and timing.
async fn real_direct(stage: Stage) {
    let Some(env) = real_env() else {
        eprintln!("llm_real: V2C_TEST_LLM_BASE_URL not set; skipped");
        return;
    };
    let cfg = real_config(&env);
    let system = match stage {
        Stage::Translate => prompts::translate(&[]),
        _ => prompts::polish(&[]),
    };
    let llm = local_llm(&cfg, stage, env.timeout);
    let started = Instant::now();
    let out = llm
        .complete(&system, &prompts::wrap_transcript(REAL_SENTENCE))
        .await;
    let ms = started.elapsed().as_millis();
    match &out {
        Ok(text) => println!(
            "llm_real {stage:?}: ok, {} chars, {ms} ms (timeout {} ms)",
            text.chars().count(),
            env.timeout.as_millis()
        ),
        Err(e) => println!("llm_real {stage:?}: error {e}, {ms} ms"),
    }
    let text = out.expect("real endpoint answered");
    assert!(!text.trim().is_empty());
    assert!(!text.contains("<think>") && !text.contains("</think>"));
}

#[tokio::test]
#[ignore = "needs V2C_TEST_LLM_BASE_URL (a real OpenAI-compatible server)"]
async fn llm_real_polish() {
    real_direct(Stage::Polish).await;
}

#[tokio::test]
#[ignore = "needs V2C_TEST_LLM_BASE_URL (a real OpenAI-compatible server)"]
async fn llm_real_translate() {
    real_direct(Stage::Translate).await;
}

/// Through the pipeline with the default `[polish] timeout_ms` (1000 ms) and the desktop's
/// cap: reports whether polish beat the budget or fell back (both are valid outcomes; the
/// fallback must then be rule-based filler removal).
#[tokio::test]
#[ignore = "needs V2C_TEST_LLM_BASE_URL (a real OpenAI-compatible server)"]
async fn llm_real_pipeline_polish_default_timeout() {
    let Some(env) = real_env() else {
        eprintln!("llm_real: V2C_TEST_LLM_BASE_URL not set; skipped");
        return;
    };
    let cfg = real_config(&env);
    assert_eq!(cfg.polish.timeout_ms, 1000);
    let asr: Arc<dyn AsrProvider> = Arc::new(FixedAsr(REAL_SENTENCE));
    let polish = Pipeline::new(
        asr.clone(),
        Some(Arc::new(local_llm(&cfg, Stage::Polish, LOCAL_LLM_TIMEOUT))),
        pipeline_config(&cfg),
    )
    .with_polish_timeout_cap_ms(LOCAL_LLM_TIMEOUT.as_millis() as u64);
    let out = polish
        .run(speech_like_clip(), Mode::Dictate)
        .await
        .unwrap()
        .unwrap();
    let fell_back = out.warnings.contains(&Warning::PolishFellBack);
    println!(
        "llm_real pipeline polish: budget {} ms, llm_ms {:?}, total_ms {}, fell_back {fell_back}, {} chars",
        polish.polish_timeout_for(REAL_SENTENCE.chars().count()),
        out.timings.llm_ms,
        out.timings.total_ms,
        out.text.chars().count()
    );
    assert!(!out.text.is_empty());
    if fell_back {
        assert_eq!(out.text, strip_fillers(REAL_SENTENCE));
    }
}

/// Translate through the pipeline (provider timeout only; translate has no polish budget).
#[tokio::test]
#[ignore = "needs V2C_TEST_LLM_BASE_URL (a real OpenAI-compatible server)"]
async fn llm_real_pipeline_translate() {
    let Some(env) = real_env() else {
        eprintln!("llm_real: V2C_TEST_LLM_BASE_URL not set; skipped");
        return;
    };
    let cfg = real_config(&env);
    let asr: Arc<dyn AsrProvider> = Arc::new(FixedAsr(REAL_SENTENCE));
    let translate = Pipeline::new(
        asr,
        Some(Arc::new(local_llm(&cfg, Stage::Translate, env.timeout))),
        pipeline_config(&cfg),
    );
    let started = Instant::now();
    let out = translate
        .run(speech_like_clip(), Mode::TranslateZhToEn)
        .await;
    let ms = started.elapsed().as_millis();
    match &out {
        Ok(Some(o)) => println!(
            "llm_real pipeline translate: ok, {} chars, llm_ms {:?}, {ms} ms",
            o.text.chars().count(),
            o.timings.llm_ms
        ),
        Ok(None) => println!("llm_real pipeline translate: no output, {ms} ms"),
        Err(e) => println!("llm_real pipeline translate: error {e}, {ms} ms"),
    }
    let out = out.unwrap().unwrap();
    assert_eq!(out.lang, Lang::En);
    assert!(!out.text.contains("<think>"));
}
