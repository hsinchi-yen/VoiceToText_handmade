use super::*;
use v2c_core::portable;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/../../tests/fixtures/config/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

fn svc(id: &str, provider: CloudProvider) -> CloudServiceConfig {
    CloudServiceConfig {
        id: id.into(),
        provider,
        model: "m".into(),
        asr_model: Some("a".into()),
        base_url: None,
        uses: vec![],
        asr_timeout_ms: None,
    }
}

fn find<'a>(p: &'a ConfigFilePreview, id: &str) -> &'a ImportedService {
    p.services.iter().find(|s| s.config.id == id).unwrap()
}

#[test]
fn gemini_claude_file_maps_to_phone_ids_and_uses() {
    let p = preview(&fixture("valid-gemini-claude.toml"), &[], &[]);
    assert_eq!(p.error_code, None);
    assert!(p.skipped.is_empty());
    assert_eq!(p.asr_service.as_deref(), Some("cloud-gemini"));
    assert_eq!(p.translate_service.as_deref(), Some("cloud-claude"));
    let g = find(&p, "cloud-gemini");
    assert_eq!(g.source_id, "my-gemini");
    assert_eq!(g.config.provider, CloudProvider::Gemini);
    assert_eq!(g.config.uses, vec![CloudUse::Asr]);
    assert_eq!(g.config.model, "gemini-3.6-flash");
    assert_eq!(g.status, ImportStatus::Added);
    assert!(g.needs_key);
    let c = find(&p, "cloud-claude");
    assert_eq!(c.config.uses, vec![CloudUse::Translate]);
    assert_eq!(c.config.model, "claude-haiku-5-5");
    assert!(c.needs_key);
    assert!(p.ignored.is_empty());
}

#[test]
fn desktop_export_is_classified() {
    let p = preview(include_str!("desktop-export.toml"), &[], &[]);
    assert_eq!(p.error_code, None);
    let reasons: Vec<(&str, SkipReason)> = p
        .skipped
        .iter()
        .map(|s| (s.id.as_str(), s.reason))
        .collect();
    assert!(reasons.contains(&("tn-qwen", SkipReason::Builtin)));
    assert!(reasons.contains(&("lan-asr", SkipReason::HttpEndpoint)));
    assert!(reasons.contains(&("lan-llm", SkipReason::HttpEndpoint)));
    // route.asr (my-gemini) wins over route.translate (my-gemini2): the other is a duplicate.
    assert!(reasons.contains(&("my-gemini2", SkipReason::DuplicateProvider)));
    assert_eq!(p.skipped.len(), 4);
    let ids: Vec<&str> = p.services.iter().map(|s| s.config.id.as_str()).collect();
    assert_eq!(ids, ["cloud-gemini", "cloud-openai", "cloud-custom"]);
    assert_eq!(p.asr_service.as_deref(), Some("cloud-gemini"));
    // translate points at the skipped duplicate: not applied, listed.
    assert_eq!(p.translate_service, None);
    assert!(p.ignored.contains(&"route.translate".to_string()));
    assert!(p.ignored.contains(&"route.polish".to_string()));
    assert!(p.ignored.contains(&"vocabulary".to_string()));
    let custom = find(&p, "cloud-custom");
    assert_eq!(custom.source_id, "relay");
    assert_eq!(
        custom.config.base_url.as_deref(),
        Some("https://relay.example.com/v1")
    );
    assert!(
        !custom.needs_key,
        "custom endpoints need a key only when the file says so"
    );
    assert!(find(&p, "cloud-openai").needs_key);
}

#[test]
fn duplicate_prefers_translate_route_then_first() {
    let text = "[v2c]\nformat = 1\n[route]\ntranslate = \"b\"\n\
        [[cloud]]\nid = \"a\"\nkind = \"gemini\"\n[[cloud]]\nid = \"b\"\nkind = \"gemini\"\n";
    let p = preview(text, &[], &[]);
    assert_eq!(p.services.len(), 1);
    assert_eq!(p.services[0].source_id, "b");
    assert_eq!(p.skipped[0].id, "a");
    let text = text.replace("translate = \"b\"", "cloud_fallback_to_local = true");
    assert_eq!(preview(&text, &[], &[]).services[0].source_id, "a");
}

#[test]
fn local_route_and_official_base_url() {
    let text = "[v2c]\nformat = 1\n[route]\nasr = \"local\"\n\
        [[cloud]]\nid = \"g\"\nkind = \"gemini\"\nbase_url = \"https://proxy.example.com\"\n\
        [[cloud]]\nid = \"o\"\nkind = \"open_ai\"\n";
    let p = preview(text, &[], &[]);
    assert_eq!(
        p.skipped,
        vec![SkippedService {
            id: "g".into(),
            reason: SkipReason::OfficialBaseUrl
        }]
    );
    assert_eq!(p.asr_service, None);
    assert!(p.ignored.is_empty());
}

#[test]
fn default_keys_are_not_used() {
    let p = preview(&fixture("valid-lan.toml"), &[], &[]);
    // Both services are HTTP: skipped; the shared key is ignored.
    assert!(p.services.is_empty());
    assert_eq!(p.skipped.len(), 2);
    assert!(p.ignored.contains(&"preset.default_keys".to_string()));
    let text = "[v2c]\nformat = 1\n[preset]\nneeds_key = [\"g\"]\ndefault_keys = { g = \"k\" }\n\
        [[cloud]]\nid = \"g\"\nkind = \"gemini\"\n";
    let p = preview(text, &[], &[]);
    assert!(
        p.services[0].needs_key,
        "a default key is never applied on the phone"
    );
}

