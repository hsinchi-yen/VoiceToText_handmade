//! T3.3: speech recognition through a remote OpenAI-compatible ASR endpoint
//! (`POST {base_url}/audio/transcriptions`), built from `Config` the way the desktop app does:
//! `[route] asr = "<id>"` → `route::resolve` → `route::build_cloud_asr` (a `CompatAsr`).
//!
//! The cloud → local ASR retry itself lives in the desktop app (`providers::FallbackAsr` in
//! `apps/desktop/src-tauri/src/providers.rs`); core only decides it (`PurposePlan::
//! fallback_to_local`), which is what is tested here.
//!
//! Hermetic by default (wiremock). `asr_real_*` is `#[ignore]` and needs
//! `V2C_TEST_ASR_BASE_URL`; the key is read from the file in `V2C_TEST_ASR_KEY_FILE` and never
//! printed; only the HTTP outcome, text length and elapsed time are printed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use v2c_core::asr::{AsrContext, AsrProvider};
use v2c_core::audio::AudioClip;
use v2c_core::config::{CloudKind, Config, LocalAsr};
use v2c_core::error::{ProviderErrorKind, Stage};
use v2c_core::pipeline::{Pipeline, PipelineConfig};
use v2c_core::route::{self, cloud_http_client, Purpose, RouteIssue, Target};
use v2c_core::secrets::{ApiKey, MemorySecretStore, SecretStore};
use v2c_core::{Mode, V2cError};

const ASR_ID: &str = "lan-asr";

fn asr_config(base_url: &str, model: Option<&str>, asr_cloud_ms: u64, fallback: bool) -> Config {
    let model = model
        .map(|m| format!("asr_model = \"{m}\"\n"))
        .unwrap_or_default();
    Config::from_toml_str(&format!(
        r#"
[route]
asr = "{ASR_ID}"
cloud_fallback_to_local = {fallback}

[timeouts]
asr_cloud_ms = {asr_cloud_ms}

[gpu]
enabled = false

[[cloud]]
id = "{ASR_ID}"
kind = "open_ai_compat"
base_url = "{base_url}"
{model}"#
    ))
    .unwrap()
}

/// Same construction as desktop `Providers::build` for the routed ASR.
fn cloud_asr(cfg: &Config, secrets: &dyn SecretStore) -> Box<dyn AsrProvider> {
    let plan = route::resolve(&cfg.route, &cfg.cloud);
    assert!(plan.issues.is_empty(), "{:?}", plan.issues);
    let Target::Cloud { service_id, kind } = &plan.get(Purpose::Asr).target else {
        panic!("ASR not routed to the cloud service");
    };
    assert_eq!(*kind, CloudKind::OpenAiCompat);
    route::build_cloud_asr(
        cfg.cloud_service(service_id).unwrap(),
        secrets,
        &cloud_http_client().unwrap(),
        Duration::from_millis(cfg.timeouts.asr_cloud_ms),
    )
    .unwrap()
    .expect("asr_model configured")
}

/// 1 s synthetic tone (not speech).
fn tone() -> AudioClip {
    AudioClip::new(
        (0..16_000)
            .map(|i| 0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / 16_000.0).sin())
            .collect(),
    )
}

fn ctx(prompt: &str) -> AsrContext {
    AsrContext {
        vocabulary: vec![],
        prompt: prompt.into(),
    }
}

