//! Format-preserving `config.toml` updates, shared by the tray and the settings page.
//!
//! [`update_config_file`] reads the file, lets the caller change a parsed [`Config`], and writes
//! back only the keys whose values changed (`toml_edit`): the user's comments, formatting, and
//! keys this version does not know survive. Arrays of tables (`[[apps]]`, `[[cloud]]`) are
//! replaced as a whole when they changed. The edited text must parse back to exactly the new
//! config, otherwise nothing is written.

use std::path::Path;

use toml_edit::{DocumentMut, Item};
use tracing::warn;
use v2c_core::config::Config;

use crate::app_state;

/// Top-level sections that are plain tables in [`Config`].
const TABLES: [&str; 8] = [
    "route", "polish", "hotkey", "audio", "local", "gpu", "history", "timeouts",
];
/// Top-level arrays of tables in [`Config`].
const ARRAYS: [&str; 2] = ["apps", "cloud"];

/// Reads `config.toml` (defaults if it does not exist), runs `change`, and writes the result
/// back if `change` returned `Ok(true)`. `change` returning `Err(code)` aborts without writing.
/// An invalid file is left untouched (`config_invalid_not_saved`). Returns the new config.
pub fn update_config_file<F>(path: &Path, change: F) -> Result<Config, &'static str>
where
    F: FnOnce(&mut Config) -> Result<bool, &'static str>,
{
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut cfg = app_state::platform_defaults();
            if change(&mut cfg)? {
                app_state::save_config(path, &cfg).map_err(save_failed)?;
            }
            return Ok(cfg);
        }
        Err(_) => return Err("config_unreadable"),
    };
    let old = Config::from_toml_str(&text).map_err(|_| "config_invalid_not_saved")?;
    let mut new = old.clone();
    if !change(&mut new)? || new == old {
        return Ok(new);
    }
    let new_text = edit_text(&text, &old, &new).ok_or("config_invalid_not_saved")?;
    // The edited file must mean exactly the config we computed; otherwise write nothing.
    match Config::from_toml_str(&new_text) {
        Ok(on_disk) if on_disk == new => {}
        _ => {
            warn!("config edit did not round-trip; config.toml left unchanged");
            return Err("config_save_failed");
        }
    }
    app_state::save_config_text(path, &new_text).map_err(save_failed)?;
    Ok(new)
}

fn save_failed(e: std::io::Error) -> &'static str {
    warn!(error = %e, "could not save config.toml");
    "config_save_failed"
}

/// Rewrites `text` (which parses to `old`) so it parses to `new`, touching only what differs.
/// `None` if `text` is not TOML or a section that must change is not a table.
pub fn edit_text(text: &str, old: &Config, new: &Config) -> Option<String> {
    let mut doc: DocumentMut = text.parse().ok()?;
    let old_v = toml::Value::try_from(old).ok()?;
    let new_v = toml::Value::try_from(new).ok()?;
    // The new config as TOML items, to copy values from.
    let new_doc: DocumentMut = toml::to_string(new).ok()?.parse().ok()?;

    for section in TABLES {
        let (Some(o), Some(n)) = (table_of(&old_v, section), table_of(&new_v, section)) else {
            continue;
        };
        if o == n {
            continue;
        }
        let target = doc
            .entry(section)
            .or_insert_with(toml_edit::table)
            .as_table_like_mut()?;
        let source = new_doc.get(section).and_then(Item::as_table_like);
        let keys: Vec<&String> = o.keys().chain(n.keys()).collect();
        for key in keys {
            if o.get(key) == n.get(key) {
                continue;
            }
            match source.and_then(|s| s.get(key)).and_then(Item::as_value) {
                Some(v) => set_value(target, key, v.clone()),
                // `None` options are not serialized: remove the key.
                None => {
                    target.remove(key);
                }
            }
        }
    }
    for array in ARRAYS {
        if old_v.get(array) == new_v.get(array) {
            continue;
        }
        match new_doc.get(array) {
            Some(item) => {
                doc.insert(array, item.clone());
            }
            None => {
                doc.remove(array);
            }
        }
    }
    Some(doc.to_string())
}

fn table_of<'a>(v: &'a toml::Value, key: &str) -> Option<&'a toml::map::Map<String, toml::Value>> {
    v.get(key)?.as_table()
}

