//! Local ASR providers implementing `v2c_core::asr::AsrProvider`.
//!
//! - `breeze`: Breeze-ASR-25 GGML via whisper-rs + CUDA (T0.1)
//! - `sensevoice`: SenseVoice-Small on CPU via sherpa-onnx (T1.10)
//! - `tea`: TEA-ASR-1.1 (Qwen3-ASR fine-tune) GGUF via transcribe.cpp + CUDA (T1.12)
//! - `select`: picks Breeze / TEA-ASR or SenseVoice from config and GPU state (T1.10, T1.10b,
//!   T1.12)
//! - `gpu_guard`: VRAM reserve / yield / idle-unload decisions, pure (T1.10b)
//! - `gpu_monitor`: NVML VRAM reader; `nvml.dll` is loaded at runtime, so it works (as
//!   unavailable) on machines without NVIDIA (T1.10b)
//!
//! Cargo features (all off by default, so `cargo build --workspace` needs neither the CUDA
//! Toolkit nor a network download; release desktop builds enable all three):
//! - `cuda`: builds whisper.cpp (and transcribe.cpp, with `tea`) with CUDA.
//! - `sensevoice`: links the official sherpa-onnx Rust API (`sherpa-onnx` crate, shared build).
//!   Its build script downloads prebuilt sherpa-onnx + onnxruntime DLLs (or uses
//!   `SHERPA_ONNX_LIB_DIR`) and copies them next to the binaries in `target/<profile>/`; they must
//!   ship next to the executable. Without the feature, `SenseVoiceAsr::load` returns an error.
//! - `tea`: links transcribe.cpp (`transcribe-cpp` crate, `shared` build: `transcribe.dll` plus
//!   its own `ggml*.dll`, kept apart from whisper.cpp's statically linked ggml). The -sys build
//!   script copies the DLLs next to the binaries in `target/<profile>/`; they must ship next to
//!   the executable. Without the feature, `TeaAsr::load` returns an error.

pub mod breeze;
pub mod gpu_guard;
pub mod gpu_monitor;
pub mod select;
pub mod sensevoice;
pub mod tea;

pub use breeze::{BreezeAsr, BreezeOptions, BreezeOutput, DecodeLang, LanguageMode};
pub use gpu_guard::{
    breeze_vram_estimate_mb, gpu_vram_estimate_mb, ForegroundInfo, GpuGuard, GpuSnapshot,
    GuardAction, GuardInput, GuardReason, GuardTuning,
};
pub use gpu_monitor::GpuMonitor;
pub use select::{
    gpu_eligibility, gpu_engine, pick, pick_with_reason, GpuInfo, LocalAsrKind, Pick, PickReason,
};
pub use sensevoice::{SenseVoiceAsr, SenseVoiceLanguage, SenseVoiceOptions, SenseVoiceOutput};
pub use tea::{TeaAsr, TeaOptions, TeaOutput};
pub use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
