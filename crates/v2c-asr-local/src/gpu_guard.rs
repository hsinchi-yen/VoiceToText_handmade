//! VRAM guard: keeps display VRAM free and moves ASR to the CPU when the GPU is needed
//! elsewhere (T1.10b, `tasks/plan.md` section 5.1).
//!
//! Pure decision logic, no I/O. The caller polls every [`POLL_INTERVAL`]: it reads a
//! [`GpuSnapshot`] (from [`crate::gpu_monitor::GpuMonitor`]), the foreground app (from
//! `v2c-platform`), fills a [`GuardInput`] and acts on the [`GuardAction`] that
//! [`GpuGuard::decide`] returns.
//!
//! Rules (plan 5.1, protection mechanisms 1-4):
//!
//! 1. **Before loading**: Breeze may load only if `free - vram_reserve_mb >= breeze need`
//!    ([`GpuGuard::can_load`]). When Breeze is already loaded the snapshot's `free` no longer
//!    contains Breeze, so the same rule becomes `free >= vram_reserve_mb`.
//! 2. **While running**: free VRAM below the reserve -> unload Breeze and use the CPU. The low
//!    reading must persist for [`GuardTuning::low_vram_unload_after`] (one more poll) so a short
//!    spike does not unload; it never unloads mid-transcription (`busy`).
//! 3. **Idle**: unloaded after `idle_unload_minutes` without use (0 = never); not reloaded by
//!    polling, only when the user dictates again (the caller then passes `idle = 0`).
//! 4. **Yield**: foreground exe in `heavy_apps` (case-insensitive), or fullscreen while
//!    `yield_on_fullscreen` -> CPU. Restored once the condition has been clear for
//!    [`GuardTuning::restore_after`].
//!
//! Hysteresis: after a low-VRAM block, reloading needs an extra
//! [`GuardTuning::reload_margin_mb`] on top of `reserve + need`, and every block must have been
//! clear for `restore_after` before reloading, so the guard does not flap between GPU and CPU.
//!
//! `[gpu] keep_loaded`:
//! - `auto` (default): rules 1-4; preloads at startup and restores after a block.
//! - `always`: once loaded, Breeze stays loaded: no idle unload and no runtime yield (low VRAM,
//!   heavy apps, fullscreen). Loading still respects rules 1 and 4. Config changes that disable
//!   Breeze (profile, `enabled`, `asr`) still unload it.
//! - `on_demand`: never preloads; Breeze is loaded when the user starts dictating
//!   ([`GpuGuard::can_load`]) and unloaded after [`GuardTuning::on_demand_unload_after`] idle.
//!   Yields like `auto`.
//!
//! Profiles: `cpu_only` (or `enabled = false`) always means CPU; `balanced` and `low_vram` are
//! identical here (they differ only in the LM Studio model, see `docs/local-setup.md`).
//! `[local] asr = "breeze"` (forced) skips the VRAM checks, like [`crate::select::pick`], but
//! still yields to heavy apps and unloads when idle.

use std::time::{Duration, Instant};

use v2c_core::config::{BreezeQuant, Config, KeepLoaded, LocalAsr};

use crate::select::{gpu_eligibility, PickReason};

/// How often the caller should call [`GpuGuard::decide`] (plan 5.1: every 5 seconds).
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Estimated VRAM Breeze-ASR-25 q5_0 needs once loaded (weights + compute buffers), MiB.
/// Plan 5.1 estimate (about 1.5 GB); replace with the T0.1 measurement.
pub const BREEZE_Q5_0_VRAM_MB: u64 = 1536;
/// Estimated VRAM for Breeze-ASR-25 q8_0, MiB (plan 5.1: about 0.6 GB more than q5_0).
pub const BREEZE_Q8_0_VRAM_MB: u64 = 2150;

/// VRAM estimate for a Breeze quantisation; fills [`crate::select::GpuInfo::breeze_vram_mb`]
/// and [`GuardInput::breeze_vram_mb`] until a measured value is available.
pub fn breeze_vram_estimate_mb(quant: BreezeQuant) -> u64 {
    match quant {
        BreezeQuant::Q5_0 => BREEZE_Q5_0_VRAM_MB,
        BreezeQuant::Q8_0 => BREEZE_Q8_0_VRAM_MB,
    }
}

