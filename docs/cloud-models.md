# 商用雲端模型：推薦選擇與設定

v2c 預設全部在本機執行；雲端是選用的。這份文件說明 Google Gemini、OpenAI、Anthropic Claude 各推薦哪個模型、怎麼設定。取得金鑰與精靈的每一步，見 [cloud-setup.md](cloud-setup.md)。

> 模型名稱與價格以 **2026 年 11 月**為準。雲端服務常推出新模型、淘汰舊模型；精靈的「選擇模型」步驟若有下拉清單，以清單上實際存在的名稱為準。

## 1. 先決定要交給雲端的用途

| 用途 | 本機（預設） | 交給雲端的好處 | 能用的雲端服務 |
|------|--------------|----------------|----------------|
| 語音辨識 | Breeze-ASR-25（GPU）／SenseVoice（CPU） | 電腦沒有 NVIDIA 顯示卡時更準 | Gemini、OpenAI（Claude **不能**辨識語音） |
| 潤飾 | LM Studio，沒開時只做規則式整理 | 不用開 LM Studio、品質穩定 | Gemini、OpenAI、Claude |
| 中翻英 | LM Studio | 翻譯品質明顯較好 | Gemini、OpenAI、Claude |

建議：**辨識維持本機**（Breeze 在 RTX 2060 實測約 0.6 秒、字錯率 2.9%，而且聲音不離開電腦），**只把潤飾與中翻英交給雲端**。v2c 的「指定用途」可以每一項分開指定不同的服務。

## 2. 推薦模型

速度目標：放開按鍵到出字，雲端 ≤ 3 秒（含網路）。潤飾與翻譯的句子很短，選**快的小模型**就夠了；大模型的品質提升有限，卻常常超過時間上限。

### Anthropic Claude（只能潤飾、中翻英）

| 選擇 | 模型 ID | 價格（每百萬 token，輸入／輸出） | 說明 |
|------|---------|-------------------------------|------|
| **推薦（精靈預設）** | `claude-haiku-5-5` | US$0.10／US$0.50 | 最快、最便宜，潤飾與翻譯品質足夠 |
| 品質優先 | `claude-sonnet-5-5` | US$2／US$10 | 長篇口述、正式文件的翻譯較自然；較慢 |
| 不建議 | `claude-opus-5-5` 等更大的模型 | US$4／US$20 起 | 對短句潤飾沒有明顯差別，延遲常超過 3 秒 |

注意：這些 Claude 模型預設會先做一小段「思考」再回答（Haiku 5.5 預設強度為 medium）。v2c 目前沒有提供調整這個強度的設定，所以偶爾會比預期慢。如果常看到「逾時：服務回應太慢」或「潤飾逾時，已改用基本整理」，把 `config.toml` 的 `[timeouts] llm_cloud_ms` 從 3000 調到 5000（Sonnet 5.5 建議 6000），並把 `[polish] timeout_ms` 從 1000 調到 4000（潤飾另有自己的上限）。手動改設定檔後要重新啟動 v2c 才會生效。

### OpenAI（語音辨識、潤飾、中翻英）

| 用途 | 精靈預設模型 | 說明 |
|------|--------------|------|
| 語音辨識 | `gpt-transcribe` | 支援中英夾雜；v2c 不指定語言，讓模型自己判斷 |
| 潤飾、中翻英 | `gpt-6-luna` | 快速的小模型 |

要更換時，在精靈第 4 步的下拉清單裡選；原則是語音辨識選名稱含 `transcribe` 的模型、文字選標示為快速／小型（mini、nano 之類）的模型。

### Google Gemini（語音辨識、潤飾、中翻英）

| 用途 | 精靈預設模型 | 說明 |
|------|--------------|------|
| 語音辨識 | `gemini-3.6-flash` | 一個金鑰同時做辨識與文字；音訊以 WAV 直接送出 |
| 潤飾、中翻英 | `gemini-3.6-flash` | 同上 |

選擇原則：用名稱含 **Flash**（或更輕的 Flash-Lite）的模型；Pro 系列較慢，不適合即時輸入。免費方案的資料可能被 Google 用於改善服務，在意隱私請用付費方案。

## 3. 推薦組合

| 情境 | 語音辨識 | 潤飾 | 中翻英 |
|------|----------|------|--------|
| **有 NVIDIA 顯示卡（建議）** | 本機 Breeze | Claude `claude-haiku-5-5` | Claude `claude-haiku-5-5` |
| 沒有顯示卡、想要準確 | Gemini `gemini-3.6-flash` 或 OpenAI `gpt-transcribe` | 同一家的文字模型 | 同一家的文字模型 |
| 只想付一家 | OpenAI 或 Gemini 全部三項 | ← | ← |
| 翻譯品質優先 | 本機 Breeze | Claude `claude-haiku-5-5` | Claude `claude-sonnet-5-5` |
| 公司內有自架伺服器 | 見 [lan-server-guide.md](lan-server-guide.md) | ← | ← |

## 4. 設定方式

### 用精靈（建議）

「服務 → 新增服務」：選服務 → 貼金鑰 → **選模型**（預設已填上第 2 節的推薦模型）→ 測試連線 → 勾選用途 → 儲存。同一家服務要用兩個不同的模型（例如 Haiku 潤飾、Sonnet 翻譯），就新增兩次，各取不同名稱。

### 直接改設定檔

`%APPDATA%\v2c\config.toml`。金鑰**不能**寫在這裡，請先用精靈貼上（存在 Windows 認證管理員）；下面只示範模型與用途：

```toml
[route]
asr = "local"                 # 辨識維持本機
polish = "claude"
translate = "claude-sonnet"
cloud_fallback_to_local = true   # 雲端失敗時自動改用本機

[[cloud]]
id = "claude"
kind = "claude"
llm_model = "claude-haiku-5-5"

[[cloud]]
id = "claude-sonnet"
kind = "claude"
llm_model = "claude-sonnet-5-5"

[timeouts]
asr_cloud_ms = 5000
llm_cloud_ms = 5000
```

`kind` 的值：`open_ai`、`gemini`、`claude`、`open_ai_compat`（自訂相容端點）。不填 `asr_model`／`llm_model` 時使用第 2 節的精靈預設模型。

## 5. 費用估算

以每天口述 200 句、每句約 40 個中文字估算（潤飾一次約 600 個輸入 token、100 個輸出 token，含提示詞）：

| 模型 | 每天約 | 每月約（30 天） |
|------|--------|-----------------|
| `claude-haiku-5-5` | 12 萬輸入＋2 萬輸出 token ≈ US$0.02 | 約 US$0.6 |
| `claude-sonnet-5-5` | ≈ US$0.44 | 約 US$13 |

實際 token 數依句子長度與詞庫大小而定；以各服務帳單頁面的實際用量為準。語音辨識依音訊長度計費，請看 OpenAI／Google 的價格頁。

## 6. 隱私

- 交給雲端的用途，你的**聲音或文字會送到該公司的伺服器**；只勾選你願意送出的用途。
- 「不記錄的 App」（例如密碼管理器）只影響輸入紀錄，**不會**阻止送到雲端；在密碼欄位請不要用語音輸入。
- v2c 自己的日誌永遠不記錄辨識或潤飾後的文字。