#[test]
fn merge_against_existing_services_and_keys() {
    let file = "[v2c]\nformat = 1\n[[cloud]]\nid = \"x\"\nkind = \"gemini\"\n\
        asr_model = \"gemini-3.6-flash\"\nllm_model = \"gemini-3.6-flash\"\n\
        [[cloud]]\nid = \"r\"\nkind = \"open_ai_compat\"\nbase_url = \"https://new.example.com/v1\"\nllm_model = \"m\"\n";
    let mut gemini = svc("cloud-gemini", CloudProvider::Gemini);
    gemini.model = "gemini-3.6-flash".into();
    gemini.asr_model = Some("gemini-3.6-flash".into());
    let mut custom = svc("cloud-custom", CloudProvider::Custom);
    custom.base_url = Some("https://old.example.com/v1".into());
    custom.model = "m".into();
    let keys = vec!["cloud-gemini".to_string(), "cloud-custom".to_string()];
    let p = preview(file, &[gemini, custom], &keys);
    let g = find(&p, "cloud-gemini");
    assert_eq!(g.status, ImportStatus::Unchanged);
    assert!(!g.needs_key, "stored key stays");
    assert!(!g.drop_old_key);
    let c = find(&p, "cloud-custom");
    assert_eq!(c.status, ImportStatus::Updated);
    assert!(c.drop_old_key, "endpoint changed: old key dropped");
}

#[test]
fn errors_carry_code_and_line_only() {
    let p = preview(&fixture("reject-unknown-kind.toml"), &[], &[]);
    assert_eq!(p.error_code.as_deref(), Some("unknown_kind"));
    assert!(p.error_line.is_some());
    assert!(p.services.is_empty());
    let p = preview(&fixture("reject-format-2.toml"), &[], &[]);
    assert_eq!(p.error_code.as_deref(), Some("format_too_new"));
    let big = "#".repeat(portable::MAX_FILE_BYTES + 1);
    assert_eq!(
        preview(&big, &[], &[]).error_code.as_deref(),
        Some("too_large")
    );
}

#[test]
fn every_reject_fixture_is_an_error() {
    let dir = format!("{}/../../tests/fixtures/config", env!("CARGO_MANIFEST_DIR"));
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let path = e.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("reject-") {
            n += 1;
            let p = preview(&std::fs::read_to_string(&path).unwrap(), &[], &[]);
            assert!(p.error_code.is_some(), "{name}");
        }
    }
    assert!(n >= 9);
}

fn phone_services() -> Vec<CloudServiceConfig> {
    let mut gemini = svc("cloud-gemini", CloudProvider::Gemini);
    gemini.uses = vec![CloudUse::Asr, CloudUse::Translate];
    let mut custom = svc("cloud-custom", CloudProvider::Custom);
    custom.base_url = Some("https://relay.example.com/v1".into());
    custom.asr_timeout_ms = Some(20_000);
    let claude = svc("cloud-claude", CloudProvider::Claude);
    vec![gemini, custom, claude]
}

#[test]
fn export_reimports_and_has_no_keys() {
    let keys = vec!["cloud-gemini".to_string()];
    let e = export(
        &phone_services(),
        Some("cloud-gemini"),
        Some("cloud-gemini"),
        &keys,
    );
    assert!(e.skipped.is_empty());
    let file = portable::parse(&e.text).expect("phone export must parse");
    assert_eq!(file.exported_from.as_deref(), Some("android"));
    assert_eq!(file.services.len(), 3);
    assert_eq!(file.route.asr.as_deref(), Some("cloud-gemini"));
    assert_eq!(file.route.translate.as_deref(), Some("cloud-gemini"));
    assert!(file.vocab.is_empty());
    // Custom without a stored key is not listed; the official ones always are.
    assert!(file.needs_key.contains(&"cloud-claude".to_string()));
    assert!(!file.needs_key.contains(&"cloud-custom".to_string()));
    assert!(!e.text.contains("default_keys"));
    assert!(!e.text.contains("polish"));
    // And the phone accepts its own export unchanged.
    let p = preview(&e.text, &phone_services(), &keys);
    assert_eq!(p.error_code, None);
    assert!(p.skipped.is_empty());
    assert!(p
        .services
        .iter()
        .all(|s| s.status == ImportStatus::Unchanged));
    assert_eq!(p.asr_service.as_deref(), Some("cloud-gemini"));
}

#[test]
fn export_without_uses_leaves_route_out() {
    let e = export(&phone_services(), None, None, &[]);
    assert!(!e.text.contains("[route]"));
    assert!(portable::parse(&e.text).unwrap().route == portable::PortableRoute::default());
    let e = export(&[], None, None, &[]);
    assert!(portable::parse(&e.text).unwrap().services.is_empty());
}

#[test]
fn export_skips_what_would_not_parse() {
    let mut bad = svc("cloud-custom", CloudProvider::Custom);
    bad.base_url = None;
    let e = export(&[bad], Some("cloud-custom"), None, &[]);
    assert_eq!(e.skipped, vec!["cloud-custom".to_string()]);
    assert!(portable::parse(&e.text).is_ok());
}