/// One VRAM reading of the NVIDIA device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuSnapshot {
    /// Free VRAM, MiB.
    pub free_mb: u64,
    /// Total VRAM, MiB.
    pub total_mb: u64,
    /// VRAM used by this process, MiB, when the driver reports it. Under Windows WDDM NVML
    /// cannot attribute memory to processes, so this is usually `None` there. Informational
    /// (settings usage bar); the guard uses [`GuardInput::breeze_vram_mb`] instead.
    pub used_by_us_mb: Option<u64>,
}

impl GpuSnapshot {
    /// VRAM used by everything, MiB.
    pub fn used_mb(&self) -> u64 {
        self.total_mb.saturating_sub(self.free_mb)
    }
}

/// The foreground application, as far as the guard cares. Built from
/// `v2c_platform::AppInfo` by the caller (this crate does not depend on the platform crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForegroundInfo<'a> {
    /// Executable file name, e.g. `blender.exe`.
    pub exe: &'a str,
    /// The foreground window covers its monitor, or the shell reports a D3D fullscreen /
    /// presentation-mode app (`v2c_platform::windows::fullscreen::foreground_fullscreen`).
    pub fullscreen: bool,
}

/// Everything [`GpuGuard`] needs for one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardInput<'a> {
    /// `None`: no NVIDIA GPU or NVML not available.
    pub gpu: Option<GpuSnapshot>,
    /// A CUDA device is usable (binary built with `cuda` and an NVIDIA device exists).
    pub cuda_available: bool,
    /// The Breeze GGML file for the configured quantisation exists.
    pub breeze_model_present: bool,
    /// VRAM Breeze needs once loaded, MiB ([`breeze_vram_estimate_mb`] until measured).
    pub breeze_vram_mb: u64,
    /// Foreground app; `None` if unknown (treated as "nothing heavy").
    pub foreground: Option<ForegroundInfo<'a>>,
    /// Breeze is currently loaded.
    pub breeze_loaded: bool,
    /// A transcription is running right now; the guard never unloads while busy.
    pub busy: bool,
    /// Time since the last dictation (since startup if none yet). Pass zero when the user has
    /// just pressed the hotkey.
    pub idle: Duration,
    /// Current time (injected for tests).
    pub now: Instant,
}

/// Why the guard keeps or puts ASR on the CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardReason {
    /// Config or environment rules out Breeze: `asr = "sense_voice"`, GPU disabled, `cpu_only`
    /// profile, model missing or no CUDA (same reasons as [`crate::select::pick_with_reason`]).
    Engine(PickReason),
    /// No VRAM reading available (NVML missing) in auto mode, so loading would be a guess.
    VramUnknown,
    /// Free VRAM minus the reserve does not fit Breeze (or, while loaded, free < reserve).
    LowVram { free_mb: u64, needed_mb: u64 },
    /// Foreground exe is in `heavy_apps`.
    HeavyApp,
    /// Foreground is fullscreen and `yield_on_fullscreen = true`.
    Fullscreen,
    /// Unused for longer than the idle limit.
    Idle,
    /// The block is gone but has not been clear for `restore_after` yet.
    RestorePending,
}

/// What the caller should do after a poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardAction {
    /// Nothing to change.
    Keep,
    /// Load Breeze (in the background) and switch ASR to it once loaded.
    Load,
    /// Breeze is loaded and must be unloaded; use SenseVoice (CPU) from now on.
    Unload(GuardReason),
    /// Breeze is not loaded and must stay unloaded; make sure SenseVoice is the active engine.
    UseCpu(GuardReason),
}

/// Hysteresis and timing knobs. Defaults follow plan 5.1; they are not in `[gpu]` config yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardTuning {
    /// Low VRAM must persist this long while loaded before unloading (one extra poll).
    pub low_vram_unload_after: Duration,
    /// A block must have been clear this long before Breeze is loaded again.
    pub restore_after: Duration,
    /// Extra free VRAM required to reload after a low-VRAM block, MiB.
    pub reload_margin_mb: u64,
    /// Idle time after which `keep_loaded = "on_demand"` unloads.
    pub on_demand_unload_after: Duration,
}

impl Default for GuardTuning {
    fn default() -> Self {
        Self {
            low_vram_unload_after: POLL_INTERVAL,
            restore_after: Duration::from_secs(20),
            reload_margin_mb: 256,
            on_demand_unload_after: Duration::from_secs(60),
        }
    }
}

