//! What the app does with a `GuardAction` from the VRAM guard (T1.10b, `tasks/plan.md` 5.1),
//! with one local model in memory at a time (T1.12, plan section 5). Pure; the polling loop that
//! feeds it lives in `engine.rs`, and the slot that enforces "one model" in `local_asr.rs`.

use v2c_asr_local::{GuardAction, GuardReason, LocalAsrKind, PickReason};

/// Status code shown once when the GPU model yields to a heavy app (`[gpu] heavy_apps`).
pub const GPU_YIELD_HEAVY_APP: &str = "gpu_yield_heavy_app";
/// Shown when the GPU model is back after one of the notices above.
pub const GPU_RESTORED: &str = "gpu_restored";
/// ... because free VRAM is below `vram_reserve_mb` (+ the model's need when loading).
pub const GPU_LOW_VRAM: &str = "gpu_low_vram";
/// Info shown while an utterance waits for a model switch to finish.
pub const ASR_SWITCHING: &str = "asr_switching";

/// What the single model slot should do after a guard poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Leave the slot as it is.
    Keep,
    /// Switch to this model (the old one is unloaded first).
    Load(LocalAsrKind),
    /// Unload the model and leave the slot empty (idle; reloaded on the next hotkey press).
    Unload,
}

/// Turns a guard action into a change of the single model slot.
///
/// - `current`: the loaded model. `gpu`: the config's GPU engine (`select::gpu_engine`). The
///   guard was asked with `breeze_loaded = (current == Some(gpu))`.
/// - `has_cpu_model`: the SenseVoice model exists, so it can stand in. Without it, a runtime
///   yield (heavy app, fullscreen, low VRAM) keeps the GPU model rather than leave nothing to
///   dictate with.
/// - `gpu_blocked`: a GPU load failed recently; SenseVoice stands in meanwhile.
///
/// Rules: `Load` -> the GPU model. `UseCpu` (GPU not allowed now) -> SenseVoice as the stand-in.
/// `Unload(Idle)` -> empty slot. `Unload(other)` -> SenseVoice. A loaded GPU model of the wrong
/// kind (the config switched Breeze <-> TEA-ASR) is replaced, or unloaded when the guard would
/// not load the right one now (idle / on demand).
pub fn target(
    action: GuardAction,
    current: Option<LocalAsrKind>,
    gpu: LocalAsrKind,
    has_cpu_model: bool,
    gpu_blocked: bool,
) -> Target {
    use LocalAsrKind::SenseVoice;
    let stand_in = || {
        if current == Some(SenseVoice) {
            Target::Keep
        } else if has_cpu_model {
            Target::Load(SenseVoice)
        } else {
            Target::Keep
        }
    };
    let wrong_gpu = current.is_some_and(|k| k != SenseVoice && k != gpu);
    match action {
        GuardAction::Load if gpu_blocked => stand_in(),
        GuardAction::Load => Target::Load(gpu),
        GuardAction::UseCpu(_) => {
            if wrong_gpu && !has_cpu_model {
                Target::Unload
            } else {
                stand_in()
            }
        }
        GuardAction::Keep if wrong_gpu => Target::Unload,
        GuardAction::Keep => Target::Keep,
        GuardAction::Unload(GuardReason::Idle) => Target::Unload,
        GuardAction::Unload(reason) => {
            if has_cpu_model {
                Target::Load(SenseVoice)
            } else if matches!(reason, GuardReason::Engine(_)) {
                // Config / environment rules the GPU model out: nothing else to load.
                Target::Unload
            } else {
                Target::Keep
            }
        }
    }
}

/// Status code for a reason the user should hear about, if any. Config reasons (no model,
/// GPU disabled, ...), idle, unknown VRAM and restore-pending are silent (logged only).
pub fn reason_code(reason: GuardReason) -> Option<&'static str> {
    match reason {
        // A fullscreen game / video counts as a heavy app for the user.
        GuardReason::HeavyApp | GuardReason::Fullscreen => Some(GPU_YIELD_HEAVY_APP),
        GuardReason::LowVram { .. } | GuardReason::Engine(PickReason::LowVram { .. }) => {
            Some(GPU_LOW_VRAM)
        }
        _ => None,
    }
}

/// Shows each yield reason once until Breeze is back on the GPU.
#[derive(Debug, Default)]
pub struct NoticeLatch {
    last: Option<&'static str>,
}