async fn mount(server: &MockServer, template: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(template)
        .mount(server)
        .await;
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn provider_kind(e: &V2cError) -> (Stage, ProviderErrorKind) {
    match e {
        V2cError::Provider { stage, kind, .. } => (*stage, *kind),
        other => panic!("expected provider error, got {other:?}"),
    }
}

// ----------------------------------------------------------------------------------------
// Request shape
// ----------------------------------------------------------------------------------------

#[tokio::test]
async fn multipart_request_with_key_model_prompt_and_wav() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(header("authorization", "Bearer asr-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "text": "我們明天見" })))
        .expect(1)
        .mount(&server)
        .await;
    let cfg = asr_config(
        &format!("{}/v1", server.uri()),
        Some("breeze-asr-25"),
        5000,
        true,
    );
    let secrets = MemorySecretStore::new();
    secrets.set(ASR_ID, &ApiKey::new("asr-test-key")).unwrap();
    let asr = cloud_asr(&cfg, &secrets);
    assert_eq!(asr.name(), ASR_ID);
    let t = asr.transcribe(&tone(), &ctx("Tauri")).await.unwrap();
    assert_eq!(t.text, "我們明天見");

    let reqs: Vec<Request> = server.received_requests().await.unwrap();
    let req = &reqs[0];
    let ct = req.headers.get("content-type").unwrap().to_str().unwrap();
    assert!(ct.starts_with("multipart/form-data"), "{ct}");
    let body = String::from_utf8_lossy(&req.body);
    assert!(body.contains("name=\"model\"\r\n\r\nbreeze-asr-25"));
    assert!(body.contains("name=\"response_format\"\r\n\r\njson"));
    assert!(body.contains("name=\"prompt\"\r\n\r\nTauri"));
    assert!(body.contains("name=\"file\"; filename=\"audio.wav\""));
    assert!(contains(&req.body, b"RIFF") && contains(&req.body, b"WAVE"));
    // 1 s of 16 kHz PCM16 mono = 32000 bytes of samples.
    assert!(req.body.len() > 32_000);
}

#[tokio::test]
async fn no_key_sends_no_authorization_and_no_empty_prompt() {
    let server = MockServer::start().await;
    mount(
        &server,
        ResponseTemplate::new(200).set_body_json(json!({ "text": "ok" })),
    )
    .await;
    let cfg = asr_config(&format!("{}/v1/", server.uri()), Some("m"), 5000, true);
    cloud_asr(&cfg, &MemorySecretStore::new())
        .transcribe(&tone(), &ctx(""))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert!(reqs[0].headers.get("authorization").is_none());
    assert!(!String::from_utf8_lossy(&reqs[0].body).contains("name=\"prompt\""));
}

// ----------------------------------------------------------------------------------------
// Failure modes
// ----------------------------------------------------------------------------------------

#[tokio::test]
async fn http_errors_map_to_kinds() {
    for (code, want) in [
        // e.g. the dev host's ASR server without a key: 401 "Invalid API Key".
        (401, ProviderErrorKind::Unauthorized),
        // A chat-only server (the dev host's llama.cpp answers 501 here).
        (501, ProviderErrorKind::Unsupported),
        (500, ProviderErrorKind::Server),
    ] {
        let server = MockServer::start().await;
        mount(
            &server,
            ResponseTemplate::new(code).set_body_json(json!({ "error": "Invalid API Key" })),
        )
        .await;
        let cfg = asr_config(&format!("{}/v1", server.uri()), Some("m"), 5000, true);
        let secrets = MemorySecretStore::new();
        secrets.set(ASR_ID, &ApiKey::new("wrong-key")).unwrap();
        let e = cloud_asr(&cfg, &secrets)
            .transcribe(&tone(), &ctx("HINT"))
            .await
            .unwrap_err();
        assert_eq!(provider_kind(&e), (Stage::Asr, want), "HTTP {code}");
        let msg = format!("{e} {e:?}");
        assert!(!msg.contains("wrong-key") && !msg.contains("HINT"), "{msg}");
    }
}

#[tokio::test]
async fn timeout_comes_from_config() {
    let server = MockServer::start().await;
    mount(
        &server,
        ResponseTemplate::new(200)
            .set_body_json(json!({ "text": "late" }))
            .set_delay(Duration::from_millis(2_000)),
    )
    .await;
    let cfg = asr_config(&format!("{}/v1", server.uri()), Some("m"), 150, true);
    let started = Instant::now();
    let e = cloud_asr(&cfg, &MemorySecretStore::new())
        .transcribe(&tone(), &ctx(""))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert!(
        matches!(
            e,
            V2cError::Timeout {
                stage: Stage::Asr,
                ms: 150
            }
        ),
        "{e:?}"
    );
}

#[tokio::test]
async fn malformed_or_textless_json_is_bad_response() {
    for template in [
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_string("{\"text\": \"我們"),
        ResponseTemplate::new(200).set_body_json(json!({ "segments": [] })),
        ResponseTemplate::new(200).set_body_json(json!({ "text": 42 })),
    ] {
        let server = MockServer::start().await;
        mount(&server, template).await;
        let cfg = asr_config(&format!("{}/v1", server.uri()), Some("m"), 5000, true);
        let e = cloud_asr(&cfg, &MemorySecretStore::new())
            .transcribe(&tone(), &ctx(""))
            .await
            .unwrap_err();
        assert_eq!(
            provider_kind(&e),
            (Stage::Asr, ProviderErrorKind::BadResponse)
        );
    }
}

#[tokio::test]
async fn connection_refused_is_unreachable() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = asr_config(
        &format!("http://127.0.0.1:{port}/v1"),
        Some("m"),
        10_000,
        true,
    );
    let e = cloud_asr(&cfg, &MemorySecretStore::new())
        .transcribe(&tone(), &ctx(""))
        .await
        .unwrap_err();
    assert_eq!(
        provider_kind(&e),
        (Stage::Asr, ProviderErrorKind::Unreachable)
    );
}

#[tokio::test]
async fn pipeline_reports_asr_failure_as_error() {
    // Core has no ASR fallback; the desktop wraps the cloud ASR in `FallbackAsr`.
    let server = MockServer::start().await;
    mount(&server, ResponseTemplate::new(401)).await;
    let cfg = asr_config(&format!("{}/v1", server.uri()), Some("m"), 5000, true);
    let asr: Arc<dyn AsrProvider> = Arc::from(cloud_asr(&cfg, &MemorySecretStore::new()));
    let pipeline = Pipeline::new(
        asr,
        None,
        PipelineConfig {
            polish_enabled: cfg.polish.enabled,
            polish_timeout_ms: cfg.polish.timeout_ms,
            min_clip_ms: cfg.audio.min_clip_ms,
            vocabulary: vec![],
        },
    );
    let e = pipeline.run(tone(), Mode::Dictate).await.unwrap_err();
    assert_eq!(
        provider_kind(&e),
        (Stage::Asr, ProviderErrorKind::Unauthorized)
    );
}

// ----------------------------------------------------------------------------------------
// Routing decisions (pure, in core)
// ----------------------------------------------------------------------------------------

#[test]
fn fallback_to_local_follows_config() {
    let cfg = asr_config("http://192.168.1.50:8002/v1", Some("m"), 5000, true);
    let plan = route::resolve(&cfg.route, &cfg.cloud);
    assert!(plan.asr.fallback_to_local);
    assert_eq!(plan.polish.target, Target::Local);
    let cfg = asr_config("http://192.168.1.50:8002/v1", Some("m"), 5000, false);
    assert!(!route::resolve(&cfg.route, &cfg.cloud).asr.fallback_to_local);
}

#[test]
fn compat_asr_without_model_stays_local() {
    // OpenAI-compatible endpoints have no default ASR model.
    let cfg = asr_config("http://192.168.1.50:8002/v1", None, 5000, true);
    let plan = route::resolve(&cfg.route, &cfg.cloud);
    assert_eq!(plan.asr.target, Target::Local);
    assert_eq!(
        plan.issues,
        vec![RouteIssue::NoModel {
            purpose: Purpose::Asr,
            service_id: ASR_ID.into()
        }]
    );
    let built = route::build_cloud_asr(
        cfg.cloud_service(ASR_ID).unwrap(),
        &MemorySecretStore::new(),
        &cloud_http_client().unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(built.is_none());
}

/// The full example from `docs/linux-setup.md` section 4: tea-asr-1 on :8001, LLM on :8000,
/// local SenseVoice as the fallback.
#[test]
fn two_remote_services_config_routes_every_purpose() {
    let cfg = Config::from_toml_str(
        r#"
[route]
asr = "lan-asr"
polish = "lan-llm"
translate = "lan-llm"
cloud_fallback_to_local = true

[timeouts]
asr_cloud_ms = 5000
llm_cloud_ms = 3000

[local]
asr = "sense_voice"

[gpu]
enabled = false

[[cloud]]
id = "lan-asr"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8001/v1"
asr_model = "tea-asr-1"

[[cloud]]
id = "lan-llm"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8000/v1"
llm_model = "Qwen3.8-27B-UD-Q6_K_L"
"#,
    )
    .unwrap();
    assert_eq!(cfg.local.asr, LocalAsr::SenseVoice);
    assert_eq!(cfg.timeouts.asr_cloud_ms, 5000);
    assert_eq!(
        cfg.cloud_service("lan-asr").unwrap().asr_model.as_deref(),
        Some("tea-asr-1")
    );
    let plan = route::resolve(&cfg.route, &cfg.cloud);
    assert!(plan.issues.is_empty(), "{:?}", plan.issues);
    for (purpose, id) in [
        (Purpose::Asr, "lan-asr"),
        (Purpose::Polish, "lan-llm"),
        (Purpose::Translate, "lan-llm"),
    ] {
        assert_eq!(
            plan.get(purpose).target,
            Target::Cloud {
                service_id: id.into(),
                kind: CloudKind::OpenAiCompat
            },
            "{purpose:?}"
        );
        assert!(plan.get(purpose).fallback_to_local);
    }
    // Plain http is allowed for compat endpoints (LAN servers).
    let secrets = MemorySecretStore::new();
    let http = cloud_http_client().unwrap();
    assert!(route::build_cloud_asr(
        cfg.cloud_service("lan-asr").unwrap(),
        &secrets,
        &http,
        Duration::from_secs(5)
    )
    .unwrap()
    .is_some());
    assert!(route::build_cloud_llm(
        cfg.cloud_service("lan-llm").unwrap(),
        &secrets,
        &http,
        Duration::from_secs(3),
        Stage::Polish
    )
    .unwrap()
    .is_some());
}

// ----------------------------------------------------------------------------------------
// Real endpoint (opt-in)
// ----------------------------------------------------------------------------------------

/// Minimal PCM16 WAV reader (any channel count, mixed down to mono). Test-only.
fn read_wav(bytes: &[u8]) -> Option<AudioClip> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let (mut channels, mut rate, mut bits) = (0u16, 0u32, 0u16);
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().ok()?) as usize;
        let data = bytes.get(i + 8..(i + 8 + len).min(bytes.len()))?;
        if id == b"fmt " && data.len() >= 16 {
            let format = u16::from_le_bytes([data[0], data[1]]);
            channels = u16::from_le_bytes([data[2], data[3]]);
            rate = u32::from_le_bytes(data[4..8].try_into().ok()?);
            bits = u16::from_le_bytes([data[14], data[15]]);
            if format != 1 {
                return None;
            }
        } else if id == b"data" {
            if bits != 16 || channels == 0 {
                return None;
            }
            let ch = channels as usize;
            let samples = data
                .chunks_exact(2 * ch)
                .map(|frame| {
                    frame
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|s| f32::from(i16::from_le_bytes(*s)) / 32768.0)
                        .sum::<f32>()
                        / ch as f32
                })
                .collect();
            return Some(AudioClip {
                samples,
                sample_rate: rate,
            });
        }
        i += 8 + len + (len & 1);
    }
    None
}

