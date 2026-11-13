//! Floating status pill (`tasks/plan.md` section 7, card T1.3).
//!
//! A small transparent, undecorated, always-on-top window at the bottom centre of the primary
//! monitor. It must never take focus (the text goes to the app the user is typing in) and is
//! click-through. It shows states, a volume bar, and short error messages; never any text the
//! user spoke.
//!
//! The decisions (what to show, how long, where) are pure functions with unit tests; the
//! Win32 calls are isolated in the `win` module at the bottom.

use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Listener, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

use crate::controller::StatusEvent;

pub const LABEL: &str = "overlay";
/// Webview event carrying an [`OverlayPayload`] to the overlay window only.
pub const OVERLAY_EVENT: &str = "v2c:overlay";
/// Sent by the overlay webview (no payload) once its `v2c:overlay` listener is registered.
/// Events emitted before that are lost, so the driver re-sends the current view (and a timed
/// view such as a startup error that was missed) when this arrives.
pub const READY_EVENT: &str = "v2c:overlay-ready";

/// While the overlay shows "recording", a Transcribing / Polishing / Done that arrives is held
/// this long. Recording level events come about every 50 ms, so if another one arrives first
/// the held event belonged to the previous utterance (still processing when the new recording
/// started) and is dropped instead of flickering over the recording pill.
const RECORDING_GRACE_MS: u64 = 200;

/// Logical (CSS) size of the overlay window and its gap above the work-area bottom. Wide enough
/// for the longest message; the pill is centred and the rest is transparent and click-through.
const WIDTH: f64 = 420.0;
const HEIGHT: f64 = 56.0;
const BOTTOM_MARGIN: f64 = 48.0;

/// "Done" is only the bar snapping to 100% and a quick fade (the webview runs the fade).
const DONE_MS: u64 = 250;
/// Informational messages (nothing recognised, microphone back) stay a little longer.
const INFO_MS: u64 = 1_500;
/// Errors stay 3 s.
const ERROR_MS: u64 = 3_000;

/// What the overlay shows. Carries states, a level, and fixed UI strings only.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayView {
    /// `level` is the volume bar fill, 0.0..=1.0. `hands_free` adds the 「免持」 badge and the
    /// "按右 Ctrl 結束" hint (the webview treats a missing field as `false`).
    Recording {
        level: f32,
        hands_free: bool,
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
        // `..`: the controller is gaining a hands-free flag on this event; pass it to
        // `recording_view` once it exists.
        StatusEvent::Recording { rms, hands_free } => {
            Reaction::Show(recording_view(*rms, *hands_free))
        }
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
        StatusEvent::Error { code } => match notice_message(code) {
            Some(message) => Reaction::ShowFor(OverlayView::Info { message }, INFO_MS),
            None => Reaction::ShowFor(
                OverlayView::Error {
                    message: error_message(code),
                },
                ERROR_MS,
            ),
        },
    }
}

/// The recording pill: volume bar from `rms`, plus the hands-free badge when `hands_free`.
pub fn recording_view(rms: f32, hands_free: bool) -> OverlayView {
    OverlayView::Recording {
        level: volume_level(rms),
        hands_free,
    }
}

/// Codes that are good news rather than errors: shown as a neutral info pill.
pub fn notice_message(code: &str) -> Option<&'static str> {
    match code {
        "gpu_restored" => Some("已恢復使用顯示卡"),
        // An utterance waits for the one local model to be swapped (T1.12).
        "asr_switching" => Some("切換模型中…"),
        _ => None,
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
        "translate_unavailable" => "中翻英需要 LM Studio 或雲端服務",
        "not_configured" => "尚未設定翻譯模型",
        "cloud_fell_back" => "雲端服務失敗，已改用地端",
        "inject_elevated" => "無法輸入到系統管理員視窗，文字已複製到剪貼簿",
        "inject_partial" => "只輸入了部分文字，其餘已複製到剪貼簿",
        "inject_paste_unsupported" => "此 App 需要貼上模式（尚未支援）",
        "inject_modifiers_held" => "請先放開按鍵再輸出",
        "inject_failed" => "無法輸入文字",
        "busy" => "處理中，這段語音已略過",
        "mic_lost" => "麥克風中斷，正在重新連線",
        "mic_not_found" => "找不到麥克風",
        "mic_config_failed" | "mic_format_unsupported" | "mic_start_failed" | "mic_open_failed" => {
            "無法開啟麥克風"
        }
        "hotkey_failed" => "熱鍵安裝失敗",
        "hands_free_limit" => "已達免持上限，已自動結束",
        "cloud_not_configured" => "雲端服務未設定完成，請到雲端服務頁檢查",
        "polish_timed_out" => "潤飾逾時，已改用基本整理",
        "gpu_yield_heavy_app" => "偵測到重度程式，暫時改用 CPU",
        "gpu_low_vram" => "顯示卡記憶體不足，暫時改用 CPU",
        "config_invalid" => "設定檔格式錯誤，已使用預設值",
        // The tray could not apply a change because config.toml is malformed.
        "config_invalid_not_saved" => "設定檔格式錯誤，未變更",
        "config_unreadable" => "無法讀取設定檔，已使用預設值",
        "config_save_failed" => "無法儲存設定檔",
        "storage_failed" => "無法寫入資料",
        "platform_unsupported" => "此平台尚未支援熱鍵與輸入",
        _ => "發生錯誤",
    }
}

