//! Floating status pill (`tasks/plan.md` section 7, card T1.3).
//!
//! A small transparent, undecorated, always-on-top window at the bottom centre of the primary
//! monitor. It must never take focus (the text goes to the app the user is typing in) and is
//! click-through. It shows states, a volume bar, and short error messages; never any text the
//! user spoke.
//!
//! The decisions (what to show, how long, where) are pure functions with unit tests; the
//! Win32 calls are isolated in the `win` module at the bottom.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::broadcast;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::controller::StatusEvent;

pub const LABEL: &str = "overlay";
/// Webview event carrying an [`OverlayPayload`] to the overlay window only.
pub const OVERLAY_EVENT: &str = "v2c:overlay";

/// Logical (CSS) size of the overlay window and its gap above the work-area bottom. Wide enough
/// for the longest message; the pill is centred and the rest is transparent and click-through.
const WIDTH: f64 = 420.0;
const HEIGHT: f64 = 56.0;
const BOTTOM_MARGIN: f64 = 48.0;

/// "Done" stays 0.5 s, then fades for 0.3 s (the webview runs the fade).
const DONE_MS: u64 = 800;
/// Informational messages (nothing recognised, microphone back) stay a little longer.
const INFO_MS: u64 = 1_500;
/// Errors stay 3 s.
const ERROR_MS: u64 = 3_000;

/// What the overlay shows. Carries states, a level, and fixed UI strings only.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayView {
    /// `level` is the volume bar fill, 0.0..=1.0.
    Recording {
        level: f32,
    },
    Transcribing,
    Polishing,
    Done {
        message: &'static str,
    },
    Info {
        message: &'static str,
    },
    Error {
        message: &'static str,
    },
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OverlayPayload {
    pub view: OverlayView,
    /// When set, the window is hidden this many ms after the view appears (the webview fades
    /// out just before).
    pub hide_after_ms: Option<u64>,
}

/// How the overlay reacts to one status event.
#[derive(Debug, Clone, PartialEq)]
pub enum Reaction {
    /// Show this view; keep it until the next event.
    Show(OverlayView),
    /// Show this view, then hide after the given time.
    ShowFor(OverlayView, u64),
    Hide,
}

/// The overlay's policy for each controller status event.
pub fn react(ev: &StatusEvent) -> Reaction {
    match ev {
        StatusEvent::Recording { rms } => Reaction::Show(OverlayView::Recording {
            level: volume_level(*rms),
        }),
        StatusEvent::Transcribing => Reaction::Show(OverlayView::Transcribing),
        StatusEvent::Polishing => Reaction::Show(OverlayView::Polishing),
        StatusEvent::Done { chars: 0, .. } => Reaction::ShowFor(
            OverlayView::Info {
                message: "沒有偵測到語音",
            },
            INFO_MS,
        ),
        StatusEvent::Done { .. } => {
            Reaction::ShowFor(OverlayView::Done { message: "完成" }, DONE_MS)
        }
        StatusEvent::Cancelled => Reaction::Hide,
        StatusEvent::MicReady => Reaction::ShowFor(
            OverlayView::Info {
                message: "麥克風已重新連線",
            },
            INFO_MS,
        ),
        StatusEvent::Error { code } => Reaction::ShowFor(
            OverlayView::Error {
                message: error_message(code),
            },
            ERROR_MS,
        ),
    }
}

/// Maps an input RMS (0.0..=1.0) to a bar fill. Speech sits far below full scale, so the bar
/// is logarithmic: -50 dBFS (room noise) is empty, -10 dBFS (loud speech) is full.
pub fn volume_level(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 50.0) / 40.0).clamp(0.0, 1.0)
}

