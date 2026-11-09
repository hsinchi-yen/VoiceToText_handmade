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
    "填入端點網址，例如別台電腦的 LM Studio：http://192.168.1.20:1234/v1（要包含 /v1）。",
    "LM Studio 要在「Developer」頁啟動伺服器，並在伺服器設定開啟「Serve on Local Network」。",
    "如果你的服務需要金鑰，下一步再貼上；不需要就留空。",
  ],
};

export const billingWarning = "雲端服務會依用量向你收費，錄音或文字會上傳到該服務。";