/// What the driver asks the window to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Send this payload to the overlay webview.
    Emit(OverlayPayload),
    /// Show the window without activating it.
    Show,
    Hide,
}

/// The overlay's state over time (pure; the async `drive` loop feeds it events, timeouts, and
/// the webview's ready signal, and carries out the returned [`Command`]s).
#[derive(Debug, Default)]
pub struct Driver {
    /// What the webview shows now (`None` = hidden) and its total display time.
    current: Option<(OverlayView, Option<u64>)>,
    hide_at: Option<Instant>,
    /// An event held while recording (see [`RECORDING_GRACE_MS`]) and when it applies.
    pending: Option<(Reaction, Instant)>,
    webview_ready: bool,
    /// The last timed view (e.g. a startup error) sent before the webview was listening;
    /// replayed once when it becomes ready, even if its time already ran out.
    missed: Option<(OverlayView, u64)>,
}

impl Driver {
    /// The next time [`Driver::on_timeout`] has something to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        match (self.pending.as_ref().map(|p| p.1), self.hide_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn on_status(&mut self, ev: &StatusEvent, now: Instant) -> Vec<Command> {
        let reaction = react(ev);
        if matches!(ev, StatusEvent::Recording { .. }) {
            // Recording wins: anything held belonged to the previous utterance.
            self.pending = None;
            return self.apply(reaction, now);
        }
        let recording = matches!(self.current, Some((OverlayView::Recording { .. }, _)));
        let deferrable = matches!(
            ev,
            StatusEvent::Transcribing | StatusEvent::Polishing | StatusEvent::Done { .. }
        );
        if recording && deferrable {
            self.pending = Some((reaction, now + Duration::from_millis(RECORDING_GRACE_MS)));
            return Vec::new();
        }
        self.pending = None;
        self.apply(reaction, now)
    }

    pub fn on_timeout(&mut self, now: Instant) -> Vec<Command> {
        let mut cmds = Vec::new();
        if self.pending.as_ref().is_some_and(|p| p.1 <= now) {
            if let Some((reaction, _)) = self.pending.take() {
                cmds.extend(self.apply(reaction, now));
            }
        }
        if self.hide_at.is_some_and(|at| at <= now) {
            cmds.extend(self.apply(Reaction::Hide, now));
        }
        cmds
    }

    /// The webview registered its listener: re-send what it missed. A timed view restarts its
    /// full time so the user gets to read it.
    pub fn on_ready(&mut self, now: Instant) -> Vec<Command> {
        let first = !self.webview_ready;
        self.webview_ready = true;
        let missed = self.missed.take();
        match self.current.clone() {
            Some((view, ms)) => {
                if let Some(ms) = ms {
                    self.hide_at = Some(now + Duration::from_millis(ms));
                }
                vec![Command::Emit(payload(view, ms))]
            }
            None => match missed {
                Some((view, ms)) if first => self.apply(Reaction::ShowFor(view, ms), now),
                _ => Vec::new(),
            },
        }
    }

    fn apply(&mut self, reaction: Reaction, now: Instant) -> Vec<Command> {
        match reaction {
            Reaction::Show(view) => {
                self.hide_at = None;
                self.show(view, None)
            }
            Reaction::ShowFor(view, ms) => {
                self.hide_at = Some(now + Duration::from_millis(ms));
                self.show(view, Some(ms))
            }
            Reaction::Hide => {
                self.hide_at = None;
                self.pending = None;
                let mut cmds = Vec::new();
                if self.current.take().is_some() {
                    cmds.push(Command::Hide);
                }
                cmds.push(Command::Emit(payload(OverlayView::Hidden, None)));
                cmds
            }
        }
    }

    fn show(&mut self, view: OverlayView, ms: Option<u64>) -> Vec<Command> {
        if !self.webview_ready {
            self.missed = ms.map(|ms| (view.clone(), ms));
        }
        let was_shown = self.current.is_some();
        self.current = Some((view.clone(), ms));
        let mut cmds = vec![Command::Emit(payload(view, ms))];
        if !was_shown {
            cmds.push(Command::Show);
        }
        cmds
    }
}

