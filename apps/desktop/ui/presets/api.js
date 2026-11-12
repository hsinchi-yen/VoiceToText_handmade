// Calls to src-tauri/src/commands/presets.rs. Errors come back as short codes (never keys).
import { invoke } from "@tauri-apps/api/core";
import * as cloud from "../cloud-setup/api.js";

const errorText = {
  forbidden: "只能在主視窗套用預設組態",
  preset_unknown: "找不到這個預設組態",
  preset_not_toml: "請選擇 .toml 檔案",
  preset_unreadable: "無法讀取這個檔案",
  preset_too_large: "檔案太大（上限 64 KB）",
  preset_invalid: "預設組態檔格式不正確（缺少 [preset]、服務代號，或不是有效的 TOML）",
  preset_id_prefix: "服務代號必須以 id_prefix 開頭",
  preset_duplicate_id: "預設組態內有重複的服務代號",
  preset_kind: "預設組態只能使用「自訂相容端點」（open_ai_compat）",
  preset_bad_reference: "預設組態內的語音辨識選項、需金鑰清單或用途指向不存在的服務",
  preset_bad_choice: "請選擇預設組態提供的語音辨識選項",
  preset_key_not_allowed: "這個服務不需要金鑰",
  invalid_key: "金鑰格式不正確（請只貼上金鑰本身）",
  invalid_base_url: "預設組態內的端點網址不正確",
  base_url_required: "預設組態內有服務沒有端點網址",
  name_taken: "預設組態內的名稱與你現有的其他服務重複，請先修改或移除該服務",
  name_too_long: "預設組態內的名稱太長（最多 32 個字）",
  invalid_name: "預設組態內的名稱含有不允許的字元",
  name_reserved: "預設組態內使用了保留名稱「local」",
  asr_timeout_invalid: "預設組態內的語音辨識逾時不是整數毫秒",
  asr_timeout_range: "預設組態內的語音辨識逾時要在 1000～60000 毫秒之間",
  config_invalid: "設定檔 config.toml 格式有誤，請先修正",
  config_unreadable: "無法讀取設定檔",
  config_save_failed: "無法儲存設定檔，設定沒有變更",
  keyring_failed: "無法存取金鑰庫，設定沒有變更",
  internal_error: "內部錯誤",
};

export function describe(code) {
  return errorText[code] ?? cloud.describe(code);
}

/** `source` is `{ builtin: "TN" | "RD" }` or `{ file: path }`. */
export const list = () => invoke("presets_list");
export const preview = (source) => invoke("presets_preview", { source });
export const apply = (source, asrChoice, keys) =>
  invoke("presets_apply", { source, asrChoice, keys });
/** Native file dialog; resolves to a path or null. */
export const pickFile = () => invoke("presets_pick_file");

/** Runs the existing connection test against one applied service (its stored key is used). */
export function testService(s) {
  return cloud.test({
    id: s.id,
    kind: "open_ai_compat",
    name: s.name,
    base_url: s.base_url,
    asr_model: s.asr_model,
    llm_model: s.llm_model,
    asr_timeout_ms: s.asr_timeout_ms,
  });
}

export const purposeLabel = { asr: "語音辨識", polish: "潤飾", translate: "中翻英" };
export const sourceLabel = { detected: "自動偵測", file: "預設值" };
