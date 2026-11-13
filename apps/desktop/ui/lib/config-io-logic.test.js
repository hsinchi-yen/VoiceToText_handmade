import { describe, expect, it } from "vitest";
import {
  appliedText,
  errorMessage,
  exportFileName,
  exportNotes,
  routeRows,
  statusCounts,
  typedKeys,
  vocabText,
} from "../config-io/logic.js";

describe("exportFileName", () => {
  it("uses the local date, zero padded", () => {
    expect(exportFileName(new Date(2026, 10, 3))).toBe("v2c-config-20261103.toml");
    expect(exportFileName(new Date(2026, 0, 31))).toBe("v2c-config-20260131.toml");
  });
});

describe("errorMessage", () => {
  const d = (c) => `text:${c}`;
  it("adds the line for file errors", () => {
    expect(errorMessage({ code: "unknown_kind", line: 12 }, d)).toBe("第 12 行：text:unknown_kind");
    expect(errorMessage({ code: "syntax", line: null }, d)).toBe("text:syntax");
  });
  it("accepts a plain code", () => {
    expect(errorMessage("forbidden", d)).toBe("text:forbidden");
  });
});

const preview = {
  names: { "my-gemini": "我的 Gemini", "tn-qwen": "TN Qwen" },
  route: [
    { field: "asr", old: "local", new: "my-gemini" },
    { field: "translate", old: "tn-qwen", new: "local" },
    { field: "cloud_fallback_to_local", old: "true", new: "false" },
  ],
  services: [{ status: "added" }, { status: "added" }, { status: "unchanged" }],
  needs_key: ["a", "b"],
  vocab_new: 3,
  vocab_blocked: 1,
};

describe("preview helpers", () => {
  it("routeRows names services and local", () => {
    expect(routeRows(preview)).toEqual([
      { label: "語音辨識", from: "本機", to: "我的 Gemini" },
      { label: "中翻英", from: "TN Qwen", to: "本機" },
      { label: "雲端失敗時改用本機", from: "開啟", to: "關閉" },
    ]);
    expect(routeRows(null)).toEqual([]);
  });
  it("statusCounts", () => {
    expect(statusCounts(preview)).toEqual({ added: 2, updated: 0, unchanged: 1 });
  });
  it("typedKeys keeps only listed, non-empty, trimmed keys", () => {
    expect(typedKeys(preview, { a: "  k1 ", b: "  ", c: "x" })).toEqual({ a: "k1" });
  });
  it("vocabText", () => {
    expect(vocabText(preview)).toBe("新增 3 個詞，1 個詞因重複上限或格式不符而略過");
    expect(vocabText({ vocab_new: 0, vocab_blocked: 0 })).toBe("");
  });
});

describe("result texts", () => {
  it("appliedText", () => {
    expect(appliedText({ added: 1, updated: 2, unchanged: 0, route_changed: 1, vocab_added: 4 })).toBe(
      "新增 1 個服務、更新 2 個服務、變更 1 項用途設定、詞庫新增 4 個詞",
    );
    expect(appliedText({ added: 0, updated: 0, unchanged: 0, route_changed: 0, vocab_added: 0 })).toBe("沒有任何變更");
  });
  it("exportNotes", () => {
    expect(exportNotes({ skipped: [], route_reset: [] })).toEqual([]);
    const n = exportNotes({ skipped: ["bad"], route_reset: ["asr"] });
    expect(n).toHaveLength(2);
    expect(n[0]).toContain("bad");
    expect(n[1]).toContain("語音辨識");
  });
});
