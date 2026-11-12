//! System tray (`tasks/plan.md` section 7, card T1.3): settings, history, polish on/off,
//! switch provider (placeholder), all-local, quit.
//!
//! The app is a background tool: closing the main window hides it (see `main.rs`); only
//! "結束" here quits.
//!
//! Config changes are written to `config.toml` and announced as [`CONFIG_CHANGED_EVENT`]. The
//! running pipeline keeps the config it was started with until it learns to reload (later
//! integration step).

use std::path::Path;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tracing::{info, warn};
use v2c_core::config::{Config, LOCAL};

use crate::app_state::AppState;
use crate::config_edit;
use crate::controller::StatusEvent;

/// Webview event sent after the tray changed `config.toml` (no payload).
pub const CONFIG_CHANGED_EVENT: &str = "v2c:config-changed";

const MAIN_WINDOW: &str = "main";

const ID_SETTINGS: &str = "settings";
const ID_HISTORY: &str = "history";
const ID_POLISH: &str = "polish";
const ID_SWITCH_PROVIDER: &str = "switch_provider";
const ID_ALL_LOCAL: &str = "all_local";
const ID_QUIT: &str = "quit";

/// A config change the tray can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigAction {
    SetPolish(bool),
    /// Route ASR, polish, and translate to the local providers.
    AllLocal,
}

/// Applies `action`; returns whether anything changed.
pub fn apply(cfg: &mut Config, action: ConfigAction) -> bool {
    match action {
        ConfigAction::SetPolish(on) => {
            let changed = cfg.polish.enabled != on;
            cfg.polish.enabled = on;
            changed
        }
        ConfigAction::AllLocal => {
            let mut changed = false;
            for route in [
                &mut cfg.route.asr,
                &mut cfg.route.polish,
                &mut cfg.route.translate,
            ] {
                if route != LOCAL {
                    *route = LOCAL.to_owned();
                    changed = true;
                }
            }
            changed
        }
    }
}

/// Reads `config.toml` (defaults if it does not exist), applies `action`, and writes it back.
/// Only the keys the action changes are rewritten (see [`config_edit`]), so the user's
/// comments, formatting, and keys this version does not know survive. An invalid file is left
/// untouched (`config_invalid_not_saved`). Returns the new config or an error code.
pub fn update_config_file(path: &Path, action: ConfigAction) -> Result<Config, &'static str> {
    config_edit::update_config_file(path, |cfg| Ok(apply(cfg, action)))
}

/// Applies `action` to the TOML text, changing only the keys involved and keeping each
/// value's surrounding comments. `None` if the text is not a valid config or a section is not
/// a table.
#[cfg(test)]
pub fn edit_config_text(text: &str, action: ConfigAction) -> Option<String> {
    let old = Config::from_toml_str(text).ok()?;
    let mut new = old.clone();
    apply(&mut new, action);
    config_edit::edit_text(text, &old, &new)
}
/// Which tray glyph to show: the glyph colour must contrast with the taskbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayVariant {
    /// Dark glyph, for a light taskbar.
    Light,
    /// Light glyph, for a dark taskbar.
    Dark,
}

impl TrayVariant {
    fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

// 32x32 straight-alpha RGBA, rendered from `icons/src/candidate-b-tray-*.svg`. Raw pixels (next
// to the reviewable `.png` copies) avoid enabling tauri's `image-png`, which pulls in new crates.
const TRAY_LIGHT_RGBA: &[u8] = include_bytes!("../icons/tray/tray-light.rgba");
const TRAY_DARK_RGBA: &[u8] = include_bytes!("../icons/tray/tray-dark.rgba");
const TRAY_ICON_SIZE: u32 = 32;

/// `SystemUsesLightTheme` (1 = light taskbar) to a variant; 0 or missing means dark.
fn tray_variant(system_uses_light: Option<u32>) -> TrayVariant {
    match system_uses_light {
        Some(1) => TrayVariant::Light,
        _ => TrayVariant::Dark,
    }
}

fn variant_icon(variant: TrayVariant) -> Option<Image<'static>> {
    // `Image::new` borrows; `to_owned` is not needed for `'static` data.
    let bytes = match variant {
        TrayVariant::Light => TRAY_LIGHT_RGBA,
        TrayVariant::Dark => TRAY_DARK_RGBA,
    };
    Some(Image::new(bytes, TRAY_ICON_SIZE, TRAY_ICON_SIZE))
}

