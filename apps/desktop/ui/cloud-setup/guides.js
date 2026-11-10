// Wizard step 2: how to get a key, per kind (keep in sync with docs/cloud-setup.md).
export const guides = {
  gemini: [
    "用 Google 帳號登入 Google AI Studio。",
    "開啟「API Keys」頁面，按「Create API key」。",
    "複製產生的金鑰（通常以 AIza 開頭）。",
  ],
  open_ai: [
    "登入 OpenAI Platform（與 ChatGPT 訂閱是分開計費的）。",
    "在「Billing」先儲值，否則金鑰無法使用。",
    "開啟「API keys」頁面，按「Create new secret key」，複製金鑰（以 sk- 開頭，只會顯示一次）。",
  ],
  claude: [
    "登入 Claude Console（platform.claude.com）。",
    "在「Billing」先儲值。",
    "開啟「API keys」頁面，按「Create Key」，複製金鑰（以 sk-ant- 開頭，只會顯示一次）。",
  ],
  open_ai_compat: [
    "填入端點網址，要包含 /v1，例如區網伺服器 http://10.0.0.5:8000/v1，或別台電腦的 LM Studio http://192.168.1.20:1234/v1。",
    "選擇這個端點提供什麼：文字模型（潤飾、中翻英）、語音辨識，或兩者都有。",
    "如果伺服器需要金鑰，下一步再貼上；不需要就留空。",
  ],
};

// Custom endpoints: what one endpoint serves. One endpoint = one service; text and speech on
// different ports are added as two services.
export const roles = {
  text: {
    label: "文字模型（潤飾、中翻英）",
    hint: "伺服器提供 /v1/chat/completions，例如 llama.cpp、Ollama、vLLM、LM Studio。",
  },
  asr: {
    label: "語音辨識",
    hint: "伺服器提供 /v1/audio/transcriptions，例如 whisper.cpp 的 whisper-server、Breeze-ASR 服務。",
  },
  both: {
    label: "兩者都有（同一個網址）",
    hint: "同一個網址同時提供文字模型與語音辨識。",
  },
};

export const otherRole = { text: "asr", asr: "text", both: null };

export const billingWarning = "雲端服務會依用量向你收費，錄音或文字會上傳到該服務。";
export const lanNote =
  "區網或自架伺服器不會收費，但錄音或文字會送到那台伺服器；http:// 在網路上是不加密的，請只在信任的網路使用。";

