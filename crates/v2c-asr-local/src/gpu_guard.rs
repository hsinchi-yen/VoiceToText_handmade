//! VRAM guard: keeps display VRAM free and moves ASR to the CPU when the GPU is needed
//! elsewhere (T1.10b, `tasks/plan.md` section 5.1).
//!
//! Pure decision logic, no I/O. The caller polls every [`POLL_INTERVAL`]: it reads a
//! [`GpuSnapshot`] (from [`crate::gpu_monitor::GpuMonitor`]), the foreground app (from
//! `v2c-platform`), fills a [`GuardInput`] and acts on the [`GuardAction`] that
//! [`GpuGuard::decide`] returns. After every Breeze load it calls [`GpuGuard::record_load`]
//! with the snapshots taken just before and after, so the guard learns what Breeze really uses.
//!
//! Rules (plan 5.1, protection mechanisms 1-4):
//!
//! 1. **Before loading**: Breeze may load only if
//!    `free >= vram_reserve_mb + breeze need + reload_margin_mb` ([`GpuGuard::can_load`]).
//!    The need is the measured usage from [`GpuGuard::record_load`] when known, otherwise the
//!    estimate in [`GuardInput::breeze_vram_mb`].
//! 2. **While running**: the snapshot's `free` no longer contains Breeze, so the rule becomes
//!    `free >= vram_reserve_mb` (no margin: the margin is the hysteresis band between loading
//!    and unloading). Below it -> unload Breeze and use the CPU, once two consecutive polls
//!    ([`GuardTuning::low_vram_unload_after`]) read low, never mid-transcription (`busy`). A
//!    missing reading (transient NVML failure) never unloads a loaded Breeze.
//! 3. **Idle**: unloaded after `idle_unload_minutes` without use (0 = never); not reloaded by
//!    polling, only when the user dictates again (the caller then passes `idle = 0`).
//! 4. **Yield**: foreground exe in `heavy_apps` (case-insensitive), or fullscreen while
//!    `yield_on_fullscreen` -> CPU.
//!
//! Restoring after any runtime block (rules 2 and 4) waits until the block has been clear for
//! the restore delay ([`GuardTuning::restore_after`]). A low-VRAM unload within
//! [`GuardTuning::flap_window`] of a load doubles that delay (up to
//! [`GuardTuning::max_restore_after`]); staying loaded for a whole `flap_window` resets it.
//! Together with the measured need this stops load / unload loops when the estimate is wrong.
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
/// Plan 5.1 estimate (about 1.5 GB); replace with the T0.1 measurement. At runtime the guard
/// prefers its own measurement ([`GpuGuard::record_load`]).
pub const BREEZE_Q5_0_VRAM_MB: u64 = 1536;
/// Estimated VRAM for Breeze-ASR-25 q8_0, MiB (plan 5.1: about 0.6 GB more than q5_0).
pub const BREEZE_Q8_0_VRAM_MB: u64 = 2150;

/// VRAM estimate for a Breeze quantisation; fills [`crate::select::GpuInfo::breeze_vram_mb`]
/// and [`GuardInput::breeze_vram_mb`].
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
    /// VRAM used by this process, MiB, when the driver reports it. Only filled by
    /// [`crate::gpu_monitor::GpuMonitor::detailed_snapshot`] (settings page); under Windows
    /// WDDM NVML cannot attribute memory to processes, so it is usually `None` there. The guard
    /// does not use it.
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
    /// `None`: no NVIDIA GPU, NVML not available, or this read failed.
    pub gpu: Option<GpuSnapshot>,
    /// A CUDA device is usable (binary built with `cuda` and an NVIDIA device exists).
    pub cuda_available: bool,
    /// The Breeze GGML file for the configured quantisation exists.
    pub breeze_model_present: bool,
    /// Estimated VRAM Breeze needs once loaded, MiB ([`breeze_vram_estimate_mb`]). The guard
    /// replaces it with its measurement for the same estimate once [`GpuGuard::record_load`]
    /// has seen a load.
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
    /// Not enough free VRAM: `needed_mb` is what `free_mb` would have to reach.
    LowVram { free_mb: u64, needed_mb: u64 },
    /// Foreground exe is in `heavy_apps`.
    HeavyApp,
    /// Foreground is fullscreen and `yield_on_fullscreen = true`.
    Fullscreen,
    /// Unused for longer than the idle limit.
    Idle,
    /// The block is gone but has not been clear for the restore delay yet.
    RestorePending,
}

