// Pure helpers for the config export / import UI (T5.2). No Tauri calls here.

/** Default export file name: `v2c-config-YYYYMMDD.toml` (local date). */
export function exportFileName(d = new Date()) {
  const p = (n) => String(n).padStart(2, "0");
  return `v2c-config-${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}.toml`;
}

export const statusLabel = { added: "新增", updated: "更新", unchanged: "不變" };
export const purposeLabel = { asr: "語音辨識", polish: "潤飾", translate: "中翻英" };
export const routeFieldLabel = {
  asr: "語音辨識",
  polish: "潤飾",
  translate: "中翻英",
  cloud_fallback_to_local: "雲端失敗時改用本機",
};

/** A command error is a code string (presets) or `{ code, line }` (config_*). */
export function errorMessage(err, describe) {
  if (err && typeof err === "object" && "code" in err) {
    const text = describe(err.code);
    return err.line ? `第 ${err.line} 行：${text}` : text;
  }
  return describe(err);
}

function routeValue(field, value, names) {
  if (field === "cloud_fallback_to_local") return value === "true" ? "開啟" : "關閉";
  if (value === "local") return "本機";
  return names?.[value] ?? value;
}

/** Route changes as `{ label, from, to }` rows for the preview. */
export function routeRows(preview) {
  return (preview?.route ?? []).map((c) => ({
    label: routeFieldLabel[c.field] ?? c.field,
    from: routeValue(c.field, c.old, preview.names),
    to: routeValue(c.field, c.new, preview.names),
  }));
}

/** Counts per status: `{ added, updated, unchanged }`. */
export function statusCounts(preview) {
  const out = { added: 0, updated: 0, unchanged: 0 };
  for (const s of preview?.services ?? []) out[s.status] += 1;
  return out;
}

/** Ids that may be given a key in the dialog: the file's `needs_key`. */
export function keyIds(preview) {
  return preview?.needs_key ?? [];
}

/** Only the typed (non-empty, trimmed) keys, by id. */
export function typedKeys(preview, keys) {
  const out = {};
  for (const id of keyIds(preview)) {
    const v = (keys?.[id] ?? "").trim();
    if (v) out[id] = v;
  }
  return out;
}

/** Vocabulary line of the preview, or "" when the file has none. */
export function vocabText(preview) {
  if (!preview) return "";
  const parts = [];
  if (preview.vocab_new) parts.push(`新增 ${preview.vocab_new} 個詞`);
  if (preview.vocab_blocked) parts.push(`${preview.vocab_blocked} 個詞因重複上限或格式不符而略過`);
  return parts.join("，");
}

/** What the apply step did, one sentence. */
export function appliedText(a) {
  if (!a) return "";
  const parts = [];
  if (a.added) parts.push(`新增 ${a.added} 個服務`);
  if (a.updated) parts.push(`更新 ${a.updated} 個服務`);
  if (a.unchanged) parts.push(`${a.unchanged} 個服務不變`);
  if (a.route_changed) parts.push(`變更 ${a.route_changed} 項用途設定`);
  if (a.vocab_added) parts.push(`詞庫新增 ${a.vocab_added} 個詞`);
  return parts.length ? parts.join("、") : "沒有任何變更";
}

/** Notes after an export: services left out and routes reset to local. */
export function exportNotes(r) {
  const notes = [];
  if (r?.skipped?.length) {
    notes.push(`有 ${r.skipped.length} 個服務因為不符合檔案格式而沒有匯出：${r.skipped.join("、")}`);
  }
  if (r?.route_reset?.length) {
    const fields = r.route_reset.map((f) => routeFieldLabel[f] ?? f).join("、");
    notes.push(`${fields}原本指向無法匯出的服務，檔案中已改為本機`);
  }
  return notes;
}
