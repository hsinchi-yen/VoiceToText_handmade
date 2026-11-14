import { describe, expect, it } from "vitest";
import { activePreset, heroState } from "../home/logic.js";

const presets = [
  { title: "TN 區網服務", id_prefix: "tn-" },
  { title: "RD 區網服務", id_prefix: "rd-" },
];

describe("activePreset", () => {
  it("all local", () => {
    expect(activePreset({ asr: "local", polish: "local", translate: "local" }, presets)).toBe("全部地端");
  });
  it("matches one prefix, ignoring local routes", () => {
    expect(activePreset({ asr: "local", polish: "tn-qwen", translate: "tn-qwen" }, presets)).toBe("TN 區網服務");
    expect(activePreset({ asr: "rd-a", polish: "rd-b", translate: "rd-b" }, presets)).toBe("RD 區網服務");
  });
  it("mixed or unknown is custom", () => {
    expect(activePreset({ asr: "tn-a", polish: "rd-b", translate: "local" }, presets)).toBe("自訂");
    expect(activePreset({ asr: "openai-1", polish: "local", translate: "local" }, presets)).toBe("自訂");
  });
  it("no route yet", () => {
    expect(activePreset(null, presets)).toBe("");
  });
});

describe("heroState", () => {
  it("loading until anything is known", () => {
    expect(heroState(null, null).tone).toBe("loading");
  });
  it("ready by default and after done", () => {
    expect(heroState(null, {}).title).toBe("可以說話了");
    expect(heroState({ kind: "done", chars: 3, latency_ms: 1 }, {}).tone).toBe("ready");
  });
  it("model loading is not a problem", () => {
    expect(heroState({ kind: "error", code: "asr_loading" }, {}).tone).toBe("loading");
  });
  it("notice codes stay ready with a note", () => {
    const h = heroState({ kind: "error", code: "gpu_restored" }, {});
    expect(h.tone).toBe("ready");
    expect(h.note).toContain("顯示卡");
    const c = heroState({ kind: "error", code: "cloud_fell_back" }, {});
    expect(c.tone).toBe("ready");
    expect(c.note).toBe("雲端服務失敗，已改用地端");
  });
  it("hands-free recording says how to stop", () => {
    expect(heroState({ kind: "recording", rms: 0, hands_free: true }, {}).sub).toBe("再按一下右 Ctrl 停止");
  });
  it("other errors are problems", () => {
    expect(heroState({ kind: "error", code: "mic_not_found" }, {}).title).toBe("有問題");
  });
  it("recording is busy", () => {
    expect(heroState({ kind: "recording", rms: 0.1 }, {}).tone).toBe("busy");
  });
});
