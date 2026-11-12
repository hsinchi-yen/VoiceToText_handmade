// Pure helpers for the overlay (T4.7): level history + ECG trace shape, and the estimated
// progress used while transcribing / polishing. No DOM here so it can be unit-tested.

/** Samples older than this (seconds) are dropped from the history. */
export const HISTORY_SECS = 3;
/** The trace shows this many seconds of signal across the pill width. */
export const WINDOW_SECS = 1.2;
/** Rendering lags the newest sample by this much so there is always a next sample to lerp to. */
export const RENDER_DELAY_SECS = 0.06;

const clamp01 = (v) => (Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0);

/** Time-stamped level history (a ring buffer by age). Times are in seconds. */
export class LevelHistory {
  constructor(maxAgeSecs = HISTORY_SECS) {
    this.maxAge = maxAgeSecs;
    this.t = [];
    this.v = [];
  }

  push(level, now) {
    this.t.push(now);
    this.v.push(clamp01(level));
    while (this.t.length > 2 && now - this.t[0] > this.maxAge) {
      this.t.shift();
      this.v.shift();
    }
  }

  clear() {
    this.t.length = 0;
    this.v.length = 0;
  }

  /** Linearly interpolated level at time `at`; 0 before the first sample, last value after. */
  sampleAt(at) {
    const n = this.t.length;
    if (n === 0) return 0;
    if (at <= this.t[0]) return at < this.t[0] ? 0 : this.v[0];
    if (at >= this.t[n - 1]) return this.v[n - 1];
    let i = n - 1;
    while (i > 0 && this.t[i - 1] > at) i--;
    const t0 = this.t[i - 1];
    const t1 = this.t[i];
    const k = t1 > t0 ? (at - t0) / (t1 - t0) : 1;
    return this.v[i - 1] + (this.v[i] - this.v[i - 1]) * k;
  }
}

/** Gaussian bump centred at `c` with width `w`. */
const bump = (x, c, w) => Math.exp(-(((x - c) / w) ** 2));

/**
 * Unit-ish heartbeat/sine mix at time `t` (seconds): a PQRST-like beat every 0.5 s plus two sines
 * so the line is never a stiff repeat. Result is within [-1, 1].
 */
export function ecgShape(t) {
  const period = 0.5;
  const ph = ((t % period) + period) % period;
  const beat =
    0.12 * bump(ph, 0.1, 0.035) - // P
    0.25 * bump(ph, 0.185, 0.015) + // Q
    1.0 * bump(ph, 0.215, 0.02) - // R (spike)
    0.4 * bump(ph, 0.25, 0.02) + // S
    0.22 * bump(ph, 0.36, 0.05); // T
  const sines = 0.35 * Math.sin(2 * Math.PI * 9 * t) + 0.25 * Math.sin(2 * Math.PI * 17 * t + 1.3);
  return Math.max(-1, Math.min(1, 0.85 * beat + 0.4 * sines));
}

/** Amplitude (0..1 of the half height) for a level: a gentle idle wiggle at silence. */
export function amplitude(level) {
  return 0.06 + 0.94 * Math.pow(clamp01(level), 0.7);
}

/**
 * The trace as `count` y values in -1..1 (index 0 = oldest/left, last = now/right). Each point
 * reads the level at its own time (interpolated), so the shape scrolls right to left and
 * jumps with the voice. With `scroll` false (reduced motion) the shape is frozen in time and
 * only scaled by the current level.
 */
export function buildTrace(history, now, count, { scroll = true } = {}) {
  const out = new Array(count);
  if (!scroll) {
    const a = amplitude(history.sampleAt(now));
    for (let i = 0; i < count; i++) out[i] = a * ecgShape((i / (count - 1)) * WINDOW_SECS);
    return out;
  }
  for (let i = 0; i < count; i++) {
    const t = now - (1 - i / (count - 1)) * WINDOW_SECS;
    const a = amplitude(history.sampleAt(t - RENDER_DELAY_SECS));
    out[i] = a * ecgShape(t);
  }
  return out;
}

// ---- Estimated progress -------------------------------------------------------------------

export const TRANSCRIBE_TARGET = 0.6;
export const TRANSCRIBE_TAU = 0.8;
export const POLISH_TARGET = 0.92;
export const POLISH_TAU = 1.2;

/**
 * Progress has no real signal: it eases asymptotically towards a per-phase target and never
 * goes backwards. `done` snaps to 1; anything else that is not transcribing/polishing resets.
 */
export class ProgressEstimator {
  constructor() {
    this.kind = null;
    this.since = 0;
    this.from = 0;
  }

  /** Call when the view kind changes (or repeats); repeated same kinds are ignored. */
  setPhase(kind, now) {
    if (kind === this.kind) return;
    if (kind === "transcribing" || kind === "polishing") {
      const keep = this.kind === "transcribing" || this.kind === "polishing";
      this.from = keep ? this.value(now) : 0;
    } else if (kind === "done") {
      this.from = 1;
    } else {
      this.from = 0;
    }
    this.kind = kind;
    this.since = now;
  }

  value(now) {
    const dt = Math.max(0, now - this.since);
    switch (this.kind) {
      case "transcribing": {
        const p = TRANSCRIBE_TARGET * (1 - Math.exp(-dt / TRANSCRIBE_TAU));
        return Math.max(this.from, p);
      }
      case "polishing": {
        const p = POLISH_TARGET - (POLISH_TARGET - Math.min(this.from, POLISH_TARGET)) * Math.exp(-dt / POLISH_TAU);
        return Math.max(this.from, p);
      }
      case "done":
        return 1;
      default:
        return 0;
    }
  }
}

// ---- Steady sampling ----------------------------------------------------------------------

export const SAMPLE_SECS = 0.05;
export const STALL_SECS = 0.3;
const STALL_TAU = 0.15;

/**
 * Feeds the history at a steady cadence from the animation loop, whether or not level events
 * arrive (they are deduplicated upstream: a constant 0 sends nothing new to the UI). If events
 * stop for longer than STALL_SECS the level decays towards 0 instead of freezing.
 */
export class LevelSampler {
  constructor(history) {
    this.history = history;
    this.level = 0;
    this.eventAt = -Infinity;
    this.pushedAt = -Infinity;
  }

  /** An overlay level event arrived (even if the value is unchanged). */
  setLevel(level, now) {
    this.level = clamp01(level);
    this.eventAt = now;
  }

  effective(now) {
    const stale = now - this.eventAt - STALL_SECS;
    return stale > 0 ? this.level * Math.exp(-stale / STALL_TAU) : this.level;
  }

  /** Call every frame; pushes a sample when at least SAMPLE_SECS passed since the last one. */
  tick(now) {
    if (now - this.pushedAt >= SAMPLE_SECS) {
      this.history.push(this.effective(now), now);
      this.pushedAt = now;
    }
  }
}