/// Stateful VRAM guard. One instance per app; call [`GpuGuard::decide`] every poll.
#[derive(Debug, Clone, Default)]
pub struct GpuGuard {
    tuning: GuardTuning,
    /// First poll in the current run of low-VRAM readings while loaded.
    low_vram_since: Option<Instant>,
    /// Last poll at which a runtime block (low VRAM, heavy app, fullscreen) was seen.
    last_blocked_at: Option<Instant>,
    /// The most recent runtime block was low VRAM (reload then needs the margin).
    last_block_low_vram: bool,
}

impl GpuGuard {
    pub fn new(tuning: GuardTuning) -> Self {
        Self {
            tuning,
            ..Self::default()
        }
    }

    pub fn tuning(&self) -> &GuardTuning {
        &self.tuning
    }

    /// Whether Breeze may be loaded (or stay loaded) right now, without hysteresis. Use it when
    /// the user presses the hotkey and Breeze is not loaded: `Ok` -> load Breeze for this
    /// utterance, `Err` -> transcribe with SenseVoice.
    pub fn can_load(config: &Config, input: &GuardInput<'_>) -> Result<(), GuardReason> {
        gpu_eligibility(config, input.cuda_available, input.breeze_model_present)
            .map_err(GuardReason::Engine)?;
        if let Some(reason) = Self::should_yield(config, input.foreground.as_ref()) {
            return Err(reason);
        }
        if config.local.asr == LocalAsr::Breeze {
            // Forced by the user: no VRAM check (same as `select::pick`).
            return Ok(());
        }
        Self::vram_check(config, input, 0)
    }

    /// Whether the foreground app means v2c should leave the GPU (`heavy_apps` or fullscreen).
    pub fn should_yield(
        config: &Config,
        foreground: Option<&ForegroundInfo<'_>>,
    ) -> Option<GuardReason> {
        let fg = foreground?;
        if config
            .gpu
            .heavy_apps
            .iter()
            .any(|app| app.eq_ignore_ascii_case(fg.exe))
        {
            return Some(GuardReason::HeavyApp);
        }
        if config.gpu.yield_on_fullscreen && fg.fullscreen {
            return Some(GuardReason::Fullscreen);
        }
        None
    }

    /// `free (+ Breeze itself when loaded) >= reserve + need + margin`.
    fn vram_check(
        config: &Config,
        input: &GuardInput<'_>,
        margin_mb: u64,
    ) -> Result<(), GuardReason> {
        let Some(gpu) = input.gpu else {
            return Err(GuardReason::VramUnknown);
        };
        let ours = if input.breeze_loaded {
            input.breeze_vram_mb
        } else {
            0
        };
        let available = gpu.free_mb.saturating_add(ours);
        let needed_mb = config
            .gpu
            .vram_reserve_mb
            .saturating_add(input.breeze_vram_mb)
            .saturating_add(margin_mb);
        if available < needed_mb {
            return Err(GuardReason::LowVram {
                free_mb: gpu.free_mb,
                needed_mb: needed_mb.saturating_sub(ours),
            });
        }
        Ok(())
    }

    /// Idle limit for this config, or `None` when idle never unloads.
    fn idle_limit(&self, config: &Config) -> Option<Duration> {
        match config.gpu.keep_loaded {
            KeepLoaded::Always => None,
            KeepLoaded::OnDemand => Some(self.tuning.on_demand_unload_after),
            KeepLoaded::Auto => match config.gpu.idle_unload_minutes {
                0 => None,
                m => Some(Duration::from_secs(m.saturating_mul(60))),
            },
        }
    }

    /// One poll. See the module docs for the rules.
    pub fn decide(&mut self, config: &Config, input: &GuardInput<'_>) -> GuardAction {
        let keep = config.gpu.keep_loaded;

        // Static reasons (config / environment): immediate, regardless of keep_loaded.
        if let Err(reason) =
            gpu_eligibility(config, input.cuda_available, input.breeze_model_present)
        {
            self.low_vram_since = None;
            let reason = GuardReason::Engine(reason);
            return if !input.breeze_loaded {
                GuardAction::UseCpu(reason)
            } else if input.busy {
                GuardAction::Keep
            } else {
                GuardAction::Unload(reason)
            };
        }

        let block = Self::can_load(config, input).err();
        if input.breeze_loaded {
            self.decide_loaded(config, input, block)
        } else {
            self.decide_unloaded(config, input, block, keep)
        }
        .unwrap_or(GuardAction::Keep)
    }

