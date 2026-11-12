import { describe, expect, it } from "vitest";
import {
  LevelHistory,
  ProgressEstimator,
  POLISH_TARGET,
  TRANSCRIBE_TARGET,
  amplitude,
  buildTrace,
  ecgShape,
} from "../overlay/wave.js";

describe("LevelHistory", () => {
  it("interpolates linearly between samples", () => {
    const h = new LevelHistory();
    h.push(0, 1.0);
    h.push(1, 1.05);
    expect(h.sampleAt(1.025)).toBeCloseTo(0.5, 5);
    expect(h.sampleAt(1.0)).toBe(0);
    expect(h.sampleAt(2)).toBe(1); // holds the last value
    expect(h.sampleAt(0.5)).toBe(0); // before the first sample
  });

  it("clamps levels and ignores junk", () => {
    const h = new LevelHistory();
    h.push(5, 0);
    h.push(NaN, 0.05);
    h.push(-1, 0.1);
    expect(h.v).toEqual([1, 0, 0]);
  });

  it("drops samples older than the max age", () => {
    const h = new LevelHistory(1);
    for (let i = 0; i <= 40; i++) h.push(0.5, i * 0.05);
    expect(h.t[0]).toBeGreaterThanOrEqual(2 - 1 - 1e-9);
    expect(h.t.length).toBeLessThan(25);
  });

  it("is empty-safe", () => {
    expect(new LevelHistory().sampleAt(1)).toBe(0);
  });
});

describe("trace", () => {
  it("ecgShape stays within -1..1 and has spikes", () => {
    let max = 0;
    for (let t = 0; t < 3; t += 0.002) {
      const y = ecgShape(t);
      expect(Math.abs(y)).toBeLessThanOrEqual(1);
      max = Math.max(max, Math.abs(y));
    }
    expect(max).toBeGreaterThan(0.5);
  });

  it("amplitude has an idle floor and grows with level", () => {
    expect(amplitude(0)).toBeGreaterThan(0);
    expect(amplitude(0)).toBeLessThan(0.15);
    expect(amplitude(1)).toBeCloseTo(1, 5);
    expect(amplitude(0.5)).toBeGreaterThan(amplitude(0.2));
  });

  const peak = (a) => Math.max(...a.map(Math.abs));

  it("is flat-ish at silence and large when loud", () => {
    const quiet = new LevelHistory();
    const loud = new LevelHistory();
    for (let i = 0; i < 60; i++) {
      quiet.push(0, i * 0.05);
      loud.push(1, i * 0.05);
    }
    expect(peak(buildTrace(quiet, 3, 100))).toBeLessThan(0.15);
    expect(peak(buildTrace(loud, 3, 100))).toBeGreaterThan(0.5);
  });

  it("scrolls: the same shape moves left as time advances", () => {
    const h = new LevelHistory();
    for (let i = 0; i < 100; i++) h.push(1, i * 0.05);
    const a = buildTrace(h, 4, 200);
    const b = buildTrace(h, 4 + (20 * 1.2) / 199, 200); // exactly 20 points later
    // Later time = same shape 20 points further left.
    const shift = 20;
    let diff = 0;
    for (let i = 0; i < 150; i++) diff = Math.max(diff, Math.abs(b[i] - a[i + shift]));
    expect(diff).toBeLessThan(1e-6);
  });

  it("reduced motion freezes the shape and only scales by level", () => {
    const h = new LevelHistory();
    h.push(0.6, 0);
    const a = buildTrace(h, 1, 50, { scroll: false });
    const b = buildTrace(h, 2, 50, { scroll: false });
    expect(b).toEqual(a);
  });
});

describe("ProgressEstimator", () => {
  it("transcribing eases towards ~60% and never reaches it", () => {
    const p = new ProgressEstimator();
    p.setPhase("transcribing", 10);
    expect(p.value(10)).toBe(0);
    expect(p.value(10.8)).toBeCloseTo(TRANSCRIBE_TARGET * (1 - Math.exp(-1)), 5);
    expect(p.value(40)).toBeLessThan(TRANSCRIBE_TARGET + 1e-9);
    expect(p.value(11)).toBeGreaterThan(p.value(10.5));
  });

  it("polishing continues from where transcribing was, towards ~92%", () => {
    const p = new ProgressEstimator();
    p.setPhase("transcribing", 0);
    const at = p.value(1.5);
    p.setPhase("polishing", 1.5);
    expect(p.value(1.5)).toBeCloseTo(at, 5);
    expect(p.value(2.5)).toBeGreaterThan(at);
    expect(p.value(60)).toBeLessThanOrEqual(POLISH_TARGET + 1e-9);
    expect(p.value(60)).toBeGreaterThan(0.9);
  });

  it("never goes backwards, even if polishing starts past its target curve", () => {
    const p = new ProgressEstimator();
    p.setPhase("transcribing", 0);
    p.setPhase("polishing", 0.1);
    let last = 0;
    for (let t = 0.1; t < 10; t += 0.1) {
      const v = p.value(t);
      expect(v).toBeGreaterThanOrEqual(last - 1e-12);
      last = v;
    }
  });

  it("polishing skipped: transcribing goes straight to done = 100%", () => {
    const p = new ProgressEstimator();
    p.setPhase("transcribing", 0);
    p.setPhase("done", 1);
    expect(p.value(1)).toBe(1);
    expect(p.value(1.2)).toBe(1);
  });

  it("repeating the same phase does not restart it", () => {
    const p = new ProgressEstimator();
    p.setPhase("transcribing", 0);
    const v = p.value(1);
    p.setPhase("transcribing", 1);
    expect(p.value(1)).toBe(v);
  });

  it("starts from zero after a non-processing phase", () => {
    const p = new ProgressEstimator();
    p.setPhase("done", 0);
    p.setPhase("recording", 1);
    p.setPhase("transcribing", 2);
    expect(p.value(2)).toBe(0);
  });
});
