//! Tauri build script: the command ACL (app manifest), staging of the runtime DLLs the
//! installer ships next to `v2c.exe`, and (Linux) the rpath for the sherpa-onnx `.so` files.
//!
//! ## Command ACL
//! Without an app manifest Tauri v2 lets every webview call every app command. Declaring the
//! commands here (`AppManifest::commands`) generates `allow-<command>` permissions, and only
//! the capabilities that grant them can call them: `capabilities/default.json` grants them to
//! the `main` window; the overlay capability grants none (events only). The commands keep
//! their own main-window caller checks as a second layer. Every command registered in
//! `main.rs` must be listed in [`COMMANDS`], otherwise the UI cannot call it.
//!
//! ## Runtime DLLs (`runtime-dlls/`, git-ignored; `tauri.windows.conf.json` bundles them to
//! the install root, which on Windows is the executable's directory)
//! - SenseVoice: `sherpa-onnx-sys` (shared build) downloads `sherpa-onnx-c-api.dll`,
//!   `onnxruntime.dll`, ... into `target/<profile>/`. Windows 11 has an older
//!   `System32\onnxruntime.dll` (1.17) that is found before `PATH`, so the bundled one must be
//!   in the application directory (searched first). The desktop crate depends on
//!   `sherpa-onnx-sys` directly (Cargo.toml); because it declares `links`, Cargo runs its
//!   build script before this one.
//! - Visual C++ runtime: whisper.cpp (C++) imports `MSVCP140.dll`, which a clean Windows
//!   (e.g. Windows Sandbox) does not have. `msvcp140.dll`, `vcruntime140.dll`,
//!   `vcruntime140_1.dll` are copied from the Visual Studio `VC\Redist\MSVC\<ver>\x64\
//!   Microsoft.VC143.CRT` folder (app-local deployment; distributable code per the Visual
//!   Studio 2022 REDIST list).
//! - CUDA (feature `cuda` only): whisper.cpp links the CUDA runtime statically
//!   (`cudart_static`) and imports `cublas64_13.dll`, which imports `cublasLt64_13.dll`.
//!   Both are redistributable per the CUDA Toolkit EULA, Attachment A ("CUDA BLAS Library").
//!   They are copied from `%CUDA_PATH%\bin\x64` (CUDA 13) or `%CUDA_PATH%\bin`. Neither needs
//!   the NVIDIA driver to load, so the app starts on machines without an NVIDIA GPU; there the
//!   GPU is never used (NVML unavailable -> SenseVoice). Without the feature they are removed
//!   from the stage folder so a CPU installer does not carry them.
//! - TEA-ASR (feature `cuda`, T1.12): transcribe.cpp is built as a shared library, so
//!   `transcribe.dll` and its own `ggml*.dll` (kept apart from whisper.cpp's statically linked
//!   ggml) are copied from `DEP_TRANSCRIBE_RUNTIME_DIR` (set by `transcribe-cpp-sys`, a direct
//!   dependency for that reason). `ggml-cuda.dll` links the CUDA runtime statically and imports
//!   only the cuBLAS DLLs above. Without the feature they are removed from the stage folder.

use std::path::{Path, PathBuf};

/// Every `#[tauri::command]` registered in `main.rs`.
const COMMANDS: &[&str] = &[
    "app_info",
    "last_status",
    "history_list",
    "history_copy",
    "history_reoutput",
    "history_delete",
    "history_clear",
    "cloud_kinds",
    "cloud_list",
    "cloud_models",
    "cloud_test_connection",
    "cloud_save",
    "cloud_remove",
    "cloud_open_key_page",
    "vocab_get",
    "vocab_set",
    "settings_get",
    "settings_apply",
    "settings_set_autostart",
    "settings_input_devices",
    "settings_preview",
    "settings_runtime_status",
    "settings_test_local_asr",
    "gpu_usage",
    "models_status",
    "models_download",
    "models_cancel",
    "models_import_breeze",
    "onboarding_status",
    "onboarding_restart",
    "open_doc",
    "mic_test_start",
    "mic_test_stop",
];