/// Reads the taskbar theme from the registry (Windows); Linux always uses the dark variant.
#[cfg(windows)]
fn current_variant() -> TrayVariant {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let subkey: Vec<u16> = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let value: Vec<u16> = "SystemUsesLightTheme"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: both names are NUL-terminated; `data`/`size` point to a live u32 and its size.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(data).cast(),
            &mut size,
        )
    };
    tray_variant((status == 0).then_some(data))
}

#[cfg(not(windows))]
fn current_variant() -> TrayVariant {
    TrayVariant::Dark
}

/// Windows has no cheap "taskbar theme changed" hook that reaches us (the webview's theme
/// event follows the app theme, not the taskbar), so a small thread re-reads the registry
/// value every few seconds and calls `set_icon` only when the variant changed.
#[cfg(windows)]
fn watch_taskbar_theme(app: &AppHandle, initial: TrayVariant) {
    let app = app.clone();
    std::thread::spawn(move || {
        let mut shown = initial;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let now = current_variant();
            if now == shown {
                continue;
            }
            if let (Some(tray), Some(icon)) = (app.tray_by_id("v2c"), variant_icon(now)) {
                if tray.set_icon(Some(icon)).is_ok() {
                    info!(variant = now.name(), "tray icon variant changed");
                    shown = now;
                }
            }
        }
    });
}

#[cfg(not(windows))]
fn watch_taskbar_theme(_app: &AppHandle, _initial: TrayVariant) {}

/// Builds the tray icon and its menu.
pub fn start(app: &AppHandle) -> tauri::Result<()> {
    let polish_on = app.state::<AppState>().config.polish.enabled;

    let settings = MenuItem::with_id(app, ID_SETTINGS, "設定", true, None::<&str>)?;
    let history = MenuItem::with_id(app, ID_HISTORY, "輸入紀錄", true, None::<&str>)?;
    let polish = CheckMenuItem::with_id(app, ID_POLISH, "潤飾", true, polish_on, None::<&str>)?;
    let switch = MenuItem::with_id(
        app,
        ID_SWITCH_PROVIDER,
        "切換服務（尚未提供）",
        false,
        None::<&str>,
    )?;
    let all_local = MenuItem::with_id(app, ID_ALL_LOCAL, "全部改用地端", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "結束", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &settings,
            &history,
            &PredefinedMenuItem::separator(app)?,
            &polish,
            &switch,
            &all_local,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("v2c")
        .tooltip("v2c 語音輸入")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| on_menu(app, &event, &polish))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle(), "");
            }
        });
    let variant = current_variant();
    if let Some(icon) = variant_icon(variant) {
        builder = builder.icon(icon);
    } else if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    info!(variant = variant.name(), "tray icon created");
    watch_taskbar_theme(app, variant);
    Ok(())
}

fn on_menu(app: &AppHandle, event: &MenuEvent, polish: &CheckMenuItem<Wry>) {
    match event.id().as_ref() {
        ID_SETTINGS => show_main(app, "settings"),
        ID_HISTORY => show_main(app, "history"),
        ID_POLISH => {
            // The check mark has already flipped; it is the wanted state.
            let wanted = polish.is_checked().unwrap_or(true);
            match change_config(app, ConfigAction::SetPolish(wanted)) {
                Some(cfg) => {
                    let _ = polish.set_checked(cfg.polish.enabled);
                }
                None => {
                    let _ = polish.set_checked(!wanted);
                }
            }
        }
        ID_ALL_LOCAL => {
            change_config(app, ConfigAction::AllLocal);
        }
        ID_QUIT => {
            info!("quit from tray");
            app.exit(0);
        }
        _ => {}
    }
}

