// Calls to src-tauri/src/commands/cloud.rs. Errors come back as short codes (never the key).
import { invoke } from "@tauri-apps/api/core";
import { pick } from "../platform.js";

const errorText = {
  forbidden: "只能在主視窗設定雲端服務",
  key_missing: "請貼上 API 金鑰",
  invalid_key: "金鑰格式不正確（請只貼上金鑰本身）",
  base_url_required: "請填入端點網址",
  invalid_base_url: "端點網址不正確：官方服務必須是 https:// 開頭",
  model_required: "請填入模型名稱",
  name_too_long: "名稱太長：最多 32 個字",
  name_taken: "這個名稱已經被其他服務使用（含代號，不分大小寫），請換一個",
  invalid_name: "名稱不能包含換行或控制字元",
  name_reserved: "「local」是保留字（代表地端），請換一個名稱",
  asr_timeout_invalid: "語音辨識逾時請填整數毫秒（例如 8000），或留空使用預設值",
  asr_timeout_range: "語音辨識逾時要在 1000～60000 毫秒之間",
  unsupported_purpose: "這個服務不支援所選的用途",
  not_found: "找不到這個雲端服務（可能已被移除）",
  keyring_failed: pick("無法存取 Windows 認證管理員（金鑰庫）", "無法存取 GNOME 鑰匙圈（金鑰庫；請確認已登入桌面且鑰匙圈已解鎖）"),
  config_invalid: "設定檔 config.toml 格式有誤，請先修正",
  config_unreadable: "無法讀取設定檔",
  config_save_failed: "無法儲存設定檔",
  open_failed: "無法開啟瀏覽器",
  test_failed: "測試失敗",
  internal_error: "內部錯誤",
};

export function describe(code) {
  return errorText[code] ?? `錯誤：${code}`;
}

/** Strips empty strings so the backend sees "not set". */
function clean(draft) {
  const out = { kind: draft.kind };
  for (const k of ["id", "name", "base_url", "api_key", "asr_model", "llm_model", "asr_timeout_ms"]) {
    const v = typeof draft[k] === "string" ? draft[k].trim() : draft[k];
    if (v) out[k] = v;
  }
  return out;
}

export const kinds = () => invoke("cloud_kinds");
export const list = () => invoke("cloud_list");
export const models = (draft) => invoke("cloud_models", { draft: clean(draft) });
export const test = (draft) => invoke("cloud_test_connection", { draft: clean(draft) });
export const save = (draft, purposes) => invoke("cloud_save", { draft: clean(draft), purposes });
export const remove = (id) => invoke("cloud_remove", { id });
export const openKeyPage = (kind) => invoke("cloud_open_key_page", { kind });

/** Longest name, in characters (backend `NAME_MAX_CHARS`). */
export const NAME_MAX_CHARS = 32;

/**
 * The one display rule (backend `display_name`): the user's name, else the id
 * (`custom`, `custom-2`, ...). Takes a `cloud_list` row or a `[[cloud]]` entry.
 */
export function displayName(s) {
  if (!s) return "";
  if (s.display_name) return s.display_name;
  return shownName(s) || s.id;
}

/** The name that counts: compatible endpoints only, trimmed ("" = none). */
function shownName(s) {
  if (s.kind !== "open_ai_compat" || typeof s.name !== "string") return "";
  return s.name.trim();
}

/**
 * The id a new compatible endpoint will get (same rule as backend `new_id`): skips ids in
 * use and ids equal to a service's name (case-insensitive).
 */
export function nextCustomId(services) {
  const taken = new Set(["local", ...services.map((s) => s.id)]);
  const names = new Set(services.map((s) => shownName(s).toLowerCase()).filter(Boolean));
  const isTaken = (id) => taken.has(id) || names.has(id.toLowerCase());
  if (!isTaken("custom")) return "custom";
  for (let n = 2; ; n++) if (!isTaken(`custom-${n}`)) return `custom-${n}`;
}

/** Client-side copy of the backend name checks; returns an error code or "". */
export function nameProblem(name, id, services) {
  const n = (name ?? "").trim();
  if (!n) return "";
  if ([...n].length > NAME_MAX_CHARS) return "name_too_long";
  // Same as Rust `char::is_control` (Unicode Cc).
  if (/\p{Cc}/u.test(n)) return "invalid_name";
  const low = n.toLowerCase();
  if (low === "local") return "name_reserved";
  const clash = services.some(
    (s) => s.id !== id && (s.id.toLowerCase() === low || shownName(s).toLowerCase() === low),
  );
  return clash ? "name_taken" : "";
}

/** Per-service speech recognition timeout range, ms (backend `ASR_TIMEOUT_MIN_MS`/`MAX`). */
export const ASR_TIMEOUT_MIN_MS = 1000;
export const ASR_TIMEOUT_MAX_MS = 60000;

/** Client-side copy of the backend `parse_asr_timeout`; returns an error code or "". */
export function asrTimeoutProblem(value) {
  const v = String(value ?? "").trim();
  if (!v) return "";
  if (!/^-?\d+$/.test(v)) return "asr_timeout_invalid";
  const n = Number(v);
  return n < ASR_TIMEOUT_MIN_MS || n > ASR_TIMEOUT_MAX_MS ? "asr_timeout_range" : "";
}

/** Same rule as the backend (`ApiKey::last4`): short keys show nothing. */
export function last4(key) {
  const k = (key ?? "").trim();
  return k.length >= 12 ? k.slice(-4) : "";
}

export const purposeLabels = { asr: "語音辨識", polish: "潤飾", translate: "中翻英" };