    fn note_block(&mut self, now: Instant, reason: GuardReason) {
        self.last_blocked_at = Some(now);
        self.last_block_low_vram = matches!(reason, GuardReason::LowVram { .. });
    }

    fn decide_loaded(
        &mut self,
        config: &Config,
        input: &GuardInput<'_>,
        block: Option<GuardReason>,
    ) -> Option<GuardAction> {
        let now = input.now;
        let always = config.gpu.keep_loaded == KeepLoaded::Always;
        match block {
            Some(reason) if !always => {
                self.note_block(now, reason);
                let low_for = if let GuardReason::LowVram { .. } = reason {
                    let since = *self.low_vram_since.get_or_insert(now);
                    Some(now.saturating_duration_since(since))
                } else {
                    self.low_vram_since = None;
                    None
                };
                if input.busy {
                    return None;
                }
                if low_for.is_some_and(|d| d < self.tuning.low_vram_unload_after) {
                    return None;
                }
                self.low_vram_since = None;
                Some(GuardAction::Unload(reason))
            }
            _ => {
                self.low_vram_since = None;
                let idle_out = self.idle_limit(config).is_some_and(|l| input.idle >= l);
                (idle_out && !input.busy).then_some(GuardAction::Unload(GuardReason::Idle))
            }
        }
    }

    fn decide_unloaded(
        &mut self,
        config: &Config,
        input: &GuardInput<'_>,
        block: Option<GuardReason>,
        keep: KeepLoaded,
    ) -> Option<GuardAction> {
        let now = input.now;
        self.low_vram_since = None;
        if let Some(reason) = block {
            self.note_block(now, reason);
            return Some(GuardAction::UseCpu(reason));
        }
        // Allowed. Only preload / restore when keep_loaded wants a resident model.
        match keep {
            KeepLoaded::OnDemand => return None,
            KeepLoaded::Auto if self.idle_limit(config).is_some_and(|l| input.idle >= l) => {
                return None;
            }
            _ => {}
        }
        if let Some(at) = self.last_blocked_at {
            if now.saturating_duration_since(at) < self.tuning.restore_after {
                return Some(GuardAction::UseCpu(GuardReason::RestorePending));
            }
            if self.last_block_low_vram && config.local.asr != LocalAsr::Breeze {
                if let Err(reason) = Self::vram_check(config, input, self.tuning.reload_margin_mb) {
                    // Fits, but not with the margin: still too close to the reserve.
                    self.note_block(now, reason);
                    return Some(GuardAction::UseCpu(reason));
                }
            }
        }
        self.last_blocked_at = None;
        self.last_block_low_vram = false;
        Some(GuardAction::Load)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2c_core::config::GpuProfile;

    const NEED: u64 = BREEZE_Q5_0_VRAM_MB; // 1536
    const RESERVE: u64 = 2048;

    fn cfg() -> Config {
        Config::with_defaults()
    }

    fn snap(free_mb: u64) -> Option<GpuSnapshot> {
        Some(GpuSnapshot {
            free_mb,
            total_mb: 6144,
            used_by_us_mb: None,
        })
    }

    fn input(t: Instant, free_mb: u64, loaded: bool) -> GuardInput<'static> {
        GuardInput {
            gpu: snap(free_mb),
            cuda_available: true,
            breeze_model_present: true,
            breeze_vram_mb: NEED,
            foreground: None,
            breeze_loaded: loaded,
            busy: false,
            idle: Duration::ZERO,
            now: t,
        }
    }