fn change_config(app: &AppHandle, action: ConfigAction) -> Option<Config> {
    let state = app.state::<AppState>();
    match update_config_file(&state.paths.config_file(), action) {
        Ok(cfg) => {
            info!(?action, "config updated from tray");
            let _ = app.emit(CONFIG_CHANGED_EVENT, ());
            Some(cfg)
        }
        Err(code) => {
            warn!(code, ?action, "tray could not update config");
            let _ = state.status.send(StatusEvent::Error { code });
            None
        }
    }
}

/// Shows, restores, and focuses the main window on hash route `route` ("" = status page).
pub fn show_main(app: &AppHandle, route: &str) {
    let Some(w) = app.get_webview_window(MAIN_WINDOW) else {
        warn!("main window not found");
        return;
    };
    // `route` is one of our fixed route names, never user input.
    let _ = w.eval(format!("location.hash = '#/{route}'"));
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("v2c-tray-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("config.toml")
    }

    #[test]
    fn tray_variant_follows_the_taskbar_theme() {
        assert_eq!(tray_variant(Some(1)), TrayVariant::Light);
        assert_eq!(tray_variant(Some(0)), TrayVariant::Dark);
        assert_eq!(tray_variant(None), TrayVariant::Dark);
        assert_eq!(tray_variant(Some(7)), TrayVariant::Dark);
    }

    #[test]
    fn embedded_tray_icons_have_the_expected_size() {
        let pixels = (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize;
        assert_eq!(TRAY_LIGHT_RGBA.len(), pixels);
        assert_eq!(TRAY_DARK_RGBA.len(), pixels);
    }

    #[test]
    fn polish_toggle_reports_changes() {
        let mut cfg = Config::with_defaults();
        assert!(cfg.polish.enabled);
        assert!(!apply(&mut cfg, ConfigAction::SetPolish(true)));
        assert!(apply(&mut cfg, ConfigAction::SetPolish(false)));
        assert!(!cfg.polish.enabled);
        assert!(apply(&mut cfg, ConfigAction::SetPolish(true)));
        assert!(cfg.polish.enabled);
    }

    #[test]
    fn all_local_resets_every_route_and_keeps_the_rest() {
        let mut cfg = Config::with_defaults();
        assert!(!apply(&mut cfg, ConfigAction::AllLocal));
        cfg.route.asr = "gemini".into();
        cfg.route.translate = "openai".into();
        cfg.route.cloud_fallback_to_local = false;
        cfg.polish.enabled = false;
        assert!(apply(&mut cfg, ConfigAction::AllLocal));
        assert_eq!(cfg.route.asr, LOCAL);
        assert_eq!(cfg.route.polish, LOCAL);
        assert_eq!(cfg.route.translate, LOCAL);
        assert!(!cfg.route.cloud_fallback_to_local);
        assert!(!cfg.polish.enabled);
    }

    #[test]
    fn config_file_is_updated_and_reloadable() {
        let path = temp_file("update");
        let mut start = Config::with_defaults();
        start.route.polish = "claude".into();
        start.audio.min_clip_ms = 450;
        std::fs::write(&path, start.to_toml_string().unwrap()).unwrap();

        let cfg = update_config_file(&path, ConfigAction::SetPolish(false)).unwrap();
        assert!(!cfg.polish.enabled);
        let cfg = update_config_file(&path, ConfigAction::AllLocal).unwrap();
        assert_eq!(cfg.route.polish, LOCAL);

        let on_disk = Config::from_toml_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, cfg);
        assert!(!on_disk.polish.enabled);
        assert_eq!(on_disk.audio.min_clip_ms, 450);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_file_is_created_and_invalid_file_is_left_alone() {
        let path = temp_file("missing");
        let cfg = update_config_file(&path, ConfigAction::SetPolish(false)).unwrap();
        assert!(!cfg.polish.enabled);
        assert!(path.exists());

        std::fs::write(&path, "route = 5").unwrap();
        assert_eq!(
            update_config_file(&path, ConfigAction::AllLocal),
            Err("config_invalid_not_saved")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "route = 5");
        std::fs::write(&path, "[polish\nenabled = ").unwrap();
        assert_eq!(
            update_config_file(&path, ConfigAction::SetPolish(false)),
            Err("config_invalid_not_saved")
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[polish\nenabled = "
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    const COMMENTED: &str = "\
# My v2c settings
[route]
asr = \"gemini\" # cloud ASR
polish = \"local\"
translate = 'openai'
my_future_flag = true

[polish]
# keep polish on at work
enabled = true # toggle
timeout_ms = 900

[experimental]
unknown = \"kept\"
";

    #[test]
    fn tray_edit_keeps_comments_and_unknown_keys() {
        let path = temp_file("preserve");
        std::fs::write(&path, COMMENTED).unwrap();

        let cfg = update_config_file(&path, ConfigAction::SetPolish(false)).unwrap();
        assert!(!cfg.polish.enabled);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            COMMENTED.replace("enabled = true # toggle", "enabled = false # toggle")
        );

        let cfg = update_config_file(&path, ConfigAction::AllLocal).unwrap();
        assert_eq!((&*cfg.route.asr, &*cfg.route.translate), (LOCAL, LOCAL));
        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "# My v2c settings",
            "# cloud ASR",
            "my_future_flag = true",
            "# keep polish on at work",
            "enabled = false # toggle",
            "timeout_ms = 900",
            "[experimental]\nunknown = \"kept\"",
            // Already local: left exactly as written.
            "polish = \"local\"",
        ] {
            assert!(text.contains(kept), "lost {kept:?} in:\n{text}");
        }
        assert!(text.contains("asr = \"local\" # cloud ASR"), "{text}");
        assert!(
            !text.contains("gemini") && !text.contains("openai"),
            "{text}"
        );
        let on_disk = Config::from_toml_str(&text).unwrap();
        assert_eq!(on_disk, cfg);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn tray_edit_adds_missing_sections_and_keys() {
        let text = edit_config_text("# only a comment\n", ConfigAction::SetPolish(false)).unwrap();
        // A comment-only file keeps its comment (toml_edit holds it as the document trailer).
        assert!(text.contains("# only a comment\n"), "{text}");
        let cfg = Config::from_toml_str(&text).unwrap();
        assert!(!cfg.polish.enabled);

        let text =
            edit_config_text("[polish]\ntimeout_ms = 5\n", ConfigAction::SetPolish(false)).unwrap();
        let cfg = Config::from_toml_str(&text).unwrap();
        assert!(!cfg.polish.enabled);
        assert_eq!(cfg.polish.timeout_ms, 5);

        // Inline tables are tables too.
        let text =
            edit_config_text("route = { asr = \"gemini\" }\n", ConfigAction::AllLocal).unwrap();
        let cfg = Config::from_toml_str(&text).unwrap();
        assert_eq!(cfg.route.asr, LOCAL);

        assert_eq!(
            edit_config_text("polish = 5", ConfigAction::SetPolish(true)),
            None
        );
        assert_eq!(edit_config_text("[x", ConfigAction::SetPolish(true)), None);
    }

    #[test]
    fn unchanged_action_does_not_rewrite_the_file() {
        let path = temp_file("unchanged");
        std::fs::write(&path, COMMENTED).unwrap();
        update_config_file(&path, ConfigAction::SetPolish(true)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), COMMENTED);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
