//! Local ASR providers implementing `v2c_core::asr::AsrProvider`.
//!
//! - `breeze`: Breeze-ASR-25 GGML via whisper-rs + CUDA (T0.1)
//! - `sensevoice`: SenseVoice-Small on CPU (T1.10)
//! - `select`: picks Breeze or SenseVoice from config and GPU state (T1.10, T1.10b)
//! - `gpu_guard`: VRAM reserve / yield decisions (T1.10b)

pub use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