/// What the caller should do after a poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardAction {
    /// Nothing to change.
    Keep,
    /// Load Breeze (in the background), switch ASR to it once loaded, then call
    /// [`GpuGuard::record_load`].
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
    /// Base restore delay: a block must have been clear this long before Breeze loads again.
    pub restore_after: Duration,
    /// Upper bound of the backed-off restore delay.
    pub max_restore_after: Duration,
    /// A low-VRAM unload within this time after a load doubles the restore delay; staying
    /// loaded this long resets it.
    pub flap_window: Duration,
    /// Extra free VRAM, on top of `reserve + need`, required to load, MiB.
    pub reload_margin_mb: u64,
    /// Idle time after which `keep_loaded = "on_demand"` unloads.
    pub on_demand_unload_after: Duration,
}

impl Default for GuardTuning {
    fn default() -> Self {
        Self {
            low_vram_unload_after: POLL_INTERVAL,
            restore_after: Duration::from_secs(20),
            max_restore_after: Duration::from_secs(600),
            flap_window: Duration::from_secs(120),
            reload_margin_mb: 256,
            on_demand_unload_after: Duration::from_secs(60),
        }
    }
}

/// Breeze VRAM measured by [`GpuGuard::record_load`], valid for one estimate (quantisation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Measured {
    estimate_mb: u64,
    used_mb: u64,
}

/// Stateful VRAM guard. One instance per app; call [`GpuGuard::decide`] every poll.
#[derive(Debug, Clone)]
pub struct GpuGuard {
    tuning: GuardTuning,
    /// First poll in the current run of low-VRAM readings while loaded.
    low_vram_since: Option<Instant>,
    /// Last poll at which a runtime block (low VRAM, heavy app, fullscreen) was seen.
    last_blocked_at: Option<Instant>,
    /// Current restore delay (backs off on flapping).
    restore_after: Duration,
    /// When Breeze was last loaded.
    last_load_at: Option<Instant>,
    measured: Option<Measured>,
}

impl Default for GpuGuard {
    fn default() -> Self {
        Self::new(GuardTuning::default())
    }
}

impl GpuGuard {
    pub fn new(tuning: GuardTuning) -> Self {
        Self {
            tuning,
            low_vram_since: None,
            last_blocked_at: None,
            restore_after: tuning.restore_after,
            last_load_at: None,
            measured: None,
        }
    }

    pub fn tuning(&self) -> &GuardTuning {
        &self.tuning
    }

    /// Current restore delay (base delay, or backed off after flapping).
    pub fn restore_delay(&self) -> Duration {
        self.restore_after
    }

    /// VRAM Breeze needs, MiB: the measurement for this estimate if there is one, otherwise the
    /// estimate. Also what the settings page should show as v2c's usage while Breeze is loaded.
    pub fn breeze_need_mb(&self, estimate_mb: u64) -> u64 {
        self.measured
            .filter(|m| m.estimate_mb == estimate_mb)
            .map_or(estimate_mb, |m| m.used_mb)
    }

    /// Call after every Breeze load (from [`GuardAction::Load`] or a hotkey-triggered load)
    /// with the snapshots taken right before and right after loading. `estimate_mb` is the
    /// [`GuardInput::breeze_vram_mb`] in use. The free-VRAM drop becomes the need for future
    /// decisions; readings that show no drop (missing, or another app freed memory meanwhile)
    /// are ignored.
    pub fn record_load(
        &mut self,
        estimate_mb: u64,
        before: Option<GpuSnapshot>,
        after: Option<GpuSnapshot>,
        now: Instant,
    ) {
        self.last_load_at = Some(now);
        if let (Some(b), Some(a)) = (before, after) {
            let used_mb = b.free_mb.saturating_sub(a.free_mb);
            if used_mb > 0 {
                self.measured = Some(Measured {
                    estimate_mb,
                    used_mb,
                });
            }
        }
    }