/// The DLLs the sherpa-onnx 2.0.1 shared Windows archive ships.
const SHERPA_DLLS: [&str; 4] = [
    "sherpa-onnx-c-api.dll",
    "sherpa-onnx-cxx-api.dll",
    "onnxruntime.dll",
    "onnxruntime_providers_shared.dll",
];
const VC_DLLS: [&str; 3] = ["msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll"];
/// File name prefixes of the cuBLAS DLLs (`cublas64_13.dll`, `cublasLt64_13.dll`), imported by
/// whisper.cpp and by transcribe.cpp's `ggml-cuda.dll`.
const CUDA_DLL_PREFIXES: [&str; 2] = ["cublas64_", "cublasLt64_"];
/// File name prefixes of the transcribe.cpp runtime DLLs (`transcribe.dll`, `ggml*.dll`).
const TEA_DLL_PREFIXES: [&str; 2] = ["transcribe", "ggml"];
const STAGE_DIR: &str = "runtime-dlls";

fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => stage_runtime_dlls(),
        Ok("linux") => linux_rpath(),
        _ => {}
    }
    let attrs = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    tauri_build::try_build(attrs).expect("tauri-build failed");
}

/// Linux: SenseVoice links `libsherpa-onnx-c-api.so` / `libonnxruntime.so` dynamically;
/// `sherpa-onnx-sys` copies them to `target/<profile>/` next to the `v2c` binary. Without an
/// rpath only `cargo run` / `cargo test` (which set `LD_LIBRARY_PATH`) can start it.
/// `$ORIGIN`: next to the binary (dev builds, AppImage); `$ORIGIN/../lib/v2c`: where the
/// .deb (T3.7) is expected to install them relative to `/usr/bin/v2c`.
fn linux_rpath() {
    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/v2c");
}

fn warn(msg: impl std::fmt::Display) {
    println!("cargo:warning={msg}");
}

fn stage_runtime_dlls() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let stage = manifest.join(STAGE_DIR);
    if let Err(e) = std::fs::create_dir_all(&stage) {
        warn(format_args!("could not create {}: {e}", stage.display()));
        return;
    }
    // Old stage folder name (before T1.11).
    let _ = std::fs::remove_dir_all(manifest.join("sherpa-dlls"));

    match profile_dir() {
        Some(dir) => {
            for dll in SHERPA_DLLS {
                stage_file(&dir.join(dll), &stage);
            }
        }
        None => warn("could not locate the Cargo profile directory; SenseVoice DLLs not staged"),
    }

    match vc_redist_dir() {
        Some(dir) => {
            for dll in VC_DLLS {
                stage_file(&dir.join(dll), &stage);
            }
        }
        None => warn("Visual C++ redist folder not found; the installer would need the VC++ runtime installed"),
    }

    let cuda = std::env::var_os("CARGO_FEATURE_CUDA").is_some();
    stage_tea_dlls(cuda, &stage);
    println!("cargo:rerun-if-env-changed=CUDA_PATH");
    if cuda {
        let found = cuda_bin_dir()
            .map(|bin| {
                let mut n = 0;
                for f in std::fs::read_dir(&bin).into_iter().flatten().flatten() {
                    if is_cuda_dll(&f.file_name().to_string_lossy()) {
                        stage_file(&f.path(), &stage);
                        n += 1;
                    }
                }
                n
            })
            .unwrap_or(0);
        if found < CUDA_DLL_PREFIXES.len() {
            warn("cuBLAS DLLs not found under %CUDA_PATH%; a CUDA installer would miss them");
        }
    } else {
        for f in std::fs::read_dir(&stage).into_iter().flatten().flatten() {
            if is_cuda_dll(&f.file_name().to_string_lossy()) {
                let _ = std::fs::remove_file(f.path());
            }
        }
    }
}

