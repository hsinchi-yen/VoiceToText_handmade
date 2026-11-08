//! Local ASR providers implementing `v2c_core::asr::AsrProvider`.
//!
//! - `breeze`: Breeze-ASR-25 GGML via whisper-rs + CUDA (T0.1)
//! - `sensevoice`: SenseVoice-Small on CPU (T1.10)
//! - `select`: picks Breeze or SenseVoice from config and GPU state (T1.10, T1.10b)
//! - `gpu_guard`: VRAM reserve / yield decisions (T1.10b)
//!
//! Cargo feature `cuda` (off by default) builds whisper.cpp with CUDA. It is off by default so
//! `cargo build --workspace` works without the CUDA Toolkit; release desktop builds enable it.

pub mod breeze;

pub use breeze::{BreezeAsr, BreezeOptions, BreezeOutput, DecodeLang, LanguageMode};
pub use v2c_core::asr::{AsrContext, AsrProvider, Transcript};