    /// Whether Breeze may be loaded (or stay loaded) right now, without the restore delay. Use
    /// it when the user presses the hotkey and Breeze is not loaded: `Ok` -> load Breeze for
    /// this utterance (then [`GpuGuard::record_load`]), `Err` -> transcribe with SenseVoice.
    /// Does not change the guard.
    pub fn can_load(&self, config: &Config, input: &GuardInput<'_>) -> Result<(), GuardReason> {
        gpu_eligibility(config, input.cuda_available, input.breeze_model_present)
            .map_err(GuardReason::Engine)?;
        if let Some(reason) = Self::should_yield(config, input.foreground.as_ref()) {
            return Err(reason);
        }
        if config.local.asr == LocalAsr::Breeze {
            // Forced by the user: no VRAM check (same as `select::pick`).
            return Ok(());
        }
        self.vram_check(config, input)
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

    /// Unloaded: `free >= reserve + need + margin`. Loaded: `free >= reserve`.
    fn vram_check(&self, config: &Config, input: &GuardInput<'_>) -> Result<(), GuardReason> {
        let Some(gpu) = input.gpu else {
            return Err(GuardReason::VramUnknown);
        };
        let needed_mb = if input.breeze_loaded {
            config.gpu.vram_reserve_mb
        } else {
            config
                .gpu
                .vram_reserve_mb
                .saturating_add(self.breeze_need_mb(input.breeze_vram_mb))
                .saturating_add(self.tuning.reload_margin_mb)
        };
        if gpu.free_mb < needed_mb {
            return Err(GuardReason::LowVram {
                free_mb: gpu.free_mb,
                needed_mb,
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

        let block = self.can_load(config, input).err();
        if input.breeze_loaded {
            self.decide_loaded(config, input, block)
        } else {
            self.decide_unloaded(config, input, block)
        }
        .unwrap_or(GuardAction::Keep)
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
            // A missing reading (transient NVML failure) says nothing about pressure.
            Some(GuardReason::VramUnknown) => None,
            Some(reason) if !always => {
                self.last_blocked_at = Some(now);
                let low = matches!(reason, GuardReason::LowVram { .. });
                let low_for = if low {
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
                let just_loaded = self
                    .last_load_at
                    .is_some_and(|at| now.saturating_duration_since(at) < self.tuning.flap_window);
                if low && just_loaded {
                    self.restore_after = self
                        .restore_after
                        .saturating_mul(2)
                        .min(self.tuning.max_restore_after);
                }
                Some(GuardAction::Unload(reason))
            }
            _ => {
                self.low_vram_since = None;
                let stable = self
                    .last_load_at
                    .is_some_and(|at| now.saturating_duration_since(at) >= self.tuning.flap_window);
                if stable {
                    self.restore_after = self.tuning.restore_after;
                }
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
    ) -> Option<GuardAction> {
        let now = input.now;
        self.low_vram_since = None;
        if let Some(reason) = block {
            self.last_blocked_at = Some(now);
            return Some(GuardAction::UseCpu(reason));
        }
        // Allowed. Only preload / restore when keep_loaded wants a resident model.
        match config.gpu.keep_loaded {
            KeepLoaded::OnDemand => return None,
            KeepLoaded::Auto if self.idle_limit(config).is_some_and(|l| input.idle >= l) => {
                return None;
            }
            _ => {}
        }
        if let Some(at) = self.last_blocked_at {
            if now.saturating_duration_since(at) < self.restore_after {
                return Some(GuardAction::UseCpu(GuardReason::RestorePending));
            }
        }
        self.last_blocked_at = None;
        // Until the caller confirms with `record_load`, count the load from now.
        self.last_load_at = Some(now);
        Some(GuardAction::Load)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2c_core::config::GpuProfile;

    const NEED: u64 = BREEZE_Q5_0_VRAM_MB; // 1536
    const RESERVE: u64 = 2048;
    const MARGIN: u64 = 256;
    /// Smallest free VRAM that loads with these test settings: 3840.
    const FITS: u64 = RESERVE + NEED + MARGIN;

    /// Defaults with the reserve pinned to [`RESERVE`], so these tests keep their boundaries
    /// independent of the configured default (600 MiB since 2026-11-10).
    fn cfg() -> Config {
        let mut c = Config::with_defaults();
        c.gpu.vram_reserve_mb = RESERVE;
        c
    }

    fn g() -> GpuGuard {
        GpuGuard::default()
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
    fn can_load_needs_reserve_plus_breeze_plus_margin() {
        let t = Instant::now();
        let c = cfg();
        assert_eq!(g().can_load(&c, &input(t, FITS, false)), Ok(()));
        assert_eq!(
            g().can_load(&c, &input(t, FITS - 1, false)),
            Err(GuardReason::LowVram {
                free_mb: FITS - 1,
                needed_mb: FITS
            })
        );
    }

    #[test]
    fn can_load_while_loaded_only_needs_reserve() {
        let t = Instant::now();
        let c = cfg();
        assert_eq!(g().can_load(&c, &input(t, RESERVE, true)), Ok(()));
        assert_eq!(
            g().can_load(&c, &input(t, RESERVE - 1, true)),
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
            g().can_load(&c, &input(t, 1024 + NEED + MARGIN, false)),
            Ok(())
        );
        c.gpu.vram_reserve_mb = u64::MAX;
        assert!(g().can_load(&c, &input(t, 6000, false)).is_err());
    }

    #[test]
    fn margin_applies_to_fresh_guard_and_poll_alike() {
        let t = Instant::now();
        let c = cfg();
        let just = RESERVE + NEED;
        assert!(matches!(
            g().decide(&c, &input(t, just, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
        assert!(g().can_load(&c, &input(t, just, false)).is_err());
        assert_eq!(g().decide(&c, &input(t, FITS, false)), GuardAction::Load);
        // Once loaded, anything at or above the reserve keeps it (the hysteresis band).
        assert_eq!(g().decide(&c, &input(t, RESERVE, true)), GuardAction::Keep);
    }

    #[test]
    fn no_vram_reading_blocks_auto_but_not_forced_breeze() {
        let t = Instant::now();
        let mut i = input(t, 0, false);
        i.gpu = None;
        let mut c = cfg();
        assert_eq!(g().can_load(&c, &i), Err(GuardReason::VramUnknown));
        assert_eq!(
            g().decide(&c, &i),
            GuardAction::UseCpu(GuardReason::VramUnknown)
        );
        c.local.asr = LocalAsr::Breeze;
        assert_eq!(g().can_load(&c, &i), Ok(()));
    }

    #[test]
    fn loaded_one_none_reading_does_not_unload() {
        let t0 = Instant::now();
        let c = cfg();
        let mut guard = g();
        assert_eq!(guard.decide(&c, &input(t0, 3000, true)), GuardAction::Keep);
        let mut i = input(secs(t0, 5), 0, true);
        i.gpu = None;
        assert_eq!(guard.decide(&c, &i), GuardAction::Keep);
        i.now = secs(t0, 10);
        assert_eq!(guard.decide(&c, &i), GuardAction::Keep);
        // A None between two low readings does not count as confirmation either way.
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 15), 3000, true)),
            GuardAction::Keep
        );
    }

    #[test]
    fn forced_breeze_skips_vram_but_still_yields() {
        let t = Instant::now();
        let mut c = cfg();
        c.local.asr = LocalAsr::Breeze;
        assert_eq!(g().can_load(&c, &input(t, 0, false)), Ok(()));
        let mut i = input(t, 0, false);
        i.foreground = fg("blender.exe", false);
        assert_eq!(g().can_load(&c, &i), Err(GuardReason::HeavyApp));
    }

    #[test]
    fn startup_with_room_loads_and_without_room_uses_cpu() {
        let t = Instant::now();
        let c = cfg();
        assert_eq!(g().decide(&c, &input(t, 4500, false)), GuardAction::Load);
        assert!(matches!(
            g().decide(&c, &input(t, 3000, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
    }

    // Rule 2: low VRAM while running, with hysteresis.

    #[test]
    fn low_vram_while_loaded_unloads_on_second_low_poll() {
        let t0 = Instant::now();
        let c = cfg();
        let mut guard = g();
        assert_eq!(guard.decide(&c, &input(t0, 2500, true)), GuardAction::Keep);
        // First low reading: wait for confirmation.
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 5), 1500, true)),
            GuardAction::Keep
        );
        // Second low reading one poll later: unload.
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 10), 1500, true)),
            GuardAction::Unload(GuardReason::LowVram {
                free_mb: 1500,
                needed_mb: RESERVE
            })
        );
    }

    #[test]
    fn short_low_vram_spike_does_not_unload() {
        let t0 = Instant::now();
        let c = cfg();
        let mut guard = g();
        assert_eq!(guard.decide(&c, &input(t0, 1500, true)), GuardAction::Keep);
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 5), 3000, true)),
            GuardAction::Keep
        );
        // The spike counter restarted.
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 10), 1500, true)),
            GuardAction::Keep
        );
    }

    #[test]
    fn never_unloads_while_busy() {
        let t0 = Instant::now();
        let c = cfg();
        let mut guard = g();
        let mut i = input(t0, 100, true);
        i.busy = true;
        for s in [0, 5, 10, 15] {
            i.now = secs(t0, s);
            assert_eq!(guard.decide(&c, &i), GuardAction::Keep);
        }
        i.busy = false;
        i.now = secs(t0, 20);
        assert!(matches!(guard.decide(&c, &i), GuardAction::Unload(_)));
    }

    #[test]
    fn reload_after_low_vram_waits_for_restore_delay() {
        let t0 = Instant::now();
        let c = cfg();
        let mut guard = g();
        guard.decide(&c, &input(t0, 1500, true));
        assert!(matches!(
            guard.decide(&c, &input(secs(t0, 5), 1500, true)),
            GuardAction::Unload(_)
        ));
        // Unloaded now: freed our own VRAM, so free went up by NEED -> still blocked.
        assert!(matches!(
            guard.decide(&c, &input(secs(t0, 10), 1500 + NEED, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
        // The game closed: plenty free, but restore waits 20 s after the last block.
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 15), FITS, false)),
            GuardAction::UseCpu(GuardReason::RestorePending)
        );
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 25), FITS, false)),
            GuardAction::UseCpu(GuardReason::RestorePending)
        );
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 31), FITS, false)),
            GuardAction::Load
        );
    }

    /// Breeze really uses `real_mb` while the estimate says 1536; other apps leave 4500 free.
    /// Runs 30 minutes of polls and returns the times at which the guard loaded Breeze.
    fn simulate(real_mb: u64, record: bool) -> Vec<u64> {
        let t0 = Instant::now();
        let c = cfg();
        let mut guard = g();
        let mut loaded = false;
        let mut loads = Vec::new();
        for s in (0..=1800).step_by(5) {
            let free = if loaded { 4500 - real_mb } else { 4500 };
            match guard.decide(&c, &input(secs(t0, s), free, loaded)) {
                GuardAction::Load => {
                    loaded = true;
                    loads.push(s);
                    if record {
                        guard.record_load(NEED, snap(4500), snap(4500 - real_mb), secs(t0, s));
                    }
                }
                GuardAction::Unload(_) => loaded = false,
                GuardAction::Keep | GuardAction::UseCpu(_) => {}
            }
        }
        loads
    }

    #[test]
    fn low_estimate_does_not_flap_with_measurement() {
        // Breeze really needs 2500 MiB: after the first load, free = 2000 < reserve.
        let loads = simulate(2500, true);
        assert_eq!(loads, vec![0], "measured need must stop reloading");
    }

    #[test]
    fn low_estimate_without_measurement_backs_off() {
        let loads = simulate(2500, false);
        // Without back-off it would reload every ~30 s (60 times in 30 minutes).
        assert!(loads.len() <= 8, "{loads:?}");
        let gaps: Vec<u64> = loads.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|w| w[1] >= w[0]), "{gaps:?}");
        assert!(gaps.last().copied().unwrap_or(0) >= 600, "{gaps:?}");
    }

    #[test]
    fn correct_estimate_stays_loaded() {
        assert_eq!(simulate(NEED, false), vec![0]);
        assert_eq!(simulate(NEED, true), vec![0]);
    }

    #[test]
    fn back_off_resets_after_stable_load() {
        let t0 = Instant::now();
        let c = cfg();
        let mut guard = g();
        assert_eq!(guard.decide(&c, &input(t0, 4500, false)), GuardAction::Load);
        guard.decide(&c, &input(secs(t0, 5), 1000, true));
        assert!(matches!(
            guard.decide(&c, &input(secs(t0, 10), 1000, true)),
            GuardAction::Unload(_)
        ));
        assert_eq!(guard.restore_delay(), Duration::from_secs(40));
        // A load that stays fine for the whole flap window resets the delay.
        guard.record_load(NEED, snap(4500), snap(4500 - NEED), secs(t0, 100));
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 150), 2964, true)),
            GuardAction::Keep
        );
        assert_eq!(guard.restore_delay(), Duration::from_secs(40));
        guard.decide(&c, &input(secs(t0, 220), 2964, true));
        assert_eq!(guard.restore_delay(), Duration::from_secs(20));
    }

    #[test]
    fn back_off_is_capped() {
        let t0 = Instant::now();
        let c = cfg();
        let mut guard = g();
        let mut s = 0;
        for _ in 0..20 {
            guard.record_load(NEED, None, None, secs(t0, s));
            guard.decide(&c, &input(secs(t0, s + 5), 1000, true));
            guard.decide(&c, &input(secs(t0, s + 10), 1000, true));
            s += 20;
        }
        assert_eq!(guard.restore_delay(), Duration::from_secs(600));
    }

    #[test]
    fn record_load_measures_and_ignores_bad_readings() {
        let t = Instant::now();
        let mut guard = g();
        assert_eq!(guard.breeze_need_mb(NEED), NEED);
        guard.record_load(NEED, snap(4500), snap(4600), t);
        assert_eq!(guard.breeze_need_mb(NEED), NEED, "no drop: ignored");
        guard.record_load(NEED, None, snap(2000), t);
        assert_eq!(guard.breeze_need_mb(NEED), NEED, "missing: ignored");
        guard.record_load(NEED, snap(4500), snap(2700), t);
        assert_eq!(guard.breeze_need_mb(NEED), 1800);
        // Another quantisation (different estimate) does not reuse the measurement.
        assert_eq!(
            guard.breeze_need_mb(BREEZE_Q8_0_VRAM_MB),
            BREEZE_Q8_0_VRAM_MB
        );
        // The measurement feeds can_load.
        let c = cfg();
        assert!(guard
            .can_load(&c, &input(t, RESERVE + 1800 + MARGIN - 1, false))
            .is_err());
        assert_eq!(
            guard.can_load(&c, &input(t, RESERVE + 1800 + MARGIN, false)),
            Ok(())
        );
    }

    // Rule 3: idle.

    #[test]
    fn idle_unloads_after_configured_minutes() {
        let t = Instant::now();
        let c = cfg();
        assert_eq!(c.gpu.idle_unload_minutes, 15);
        let mut guard = g();
        let mut i = input(t, 3000, true);
        i.idle = Duration::from_secs(15 * 60 - 1);
        assert_eq!(guard.decide(&c, &i), GuardAction::Keep);
        i.idle = Duration::from_secs(15 * 60);
        assert_eq!(guard.decide(&c, &i), GuardAction::Unload(GuardReason::Idle));
    }

    #[test]
    fn idle_unloaded_is_not_reloaded_until_used_again() {
        let t = Instant::now();
        let c = cfg();
        let mut guard = g();
        let mut i = input(t, 4500, false);
        i.idle = Duration::from_secs(16 * 60);
        assert_eq!(guard.decide(&c, &i), GuardAction::Keep);
        // Hotkey pressed: caller checks can_load, then idle restarts from zero.
        assert_eq!(guard.can_load(&c, &i), Ok(()));
        i.idle = Duration::ZERO;
        assert_eq!(guard.decide(&c, &i), GuardAction::Load);
    }

    #[test]
    fn idle_minutes_zero_never_unloads() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.idle_unload_minutes = 0;
        let mut i = input(t, 3000, true);
        i.idle = Duration::from_secs(100 * 3600);
        assert_eq!(g().decide(&c, &i), GuardAction::Keep);
    }

    #[test]
    fn idle_does_not_unload_while_busy() {
        let t = Instant::now();
        let c = cfg();
        let mut i = input(t, 3000, true);
        i.idle = Duration::from_secs(3600);
        i.busy = true;
        assert_eq!(g().decide(&c, &i), GuardAction::Keep);
    }

    // Rule 4: heavy apps and fullscreen.

    #[test]
    fn heavy_app_yields_immediately_case_insensitive() {
        let t = Instant::now();
        let c = cfg();
        let mut i = input(t, 4500, true);
        i.foreground = fg("Blender.EXE", false);
        assert_eq!(
            g().decide(&c, &i),
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
        let mut guard = g();
        let mut i = input(t0, 4500, true);
        i.foreground = fg("Adobe Premiere Pro.exe", false);
        assert_eq!(
            guard.decide(&c, &i),
            GuardAction::Unload(GuardReason::HeavyApp)
        );
        i.breeze_loaded = false;
        i.now = secs(t0, 5);
        assert_eq!(
            guard.decide(&c, &i),
            GuardAction::UseCpu(GuardReason::HeavyApp)
        );
        // Premiere closed (or left the foreground).
        i.foreground = fg("explorer.exe", false);
        i.now = secs(t0, 10);
        assert_eq!(
            guard.decide(&c, &i),
            GuardAction::UseCpu(GuardReason::RestorePending)
        );
        // Alt-tab back within the delay restarts the wait.
        i.foreground = fg("Adobe Premiere Pro.exe", false);
        i.now = secs(t0, 15);
        assert_eq!(
            guard.decide(&c, &i),
            GuardAction::UseCpu(GuardReason::HeavyApp)
        );
        i.foreground = fg("explorer.exe", false);
        i.now = secs(t0, 30);
        assert_eq!(
            guard.decide(&c, &i),
            GuardAction::UseCpu(GuardReason::RestorePending)
        );
        i.now = secs(t0, 35);
        assert_eq!(guard.decide(&c, &i), GuardAction::Load);
        // A yield (not low VRAM) does not back off.
        assert_eq!(guard.restore_delay(), Duration::from_secs(20));
    }

    #[test]
    fn fullscreen_yields_only_when_enabled() {
        let t = Instant::now();
        let mut c = cfg();
        let mut i = input(t, 4500, true);
        i.foreground = fg("game.exe", true);
        assert_eq!(
            g().decide(&c, &i),
            GuardAction::Unload(GuardReason::Fullscreen)
        );
        c.gpu.yield_on_fullscreen = false;
        assert_eq!(g().decide(&c, &i), GuardAction::Keep);
        // heavy_apps still apply without fullscreen yielding.
        i.foreground = fg("blender.exe", true);
        assert_eq!(
            g().decide(&c, &i),
            GuardAction::Unload(GuardReason::HeavyApp)
        );
    }

    #[test]
    fn hotkey_during_heavy_app_uses_cpu() {
        let t = Instant::now();
        let c = cfg();
        let mut i = input(t, 4500, false);
        i.foreground = fg("blender.exe", false);
        assert_eq!(g().can_load(&c, &i), Err(GuardReason::HeavyApp));
    }

    // keep_loaded semantics.

    #[test]
    fn keep_always_ignores_idle_and_runtime_pressure() {
        let t0 = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::Always;
        let mut guard = g();
        let mut i = input(t0, 100, true);
        i.idle = Duration::from_secs(24 * 3600);
        i.foreground = fg("blender.exe", true);
        for s in [0, 5, 10, 60] {
            i.now = secs(t0, s);
            assert_eq!(guard.decide(&c, &i), GuardAction::Keep);
        }
    }

    #[test]
    fn keep_always_still_checks_before_loading_and_preloads() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::Always;
        assert!(matches!(
            g().decide(&c, &input(t, 1000, false)),
            GuardAction::UseCpu(GuardReason::LowVram { .. })
        ));
        let mut i = input(t, 4500, false);
        i.idle = Duration::from_secs(24 * 3600);
        assert_eq!(g().decide(&c, &i), GuardAction::Load);
    }

    #[test]
    fn keep_always_still_unloads_on_config_change() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::Always;
        c.gpu.profile = GpuProfile::CpuOnly;
        assert_eq!(
            g().decide(&c, &input(t, 4500, true)),
            GuardAction::Unload(GuardReason::Engine(PickReason::CpuOnlyProfile))
        );
    }

    #[test]
    fn keep_on_demand_never_preloads_and_unloads_soon() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::OnDemand;
        let mut guard = g();
        assert_eq!(guard.decide(&c, &input(t, 4500, false)), GuardAction::Keep);
        assert_eq!(guard.can_load(&c, &input(t, 4500, false)), Ok(()));
        let mut i = input(t, 3000, true);
        i.idle = Duration::from_secs(59);
        assert_eq!(guard.decide(&c, &i), GuardAction::Keep);
        i.idle = Duration::from_secs(60);
        assert_eq!(guard.decide(&c, &i), GuardAction::Unload(GuardReason::Idle));
    }

    #[test]
    fn keep_on_demand_still_yields() {
        let t = Instant::now();
        let mut c = cfg();
        c.gpu.keep_loaded = KeepLoaded::OnDemand;
        let mut i = input(t, 4500, true);
        i.foreground = fg("blender.exe", false);
        assert_eq!(
            g().decide(&c, &i),
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
            g().decide(&c, &input(t, 6000, true)),
            GuardAction::Unload(r)
        );
        assert_eq!(
            g().decide(&c, &input(t, 6000, false)),
            GuardAction::UseCpu(r)
        );
        assert_eq!(g().can_load(&c, &input(t, 6000, false)), Err(r));
    }

    #[test]
    fn balanced_and_low_vram_profiles_both_run_breeze() {
        let t = Instant::now();
        for profile in [GpuProfile::Balanced, GpuProfile::LowVram] {
            let mut c = cfg();
            c.gpu.profile = profile;
            assert_eq!(
                g().decide(&c, &input(t, 4500, false)),
                GuardAction::Load,
                "{profile:?}"
            );
        }
    }

    #[test]
    fn switching_profile_back_reloads() {
        let t0 = Instant::now();
        let mut c = cfg();
        let mut guard = g();
        c.gpu.profile = GpuProfile::CpuOnly;
        assert!(matches!(
            guard.decide(&c, &input(t0, 4500, true)),
            GuardAction::Unload(_)
        ));
        c.gpu.profile = GpuProfile::Balanced;
        // A config switch is not a runtime block: reload right away.
        assert_eq!(
            guard.decide(&c, &input(secs(t0, 5), 4500, false)),
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
                g().decide(&c, &i),
                GuardAction::Unload(GuardReason::Engine(reason))
            );
            i.busy = true;
            assert_eq!(g().decide(&c, &i), GuardAction::Keep);
        }
    }
}