fn payload(view: OverlayView, hide_after_ms: Option<u64>) -> OverlayPayload {
    OverlayPayload {
        view,
        hide_after_ms,
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

/// Creates the hidden overlay window and starts the task that drives it from `rx` (subscribed
/// by the caller earlier, so events sent before the window exists are still delivered).
pub fn start(app: &AppHandle, rx: broadcast::Receiver<StatusEvent>) -> tauri::Result<()> {
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
    // Click-through (tao adds WS_EX_TRANSPARENT | WS_EX_LAYERED). Linux: tao unwraps the
    // GdkWindow, which a never-shown GTK window does not have yet (abort at startup), so
    // there it is set after each `show()` (see `set_visible`).
    #[cfg(not(target_os = "linux"))]
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

    // The overlay webview announces when it listens (`Overlay.svelte`); the event has no
    // payload, so only the signal is forwarded.
    let (ready_tx, ready_rx) = mpsc::unbounded_channel();
    app.listen_any(READY_EVENT, move |_| {
        let _ = ready_tx.send(());
    });

    tauri::async_runtime::spawn(drive(app.clone(), window, rx, ready_rx));
    info!("overlay window created");
    Ok(())
}

/// Feeds status events, the webview's ready signal, and timeouts to a [`Driver`] and carries
/// out its commands.
async fn drive(
    app: AppHandle,
    window: WebviewWindow,
    mut rx: broadcast::Receiver<StatusEvent>,
    mut ready: mpsc::UnboundedReceiver<()>,
) {
    let mut driver = Driver::default();
    loop {
        let deadline = driver.next_deadline();
        let timeout = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                None => std::future::pending().await,
            }
        };
        let cmds = tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => driver.on_status(&ev, Instant::now()),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            Some(()) = ready.recv() => {
                info!("overlay webview ready");
                driver.on_ready(Instant::now())
            }
            () = timeout => driver.on_timeout(Instant::now()),
        };
        for cmd in cmds {
            match cmd {
                Command::Emit(p) => {
                    let _ = app.emit_to(LABEL, OVERLAY_EVENT, p);
                }
                Command::Show => set_visible(&app, &window, true),
                Command::Hide => set_visible(&app, &window, false),
            }
        }
    }
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
            #[cfg(target_os = "linux")]
            if let Err(e) = window.set_ignore_cursor_events(true) {
                warn!(error = %e, "overlay: could not make the window click-through");
            }
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
            let _ = status.send(StatusEvent::Recording {
                rms,
                hands_free: false,
            });
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

/// Environment flag for [`win`]: `V2C_OVERLAY_LAYERED_ALPHA=1` also calls
/// `SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA)` on the overlay.
#[cfg_attr(not(windows), allow(dead_code))]
pub const LAYERED_ALPHA_ENV: &str = "V2C_OVERLAY_LAYERED_ALPHA";

/// Whether the [`LAYERED_ALPHA_ENV`] value turns the fallback on (exactly `1`).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn layered_alpha_requested(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| v == "1")
}