impl NoticeLatch {
    /// The code to show for this action, if it is new.
    pub fn update(&mut self, action: GuardAction) -> Option<&'static str> {
        let reason = match action {
            GuardAction::Unload(r) | GuardAction::UseCpu(r) => r,
            GuardAction::Keep | GuardAction::Load => return None,
        };
        self.offer(reason_code(reason)?)
    }

    /// Shows `code` unless it was the last one shown.
    pub fn offer(&mut self, code: &'static str) -> Option<&'static str> {
        (self.last != Some(code)).then(|| {
            self.last = Some(code);
            code
        })
    }

    /// Breeze is loaded again: the next yield is announced again. Returns whether a yield
    /// had been announced (then the user should hear it is restored).
    pub fn reset(&mut self) -> bool {
        self.last.take().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOW: GuardReason = GuardReason::LowVram {
        free_mb: 100,
        needed_mb: 2048,
    };

    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use v2c_asr_local::{ForegroundInfo, GpuGuard, GpuSnapshot, GuardInput};
    use v2c_core::config::{Config, LocalAsr};

    use crate::local_asr::tests::{Census, FakeAsr};
    use crate::local_asr::LocalEngines;

    use LocalAsrKind::{Breeze, SenseVoice, TeaAsr};

    #[test]
    fn targets_follow_actions() {
        let t = |a, cur, cpu, blocked| target(a, cur, Breeze, cpu, blocked);
        assert_eq!(
            t(GuardAction::Keep, Some(Breeze), true, false),
            Target::Keep
        );
        assert_eq!(t(GuardAction::Keep, None, true, false), Target::Keep);
        assert_eq!(
            t(GuardAction::Load, Some(SenseVoice), true, false),
            Target::Load(Breeze)
        );
        assert_eq!(
            t(GuardAction::Load, None, true, false),
            Target::Load(Breeze)
        );
        // A failed GPU load: SenseVoice stands in.
        assert_eq!(
            t(GuardAction::Load, None, true, true),
            Target::Load(SenseVoice)
        );
        assert_eq!(
            t(GuardAction::Load, Some(SenseVoice), true, true),
            Target::Keep
        );
        // Not allowed on the GPU now: SenseVoice, loaded once.
        assert_eq!(
            t(
                GuardAction::UseCpu(GuardReason::HeavyApp),
                None,
                true,
                false
            ),
            Target::Load(SenseVoice)
        );
        assert_eq!(
            t(
                GuardAction::UseCpu(GuardReason::HeavyApp),
                Some(SenseVoice),
                true,
                false
            ),
            Target::Keep
        );
        // Yield / config reasons replace the GPU model with SenseVoice; idle empties the slot.
        for r in [
            GuardReason::HeavyApp,
            GuardReason::Fullscreen,
            LOW,
            GuardReason::Engine(PickReason::Configured),
        ] {
            assert_eq!(
                t(GuardAction::Unload(r), Some(Breeze), true, false),
                Target::Load(SenseVoice),
                "{r:?}"
            );
        }
        assert_eq!(
            t(
                GuardAction::Unload(GuardReason::Idle),
                Some(Breeze),
                true,
                false
            ),
            Target::Unload
        );
    }

    #[test]
    fn without_sensevoice_runtime_yields_keep_the_gpu_model() {
        for r in [GuardReason::HeavyApp, GuardReason::Fullscreen, LOW] {
            assert_eq!(
                target(GuardAction::Unload(r), Some(Breeze), Breeze, false, false),
                Target::Keep
            );
        }
        for r in [
            GuardReason::Idle,
            GuardReason::Engine(PickReason::CpuOnlyProfile),
        ] {
            assert_eq!(
                target(GuardAction::Unload(r), Some(Breeze), Breeze, false, false),
                Target::Unload
            );
        }
        assert_eq!(
            target(GuardAction::UseCpu(LOW), None, Breeze, false, false),
            Target::Keep
        );
    }

    #[test]
    fn a_gpu_model_of_the_wrong_kind_is_replaced_or_unloaded() {
        // Config switched Breeze -> TEA-ASR while Breeze is loaded (guard sees "not loaded").
        assert_eq!(
            target(GuardAction::Load, Some(Breeze), TeaAsr, true, false),
            Target::Load(TeaAsr)
        );
        assert_eq!(
            target(GuardAction::Keep, Some(Breeze), TeaAsr, true, false),
            Target::Unload
        );
        assert_eq!(
            target(GuardAction::UseCpu(LOW), Some(TeaAsr), Breeze, true, false),
            Target::Load(SenseVoice)
        );
        assert_eq!(
            target(GuardAction::UseCpu(LOW), Some(TeaAsr), Breeze, false, false),
            Target::Unload
        );
    }

    /// Drives the real guard and the real single slot through yield -> stand-in -> recover
    /// sequences for both GPU engines; at no point may two models be alive.
    #[test]
    fn yield_stand_in_and_recover_keep_one_model_at_a_time() {
        for (asr, gpu) in [(LocalAsr::Breeze, Breeze), (LocalAsr::TeaAsr, TeaAsr)] {
            let census = Arc::new(Census::default());
            let slot = LocalEngines::default();
            let mut guard = GpuGuard::default();
            let mut cfg = Config::with_defaults();
            cfg.local.asr = asr;
            let t0 = Instant::now();
            let mut step = |secs: u64, fg: Option<ForegroundInfo<'static>>| {
                let input = GuardInput {
                    gpu: Some(GpuSnapshot {
                        free_mb: 5000,
                        total_mb: 6144,
                        used_by_us_mb: None,
                    }),
                    cuda_available: true,
                    breeze_model_present: true,
                    breeze_vram_mb: 1500,
                    foreground: fg,
                    breeze_loaded: slot.current() == Some(gpu),
                    busy: false,
                    idle: Duration::ZERO,
                    now: t0 + Duration::from_secs(secs),
                };
                let action = guard.decide(&cfg, &input);
                match target(action, slot.current(), gpu, true, false) {
                    Target::Keep => {}
                    Target::Load(kind) => slot
                        .switch_model(Some(kind), false, Duration::ZERO, || FakeAsr::load(&census))
                        .unwrap(),
                    Target::Unload => slot
                        .switch_model(None, false, Duration::ZERO, || unreachable!())
                        .unwrap(),
                }
                assert!(census.live() <= 1);
                slot.current()
            };
            let game = Some(ForegroundInfo {
                exe: "game.exe",
                fullscreen: true,
            });
            assert_eq!(
                step(0, None),
                Some(gpu),
                "{asr:?}: startup loads the GPU model"
            );
            assert_eq!(
                step(5, game),
                Some(SenseVoice),
                "{asr:?}: yield -> stand-in"
            );
            assert_eq!(step(10, game), Some(SenseVoice));
            // Recover only after the restore delay (20 s clear).
            assert_eq!(step(15, None), Some(SenseVoice));
            assert_eq!(step(40, None), Some(gpu), "{asr:?}: recovered");
            assert_eq!(census.max(), 1, "{asr:?}");
            assert_eq!(census.live(), 1);
        }
    }

    #[test]
    fn each_reason_is_announced_once_until_reloaded() {
        let mut l = NoticeLatch::default();
        assert_eq!(
            l.update(GuardAction::Unload(GuardReason::HeavyApp)),
            Some(GPU_YIELD_HEAVY_APP)
        );
        assert_eq!(l.update(GuardAction::UseCpu(GuardReason::HeavyApp)), None);
        assert_eq!(
            l.update(GuardAction::UseCpu(GuardReason::RestorePending)),
            None
        );
        assert_eq!(l.update(GuardAction::Keep), None);
        assert_eq!(l.update(GuardAction::UseCpu(GuardReason::Fullscreen)), None);
        assert_eq!(l.update(GuardAction::UseCpu(LOW)), Some(GPU_LOW_VRAM));
        assert_eq!(l.update(GuardAction::UseCpu(LOW)), None);
        assert!(l.reset());
        assert!(!l.reset());
        assert_eq!(l.update(GuardAction::Unload(LOW)), Some(GPU_LOW_VRAM));
        // Silent reasons.
        let mut l = NoticeLatch::default();
        for r in [
            GuardReason::Idle,
            GuardReason::VramUnknown,
            GuardReason::Engine(PickReason::BreezeModelMissing),
        ] {
            assert_eq!(l.update(GuardAction::UseCpu(r)), None);
        }
        assert_eq!(
            reason_code(GuardReason::Engine(PickReason::LowVram {
                free_mb: 1,
                needed_mb: 2
            })),
            Some(GPU_LOW_VRAM)
        );
    }
}
