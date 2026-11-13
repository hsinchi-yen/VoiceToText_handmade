// Calls to src-tauri/src/commands/presets.rs. Errors come back as short codes (never keys).
import { invoke } from "@tauri-apps/api/core";
import * as cloud from "../cloud-setup/api.js";

const errorText = {
  forbidden: "只能在主視窗套用預設組態",
  preset_unknown: "找不到這個預設組態",
  preset_invalid: "預設組態檔格式不正確（缺少 [preset]、服務代號，或不是有效的 TOML）",
  preset_id_prefix: "服務代號必須以 id_prefix 開頭，且 id_prefix 不可為空或以 openai／gemini／claude／custom／local 開頭",
  preset_id_conflict: "預設組態的服務代號與你現有的其他類型服務重複，已拒絕以免覆蓋它",
  preset_duplicate_id: "預設組態內有重複的服務代號",
  preset_kind: "預設組態只能使用「自訂相容端點」（open_ai_compat）",
  preset_bad_reference: "預設組態內的語音辨識選項、需金鑰清單或用途指向不存在的服務，或缺少所需的模型名稱",
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
  // Configuration files (T5.2): codes from v2c_core::portable and config_io.rs.
  file_not_toml: "請選擇 .toml 檔案",
  file_unreadable: "無法讀取這個檔案（不能是網路位置，且要是 UTF-8 文字檔）",
  file_changed: "檔案在預覽後被改過，請重新匯入",
  too_large: "檔案太大（上限 256 KB）",
  syntax: "檔案不是有效的 TOML，請檢查括號與引號",
  invalid_value: "有欄位的值型別不對或不被允許",
  missing_field: "缺少必要欄位",
  unknown_kind: "服務種類 kind 不認得（可用：gemini、open_ai、claude、open_ai_compat）",
  format_missing: "檔案缺少 [v2c] 的 format 版本",
  format_invalid: "format 不是有效的版本數字",
  format_too_new: "檔案由較新版本的 v2c 產生，請先更新 v2c",
  id_invalid: "服務代號只能用英數、-、_，前後不可有空白",
  id_reserved: "服務代號是保留字（例如 local），請換一個",
  route_unknown_service: "[route] 指向的服務不在檔案裡（只能是 local 或檔內服務）",
  route_no_asr: "[route] 的語音辨識指向了不支援語音辨識的服務（例如 Claude）",
  route_no_model: "[route] 指向的服務沒有對應用途的模型",
  vocab_failed: "無法讀寫詞庫檔，設定沒有變更",
  export_invalid: "匯出內容檢查未通過，沒有寫出檔案",
  write_failed: "無法寫入檔案，請換個位置或檢查權限",
  config_invalid: "設定檔 config.toml 格式有誤，請先修正",
  config_unreadable: "無法讀取設定檔",
  config_save_failed: "無法儲存設定檔，設定沒有變更",
  keyring_failed: "無法存取金鑰庫，設定沒有變更",
  internal_error: "內部錯誤",
};

export function describe(code) {
  return errorText[code] ?? cloud.describe(code);
}

/** `source` is `{ builtin: "TN" | "RD" }`. */
export const list = () => invoke("presets_list");
export const preview = (source) => invoke("presets_preview", { source });
export const apply = (source, asrChoice, keys) =>
  invoke("presets_apply", { source, asrChoice, keys });

export const purposeLabel = { asr: "語音辨識", polish: "潤飾", translate: "中翻英" };
export const sourceLabel = { detected: "自動偵測", file: "預設值" };
