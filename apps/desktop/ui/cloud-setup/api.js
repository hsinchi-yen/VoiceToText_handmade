// Calls to src-tauri/src/commands/cloud.rs. Errors come back as short codes (never the key).
import { invoke } from "@tauri-apps/api/core";

const errorText = {
  forbidden: "只能在主視窗設定雲端服務",
  key_missing: "請貼上 API 金鑰",
  invalid_key: "金鑰格式不正確（請只貼上金鑰本身）",
  base_url_required: "請填入端點網址",
  invalid_base_url: "端點網址不正確：官方服務必須是 https:// 開頭",
  model_required: "請填入模型名稱",
  unsupported_purpose: "這個服務不支援所選的用途",
  not_found: "找不到這個雲端服務（可能已被移除）",
  keyring_failed: "無法存取 Windows 認證管理員（金鑰庫）",
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
  for (const k of ["id", "base_url", "api_key", "asr_model", "llm_model"]) {
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

/** Same rule as the backend (`ApiKey::last4`): short keys show nothing. */
export function last4(key) {
  const k = (key ?? "").trim();
  return k.length >= 12 ? k.slice(-4) : "";
}

export const purposeLabels = { asr: "語音辨識", polish: "潤飾", translate: "中翻英" };
