//! Local ASR providers implementing `v2c_core::asr::AsrProvider`.
//!
//! - `breeze`: Breeze-ASR-25 GGML via whisper-rs + CUDA (T0.1)
//! - `sensevoice`: SenseVoice-Small on CPU via sherpa-onnx (T1.10)
//! - `select`: picks Breeze or SenseVoice from config and GPU state (T1.10, T1.10b)
//! - `gpu_guard`: VRAM reserve / yield / idle-unload decisions, pure (T1.10b)
//! - `gpu_monitor`: NVML VRAM reader; `nvml.dll` is loaded at runtime, so it works (as
//!   unavailable) on machines without NVIDIA (T1.10b)
//!
//! Cargo features (both off by default, so `cargo build --workspace` needs neither the CUDA
//! Toolkit nor a network download; release desktop builds enable both):
//! - `cuda`: builds whisper.cpp with CUDA.
//! - `sensevoice`: links the official sherpa-onnx Rust API (`sherpa-onnx` crate, shared build).
//!   Its build script downloads prebuilt sherpa-onnx + onnxruntime DLLs (or uses
//!   `SHERPA_ONNX_LIB_DIR`) and copies them next to the binaries in `target/<profile>/`; they must
//!   ship next to the executable. Without the feature, `SenseVoiceAsr::load` returns an error.

pub mod breeze;
pub mod gpu_guard;
pub mod gpu_monitor;
pub mod select;
pub mod sensevoice;

pub use breeze::{BreezeAsr, BreezeOptions, BreezeOutput, DecodeLang, LanguageMode};
pub use gpu_guard::{
    breeze_vram_estimate_mb, ForegroundInfo, GpuGuard, GpuSnapshot, GuardAction, GuardInput,
    GuardReason, GuardTuning,
};
pub use gpu_monitor::GpuMonitor;
pub use select::{
    gpu_eligibility, pick, pick_with_reason, GpuInfo, LocalAsrKind, Pick, PickReason,
};
pub use sensevoice::{SenseVoiceAsr, SenseVoiceLanguage, SenseVoiceOptions, SenseVoiceOutput};
pub use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