/// Short Traditional Chinese message for a status error code. Codes come from the controller,
/// the recorder, and startup (`main.rs`); unknown codes get a generic message.
pub fn error_message(code: &str) -> &'static str {
    match code {
        "model_missing" => "找不到語音模型，請到設定下載",
        "model_load_failed" => "語音模型載入失敗",
        "sensevoice_not_available" => "SenseVoice 模型無法使用",
        "asr_loading" => "語音模型載入中，請稍候",
        "asr_failed" => "語音辨識失敗",
        "audio_invalid" => "錄音資料異常",
        // tasks/plan.md section 6.1: LM Studio not running.
        "polish_fell_back" => "LM Studio 未啟動，已改用基本整理",
        "polish_failed" => "潤飾失敗",
        "translate_failed" | "llm_unavailable" => "請啟動 LM Studio，或串接雲端服務",
        "not_configured" => "尚未設定翻譯模型",
        "cloud_fell_back" => "雲端服務失敗，已改用地端",
        "inject_elevated" => "無法輸入到系統管理員視窗，文字已複製到剪貼簿",
        "inject_partial" => "只輸入了部分文字，其餘已複製到剪貼簿",
        "inject_paste_unsupported" => "此 App 需要貼上模式（尚未支援）",
        "inject_modifiers_held" => "請放開修飾鍵後再說一次",
        "inject_failed" => "無法輸入文字",
        "busy" => "處理中，這段語音已略過",
        "mic_lost" => "麥克風中斷，正在重新連線",
        "mic_not_found" => "找不到麥克風",
        "mic_config_failed" | "mic_format_unsupported" | "mic_start_failed" | "mic_open_failed" => {
            "無法開啟麥克風"
        }
        "hotkey_failed" => "熱鍵安裝失敗",
        "config_invalid" => "設定檔格式錯誤，已使用預設值",
        "config_unreadable" => "無法讀取設定檔，已使用預設值",
        "config_save_failed" => "無法儲存設定檔",
        "storage_failed" => "無法寫入資料",
        "platform_unsupported" => "此平台尚未支援熱鍵與輸入",
        _ => "發生錯誤",
    }
}

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PxRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Where the overlay goes: bottom centre of `work_area` (the monitor minus the taskbar), sized
/// for the monitor's `scale` (1.0 = 96 DPI). Narrow monitors clamp the width.
pub fn overlay_rect(work_area: PxRect, scale: f64) -> PxRect {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let width = ((WIDTH * scale).round() as u32).min(work_area.width);
    let height = ((HEIGHT * scale).round() as u32).min(work_area.height);
    let margin = (BOTTOM_MARGIN * scale).round() as i64;
    let x = work_area.x as i64 + (work_area.width as i64 - width as i64) / 2;
    let bottom = work_area.y as i64 + work_area.height as i64;
    let y = (bottom - margin - height as i64).max(work_area.y as i64);
    PxRect {
        x: x as i32,
        y: y as i32,
        width,
        height,
    }
}

/// Creates the hidden overlay window and starts the task that drives it from `status`.
pub fn start(app: &AppHandle, status: &broadcast::Sender<StatusEvent>) -> tauri::Result<()> {
    let window =
        WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html#/overlay".into()))
            .title("v2c overlay")
            .inner_size(WIDTH, HEIGHT)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .focusable(false)
            .visible(false)
            .build()?;
    // Click-through (tao adds WS_EX_TRANSPARENT | WS_EX_LAYERED).
    if let Err(e) = window.set_ignore_cursor_events(true) {
        warn!(error = %e, "overlay: could not make the window click-through");
    }
    #[cfg(windows)]
    {
        let w = window.clone();
        let _ = app.run_on_main_thread(move || {
            if let Ok(hwnd) = w.hwnd() {
                win::apply_styles(hwnd.0);
            }
        });
    }

    let rx = status.subscribe();
    tauri::async_runtime::spawn(drive(app.clone(), window, rx));
    info!("overlay window created");
    Ok(())
}

/// Applies each status event to the overlay and hides it when its time is up.
async fn drive(app: AppHandle, window: WebviewWindow, mut rx: broadcast::Receiver<StatusEvent>) {
    let mut shown = false;
    let mut hide_at: Option<Instant> = None;
    loop {
        let ev = match hide_at {
            Some(at) => tokio::select! {
                ev = rx.recv() => ev,
                () = tokio::time::sleep_until(at) => {
                    hide_at = None;
                    set_visible(&app, &window, false);
                    shown = false;
                    emit(&app, OverlayView::Hidden, None);
                    continue;
                }
            },
            None => rx.recv().await,
        };
        let ev = match ev {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        };
        match react(&ev) {
            Reaction::Show(view) => {
                hide_at = None;
                emit(&app, view, None);
                if !shown {
                    set_visible(&app, &window, true);
                    shown = true;
                }
            }
            Reaction::ShowFor(view, ms) => {
                hide_at = Some(Instant::now() + Duration::from_millis(ms));
                emit(&app, view, Some(ms));
                if !shown {
                    set_visible(&app, &window, true);
                    shown = true;
                }
            }
            Reaction::Hide => {
                hide_at = None;
                if shown {
                    set_visible(&app, &window, false);
                    shown = false;
                }
                emit(&app, OverlayView::Hidden, None);
            }
        }
    }
}

fn emit(app: &AppHandle, view: OverlayView, hide_after_ms: Option<u64>) {
    let _ = app.emit_to(
        LABEL,
        OVERLAY_EVENT,
        OverlayPayload {
            view,
            hide_after_ms,
        },
    );
}

/// The overlay's rectangle on the primary monitor (`tasks/plan.md` section 7: "主螢幕下方中央").
fn target_rect(window: &WebviewWindow) -> Option<PxRect> {
    let monitor = window.primary_monitor().ok().flatten()?;
    let wa = monitor.work_area();
    Some(overlay_rect(
        PxRect {
            x: wa.position.x,
            y: wa.position.y,
            width: wa.size.width,
            height: wa.size.height,
        },
        monitor.scale_factor(),
    ))
}