/// Character error rate between two texts after dropping case, whitespace and punctuation
/// (Levenshtein distance over chars / expected length). Test-only.
fn cer(expected: &str, got: &str) -> f64 {
    let norm = |s: &str| -> Vec<char> {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (a, b) = (norm(expected), norm(got));
    if a.is_empty() {
        return if b.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()] as f64 / a.len() as f64
}

#[test]
fn cer_ignores_case_space_and_punctuation() {
    assert_eq!(cer("Hello, world.", "hello world"), 0.0);
    assert_eq!(cer("我們明天見", "我們明天見。"), 0.0);
    assert!((cer("abcd", "abed") - 0.25).abs() < 1e-9);
    assert_eq!(cer("", ""), 0.0);
}

/// Real ASR endpoint. Env: `V2C_TEST_ASR_BASE_URL` (required), `V2C_TEST_ASR_MODEL`,
/// `V2C_TEST_ASR_KEY_FILE` (key never printed), `V2C_TEST_ASR_WAV` (PCM16; otherwise a 1 s
/// synthetic tone and only auth + protocol are checked), `V2C_TEST_ASR_EXPECT` (expected
/// transcript; only match/CER are printed), `V2C_TEST_ASR_TIMEOUT_MS` (default: the config
/// default `[timeouts] asr_cloud_ms`), `V2C_TEST_ASR_RUNS` (default 2: cold, then warm).
#[tokio::test]
#[ignore = "needs V2C_TEST_ASR_BASE_URL (a real OpenAI-compatible ASR server)"]
async fn asr_real_transcribe() {
    let Some(env) = real_asr_env() else {
        eprintln!("asr_real: V2C_TEST_ASR_BASE_URL not set; skipped");
        return;
    };
    let cfg = asr_config(&env.base_url, Some(&env.model), env.timeout_ms, true);
    let asr = cloud_asr(&cfg, &env.secrets);
    println!(
        "asr_real: model {}, key {}, audio {} ms ({}), timeout {} ms",
        env.model,
        if env.has_key { "loaded" } else { "none" },
        env.clip.duration_ms(),
        if env.real_speech {
            "WAV"
        } else {
            "synthetic tone"
        },
        env.timeout_ms
    );
    let runs: usize = std::env::var("V2C_TEST_ASR_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let mut failures = 0;
    for run in 1..=runs {
        let started = Instant::now();
        let out = asr.transcribe(&env.clip, &ctx("")).await;
        let ms = started.elapsed().as_millis();
        match &out {
            Ok(t) => {
                let score = env.expect.as_deref().map(|e| cer(e, &t.text));
                println!(
                    "asr_real run {run}: HTTP ok, {} chars, lang {:?}, {ms} ms, match {:?}, CER {:?}",
                    t.text.chars().count(),
                    t.lang,
                    score.map(|c| c == 0.0),
                    score.map(|c| format!("{c:.3}"))
                );
                if env.real_speech {
                    assert!(!t.text.trim().is_empty(), "speech WAV gave no text");
                }
                if let Some(c) = score {
                    assert!(c <= 0.1, "CER {c:.3} above 0.1");
                }
            }
            Err(e) => {
                failures += 1;
                println!("asr_real run {run}: error {e}, {ms} ms");
            }
        }
    }
    assert_eq!(failures, 0, "{failures} of {runs} ASR requests failed");
}

/// Settings for the real ASR tests, from the environment (copied in `linux_e2e_sim.rs`: test
/// files cannot share a module within this card's allowed paths).
struct RealAsrEnv {
    base_url: String,
    model: String,
    timeout_ms: u64,
    secrets: MemorySecretStore,
    has_key: bool,
    clip: AudioClip,
    real_speech: bool,
    expect: Option<String>,
}

fn real_asr_env() -> Option<RealAsrEnv> {
    let var = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
    let base_url = var("V2C_TEST_ASR_BASE_URL")?;
    let model = var("V2C_TEST_ASR_MODEL").unwrap_or_else(|| "whisper-1".into());
    let timeout_ms = var("V2C_TEST_ASR_TIMEOUT_MS")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| Config::default().timeouts.asr_cloud_ms);
    let secrets = MemorySecretStore::new();
    let mut has_key = false;
    if let Some(p) = var("V2C_TEST_ASR_KEY_FILE") {
        // Never printed: only whether a key was loaded.
        let key = std::fs::read_to_string(p.trim()).expect("V2C_TEST_ASR_KEY_FILE readable");
        let key = key.trim();
        if !key.is_empty() {
            secrets.set(ASR_ID, &ApiKey::new(key)).unwrap();
            has_key = true;
        }
    }
    let (clip, real_speech) = match var("V2C_TEST_ASR_WAV") {
        Some(p) => {
            let bytes = std::fs::read(p.trim()).expect("V2C_TEST_ASR_WAV readable");
            (
                read_wav(&bytes).expect("V2C_TEST_ASR_WAV is a PCM16 WAV"),
                true,
            )
        }
        None => (tone(), false),
    };
    Some(RealAsrEnv {
        base_url,
        model,
        timeout_ms,
        secrets,
        has_key,
        clip,
        real_speech,
        expect: var("V2C_TEST_ASR_EXPECT"),
    })
}
