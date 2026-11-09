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

use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};
use tracing::{info, warn};
use v2c_core::config::{Config, LOCAL};

use crate::app_state::{self, AppState};
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
/// An invalid file is left untouched. Returns the new config or an error code.
pub fn update_config_file(path: &Path, action: ConfigAction) -> Result<Config, &'static str> {
    let mut cfg = match std::fs::read_to_string(path) {
        Ok(s) => Config::from_toml_str(&s).map_err(|_| "config_invalid")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::with_defaults(),
        Err(_) => return Err("config_unreadable"),
    };
    if apply(&mut cfg, action) {
        app_state::save_config(path, &cfg).map_err(|e| {
            warn!(error = %e, "could not save config.toml");
            "config_save_failed"
        })?;
    }
    Ok(cfg)
}

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
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    info!("tray icon created");
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
            Err("config_invalid")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "route = 5");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
