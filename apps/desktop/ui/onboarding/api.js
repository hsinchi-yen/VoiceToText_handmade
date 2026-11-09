// Calls to src-tauri/src/commands/{onboarding,models}.rs. Errors are short codes.
import { invoke } from "@tauri-apps/api/core";

const errorText = {
  forbidden: "只能在主視窗操作",
  model_not_hosted: "這個模型還沒有提供下載，請用「從本機檔案匯入」",
  already_running: "已經在進行中",
  not_running: "沒有進行中的下載",
  not_found: "找不到這個項目",
  insecure_url: "下載網址必須是 https://",
  redirect_refused: "下載被轉到不安全的網址，已停止",
  network: "網路連線中斷，可以按「繼續下載」從中斷處接著下載",
  download_http_error: "下載伺服器回應錯誤，請稍後再試",
  download_url_invalid: "下載網址不正確（models.json）",
  download_client_failed: "無法建立下載連線",
  hash_mismatch: "檔案驗證失敗（SHA-256 不符），已刪除，請重新下載",
  size_mismatch: "檔案大小不符，已刪除，請重新下載",
  archive_invalid: "壓縮檔內容不正確",
  manifest_invalid: "模型清單 models.json 格式有誤",
  storage: "無法寫入模型資料夾（磁碟空間不足或沒有權限？）",
  not_ggml: "這不是 whisper.cpp 的 GGML 模型檔（請選 tools/convert-breeze 產生的 .bin）",
  ggml_unsupported_quant: "只支援 q5_0 或 q8_0 版本（f16 版本太大，請先量化）",
  cancelled: "已暫停，可以稍後繼續",
  internal_error: "內部錯誤",
};

export function describe(code) {
  return errorText[code] ?? `錯誤：${code}`;
}

export const status = () => invoke("onboarding_status");
export const models = () => invoke("models_status");
export const download = (id) => invoke("models_download", { id });
export const cancel = (id) => invoke("models_cancel", { id });
export const importBreeze = () => invoke("models_import_breeze");
export const restart = () => invoke("onboarding_restart");
export const openDoc = (name) => invoke("open_doc", { name });

/** Where onboarding resumes after the restart that loads new models. */
export const RESUME_KEY = "v2c.onboarding.resume";

export function resumeStep() {
  try {
    return localStorage.getItem(RESUME_KEY);
  } catch {
    return null;
  }
}

export function setResumeStep(step) {
  try {
    if (step) localStorage.setItem(RESUME_KEY, step);
    else localStorage.removeItem(RESUME_KEY);
  } catch {
    // Storage unavailable: onboarding then simply starts over if needed.
  }
}

export function mb(bytes) {
  return bytes == null ? "?" : `${Math.round(bytes / 1048576)} MB`;
}
