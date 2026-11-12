//! Tauri commands, one file per feature. Register new commands in `main.rs`.

pub mod cloud; // T1.7b
pub mod gpu; // T1.10b / T1.11 (settings VRAM bar)
pub mod history; // T1.9
pub mod models; // T1.11
pub mod onboarding; // T1.11
pub mod presets; // T4.1
pub mod settings; // T1.11
pub mod status; // T1.1
pub mod vocab; // T1.8

/// The command ACL (see `build.rs`): every registered command is declared in the app manifest
/// and granted to the main window only; the overlay capability grants no app command.
#[cfg(test)]
mod acl_tests {
    const MAIN_RS: &str = include_str!("../main.rs");
    const BUILD_RS: &str = include_str!("../../build.rs");
    const DEFAULT_CAP: &str = include_str!("../../capabilities/default.json");
    const OVERLAY_CAP: &str = include_str!("../../capabilities/overlay.json");

    fn registered() -> Vec<String> {
        let start = MAIN_RS.find("generate_handler![").unwrap();
        let end = start + MAIN_RS[start..].find("])").unwrap();
        MAIN_RS[start..end]
            .split(',')
            .filter_map(|s| s.trim().rsplit("::").next().map(str::to_owned))
            .filter(|s| !s.is_empty() && !s.contains('['))
            .collect()
    }

    fn manifest() -> Vec<String> {
        let start = BUILD_RS.find("const COMMANDS: &[&str] = &[").unwrap();
        let end = start + BUILD_RS[start..].find("];").unwrap();
        BUILD_RS[start..end]
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_prefix('"')?
                    .strip_suffix("\",")
                    .map(str::to_owned)
            })
            .collect()
    }

    fn permissions(cap: &str) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(cap).unwrap();
        v["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn every_command_is_declared_and_granted_to_main_only() {
        let mut reg = registered();
        let mut man = manifest();
        assert!(reg.len() > 20, "{reg:?}");
        reg.sort();
        man.sort();
        assert_eq!(
            reg, man,
            "build.rs COMMANDS must match main.rs generate_handler!"
        );

        let main = permissions(DEFAULT_CAP);
        for c in &man {
            let p = format!("allow-{}", c.replace('_', "-"));
            assert!(main.contains(&p), "main window lacks {p}");
        }
        let v: serde_json::Value = serde_json::from_str(DEFAULT_CAP).unwrap();
        assert_eq!(v["windows"], serde_json::json!(["main"]));

        let overlay = permissions(OVERLAY_CAP);
        assert!(
            overlay.iter().all(|p| p.starts_with("core:event:")),
            "overlay must stay events-only: {overlay:?}"
        );
    }
}
