// Calls to src-tauri/src/commands/{settings,gpu,onboarding}.rs. Errors are short codes.
import { invoke } from "@tauri-apps/api/core";

const errorText = {
  forbidden: "只能在主視窗修改設定",
  out_of_range: "數值超出允許範圍",
  hotkey_unsupported: "目前只支援右 Ctrl（中翻英：右 Ctrl + Shift）",
  route_unknown_service: "找不到這個雲端服務，請到「雲端服務」頁重新串接",
  route_no_asr: "這個服務不支援語音辨識（例如 Claude）",
  invalid_exe: "請輸入執行檔名稱，例如 LINE.exe（不含資料夾路徑）",
  duplicate_app: "這個 App 已經有規則了",
  app_rules_empty: "至少要保留一條 App 規則",
  too_many: "項目太多",
  invalid_url: "網址格式不正確，例如 http://localhost:1234/v1",
  invalid_text: "內容含有不允許的字元",
  config_invalid_not_saved: "設定檔 config.toml 格式有誤，沒有儲存（請先修正檔案）",
  config_unreadable: "無法讀取設定檔",
  config_save_failed: "無法儲存設定檔",
  autostart_failed: "無法變更開機自動啟動",
  unreachable: "連不到 LM Studio：請確認已啟動並開啟 Local Server",
  timeout: "LM Studio 沒有回應（逾時）",
  http_error: "LM Studio 回應錯誤：網址是否少了 /v1？",
  bad_response: "回應格式不是 OpenAI 相容的 /models",
  mic_not_found: "找不到麥克風",
  mic_open_failed: "麥克風無法開啟（可能被其他程式獨占，或 Windows 隱私權設定不允許）",
  mic_format_unsupported: "這個麥克風的格式不支援測試",
  already_running: "正在進行中",
  internal_error: "內部錯誤",
};

export function describe(code) {
  return errorText[code] ?? `錯誤：${code}`;
}

export const get = () => invoke("settings_get");
/** One or more changes, e.g. `[{ field: "polish_enabled", value: false }]`. */
export const apply = (changes) => invoke("settings_apply", { changes });
export const set = (field, value) => apply([{ field, value }]);
export const setAutostart = (enabled) => invoke("settings_set_autostart", { enabled });
export const inputDevices = () => invoke("settings_input_devices");
export const testLm = (baseUrl) => invoke("settings_test_lm", { baseUrl });
export const gpuUsage = () => invoke("gpu_usage");
export const micTestStart = (device) => invoke("mic_test_start", { device: device ?? null });
export const micTestStop = () => invoke("mic_test_stop");
export const openDoc = (name) => invoke("open_doc", { name });

export const kindLabels = {
  open_ai: "OpenAI",
  gemini: "Gemini",
  claude: "Claude",
  open_ai_compat: "自訂端點",
};

/** Parses a one-per-line / comma separated list of executables. */
export function parseExeList(text) {
  return text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean);
}
