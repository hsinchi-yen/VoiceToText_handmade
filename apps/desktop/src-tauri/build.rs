//! Tauri build script, plus staging of the SenseVoice runtime DLLs for the installer.
//!
//! `sherpa-onnx-sys` (shared build) downloads `sherpa-onnx-c-api.dll`, `onnxruntime.dll`, ... and
//! copies them into `target/<profile>/`, next to `v2c.exe`, which is all `cargo run` /
//! `tauri dev` need. The installer needs them next to the installed `v2c.exe` too: Windows 11
//! has an older `System32\onnxruntime.dll` (1.17) that is found before `PATH`, so the bundled
//! one must be in the application directory (searched first). We copy them to
//! `src-tauri/sherpa-dlls/` (git-ignored); `tauri.windows.conf.json` maps them as bundle
//! resources to the install root, which on Windows is the executable's directory.
//!
//! Ordering: the desktop crate depends on `sherpa-onnx-sys` directly (Cargo.toml). Because that
//! package declares `links`, Cargo runs its build script before this one.

use std::path::{Path, PathBuf};

/// The DLLs the sherpa-onnx 2.0.1 shared Windows archive ships.
const SHERPA_DLLS: [&str; 4] = [
    "sherpa-onnx-c-api.dll",
    "sherpa-onnx-cxx-api.dll",
    "onnxruntime.dll",
    "onnxruntime_providers_shared.dll",
];
const STAGE_DIR: &str = "sherpa-dlls";

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        stage_sherpa_dlls();
    }
    tauri_build::build()
}

fn stage_sherpa_dlls() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let stage = manifest.join(STAGE_DIR);
    let Some(profile_dir) = profile_dir() else {
        println!("cargo:warning=could not locate the Cargo profile directory; DLLs not staged");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&stage) {
        println!("cargo:warning=could not create {}: {e}", stage.display());
        return;
    }
    for dll in SHERPA_DLLS {
        let src = profile_dir.join(dll);
        let dst = stage.join(dll);
        println!("cargo:rerun-if-changed={}", src.display());
        if !src.is_file() {
            println!(
                "cargo:warning={dll} not found in {}; the installer would miss it",
                profile_dir.display()
            );
            continue;
        }
        if !same_size(&src, &dst) {
            if let Err(e) = std::fs::copy(&src, &dst) {
                println!("cargo:warning=could not stage {dll}: {e}");
            }
        }
    }
}

/// `target/<profile>` from `OUT_DIR` = `target/<profile>/build/<pkg>-<hash>/out` (the same rule
/// `sherpa-onnx-sys` uses to place the DLLs).
fn profile_dir() -> Option<PathBuf> {
    let out = PathBuf::from(std::env::var_os("OUT_DIR")?);
    out.ancestors().nth(3).map(Path::to_path_buf)
}

fn same_size(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.len() == y.len(),
        _ => false,
    }
}
