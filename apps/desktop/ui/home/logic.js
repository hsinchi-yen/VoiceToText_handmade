// Pure helpers for the Home page (unit-tested in logic.test.js). No Tauri, no user text.

/** Status codes that mean "still loading / switching" rather than "broken". */
const LOADING_CODES = new Set(["asr_loading", "asr_switching"]);

/**
 * Hero state from the controller status event. `tone` picks the colours:
 * ready | busy | loading | problem.
 */
export function heroState(status, info) {
  if (!status && !info) return { tone: "loading", title: "正在載入…", sub: "" };
  switch (status?.kind) {
    case "recording":
      return { tone: "busy", title: "錄音中…", sub: "說完放開右 Ctrl" };
    case "transcribing":
      return { tone: "busy", title: "辨識中…", sub: "" };
    case "polishing":
      return { tone: "busy", title: "潤飾中…", sub: "" };
    case "error":
      if (LOADING_CODES.has(status.code)) return { tone: "loading", title: "正在載入…", sub: "" };
      return { tone: "problem", title: "有問題", sub: "" };
    default:
      return { tone: "ready", title: "可以說話了", sub: "按住右 Ctrl 說話" };
  }
}

/** Active preset title for the current routes (see tasks/plan.md 13.1). */
export function activePreset(route, presets) {
  if (!route) return "";
  const ids = [route.asr, route.polish, route.translate].filter((id) => id && id !== "local");
  if (ids.length === 0) return "全部地端";
  for (const p of presets) {
    if (p.id_prefix && ids.every((id) => id.startsWith(p.id_prefix))) return p.title;
  }
  return "自訂";
}

/** Options for the 語音辨識 select: local engines first, then cloud services with ASR. */
export function asrSelectOptions(localOptions, cloud, label) {
  return [
    ...localOptions.map((o) => ({ value: "local:" + o.value, label: "地端：" + o.label })),
    ...cloud.filter((s) => s.supports_asr).map((s) => ({ value: s.id, label: label(s) })),
  ];
}
