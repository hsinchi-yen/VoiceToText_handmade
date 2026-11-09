//! First-run onboarding (T1.11, `tasks/plan.md` sections 5.1 and 6.1): whether it is needed,
//! GPU detection and the model recommendation, the microphone level check, the documentation
//! links, and the restart that loads newly installed models.
//!
//! Local first: nothing here mentions or needs a cloud service. The microphone check opens its
//! own short-lived input stream and sends only levels (`v2c:mic-level`, RMS 0..1) to the main
//! window; audio is never stored. Every command refuses callers other than the main window.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tracing::{info, warn};
use v2c_asr_local::breeze_vram_estimate_mb;
use v2c_core::config::BreezeQuant;

use crate::app_state::AppState;
use crate::commands::{gpu, models};

const MAIN_WINDOW: &str = "main";
/// Main-window event: `{ rms: f32 }` about 20 times per second during the microphone check.
pub const MIC_LEVEL_EVENT: &str = "v2c:mic-level";
/// The microphone check stops by itself after this long.
const MIC_TEST_MAX: Duration = Duration::from_secs(30);
/// Documents that can be opened from the UI (bundled to `<install>\docs\`).
const DOCS: [&str; 2] = ["local-setup", "cloud-setup"];

static MIC_TEST_RUNNING: AtomicBool = AtomicBool::new(false);
static MIC_TEST_STOP: AtomicBool = AtomicBool::new(false);

fn check_caller(label: &str) -> Result<(), &'static str> {
    if label == MAIN_WINDOW {
        Ok(())
    } else {
        warn!(window = label, "onboarding command refused");
        Err("forbidden")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    /// NVIDIA GPU + CUDA build: Breeze-ASR-25 q5_0 on the GPU (SenseVoice as the CPU fallback).
    BreezeQ5_0,
    /// No usable NVIDIA GPU: SenseVoice-Small on the CPU only.
    SenseVoice,
}

/// Breeze needs an NVIDIA GPU, a CUDA build, and room for the model next to the display
/// (at least the q5_0 estimate plus 1 GB).
pub fn recommend(nvidia: bool, cuda_build: bool, total_vram_mb: u64) -> Recommendation {
    let room = breeze_vram_estimate_mb(BreezeQuant::Q5_0) + 1024;
    if nvidia && cuda_build && total_vram_mb >= room {
        Recommendation::BreezeQ5_0
    } else {
        Recommendation::SenseVoice
    }
}

#[derive(Debug, Serialize)]
pub struct OnboardingStatus {
    /// No local speech model is installed: show onboarding.
    pub needed: bool,
    pub nvidia: bool,
    pub gpu_name: Option<String>,
    pub total_vram_mb: u64,
    pub cuda_build: bool,
    pub recommended: Recommendation,
}

#[tauri::command]
pub async fn onboarding_status(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<OnboardingStatus, &'static str> {
    check_caller(window.label())?;
    let paths = state.paths.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let m = gpu::monitor();
        let snap = m.snapshot();
        let cuda_build = v2c_asr_local::breeze::cuda_enabled();
        let total = snap.map_or(0, |s| s.total_mb);
        OnboardingStatus {
            needed: !models::any_asr_model(&paths),
            nvidia: m.available(),
            gpu_name: m.device_name(),
            total_vram_mb: total,
            cuda_build,
            recommended: recommend(m.available(), cuda_build, total),
        }
    })
    .await
    .map_err(|_| "internal_error")
}

/// Marker file (data folder) that makes the next start show the main window even when it was
/// launched with `--autostart` (`app.restart()` keeps the original arguments).
const SHOW_FLAG_FILE: &str = "show-main-on-start";

pub fn set_show_flag(paths: &crate::paths::Paths) -> std::io::Result<()> {
    std::fs::create_dir_all(&paths.data_dir)?;
    std::fs::write(paths.data_dir.join(SHOW_FLAG_FILE), b"1")
}