// =============================================================================================
// WINDOWS: DO NOT call any Tauri window setter on the overlay window: no `show()`, `hide()`,
// `set_always_on_top()`, `set_size()`, `set_position()`, `set_focus()`, `set_decorations()`,
// `set_ignore_cursor_events()` (after `start`) ... Tao never learns that `win::show_no_activate`
// made the window visible, so its internal state still says "hidden"; any setter goes through
// tao's `apply_diff`, which then calls `ShowWindow(SW_HIDE)` and rewrites `GWL_EXSTYLE` from
// its own flags, dropping WS_EX_NOACTIVATE / WS_EX_TOOLWINDOW. Show, hide, move, and resize only
// through the functions in `win` below (on the main thread).
// =============================================================================================
#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow,
        GWL_EXSTYLE, HWND_TOPMOST, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
        SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TRANSPARENT,
    };

    use super::PxRect;

    /// No activation, click-through, no taskbar / Alt+Tab entry.
    ///
    /// WS_EX_LAYERED is set WITHOUT `SetLayeredWindowAttributes`. That is deliberate and matches
    /// Tauri's own `set_ignore_cursor_events(true)` (tao adds WS_EX_LAYERED | WS_EX_TRANSPARENT
    /// the same way); WebView2 draws the transparent pill through DirectComposition, so the
    /// label is visible. If on some machine the pill turns out invisible (a layered window that
    /// never got attributes may not be drawn), start the app with `V2C_OVERLAY_LAYERED_ALPHA=1`:
    /// `apply_styles` then also calls `SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA)`
    /// (fully opaque layer; per-pixel transparency still comes from the webview).
    const OVERLAY_EX_STYLE: u32 =
        WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW;

    /// Adds the overlay's extended styles. Tao rewrites `GWL_EXSTYLE` when its own window flags
    /// change, so this runs again before every show.
    pub fn apply_styles(hwnd: *mut c_void) {
        let layered_alpha =
            super::layered_alpha_requested(std::env::var_os(super::LAYERED_ALPHA_ENV).as_deref());
        // SAFETY: `hwnd` is the overlay's live window handle, used on the UI thread.
        unsafe {
            let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let wanted = current | OVERLAY_EX_STYLE as isize;
            if wanted != current {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
            }
            if layered_alpha {
                SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
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
            react(&StatusEvent::Recording {
                rms: 0.0,
                hands_free: false
            }),
            Reaction::Show(OverlayView::Recording {
                level: 0.0,
                hands_free: false
            })
        );
    }

    #[test]
    fn done_hides_quickly_and_error_after_three_seconds() {
        assert!((150..=400).contains(&DONE_MS));
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
        assert_eq!(
            error_message("hands_free_limit"),
            "已達免持上限，已自動結束"
        );
        assert_eq!(
            error_message("cloud_not_configured"),
            "雲端服務未設定完成，請到雲端服務頁檢查"
        );
        assert_eq!(
            error_message("polish_timed_out"),
            "潤飾逾時，已改用基本整理"
        );
        assert_eq!(
            error_message("translate_unavailable"),
            "中翻英需要 LM Studio 或雲端服務"
        );
        assert_eq!(
            error_message("gpu_yield_heavy_app"),
            "偵測到重度程式，暫時改用 CPU"
        );
        assert_eq!(
            error_message("gpu_low_vram"),
            "顯示卡記憶體不足，暫時改用 CPU"
        );
        assert_eq!(error_message("inject_modifiers_held"), "請先放開按鍵再輸出");
        assert_eq!(
            error_message("config_invalid_not_saved"),
            "設定檔格式錯誤，未變更"
        );
        assert_eq!(notice_message("gpu_restored"), Some("已恢復使用顯示卡"));
        assert_eq!(notice_message("gpu_low_vram"), None);
        // Every known code fits the pill.
        for code in [
            "polish_fell_back",
            "llm_unavailable",
            "cloud_fell_back",
            "mic_config_failed",
            "mic_format_unsupported",
            "mic_start_failed",
            "mic_open_failed",
            "hands_free_limit",
            "cloud_not_configured",
            "polish_timed_out",
            "translate_unavailable",
            "gpu_yield_heavy_app",
            "gpu_low_vram",
            "config_invalid_not_saved",
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
        const NOTICE_CODES: &[&str] = &["gpu_restored", "asr_switching"];
        for &code in NOTICE_CODES {
            let m = notice_message(code).expect(code);
            assert!(m.chars().count() <= 26, "{code}: message too long");
        }
    }

    #[test]
    fn notices_show_as_info_not_error() {
        assert_eq!(
            react(&StatusEvent::Error {
                code: "gpu_restored"
            }),
            Reaction::ShowFor(
                OverlayView::Info {
                    message: "已恢復使用顯示卡"
                },
                INFO_MS
            )
        );
        assert!(matches!(
            react(&StatusEvent::Error {
                code: "gpu_low_vram"
            }),
            Reaction::ShowFor(OverlayView::Error { .. }, ERROR_MS)
        ));
    }

    #[test]
    fn recording_view_carries_hands_free() {
        assert_eq!(
            recording_view(1.0, true),
            OverlayView::Recording {
                level: 1.0,
                hands_free: true
            }
        );
        assert_eq!(
            recording_view(0.0, false),
            OverlayView::Recording {
                level: 0.0,
                hands_free: false
            }
        );
    }

    #[test]
    fn layered_alpha_flag_needs_exactly_one() {
        use std::ffi::OsStr;
        assert!(layered_alpha_requested(Some(OsStr::new("1"))));
        assert!(!layered_alpha_requested(Some(OsStr::new("0"))));
        assert!(!layered_alpha_requested(Some(OsStr::new(""))));
        assert!(!layered_alpha_requested(None));
    }

    // ---- Driver --------------------------------------------------------------------------

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn rec(rms: f32) -> StatusEvent {
        StatusEvent::Recording {
            rms,
            hands_free: false,
        }
    }

    fn done() -> StatusEvent {
        StatusEvent::Done {
            chars: 4,
            latency_ms: 500,
        }
    }

    fn emitted(cmds: &[Command]) -> Vec<OverlayView> {
        cmds.iter()
            .filter_map(|c| match c {
                Command::Emit(p) => Some(p.view.clone()),
                _ => None,
            })
            .collect()
    }

    fn ready_driver(t0: Instant) -> Driver {
        let mut d = Driver::default();
        assert!(d.on_ready(t0).is_empty());
        d
    }

    #[test]
    fn driver_shows_once_and_hides_when_time_is_up() {
        let t0 = Instant::now();
        let mut d = ready_driver(t0);
        let c = d.on_status(&StatusEvent::Transcribing, t0);
        assert_eq!(
            c,
            vec![
                Command::Emit(payload(OverlayView::Transcribing, None)),
                Command::Show
            ]
        );
        let c = d.on_status(&done(), t0 + ms(10));
        assert_eq!(
            c,
            vec![Command::Emit(payload(
                OverlayView::Done { message: "完成" },
                Some(DONE_MS)
            ))]
        );
        assert_eq!(d.next_deadline(), Some(t0 + ms(10 + DONE_MS)));
        assert!(d.on_timeout(t0 + ms(DONE_MS)).is_empty());
        let c = d.on_timeout(t0 + ms(10 + DONE_MS));
        assert_eq!(
            c,
            vec![
                Command::Hide,
                Command::Emit(payload(OverlayView::Hidden, None))
            ]
        );
        assert_eq!(d.next_deadline(), None);
        // Cancel while hidden: no window command, just the hidden view.
        assert_eq!(
            d.on_status(&StatusEvent::Cancelled, t0 + ms(2_000)),
            vec![Command::Emit(payload(OverlayView::Hidden, None))]
        );
    }

    #[test]
    fn startup_error_before_ready_is_replayed_with_full_time() {
        let t0 = Instant::now();
        let mut d = Driver::default();
        let c = d.on_status(
            &StatusEvent::Error {
                code: "model_missing",
            },
            t0,
        );
        assert_eq!(c.last(), Some(&Command::Show));
        // Webview listens 1 s later: the error is re-sent and gets its full 3 s again.
        let c = d.on_ready(t0 + ms(1_000));
        let err = OverlayView::Error {
            message: "找不到語音模型，請到設定下載",
        };
        assert_eq!(c, vec![Command::Emit(payload(err.clone(), Some(ERROR_MS)))]);
        assert_eq!(d.next_deadline(), Some(t0 + ms(1_000 + ERROR_MS)));
        // A second ready (webview reload) re-sends only what is current.
        assert_eq!(d.on_ready(t0 + ms(1_500)).len(), 1);
    }

    #[test]
    fn startup_error_that_already_timed_out_is_shown_again_on_first_ready() {
        let t0 = Instant::now();
        let mut d = Driver::default();
        d.on_status(
            &StatusEvent::Error {
                code: "config_invalid",
            },
            t0,
        );
        assert_eq!(
            d.on_timeout(t0 + ms(ERROR_MS)).first(),
            Some(&Command::Hide)
        );
        let c = d.on_ready(t0 + ms(4_000));
        let err = OverlayView::Error {
            message: "設定檔格式錯誤，已使用預設值",
        };
        assert_eq!(
            c,
            vec![Command::Emit(payload(err, Some(ERROR_MS))), Command::Show]
        );
        // Only once.
        d.on_timeout(t0 + ms(4_000 + ERROR_MS));
        assert!(d.on_ready(t0 + ms(9_000)).is_empty());
    }

    #[test]
    fn untimed_view_before_ready_supersedes_a_missed_error() {
        let t0 = Instant::now();
        let mut d = Driver::default();
        d.on_status(&StatusEvent::Error { code: "busy" }, t0);
        d.on_status(&rec(0.0), t0 + ms(10));
        d.on_status(&StatusEvent::Cancelled, t0 + ms(20));
        assert!(d.on_ready(t0 + ms(30)).is_empty());
    }

    #[test]
    fn recording_beats_late_events_of_the_previous_utterance() {
        let t0 = Instant::now();
        let mut d = ready_driver(t0);
        // Utterance A is still being transcribed when B's recording starts.
        d.on_status(&StatusEvent::Transcribing, t0);
        let c = d.on_status(&rec(0.05), t0 + ms(100));
        assert!(matches!(emitted(&c)[..], [OverlayView::Recording { .. }]));
        // A finishes during B's recording: held, then dropped by B's next level event.
        assert!(d.on_status(&done(), t0 + ms(130)).is_empty());
        assert_eq!(d.next_deadline(), Some(t0 + ms(130 + RECORDING_GRACE_MS)));
        let c = d.on_status(&rec(0.05), t0 + ms(150));
        assert!(matches!(emitted(&c)[..], [OverlayView::Recording { .. }]));
        assert_eq!(d.next_deadline(), None);
        assert!(d.on_timeout(t0 + ms(1_000)).is_empty());
    }

    #[test]
    fn transcribing_after_recording_ends_applies_after_the_grace_period() {
        let t0 = Instant::now();
        let mut d = ready_driver(t0);
        d.on_status(&rec(0.05), t0);
        assert!(d
            .on_status(&StatusEvent::Transcribing, t0 + ms(20))
            .is_empty());
        let at = t0 + ms(20 + RECORDING_GRACE_MS);
        assert_eq!(d.next_deadline(), Some(at));
        assert!(d.on_timeout(at - ms(1)).is_empty());
        assert_eq!(emitted(&d.on_timeout(at)), vec![OverlayView::Transcribing]);
        // Once not recording, events apply immediately.
        assert_eq!(
            emitted(&d.on_status(&StatusEvent::Polishing, at + ms(5))),
            vec![OverlayView::Polishing]
        );
    }

    #[test]
    fn later_held_event_replaces_an_earlier_one() {
        let t0 = Instant::now();
        let mut d = ready_driver(t0);
        d.on_status(&rec(0.05), t0);
        d.on_status(&StatusEvent::Transcribing, t0 + ms(10));
        d.on_status(&done(), t0 + ms(50));
        let c = d.on_timeout(t0 + ms(50 + RECORDING_GRACE_MS));
        assert_eq!(emitted(&c), vec![OverlayView::Done { message: "完成" }]);
        assert_eq!(
            d.next_deadline(),
            Some(t0 + ms(50 + RECORDING_GRACE_MS + DONE_MS))
        );
    }

    #[test]
    fn errors_and_cancel_during_recording_are_not_held() {
        let t0 = Instant::now();
        let mut d = ready_driver(t0);
        d.on_status(&rec(0.05), t0);
        d.on_status(&StatusEvent::Transcribing, t0 + ms(10));
        let c = d.on_status(&StatusEvent::Error { code: "mic_lost" }, t0 + ms(20));
        assert!(matches!(emitted(&c)[..], [OverlayView::Error { .. }]));
        assert_eq!(d.next_deadline(), Some(t0 + ms(20 + ERROR_MS)));

        d.on_status(&rec(0.05), t0 + ms(100));
        d.on_status(&done(), t0 + ms(110));
        let c = d.on_status(&StatusEvent::Cancelled, t0 + ms(120));
        assert_eq!(
            c,
            vec![
                Command::Hide,
                Command::Emit(payload(OverlayView::Hidden, None))
            ]
        );
        assert_eq!(d.next_deadline(), None);
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
            view: recording_view(1.0, true),
            hide_after_ms: None,
        };
        assert_eq!(
            serde_json::to_value(&p).unwrap(),
            serde_json::json!({
                "view": { "kind": "recording", "level": 1.0, "hands_free": true },
                "hide_after_ms": null
            })
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