/// transcribe.cpp's runtime DLLs: staged with `cuda`, removed from the stage folder otherwise.
fn stage_tea_dlls(cuda: bool, stage: &Path) {
    println!("cargo:rerun-if-env-changed=DEP_TRANSCRIBE_RUNTIME_DIR");
    if !cuda {
        for f in std::fs::read_dir(stage).into_iter().flatten().flatten() {
            if is_tea_dll(&f.file_name().to_string_lossy()) {
                let _ = std::fs::remove_file(f.path());
            }
        }
        return;
    }
    let Some(dir) = std::env::var_os("DEP_TRANSCRIBE_RUNTIME_DIR").map(PathBuf::from) else {
        warn("DEP_TRANSCRIBE_RUNTIME_DIR not set; TEA-ASR DLLs not staged");
        return;
    };
    let mut n = 0;
    for f in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        if is_tea_dll(&f.file_name().to_string_lossy()) {
            stage_file(&f.path(), stage);
            n += 1;
        }
    }
    if n == 0 {
        warn(format_args!(
            "no transcribe.cpp DLLs in {}; a CUDA installer would miss TEA-ASR",
            dir.display()
        ));
    }
}

fn is_tea_dll(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".dll") && TEA_DLL_PREFIXES.iter().any(|p| lower.starts_with(p))
}

fn is_cuda_dll(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".dll")
        && CUDA_DLL_PREFIXES
            .iter()
            .any(|p| lower.starts_with(&p.to_ascii_lowercase()))
}

fn stage_file(src: &Path, stage: &Path) {
    let Some(name) = src.file_name() else { return };
    println!("cargo:rerun-if-changed={}", src.display());
    if !src.is_file() {
        warn(format_args!(
            "{} not found; the installer would miss it",
            src.display()
        ));
        return;
    }
    let dst = stage.join(name);
    if !same_size(src, &dst) {
        if let Err(e) = std::fs::copy(src, &dst) {
            warn(format_args!("could not stage {}: {e}", src.display()));
        }
    }
}

/// `target/<profile>` from `OUT_DIR` = `target/<profile>/build/<pkg>-<hash>/out` (the same rule
/// `sherpa-onnx-sys` uses to place the DLLs).
fn profile_dir() -> Option<PathBuf> {
    let out = PathBuf::from(std::env::var_os("OUT_DIR")?);
    out.ancestors().nth(3).map(Path::to_path_buf)
}

/// `%CUDA_PATH%\bin\x64` (CUDA 13 layout) or `%CUDA_PATH%\bin`.
fn cuda_bin_dir() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("CUDA_PATH")?);
    [root.join("bin").join("x64"), root.join("bin")]
        .into_iter()
        .find(|d| has_cublas(d))
}

fn has_cublas(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|f| is_cuda_dll(&f.file_name().to_string_lossy()))
}

/// The newest `Microsoft.VC14x.CRT` x64 folder: `%VCToolsRedistDir%` (developer prompt) or the
/// Visual Studio 2022 install folders.
fn vc_redist_dir() -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed=VCToolsRedistDir");
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("VCToolsRedistDir") {
        roots.push(PathBuf::from(d));
    }
    for pf in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Some(base) = std::env::var_os(pf) else {
            continue;
        };
        let vs = PathBuf::from(base)
            .join("Microsoft Visual Studio")
            .join("2022");
        for edition in std::fs::read_dir(&vs).into_iter().flatten().flatten() {
            let msvc = edition.path().join("VC").join("Redist").join("MSVC");
            let mut versions: Vec<PathBuf> = std::fs::read_dir(&msvc)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            versions.sort();
            roots.extend(versions.into_iter().rev());
        }
    }
    roots.into_iter().find_map(|root| {
        let x64 = root.join("x64");
        std::fs::read_dir(&x64)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".CRT"))
                    && p.join("msvcp140.dll").is_file()
            })
    })
}

fn same_size(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.len() == y.len(),
        _ => false,
    }
}
