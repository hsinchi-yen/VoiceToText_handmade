//! What the app does with a `GuardAction` from the VRAM guard (T1.10b, `tasks/plan.md` 5.1).
//! Pure; the polling loop that feeds it lives in `engine.rs`.

use v2c_asr_local::{GuardAction, GuardReason, PickReason};

/// Status code shown once when Breeze yields to a heavy app (`[gpu] heavy_apps`).
pub const GPU_YIELD_HEAVY_APP: &str = "gpu_yield_heavy_app";
/// Shown when Breeze is back on the GPU after one of the notices above.
pub const GPU_RESTORED: &str = "gpu_restored";
/// ... because free VRAM is below `vram_reserve_mb` (+ Breeze's need when loading).
pub const GPU_LOW_VRAM: &str = "gpu_low_vram";

/// What to do after a guard poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardStep {
    Nothing,
    /// Load Breeze (on the guard thread) and make it the active engine.
    Load,
    /// Remove Breeze; SenseVoice becomes the active engine.
    Unload(GuardReason),
}

/// Turns a guard action into a step. `has_cpu`: SenseVoice is loaded, so unloading Breeze
/// leaves a working engine. Without it, a runtime yield (heavy app, fullscreen, low VRAM) keeps
/// Breeze rather than leave nothing to dictate with; idle and config reasons still unload
/// (an idle Breeze is reloaded on the next hotkey press). `load_blocked`: a Breeze load is
/// running or failed recently.
pub fn guard_step(action: GuardAction, has_cpu: bool, load_blocked: bool) -> GuardStep {
    match action {
        GuardAction::Keep | GuardAction::UseCpu(_) => GuardStep::Nothing,
        GuardAction::Load if load_blocked => GuardStep::Nothing,
        GuardAction::Load => GuardStep::Load,
        GuardAction::Unload(reason) => {
            if has_cpu || matches!(reason, GuardReason::Idle | GuardReason::Engine(_)) {
                GuardStep::Unload(reason)
            } else {
                GuardStep::Nothing
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

    #[test]
    fn steps_follow_actions() {
        assert_eq!(
            guard_step(GuardAction::Keep, true, false),
            GuardStep::Nothing
        );
        assert_eq!(guard_step(GuardAction::Load, true, false), GuardStep::Load);
        assert_eq!(
            guard_step(GuardAction::Load, true, true),
            GuardStep::Nothing
        );
        assert_eq!(
            guard_step(GuardAction::UseCpu(GuardReason::HeavyApp), true, false),
            GuardStep::Nothing
        );
        assert_eq!(
            guard_step(GuardAction::Unload(GuardReason::HeavyApp), true, false),
            GuardStep::Unload(GuardReason::HeavyApp)
        );
    }

    #[test]
    fn without_sensevoice_runtime_yields_keep_breeze() {
        for r in [GuardReason::HeavyApp, GuardReason::Fullscreen, LOW] {
            assert_eq!(
                guard_step(GuardAction::Unload(r), false, false),
                GuardStep::Nothing
            );
        }
        for r in [
            GuardReason::Idle,
            GuardReason::Engine(PickReason::CpuOnlyProfile),
        ] {
            assert_eq!(
                guard_step(GuardAction::Unload(r), false, false),
                GuardStep::Unload(r)
            );
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