    fn secs(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    fn fg(exe: &'static str, fullscreen: bool) -> Option<ForegroundInfo<'static>> {
        Some(ForegroundInfo { exe, fullscreen })
    }

    #[test]
    fn estimates_follow_quant() {
        assert_eq!(breeze_vram_estimate_mb(BreezeQuant::Q5_0), 1536);
        assert_eq!(breeze_vram_estimate_mb(BreezeQuant::Q8_0), 2150);
        assert_eq!(
            breeze_vram_estimate_mb(cfg().local.breeze_quant),
            BREEZE_Q5_0_VRAM_MB
        );
    }

    #[test]
    fn snapshot_used_is_total_minus_free() {
        assert_eq!(snap(4500).unwrap().used_mb(), 1644);
        let odd = GpuSnapshot {
            free_mb: 10,
            total_mb: 5,
            used_by_us_mb: None,
        };
        assert_eq!(odd.used_mb(), 0);
    }

    // Rule 1: before loading.

    #[test]
    fn can_load_needs_reserve_plus_breeze() {
        let t = Instant::now();
        let c = cfg();
        assert_eq!(
            GpuGuard::can_load(&c, &input(t, RESERVE + NEED, false)),
            Ok(())
        );
        assert_eq!(
            GpuGuard::can_load(&c, &input(t, RESERVE + NEED - 1, false)),
            Err(GuardReason::LowVram {
                free_mb: RESERVE + NEED - 1,
                needed_mb: RESERVE + NEED
            })
        );
    }

    #[test]
    fn can_load_while_loaded_only_needs_reserve() {
        let t = Instant::now();
        let c = cfg();
        assert_eq!(GpuGuard::can_load(&c, &input(t, RESERVE, true)), Ok(()));
        assert_eq!(
            GpuGuard::can_load(&c, &input(t, RESERVE - 1, true)),
            Err(GuardReason::LowVram {
                free_mb: RESERVE - 1,
                needed_mb: RESERVE
            })
        );
    }

    #[test]
    fn reserve_comes_from_config() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.vram_reserve_mb = 1024;
        assert_eq!(
            GpuGuard::can_load(&c, &input(t, 1024 + NEED, false)),
            Ok(())
        );
        c.gpu.vram_reserve_mb = u64::MAX;
        assert!(GpuGuard::can_load(&c, &input(t, 6000, false)).is_err());
    }

    #[test]
    fn no_vram_reading_blocks_auto_but_not_forced_breeze() {
        let t = Instant::now();
        let mut i = input(t, 0, false);
        i.gpu = None;
        let mut c = cfg();
        assert_eq!(GpuGuard::can_load(&c, &i), Err(GuardReason::VramUnknown));
        c.local.asr = LocalAsr::Breeze;
        assert_eq!(GpuGuard::can_load(&c, &i), Ok(()));
    }

    #[test]
    fn forced_breeze_skips_vram_but_still_yields() {
        let t = Instant::now();
        let mut c = cfg();
        c.local.asr = LocalAsr::Breeze;
        assert_eq!(GpuGuard::can_load(&c, &input(t, 0, false)), Ok(()));
        let mut i = input(t, 0, false);
        i.foreground = fg("blender.exe", false);
        assert_eq!(GpuGuard::can_load(&c, &i), Err(GuardReason::HeavyApp));
    }

    #[test]
    fn startup_with_room_loads_and_without_room_uses_cpu() {
        let t = Instant::now();
        let c = cfg();
        assert_eq!(
            GpuGuard::default().decide(&c, &input(t, 4500, false)),
            GuardAction::Load
        );
        assert!(matches!(
            GpuGuard::default().decide(&c, &input(t, 3000, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
    }

    // Rule 2: low VRAM while running, with hysteresis.

    #[test]
    fn low_vram_while_loaded_unloads_after_second_poll() {
        let t0 = Instant::now();
        let c = cfg();
        let mut g = GpuGuard::default();
        assert_eq!(g.decide(&c, &input(t0, 2500, true)), GuardAction::Keep);
        // First low reading: wait for confirmation.
        assert_eq!(
            g.decide(&c, &input(secs(t0, 5), 1500, true)),
            GuardAction::Keep
        );
        // Second low reading one poll later: unload.
        assert!(matches!(
            g.decide(&c, &input(secs(t0, 10), 1500, true)),
            GuardAction::Unload(GuardReason::LowVram {
                free_mb: 1500,
                needed_mb: RESERVE
            })
        ));
    }

    #[test]
    fn short_low_vram_spike_does_not_unload() {
        let t0 = Instant::now();
        let c = cfg();
        let mut g = GpuGuard::default();
        assert_eq!(g.decide(&c, &input(t0, 1500, true)), GuardAction::Keep);
        assert_eq!(
            g.decide(&c, &input(secs(t0, 5), 3000, true)),
            GuardAction::Keep
        );
        // The spike counter restarted.
        assert_eq!(
            g.decide(&c, &input(secs(t0, 10), 1500, true)),
            GuardAction::Keep
        );
    }

    #[test]
    fn never_unloads_while_busy() {
        let t0 = Instant::now();
        let c = cfg();
        let mut g = GpuGuard::default();
        let mut i = input(t0, 100, true);
        i.busy = true;
        for s in [0, 5, 10, 15] {
            i.now = secs(t0, s);
            assert_eq!(g.decide(&c, &i), GuardAction::Keep);
        }
        i.busy = false;
        i.now = secs(t0, 20);
        assert!(matches!(g.decide(&c, &i), GuardAction::Unload(_)));
    }

    #[test]
    fn reload_after_low_vram_needs_delay_and_margin() {
        let t0 = Instant::now();
        let c = cfg();
        let mut g = GpuGuard::default();
        g.decide(&c, &input(t0, 1500, true));
        assert!(matches!(
            g.decide(&c, &input(secs(t0, 5), 1500, true)),
            GuardAction::Unload(_)
        ));
        // Unloaded now: freed our own VRAM, so free went up by NEED -> still blocked.
        assert!(matches!(
            g.decide(&c, &input(secs(t0, 10), 1500 + NEED, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
        // The game closed: plenty free, but restore waits `restore_after` (20 s).
        let ok = RESERVE + NEED + 256;
        assert_eq!(
            g.decide(&c, &input(secs(t0, 15), ok, false)),
            GuardAction::UseCpu(GuardReason::RestorePending)
        );
        assert_eq!(
            g.decide(&c, &input(secs(t0, 25), ok, false)),
            GuardAction::UseCpu(GuardReason::RestorePending)
        );
        assert_eq!(
            g.decide(&c, &input(secs(t0, 31), ok, false)),
            GuardAction::Load
        );
    }

    #[test]
    fn reload_margin_prevents_flapping() {
        let t0 = Instant::now();
        let c = cfg();
        let mut g = GpuGuard::default();
        // Blocked by low VRAM while unloaded.
        assert!(matches!(
            g.decide(&c, &input(t0, 3000, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
        // Just fits without margin, after the delay: still on CPU.
        let just = RESERVE + NEED;
        assert!(matches!(
            g.decide(&c, &input(secs(t0, 30), just, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
        // That counted as a block again; after another delay with the margin: load.
        assert_eq!(
            g.decide(&c, &input(secs(t0, 60), just + 256, false)),
            GuardAction::Load
        );
        // A fresh guard (no recent block) loads without the margin.
        assert_eq!(
            GpuGuard::default().decide(&c, &input(t0, just, false)),
            GuardAction::Load
        );
    }

    // Rule 3: idle.

    #[test]
    fn idle_unloads_after_configured_minutes() {
        let t = Instant::now();
        let c = cfg();
        assert_eq!(c.gpu.idle_unload_minutes, 15);
        let mut g = GpuGuard::default();
        let mut i = input(t, 3000, true);
        i.idle = Duration::from_secs(15 * 60 - 1);
        assert_eq!(g.decide(&c, &i), GuardAction::Keep);
        i.idle = Duration::from_secs(15 * 60);
        assert_eq!(g.decide(&c, &i), GuardAction::Unload(GuardReason::Idle));
    }

    #[test]
    fn idle_unloaded_is_not_reloaded_until_used_again() {
        let t = Instant::now();
        let c = cfg();
        let mut g = GpuGuard::default();
        let mut i = input(t, 4500, false);
        i.idle = Duration::from_secs(16 * 60);
        assert_eq!(g.decide(&c, &i), GuardAction::Keep);
        // Hotkey pressed: caller checks can_load, then idle restarts from zero.
        assert_eq!(GpuGuard::can_load(&c, &i), Ok(()));
        i.idle = Duration::ZERO;
        assert_eq!(g.decide(&c, &i), GuardAction::Load);
    }

    #[test]
    fn idle_minutes_zero_never_unloads() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.idle_unload_minutes = 0;
        let mut i = input(t, 3000, true);
        i.idle = Duration::from_secs(100 * 3600);
        assert_eq!(GpuGuard::default().decide(&c, &i), GuardAction::Keep);
    }

    #[test]
    fn idle_does_not_unload_while_busy() {
        let t = Instant::now();
        let c = cfg();
        let mut i = input(t, 3000, true);
        i.idle = Duration::from_secs(3600);
        i.busy = true;
        assert_eq!(GpuGuard::default().decide(&c, &i), GuardAction::Keep);
    }

    // Rule 4: heavy apps and fullscreen.

    #[test]
    fn heavy_app_yields_immediately_case_insensitive() {
        let t = Instant::now();
        let c = cfg();
        let mut i = input(t, 4500, true);
        i.foreground = fg("Blender.EXE", false);
        assert_eq!(
            GpuGuard::default().decide(&c, &i),
            GuardAction::Unload(GuardReason::HeavyApp)
        );
        assert_eq!(
            GpuGuard::should_yield(&c, i.foreground.as_ref()),
            Some(GuardReason::HeavyApp)
        );
        i.foreground = fg("notepad.exe", false);
        assert_eq!(GpuGuard::should_yield(&c, i.foreground.as_ref()), None);
        assert_eq!(GpuGuard::should_yield(&c, None), None);
    }

    #[test]
    fn heavy_app_restores_after_it_is_gone() {
        let t0 = Instant::now();
        let c = cfg();
        let mut g = GpuGuard::default();
        let mut i = input(t0, 4500, true);
        i.foreground = fg("Adobe Premiere Pro.exe", false);
        assert_eq!(g.decide(&c, &i), GuardAction::Unload(GuardReason::HeavyApp));
        i.breeze_loaded = false;
        i.now = secs(t0, 5);
        assert_eq!(g.decide(&c, &i), GuardAction::UseCpu(GuardReason::HeavyApp));
        // Premiere closed (or left the foreground).
        i.foreground = fg("explorer.exe", false);
        i.now = secs(t0, 10);
        assert_eq!(
            g.decide(&c, &i),
            GuardAction::UseCpu(GuardReason::RestorePending)
        );
        // Alt-tab back within the delay restarts the wait.
        i.foreground = fg("Adobe Premiere Pro.exe", false);
        i.now = secs(t0, 15);
        assert_eq!(g.decide(&c, &i), GuardAction::UseCpu(GuardReason::HeavyApp));
        i.foreground = fg("explorer.exe", false);
        i.now = secs(t0, 30);
        assert_eq!(
            g.decide(&c, &i),
            GuardAction::UseCpu(GuardReason::RestorePending)
        );
        i.now = secs(t0, 35);
        assert_eq!(g.decide(&c, &i), GuardAction::Load);
    }

    #[test]
    fn fullscreen_yields_only_when_enabled() {
        let t = Instant::now();
        let mut c = cfg();
        let mut i = input(t, 4500, true);
        i.foreground = fg("game.exe", true);
        assert_eq!(
            GpuGuard::default().decide(&c, &i),
            GuardAction::Unload(GuardReason::Fullscreen)
        );
        c.gpu.yield_on_fullscreen = false;
        assert_eq!(GpuGuard::default().decide(&c, &i), GuardAction::Keep);
        // heavy_apps still apply without fullscreen yielding.
        i.foreground = fg("blender.exe", true);
        assert_eq!(
            GpuGuard::default().decide(&c, &i),
            GuardAction::Unload(GuardReason::HeavyApp)
        );
    }

    #[test]
    fn hotkey_during_heavy_app_uses_cpu() {
        let t = Instant::now();
        let c = cfg();
        let mut i = input(t, 4500, false);
        i.foreground = fg("blender.exe", false);
        assert_eq!(GpuGuard::can_load(&c, &i), Err(GuardReason::HeavyApp));
    }

    // keep_loaded semantics.

    #[test]
    fn keep_always_ignores_idle_and_runtime_pressure() {
        let t0 = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::Always;
        let mut g = GpuGuard::default();
        let mut i = input(t0, 100, true);
        i.idle = Duration::from_secs(24 * 3600);
        i.foreground = fg("blender.exe", true);
        for s in [0, 5, 10, 60] {
            i.now = secs(t0, s);
            assert_eq!(g.decide(&c, &i), GuardAction::Keep);
        }
    }

    #[test]
    fn keep_always_still_checks_before_loading_and_preloads() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::Always;
        assert!(matches!(
            GpuGuard::default().decide(&c, &input(t, 1000, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
        let mut i = input(t, 4500, false);
        i.idle = Duration::from_secs(24 * 3600);
        assert_eq!(GpuGuard::default().decide(&c, &i), GuardAction::Load);
    }

    #[test]
    fn keep_always_still_unloads_on_config_change() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::Always;
        c.gpu.profile = GpuProfile::CpuOnly;
        assert_eq!(
            GpuGuard::default().decide(&c, &input(t, 4500, true)),
            GuardAction::Unload(GuardReason::Engine(PickReason::CpuOnlyProfile))
        );
    }

    #[test]
    fn keep_on_demand_never_preloads_and_unloads_soon() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::OnDemand;
        let mut g = GpuGuard::default();
        assert_eq!(g.decide(&c, &input(t, 4500, false)), GuardAction::Keep);
        assert_eq!(GpuGuard::can_load(&c, &input(t, 4500, false)), Ok(()));
        let mut i = input(t, 3000, true);
        i.idle = Duration::from_secs(59);
        assert_eq!(g.decide(&c, &i), GuardAction::Keep);
        i.idle = Duration::from_secs(60);
        assert_eq!(g.decide(&c, &i), GuardAction::Unload(GuardReason::Idle));
    }

    #[test]
    fn keep_on_demand_still_yields() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::OnDemand;
        let mut i = input(t, 4500, true);
        i.foreground = fg("blender.exe", false);
        assert_eq!(
            GpuGuard::default().decide(&c, &i),
            GuardAction::Unload(GuardReason::HeavyApp)
        );
    }

    // Profiles and static reasons.

    #[test]
    fn cpu_only_profile_switches_to_cpu() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.profile = GpuProfile::CpuOnly;
        let r = GuardReason::Engine(PickReason::CpuOnlyProfile);
        assert_eq!(
            GpuGuard::default().decide(&c, &input(t, 6000, true)),
            GuardAction::Unload(r)
        );
        assert_eq!(
            GpuGuard::default().decide(&c, &input(t, 6000, false)),
            GuardAction::UseCpu(r)
        );
        assert_eq!(GpuGuard::can_load(&c, &input(t, 6000, false)), Err(r));
    }

    #[test]
    fn balanced_and_low_vram_profiles_both_run_breeze() {
        let t = Instant::now();
        for profile in [GpuProfile::Balanced, GpuProfile::LowVram] {
            let mut c = cfg();
            c.gpu.profile = profile;
            assert_eq!(
                GpuGuard::default().decide(&c, &input(t, 4500, false)),
                GuardAction::Load,
                "{profile:?}"
            );
        }
    }

    #[test]
    fn switching_profile_back_reloads() {
        let t0 = Instant::now();
        let mut c = cfg();
        let mut g = GpuGuard::default();
        c.gpu.profile = GpuProfile::CpuOnly;
        assert!(matches!(
            g.decide(&c, &input(t0, 4500, true)),
            GuardAction::Unload(_)
        ));
        c.gpu.profile = GpuProfile::Balanced;
        // A config switch is not a runtime block: reload right away.
        assert_eq!(
            g.decide(&c, &input(secs(t0, 5), 4500, false)),
            GuardAction::Load
        );
    }

    #[test]
    fn static_reasons_match_select() {
        type Tweak = fn(&mut Config, &mut GuardInput<'static>);
        let t = Instant::now();
        let cases: [(Tweak, PickReason); 4] = [
            (|c, _| c.gpu.enabled = false, PickReason::GpuDisabled),
            (
                |c, _| c.local.asr = LocalAsr::SenseVoice,
                PickReason::Configured,
            ),
            (|_, i| i.cuda_available = false, PickReason::NoCuda),
            (
                |_, i| i.breeze_model_present = false,
                PickReason::BreezeModelMissing,
            ),
        ];
        for (tweak, reason) in cases {
            let mut c = cfg();
            let mut i = input(t, 6000, true);
            tweak(&mut c, &mut i);
            assert_eq!(
                GpuGuard::default().decide(&c, &i),
                GuardAction::Unload(GuardReason::Engine(reason))
            );
            i.busy = true;
            assert_eq!(GpuGuard::default().decide(&c, &i), GuardAction::Keep);
        }
    }
}
