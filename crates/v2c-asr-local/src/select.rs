//! Chooses the local ASR engine (Breeze or TEA-ASR on the GPU, or SenseVoice on the CPU) from
//! config and GPU state.
//!
//! Pure logic, no I/O: the caller measures the GPU and fills [`GpuInfo`] (T1.10b reads it from
//! NVML via [`crate::gpu_monitor`]). Runtime unload / yield decisions live in
//! [`crate::gpu_guard`]. Rules (`tasks/plan.md` section 5 and 5.1):
//!
//! 0. The *GPU engine* of a config is TEA-ASR when `[local] asr = "tea_asr"`, otherwise Breeze
//!    ([`gpu_engine`]). [`GpuInfo`]'s and the guard's `breeze_*` fields describe that engine
//!    (the names predate TEA-ASR).
//! 1. `[local] asr = "sense_voice"` -> SenseVoice.
//! 2. GPU disallowed (`[gpu] enabled = false` or `profile = "cpu_only"`) -> SenseVoice, even when
//!    `asr = "breeze"` / `"tea_asr"`: both run only on the GPU (far too slow on CPU).
//! 3. The GPU engine's model file missing -> SenseVoice.
//! 4. No usable CUDA device -> SenseVoice.
//! 5. `asr = "breeze"` / `"tea_asr"` (explicit) -> that engine without the VRAM check (the user
//!    forced it).
//! 6. `asr = "auto"` -> Breeze only when `free_vram_mb >= vram_reserve_mb + breeze_vram_mb`;
//!    otherwise SenseVoice. `auto` never picks TEA-ASR.
//!
//! `low_vram` behaves like `balanced` here: both run the GPU engine (they differ in the LM
//! Studio model, which is not chosen here).

use v2c_core::config::{Config, GpuProfile, LocalAsr};

/// Local ASR engine to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalAsrKind {
    /// Breeze-ASR-25 via whisper.cpp on the GPU.
    Breeze,
    /// SenseVoice-Small via sherpa-onnx on the CPU.
    SenseVoice,
    /// TEA-ASR-1.1 (Qwen3-ASR fine-tune) via transcribe.cpp on the GPU.
    TeaAsr,
}

impl LocalAsrKind {
    /// Provider name used in logs (`asr="..."`).
    pub fn provider_name(self) -> &'static str {
        match self {
            LocalAsrKind::Breeze => crate::breeze::PROVIDER_NAME,
            LocalAsrKind::SenseVoice => crate::sensevoice::PROVIDER_NAME,
            LocalAsrKind::TeaAsr => crate::tea::PROVIDER_NAME,
        }
    }
}

/// The GPU engine this config runs: TEA-ASR when `asr = "tea_asr"`, otherwise Breeze (`auto`
/// and `breeze`; for `sense_voice` the answer is not used).
pub fn gpu_engine(config: &Config) -> LocalAsrKind {
    if config.local.asr == LocalAsr::TeaAsr {
        LocalAsrKind::TeaAsr
    } else {
        LocalAsrKind::Breeze
    }
}

/// GPU and model state at decision time. Plain data so it is easy to fill and to test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuInfo {
    /// A CUDA device is usable: the binary has the `cuda` feature and an NVIDIA driver/device
    /// was found.
    pub cuda_available: bool,
    /// Free VRAM on that device right now, in MiB (NVML `memory_info().free`).
    pub free_vram_mb: u64,
    /// Total VRAM, in MiB. Informational (settings UI); not used by [`pick`].
    pub total_vram_mb: u64,
    /// The model file of the GPU engine ([`gpu_engine`]) exists on disk: the Breeze GGML for
    /// the configured `breeze_quant`, or the TEA-ASR GGUF when `asr = "tea_asr"`.
    pub breeze_model_present: bool,
    /// VRAM the GPU engine needs once loaded, in MiB (weights + compute buffers). Use
    /// [`crate::gpu_guard::gpu_vram_estimate_mb`]; it is not a config value because it depends
    /// on the model file.
    pub breeze_vram_mb: u64,
}

