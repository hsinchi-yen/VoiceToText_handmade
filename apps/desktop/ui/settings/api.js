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
  mic_not_found: "找不到麥克風",
  mic_open_failed: "麥克風無法開啟（可能被其他程式獨占，或 Windows 隱私權設定不允許）",
  mic_format_unsupported: "這個麥克風的格式不支援測試",
  already_running: "正在進行中",
  busy: "正在聽寫中，請說完再測試",
  asr_switching: "正在切換語音模型，請稍候再測試",
  asr_loading: "語音模型還在載入，請稍後再試",
  model_missing: "沒有安裝地端語音模型，請到「初次設定」下載或匯入",
  model_load_failed: "語音模型無法載入（原因見記錄檔）",
  sensevoice_not_available: "SenseVoice 無法載入（原因見記錄檔）",
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
/** Validates a draft without saving; returns `{ settings, unchanged }`. */
export const preview = (changes) => invoke("settings_preview", { changes });
export const runtimeStatus = () => invoke("settings_runtime_status");
export const testLocalAsr = () => invoke("settings_test_local_asr");
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

/**
 * Local speech engines, shared by the 潤飾與用途 and 地端語音模型 menus (same value, same
 * text). The suffix says when the engine cannot run in this install.
 */
export function localAsrOptions(view) {
  const breezeInstalled = view.config.local.breeze_quant === "q8_0" ? view.breeze_q8_0 : view.breeze_q5_0;
  const breezeNote = !breezeInstalled ? "（未安裝）" : !view.cuda_build ? "（此版本無 CUDA）" : "";
  return [
    {
      value: "auto",
      label: "自動（有 NVIDIA 顯示卡用 Breeze，否則 SenseVoice）" +
        (!breezeInstalled && !view.sensevoice ? "（未安裝）" : ""),
    },
    { value: "breeze", label: "Breeze-ASR-25（NVIDIA 顯示卡）" + breezeNote },
    {
      value: "tea_asr",
      label: "TEA-ASR-1.1（NVIDIA 顯示卡）" +
        (!view.tea_asr ? "（未安裝）" : !view.tea_build ? "（此版本無 CUDA）" : ""),
    },
    { value: "sense_voice", label: "SenseVoice-Small（CPU）" + (view.sensevoice ? "" : "（未安裝）") },
  ];
}

/** Changes for a 語音辨識 menu value: `local:<engine>` or a cloud service id. */
export function asrChoiceChanges(value) {
  if (value.startsWith("local:")) {
    return [
      { field: "route_asr", value: "local" },
      { field: "local_asr", value: value.slice("local:".length) },
    ];
  }
  return [{ field: "route_asr", value }];
}

/** Parses a one-per-line / comma separated list of executables. */
export function parseExeList(text) {
  return text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean);
}
