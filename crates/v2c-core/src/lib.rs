//! v2c core: the speech-to-text pipeline, provider interfaces, and pure decision logic.
//!
//! This crate must stay free of OS-specific dependencies so it can be compiled for
//! Android through `v2c-ffi`. OS integration lives in `v2c-platform`.
//!
//! Privacy rule: transcribed text, polished text, and vocabulary must never reach logs.
//! Types that carry user text implement `Debug` with the text redacted.

pub mod asr;
pub mod audio;
pub mod config;
pub mod error;
pub mod history;
pub mod inject_plan;
pub mod llm;
pub mod pipeline;
pub mod postprocess;
pub mod prompts;
pub mod redact;
pub mod trigger;
pub mod vocab;

pub use error::{Result, V2cError};

/// What the user asked for when they pressed the hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Speak Chinese and/or English; output the same language(s).
    Dictate,
    /// Speak Chinese (possibly with English words); output English.
    TranslateZhToEn,
}

/// Detected spoken language of a transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Zh,
    En,
    Mixed,
}