/// Sets `key = value` in `t`; a replaced value keeps its decor (surrounding comments).
fn set_value(t: &mut dyn toml_edit::TableLike, key: &str, mut value: toml_edit::Value) {
    match t.get_mut(key).and_then(|item| item.as_value_mut()) {
        Some(old) => {
            let decor = old.decor().clone();
            *value.decor_mut() = decor;
            *old = value;
        }
        None => {
            value.decor_mut().clear();
            t.insert(key, toml_edit::value(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use v2c_core::config::{AppRule, GpuProfile, NewlineKey};

    use super::*;

    const COMMENTED: &str = "\
# my settings
[gpu]
# keep 2 GB for the display
vram_reserve_mb = 2048 # reserve
profile = \"balanced\"
heavy_apps = [\"blender.exe\"]
future = 1

[local]
llm_model = \"qwen\" # model
";

    fn edit(text: &str, f: impl FnOnce(&mut Config)) -> (String, Config) {
        let old = Config::from_toml_str(text).unwrap();
        let mut new = old.clone();
        f(&mut new);
        let out = edit_text(text, &old, &new).unwrap();
        assert_eq!(Config::from_toml_str(&out).unwrap(), new, "{out}");
        (out, new)
    }

    #[test]
    fn changed_keys_keep_comments_and_unknown_keys() {
        let (out, _) = edit(COMMENTED, |c| {
            c.gpu.vram_reserve_mb = 3072;
            c.gpu.profile = GpuProfile::LowVram;
            c.gpu.heavy_apps.push("game.exe".into());
        });
        for kept in [
            "# my settings",
            "# keep 2 GB for the display",
            "vram_reserve_mb = 3072 # reserve",
            "profile = \"low_vram\"",
            "future = 1",
            "llm_model = \"qwen\" # model",
        ] {
            assert!(out.contains(kept), "lost {kept:?} in:\n{out}");
        }
        assert!(out.contains("game.exe"), "{out}");
    }

    #[test]
    fn none_removes_the_key_and_missing_sections_are_created() {
        let (out, new) = edit(COMMENTED, |c| {
            c.local.llm_model = None;
            c.audio.device = Some("USB Mic".into());
        });
        assert!(!out.contains("llm_model"), "{out}");
        assert!(out.contains("[audio]") && out.contains("device = \"USB Mic\""));
        assert_eq!(new.audio.device.as_deref(), Some("USB Mic"));
    }

    #[test]
    fn unchanged_text_is_identical() {
        let old = Config::from_toml_str(COMMENTED).unwrap();
        assert_eq!(edit_text(COMMENTED, &old, &old).unwrap(), COMMENTED);
    }

    #[test]
    fn app_rules_are_replaced_as_a_whole() {
        let (out, new) = edit("[polish]\nenabled = true\n", |c| {
            c.apps = vec![AppRule {
                exe: "LINE.exe".into(),
                newline: NewlineKey::ShiftEnter,
                always_paste: true,
            }];
        });
        assert!(out.contains("[[apps]]"), "{out}");
        assert_eq!(new.apps.len(), 1);
        // Back to the built-in rules.
        let old = new.clone();
        let mut back = new;
        back.apps = v2c_core::config::default_app_rules();
        let out2 = edit_text(&out, &old, &back).unwrap();
        assert_eq!(Config::from_toml_str(&out2).unwrap(), back);
    }

    #[test]
    fn file_update_writes_only_on_change_and_refuses_invalid_files() {
        let dir = std::env::temp_dir().join(format!("v2c-cfgedit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        // Missing file: created with defaults + the change.
        let cfg = update_config_file(&path, |c| {
            c.polish.enabled = false;
            Ok(true)
        })
        .unwrap();
        assert!(!cfg.polish.enabled && path.exists());

        std::fs::write(&path, COMMENTED).unwrap();
        // No change: file untouched.
        update_config_file(&path, |_| Ok(true)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), COMMENTED);
        // A refused change writes nothing.
        assert_eq!(
            update_config_file(&path, |c| {
                c.gpu.vram_reserve_mb = 1;
                Err("invalid")
            }),
            Err("invalid")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), COMMENTED);

        std::fs::write(&path, "gpu = 5").unwrap();
        assert_eq!(
            update_config_file(&path, |c| {
                c.gpu.vram_reserve_mb = 1;
                Ok(true)
            }),
            Err("config_invalid_not_saved")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "gpu = 5");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