/// Why [`pick_with_reason`] chose what it chose, for the one-time overlay hint and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickReason {
    /// `[local] asr` names this engine explicitly.
    Configured,
    /// `[gpu] enabled = false`.
    GpuDisabled,
    /// `[gpu] profile = "cpu_only"`.
    CpuOnlyProfile,
    /// The Breeze model file is missing.
    BreezeModelMissing,
    /// The TEA-ASR model file is missing (`asr = "tea_asr"`).
    TeaModelMissing,
    /// No usable CUDA device (or built without the `cuda` feature).
    NoCuda,
    /// Not enough free VRAM after keeping `vram_reserve_mb` free.
    LowVram { free_mb: u64, needed_mb: u64 },
    /// Auto mode and Breeze fits.
    BreezeFits,
}

/// The decision and its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pick {
    pub kind: LocalAsrKind,
    pub reason: PickReason,
}

/// Rules 1-4: whether config and environment allow the GPU engine at all, before looking at
/// VRAM. `gpu_model_present`: the [`gpu_engine`]'s model file exists. `Err` carries the reason
/// SenseVoice must be used. Shared with [`crate::gpu_guard`].
pub fn gpu_eligibility(
    config: &Config,
    cuda_available: bool,
    gpu_model_present: bool,
) -> Result<(), PickReason> {
    if config.local.asr == LocalAsr::SenseVoice {
        return Err(PickReason::Configured);
    }
    if !config.gpu.enabled {
        return Err(PickReason::GpuDisabled);
    }
    if config.gpu.profile == GpuProfile::CpuOnly {
        return Err(PickReason::CpuOnlyProfile);
    }
    if !gpu_model_present {
        return Err(match gpu_engine(config) {
            LocalAsrKind::TeaAsr => PickReason::TeaModelMissing,
            _ => PickReason::BreezeModelMissing,
        });
    }
    if !cuda_available {
        return Err(PickReason::NoCuda);
    }
    Ok(())
}

/// Local ASR engine for this config and GPU state. See the module docs for the rules.
pub fn pick(config: &Config, gpu: &GpuInfo) -> LocalAsrKind {
    pick_with_reason(config, gpu).kind
}