/// Whether the marker was set; removes it (one start only).
pub fn take_show_flag(paths: &crate::paths::Paths) -> bool {
    std::fs::remove_file(paths.data_dir.join(SHOW_FLAG_FILE)).is_ok()
}

/// Show the main window at startup: a normal launch, the restart after onboarding, or no local
/// speech model yet (onboarding must appear even when started by the autostart entry).
pub fn show_main_at_start(autostart: bool, resumed: bool, has_model: bool) -> bool {
    !autostart || resumed || !has_model
}

/// Restarts v2c so the engine loads the models that were just installed; the main window
/// comes back visible (see [`show_main_at_start`]).
#[tauri::command]
pub fn onboarding_restart(app: AppHandle, window: WebviewWindow) -> Result<(), &'static str> {
    check_caller(window.label())?;
    if let Err(e) = set_show_flag(&app.state::<AppState>().paths) {
        warn!(error = %e, "could not write the show-on-start marker");
    }
    info!("restart after onboarding");
    app.restart()
}

/// Opens a bundled document (`local-setup` / `cloud-setup`) with the default program.
#[tauri::command]
pub fn open_doc(app: AppHandle, window: WebviewWindow, name: String) -> Result<(), &'static str> {
    check_caller(window.label())?;
    if !DOCS.contains(&name.as_str()) {
        return Err("not_found");
    }
    let file = format!("{name}.md");
    let mut candidates = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("docs").join(&file));
    }
    // `tauri dev`: the repository's docs folder.
    candidates.push(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs")
            .join(&file),
    );
    let path = candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or("not_found")?;
    #[cfg(windows)]
    let r = std::process::Command::new("explorer.exe")
        .arg(&path)
        .spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(&path).spawn();
    r.map(|_| ()).map_err(|_| "open_failed")
}

#[derive(Debug, Clone, Serialize)]
struct MicLevel {
    rms: f32,
    /// The check ended (timeout, stop, or device error).
    done: bool,
}

/// Starts the microphone check on `device` (`None` = system default). Levels arrive as
/// [`MIC_LEVEL_EVENT`]; it stops after 30 s or on [`mic_test_stop`].
#[tauri::command]
pub fn mic_test_start(
    app: AppHandle,
    window: WebviewWindow,
    device: Option<String>,
) -> Result<(), &'static str> {
    check_caller(window.label())?;
    if MIC_TEST_RUNNING.swap(true, Ordering::SeqCst) {
        return Err("already_running");
    }
    MIC_TEST_STOP.store(false, Ordering::SeqCst);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("v2c-mic-test".into())
        .spawn(move || {
            mic_test_thread(&app, device.as_deref(), &ready_tx);
            MIC_TEST_RUNNING.store(false, Ordering::SeqCst);
        });
    if spawned.is_err() {
        MIC_TEST_RUNNING.store(false, Ordering::SeqCst);
        return Err("internal_error");
    }
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or(Err("mic_open_failed"))
}

#[tauri::command]
pub fn mic_test_stop(window: WebviewWindow) -> Result<(), &'static str> {
    check_caller(window.label())?;
    MIC_TEST_STOP.store(true, Ordering::SeqCst);
    Ok(())
}

