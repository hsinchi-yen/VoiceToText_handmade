import { describe, expect, it } from "vitest";
import { isAutoModel, modelLabel } from "./model-label.js";

describe("modelLabel", () => {
  it("shows auto as 自動偵測", () => {
    expect(isAutoModel("auto")).toBe(true);
    expect(isAutoModel(" AUTO ")).toBe(true);
    expect(modelLabel("Auto")).toBe("自動偵測");
  });
  it("keeps real model ids", () => {
    expect(isAutoModel("auto-1")).toBe(false);
    expect(isAutoModel(null)).toBe(false);
    expect(modelLabel("Qwen3.8-27B-FP8")).toBe("Qwen3.8-27B-FP8");
  });
});