/// Shows (without activating) or hides the overlay.
fn set_visible(app: &AppHandle, window: &WebviewWindow, visible: bool) {
    let rect = if visible { target_rect(window) } else { None };
    #[cfg(windows)]
    {
        // Tauri's `show()` ends in `ShowWindow(SW_SHOW)`, which activates the window; use
        // `SWP_NOACTIVATE | SWP_SHOWWINDOW` on the HWND instead.
        let w = window.clone();
        let result = app.run_on_main_thread(move || {
            if let Ok(hwnd) = w.hwnd() {
                if visible {
                    win::show_no_activate(hwnd.0, rect);
                } else {
                    win::hide(hwnd.0);
                }
            }
        });
        if result.is_err() {
            warn!("overlay: could not reach the main thread");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        if visible {
            if let Some(r) = rect {
                let _ = window.set_size(tauri::PhysicalSize::new(r.width, r.height));
                let _ = window.set_position(tauri::PhysicalPosition::new(r.x, r.y));
            }
            let _ = window.show();
        } else {
            let _ = window.hide();
        }
    }
}

/// Debug builds only: `V2C_OVERLAY_DEMO=1` cycles through the overlay states once at startup,
/// so the look can be checked without speaking.
#[cfg(debug_assertions)]
pub fn start_demo_if_requested(status: &broadcast::Sender<StatusEvent>) {
    if std::env::var_os("V2C_OVERLAY_DEMO").is_none() {
        return;
    }
    info!("overlay demo: cycling states");
    let status = status.clone();
    tauri::async_runtime::spawn(async move {
        let pause = |ms| tokio::time::sleep(Duration::from_millis(ms));
        pause(3_000).await;
        for i in 0..60u32 {
            let rms = 0.002 + 0.15 * ((i as f32) * 0.35).sin().abs();
            let _ = status.send(StatusEvent::Recording { rms });
            pause(50).await;
        }
        let _ = status.send(StatusEvent::Transcribing);
        pause(1_500).await;
        let _ = status.send(StatusEvent::Polishing);
        pause(1_500).await;
        let _ = status.send(StatusEvent::Done {
            chars: 12,
            latency_ms: 900,
        });
        pause(2_000).await;
        let _ = status.send(StatusEvent::Error {
            code: "inject_elevated",
        });
    });
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    use super::PxRect;

    /// No activation, click-through, no taskbar / Alt+Tab entry.
    const OVERLAY_EX_STYLE: u32 =
        WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW;

    /// Adds the overlay's extended styles. Tao rewrites `GWL_EXSTYLE` when its own window flags
    /// change, so this runs again before every show.
    pub fn apply_styles(hwnd: *mut c_void) {
        // SAFETY: `hwnd` is the overlay's live window handle, used on the UI thread.
        unsafe {
            let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let wanted = current | OVERLAY_EX_STYLE as isize;
            if wanted != current {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
            }
        }
    }

    pub fn show_no_activate(hwnd: *mut c_void, rect: Option<PxRect>) {
        apply_styles(hwnd);
        let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOOWNERZORDER;
        let (x, y, w, h) = match rect {
            Some(r) => (r.x, r.y, r.width as i32, r.height as i32),
            None => {
                flags |= SWP_NOMOVE | SWP_NOSIZE;
                (0, 0, 0, 0)
            }
        };
        // SAFETY: as above.
        unsafe {
            SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, flags);
        }
    }

    pub fn hide(hwnd: *mut c_void) {
        // SAFETY: as above.
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_map_to_views_and_timings() {
        assert_eq!(
            react(&StatusEvent::Transcribing),
            Reaction::Show(OverlayView::Transcribing)
        );
        assert_eq!(
            react(&StatusEvent::Polishing),
            Reaction::Show(OverlayView::Polishing)
        );
        assert_eq!(
            react(&StatusEvent::Done {
                chars: 5,
                latency_ms: 700
            }),
            Reaction::ShowFor(OverlayView::Done { message: "完成" }, DONE_MS)
        );
        assert!(matches!(
            react(&StatusEvent::Done {
                chars: 0,
                latency_ms: 10
            }),
            Reaction::ShowFor(OverlayView::Info { .. }, INFO_MS)
        ));
        assert_eq!(react(&StatusEvent::Cancelled), Reaction::Hide);
        assert_eq!(
            react(&StatusEvent::Error { code: "busy" }),
            Reaction::ShowFor(
                OverlayView::Error {
                    message: "處理中，這段語音已略過"
                },
                ERROR_MS
            )
        );
        assert_eq!(
            react(&StatusEvent::Recording { rms: 0.0 }),
            Reaction::Show(OverlayView::Recording { level: 0.0 })
        );
    }

    #[test]
    fn done_hides_after_half_second_plus_fade_and_error_after_three_seconds() {
        assert!((500..=1_000).contains(&DONE_MS));
        assert_eq!(ERROR_MS, 3_000);
    }

    #[test]
    fn error_codes_have_short_messages() {
        assert_eq!(
            error_message("inject_elevated"),
            "無法輸入到系統管理員視窗，文字已複製到剪貼簿"
        );
        assert_eq!(error_message("mic_lost"), "麥克風中斷，正在重新連線");
        assert_eq!(
            error_message("model_missing"),
            "找不到語音模型，請到設定下載"
        );
        assert_eq!(error_message("busy"), "處理中，這段語音已略過");
        assert_eq!(
            error_message("polish_fell_back"),
            "LM Studio 未啟動，已改用基本整理"
        );
        assert_eq!(
            error_message("translate_failed"),
            "請啟動 LM Studio，或串接雲端服務"
        );
        assert_eq!(error_message("mic_open_failed"), "無法開啟麥克風");
        assert_eq!(error_message("something_new"), "發生錯誤");
        // Every known code fits the pill.
        for code in [
            "model_missing",
            "model_load_failed",
            "sensevoice_not_available",
            "asr_loading",
            "asr_failed",
            "audio_invalid",
            "polish_failed",
            "translate_failed",
            "not_configured",
            "inject_elevated",
            "inject_partial",
            "inject_paste_unsupported",
            "inject_modifiers_held",
            "inject_failed",
            "busy",
            "mic_lost",
            "mic_not_found",
            "hotkey_failed",
            "config_invalid",
            "config_unreadable",
            "config_save_failed",
            "storage_failed",
            "platform_unsupported",
        ] {
            let m = error_message(code);
            assert_ne!(m, "發生錯誤", "{code} has no message");
            assert!(m.chars().count() <= 26, "{code}: message too long");
        }
    }

    #[test]
    fn volume_level_is_logarithmic_and_clamped() {
        assert_eq!(volume_level(0.0), 0.0);
        assert_eq!(volume_level(-1.0), 0.0);
        assert_eq!(volume_level(f32::NAN), 0.0);
        assert_eq!(volume_level(0.001), 0.0); // -60 dBFS
        assert!((volume_level(0.01) - 0.25).abs() < 1e-4); // -40 dBFS
        assert!((volume_level(0.1) - 0.75).abs() < 1e-4); // -20 dBFS
        assert_eq!(volume_level(0.5), 1.0);
        assert_eq!(volume_level(1.0), 1.0);
        assert!(volume_level(0.03) > volume_level(0.01));
    }

    #[test]
    fn rect_is_bottom_centre_at_100_percent() {
        // 1920x1080 primary monitor with a 40 px taskbar at the bottom.
        let wa = PxRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        let r = overlay_rect(wa, 1.0);
        assert_eq!((r.width, r.height), (420, 56));
        assert_eq!(r.x, (1920 - 420) / 2);
        assert_eq!(r.y, 1040 - 48 - 56);
    }

    #[test]
    fn rect_scales_with_dpi() {
        // 2560x1440 at 150 %, taskbar 72 px.
        let wa = PxRect {
            x: 0,
            y: 0,
            width: 2560,
            height: 1368,
        };
        let r = overlay_rect(wa, 1.5);
        assert_eq!((r.width, r.height), (630, 84));
        assert_eq!(r.x, (2560 - 630) / 2);
        assert_eq!(r.y, 1368 - 72 - 84);
    }

    #[test]
    fn rect_respects_monitor_origin_and_small_screens() {
        let wa = PxRect {
            x: -1280,
            y: 200,
            width: 1280,
            height: 984,
        };
        let r = overlay_rect(wa, 1.0);
        assert_eq!(r.x, -1280 + (1280 - 420) / 2);
        assert_eq!(r.y, 200 + 984 - 48 - 56);

        let tiny = PxRect {
            x: 0,
            y: 0,
            width: 200,
            height: 60,
        };
        let r = overlay_rect(tiny, 1.0);
        assert_eq!((r.x, r.width), (0, 200));
        assert_eq!(r.y, 0); // never above the work area
        assert_eq!(overlay_rect(tiny, f64::NAN), overlay_rect(tiny, 1.0));
    }

    #[test]
    fn payload_serialises_without_any_text_field() {
        let p = OverlayPayload {
            view: OverlayView::Recording { level: 0.5 },
            hide_after_ms: None,
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({ "view": { "kind": "recording", "level": 0.5 }, "hide_after_ms": null })
        );
        let p = OverlayPayload {
            view: OverlayView::Error {
                message: error_message("busy"),
            },
            hide_after_ms: Some(ERROR_MS),
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({
                "view": { "kind": "error", "message": "處理中，這段語音已略過" },
                "hide_after_ms": 3000
            })
        );
    }
}