fn mic_test_thread(
    app: &AppHandle,
    device: Option<&str>,
    ready: &std::sync::mpsc::Sender<Result<(), &'static str>>,
) {
    let level = Arc::new(AtomicU32::new(0));
    let stream = match open_level_stream(device, Arc::clone(&level)) {
        Ok(s) => s,
        Err(code) => {
            warn!(code, "microphone check could not open the device");
            let _ = ready.send(Err(code));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    info!("microphone check started");
    let start = Instant::now();
    while !MIC_TEST_STOP.load(Ordering::SeqCst) && start.elapsed() < MIC_TEST_MAX {
        std::thread::sleep(Duration::from_millis(50));
        let rms = f32::from_bits(level.load(Ordering::Relaxed));
        let _ = app.emit_to(MAIN_WINDOW, MIC_LEVEL_EVENT, MicLevel { rms, done: false });
    }
    drop(stream);
    let _ = app.emit_to(
        MAIN_WINDOW,
        MIC_LEVEL_EVENT,
        MicLevel {
            rms: 0.0,
            done: true,
        },
    );
    info!("microphone check stopped");
}

/// RMS of one callback buffer (any channel layout; only the level matters).
pub fn rms(samples: impl Iterator<Item = f32>) -> f32 {
    let (sum, n) = samples.fold((0.0f64, 0usize), |(s, n), x| (s + f64::from(x * x), n + 1));
    if n == 0 {
        0.0
    } else {
        (sum / n as f64).sqrt() as f32
    }
}

fn open_level_stream(
    device: Option<&str>,
    level: Arc<AtomicU32>,
) -> Result<cpal::Stream, &'static str> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let dev = match device {
        Some(name) => host
            .input_devices()
            .ok()
            .and_then(|mut it| {
                it.find(|d| d.description().map(|x| x.name() == name).unwrap_or(false))
            })
            .or_else(|| host.default_input_device()),
        None => host.default_input_device(),
    }
    .ok_or("mic_not_found")?;
    let supported = dev.default_input_config().map_err(|_| "mic_open_failed")?;
    let config: cpal::StreamConfig = supported.config();
    let err_fn = |_e: cpal::Error| {};
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => dev.build_input_stream::<f32, _, _>(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                level.store(rms(data.iter().copied()).to_bits(), Ordering::Relaxed);
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => dev.build_input_stream::<i16, _, _>(
            config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let it = data.iter().map(|s| f32::from(*s) / 32768.0);
                level.store(rms(it).to_bits(), Ordering::Relaxed);
            },
            err_fn,
            None,
        ),
        _ => return Err("mic_format_unsupported"),
    }
    .map_err(|_| "mic_open_failed")?;
    stream.play().map_err(|_| "mic_open_failed")?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommendation_follows_the_gpu() {
        assert_eq!(recommend(true, true, 6144), Recommendation::BreezeQ5_0);
        assert_eq!(recommend(false, true, 0), Recommendation::SenseVoice);
        // CPU-only build: Breeze would run on the CPU, too slow.
        assert_eq!(recommend(true, false, 6144), Recommendation::SenseVoice);
        // A 2 GB card has no room next to the display.
        assert_eq!(recommend(true, true, 2048), Recommendation::SenseVoice);
    }

    #[test]
    fn main_window_visibility_at_start() {
        assert!(show_main_at_start(false, false, true));
        // Autostart with a model: stay in the tray.
        assert!(!show_main_at_start(true, false, true));
        // Restart after onboarding keeps `--autostart` but must show the window.
        assert!(show_main_at_start(true, true, true));
        // No model yet: onboarding has to appear.
        assert!(show_main_at_start(true, false, false));
    }

    #[test]
    fn show_flag_is_taken_once() {
        let dir = std::env::temp_dir().join(format!("v2c-showflag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = crate::paths::Paths {
            data_dir: dir.clone(),
            local_data_dir: dir.clone(),
        };
        assert!(!take_show_flag(&paths));
        set_show_flag(&paths).unwrap();
        assert!(take_show_flag(&paths));
        assert!(!take_show_flag(&paths));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rms_levels() {
        assert_eq!(rms(std::iter::empty()), 0.0);
        assert!((rms([0.5f32, -0.5].into_iter()) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn only_known_docs_and_the_main_window() {
        assert!(DOCS.contains(&"local-setup"));
        assert!(!DOCS.iter().any(|d| d.contains(['.', '/', '\\'])));
        assert_eq!(check_caller("overlay"), Err("forbidden"));
        assert_eq!(check_caller("main"), Ok(()));
    }
}