/// Like [`pick`], with the reason.
pub fn pick_with_reason(config: &Config, gpu: &GpuInfo) -> Pick {
    use LocalAsrKind::{Breeze, SenseVoice, TeaAsr};
    let sv = |reason| Pick {
        kind: SenseVoice,
        reason,
    };

    if let Err(reason) = gpu_eligibility(config, gpu.cuda_available, gpu.breeze_model_present) {
        return sv(reason);
    }
    let forced = match config.local.asr {
        LocalAsr::Breeze => Some(Breeze),
        LocalAsr::TeaAsr => Some(TeaAsr),
        LocalAsr::Auto | LocalAsr::SenseVoice => None,
    };
    if let Some(kind) = forced {
        return Pick {
            kind,
            reason: PickReason::Configured,
        };
    }
    // Auto.
    let needed_mb = config
        .gpu
        .vram_reserve_mb
        .saturating_add(gpu.breeze_vram_mb);
    if gpu.free_vram_mb < needed_mb {
        return sv(PickReason::LowVram {
            free_mb: gpu.free_vram_mb,
            needed_mb,
        });
    }
    Pick {
        kind: Breeze,
        reason: PickReason::BreezeFits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(asr: LocalAsr) -> Config {
        let mut c = Config::with_defaults();
        c.local.asr = asr;
        c
    }

    /// A healthy GPU: 4500 MiB free, Breeze needs 1500 MiB; default reserve is 600 MiB.
    fn good_gpu() -> GpuInfo {
        GpuInfo {
            cuda_available: true,
            free_vram_mb: 4500,
            total_vram_mb: 6144,
            breeze_model_present: true,
            breeze_vram_mb: 1500,
        }
    }

    fn p(c: &Config, g: &GpuInfo) -> (LocalAsrKind, PickReason) {
        let r = pick_with_reason(c, g);
        assert_eq!(pick(c, g), r.kind);
        (r.kind, r.reason)
    }

    #[test]
    fn auto_with_cuda_model_and_vram_picks_breeze() {
        assert_eq!(
            p(&cfg(LocalAsr::Auto), &good_gpu()),
            (LocalAsrKind::Breeze, PickReason::BreezeFits)
        );
    }

    #[test]
    fn default_config_is_auto() {
        assert_eq!(Config::with_defaults().local.asr, LocalAsr::Auto);
        assert_eq!(
            pick(&Config::with_defaults(), &GpuInfo::default()),
            LocalAsrKind::SenseVoice
        );
    }

    #[test]
    fn auto_without_cuda_picks_sensevoice() {
        let g = GpuInfo {
            cuda_available: false,
            ..good_gpu()
        };
        assert_eq!(
            p(&cfg(LocalAsr::Auto), &g),
            (LocalAsrKind::SenseVoice, PickReason::NoCuda)
        );
    }

    #[test]
    fn auto_without_breeze_model_picks_sensevoice() {
        let g = GpuInfo {
            breeze_model_present: false,
            ..good_gpu()
        };
        assert_eq!(
            p(&cfg(LocalAsr::Auto), &g),
            (LocalAsrKind::SenseVoice, PickReason::BreezeModelMissing)
        );
    }

    #[test]
    fn auto_vram_threshold_uses_reserve_from_config() {
        let mut c = cfg(LocalAsr::Auto);
        c.gpu.vram_reserve_mb = 2048;
        let mut g = good_gpu();
        // Exactly reserve + model fits.
        g.free_vram_mb = 2048 + 1500;
        assert_eq!(pick(&c, &g), LocalAsrKind::Breeze);
        // One MiB short does not.
        g.free_vram_mb = 2048 + 1499;
        assert_eq!(
            p(&c, &g),
            (
                LocalAsrKind::SenseVoice,
                PickReason::LowVram {
                    free_mb: 3547,
                    needed_mb: 3548
                }
            )
        );
        // A smaller reserve in config lets the same GPU run Breeze.
        c.gpu.vram_reserve_mb = 1024;
        assert_eq!(pick(&c, &g), LocalAsrKind::Breeze);
    }

    #[test]
    fn huge_reserve_does_not_overflow() {
        let mut c = cfg(LocalAsr::Auto);
        c.gpu.vram_reserve_mb = u64::MAX;
        assert_eq!(pick(&c, &good_gpu()), LocalAsrKind::SenseVoice);
    }

    #[test]
    fn cpu_only_profile_picks_sensevoice_even_when_breeze_requested() {
        for asr in [LocalAsr::Auto, LocalAsr::Breeze] {
            let mut c = cfg(asr);
            c.gpu.profile = GpuProfile::CpuOnly;
            assert_eq!(
                p(&c, &good_gpu()),
                (LocalAsrKind::SenseVoice, PickReason::CpuOnlyProfile),
                "{asr:?}"
            );
        }
    }

    #[test]
    fn gpu_disabled_picks_sensevoice_even_when_breeze_requested() {
        for asr in [LocalAsr::Auto, LocalAsr::Breeze] {
            let mut c = cfg(asr);
            c.gpu.enabled = false;
            assert_eq!(
                p(&c, &good_gpu()),
                (LocalAsrKind::SenseVoice, PickReason::GpuDisabled),
                "{asr:?}"
            );
        }
    }

    #[test]
    fn low_vram_profile_behaves_like_balanced() {
        let mut c = cfg(LocalAsr::Auto);
        c.gpu.profile = GpuProfile::LowVram;
        assert_eq!(pick(&c, &good_gpu()), LocalAsrKind::Breeze);
    }

    #[test]
    fn explicit_sensevoice_always_wins() {
        assert_eq!(
            p(&cfg(LocalAsr::SenseVoice), &good_gpu()),
            (LocalAsrKind::SenseVoice, PickReason::Configured)
        );
    }

    #[test]
    fn explicit_breeze_skips_vram_check_but_needs_cuda_and_the_model() {
        let starved = GpuInfo {
            free_vram_mb: 0,
            ..good_gpu()
        };
        assert_eq!(
            p(&cfg(LocalAsr::Breeze), &starved),
            (LocalAsrKind::Breeze, PickReason::Configured)
        );
        let no_cuda = GpuInfo {
            cuda_available: false,
            ..good_gpu()
        };
        assert_eq!(
            p(&cfg(LocalAsr::Breeze), &no_cuda),
            (LocalAsrKind::SenseVoice, PickReason::NoCuda)
        );
        let missing = GpuInfo {
            breeze_model_present: false,
            ..good_gpu()
        };
        assert_eq!(
            p(&cfg(LocalAsr::Breeze), &missing),
            (LocalAsrKind::SenseVoice, PickReason::BreezeModelMissing)
        );
    }

    #[test]
    fn gpu_engine_is_tea_only_when_selected() {
        assert_eq!(gpu_engine(&cfg(LocalAsr::Auto)), LocalAsrKind::Breeze);
        assert_eq!(gpu_engine(&cfg(LocalAsr::Breeze)), LocalAsrKind::Breeze);
        assert_eq!(gpu_engine(&cfg(LocalAsr::TeaAsr)), LocalAsrKind::TeaAsr);
    }

    #[test]
    fn explicit_tea_skips_vram_check() {
        let starved = GpuInfo {
            free_vram_mb: 0,
            ..good_gpu()
        };
        assert_eq!(
            p(&cfg(LocalAsr::TeaAsr), &starved),
            (LocalAsrKind::TeaAsr, PickReason::Configured)
        );
        assert_eq!(
            p(&cfg(LocalAsr::TeaAsr), &good_gpu()),
            (LocalAsrKind::TeaAsr, PickReason::Configured)
        );
    }

    #[test]
    fn tea_falls_back_to_sensevoice_with_a_reason() {
        let c = cfg(LocalAsr::TeaAsr);
        let no_cuda = GpuInfo {
            cuda_available: false,
            ..good_gpu()
        };
        assert_eq!(
            p(&c, &no_cuda),
            (LocalAsrKind::SenseVoice, PickReason::NoCuda)
        );
        let missing = GpuInfo {
            breeze_model_present: false,
            ..good_gpu()
        };
        assert_eq!(
            p(&c, &missing),
            (LocalAsrKind::SenseVoice, PickReason::TeaModelMissing)
        );
        let mut off = c.clone();
        off.gpu.enabled = false;
        assert_eq!(
            p(&off, &good_gpu()),
            (LocalAsrKind::SenseVoice, PickReason::GpuDisabled)
        );
        let mut cpu = c.clone();
        cpu.gpu.profile = GpuProfile::CpuOnly;
        assert_eq!(
            p(&cpu, &good_gpu()),
            (LocalAsrKind::SenseVoice, PickReason::CpuOnlyProfile)
        );
    }

    #[test]
    fn auto_never_picks_tea() {
        // Even with plenty of VRAM, auto means Breeze (or SenseVoice), never TEA-ASR.
        let g = GpuInfo {
            free_vram_mb: 100_000,
            ..good_gpu()
        };
        assert_eq!(pick(&cfg(LocalAsr::Auto), &g), LocalAsrKind::Breeze);
    }

    #[test]
    fn tea_asr_config_round_trips_and_old_files_still_load() {
        let c = Config::from_toml_str("[local]\nasr = \"tea_asr\"\n").unwrap();
        assert_eq!(c.local.asr, LocalAsr::TeaAsr);
        let back = Config::from_toml_str(&c.to_toml_string().unwrap()).unwrap();
        assert_eq!(back.local.asr, LocalAsr::TeaAsr);
        // A config.toml written before TEA-ASR existed.
        let old =
            Config::from_toml_str("[local]\nasr = \"breeze\"\nbreeze_quant = \"q8_0\"\n").unwrap();
        assert_eq!(old.local.asr, LocalAsr::Breeze);
        assert_eq!(Config::from_toml_str("").unwrap().local.asr, LocalAsr::Auto);
    }

    #[test]
    fn provider_names() {
        assert_eq!(LocalAsrKind::Breeze.provider_name(), "breeze");
        assert_eq!(LocalAsrKind::SenseVoice.provider_name(), "sensevoice");
        assert_eq!(LocalAsrKind::TeaAsr.provider_name(), "tea_asr");
    }
}
