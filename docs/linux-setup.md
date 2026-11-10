# Linux 設定說明（Phase 3，開發中）

這份文件說明在 Linux（目前以 Ubuntu 22.04／GNOME 42 Wayland 開發）上，怎麼把 v2c 的**潤飾**、**中翻英**接到你已經架好的 OpenAI 相容文字模型伺服器（例如 llama.cpp `llama-server`、vLLM、Ollama、另一台電腦的 LM Studio），以及語音辨識有哪些選項。

> 狀態：Linux 版桌面 App 已可在這台機器編譯、啟動（T3.6，見第 6 節），但**還沒有實機按住說話驗證**（沒有麥克風、v2c 輸入法元件尚未安裝）。下面的設定格式和 Windows 版共用同一份 `config.toml`，已用自動測試驗證（`crates/v2c-platform/tests/linux_*.rs`）。Linux 上設定檔在 `~/.config/v2c/config.toml`（見第 6.1 節）。手動改設定檔後要重新啟動 v2c 才會生效。

文中的 `192.168.1.50` 是範例 IP，請換成你伺服器的位址。

## 1. 這台開發機的限制

| 項目 | 狀況 | 影響 |
|------|------|------|
| 顯示卡 | 沒有 NVIDIA（Intel 內顯） | 不能用 Breeze-ASR-25（需要 CUDA）；設定 `[gpu] enabled = false` |
| 麥克風 | 沒有任何錄音裝置 | 無法實機錄音；目前只能用模擬音訊與 mock 語音辨識測試 |
| 建置工具 | 2026-11-10 已安裝 cmake、clang、webkit2gtk-4.1 等（`bash tools/build-linux.sh --check` 通過） | 整個 workspace（含 Tauri 桌面 App、whisper-rs CPU 版）可以編譯與測試 |
| 熱鍵／注入 | GNOME 42 沒有 GlobalShortcuts、Clipboard portal | 見 `tasks/plan.md` 第 9 節 |
| 既有文字模型 | llama.cpp，Qwen3 27B，不需金鑰 | 可做潤飾、中翻英；**不能做語音辨識**（沒有音訊能力，`/v1/audio/transcriptions` 回 501） |

## 2. 潤飾、中翻英接既有的文字模型

有兩種寫法，選一種即可。

### 方法 A：`[local]`（當成「本機的 LM Studio」）

```toml
[local]
llm_base_url = "http://192.168.1.50:8000/v1"   # 伺服器的 OpenAI 相容 API 根目錄（含 /v1）
llm_model = "Qwen3.8-27B-UD-Q6_K_L"            # 伺服器的模型 id；不填 = 伺服器目前載入的模型

[polish]
enabled = true
timeout_ms = 1000      # 潤飾超過這個時間就改用規則式去贅字

[gpu]
enabled = false        # 沒有 NVIDIA：關閉顯示卡記憶體管理
```

- 不送金鑰（`[local]` 沒有金鑰欄位）。
- 潤飾的時間預算是 `[polish] timeout_ms`（預設 1 秒，長句會依字數加長，最多 8 秒）；中翻英最多等 8 秒。
- 伺服器連不上或出錯時：潤飾改用規則式去贅字後照樣輸出；中翻英回報錯誤（不會把中文原文當成翻譯輸出）。

### 方法 B：`[[cloud]]` 自訂相容端點

```toml
[route]
polish = "lan-llm"
translate = "lan-llm"
cloud_fallback_to_local = true    # 這個服務失敗時改用地端（[local]）

[timeouts]
llm_cloud_ms = 3000               # 潤飾、中翻英的時間上限

[gpu]
enabled = false

[[cloud]]
id = "lan-llm"                    # 自己取的名稱；[route] 用它指定
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8000/v1"
llm_model = "Qwen3.8-27B-UD-Q6_K_L"   # 自訂端點沒有預設模型，一定要填
```

- 自訂端點可以用 `http://`（區網伺服器）；需要金鑰時在設定視窗的「雲端服務」頁貼上，存在系統金鑰庫，不寫進設定檔。不需要金鑰就留空。
- 走這條路時，潤飾的時間預算是 `[timeouts] llm_cloud_ms`（預設 3 秒），比方法 A 的 1 秒寬鬆，比較適合較慢的大模型。

### 會「思考」的模型（Qwen3 等）

這台開發機的 llama.cpp 預設會先「思考」再回答（回應裡有獨立的 `reasoning_content` 欄位；v2c 只取 `content`，也會去掉夾在內容裡的 `<think>…</think>`）。2026-11-10 實測（合成測試句，思考開啟）：

| 項目 | 結果 |
|------|------|
| 潤飾（直接呼叫，等 15 秒） | 成功，約 2.5～2.7 秒 |
| 潤飾（經過管線，預設 1 秒預算） | **逾時**，改用規則式去贅字 |
| 中翻英 | **時好時壞**：思考用掉的 token 會吃掉 v2c 給的輸出上限（約「字數×3＋64」），回應被截斷（`finish_reason=length`、`content` 為空），v2c 回報錯誤；成功時約 3.7 秒 |

關閉思考（請求加上 `"chat_template_kwargs": {"enable_thinking": false}`）實測約 0.23 秒。

> **0.1.3 起**：OpenAI 相容請求已一律送出 `chat_template_kwargs: {"enable_thinking": false}`，不需要設定。上表是尚未包含該修改時的實測結果；套用後同一組測試：潤飾直接呼叫約 0.66 秒、經管線約 0.77 秒（1 秒預算內，沒有退回）、中翻英約 0.66～1.0 秒，全部成功。

#### 待核准：可設定的額外請求欄位

如果之後需要讓使用者自己決定（例如某台伺服器不接受未知欄位，或想對某個模型保留思考），提案如下。這屬於 `v2c-core` 公開設定的變更，**待核准，尚未實作**：

```toml
[local]
llm_base_url = "http://192.168.1.50:8000/v1"
llm_model = "Qwen3.8-27B-UD-Q6_K_L"
# 原樣併入 /chat/completions 請求本文的 JSON 物件（不可覆蓋 model、messages、max_tokens、stream）
llm_extra_body = { chat_template_kwargs = { enable_thinking = false } }

[[cloud]]
id = "lan-llm"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8000/v1"
llm_model = "Qwen3.8-27B-UD-Q6_K_L"
llm_extra_body = { chat_template_kwargs = { enable_thinking = false } }
```

- 欄位：`LocalConfig::llm_extra_body` 與 `CloudService::llm_extra_body`，型別 `Option<toml::Table>`（序列化成 JSON 物件），只用於 `open_ai_compat`／`[local]`。
- 預設：不填 = 使用內建預設（0.1.3 起為關閉思考）；填 `{}` = 不送任何額外欄位。
- `OpenAiCompatLlm` 需要新增一個建構選項（例如 `with_extra_body(serde_json::Map)`），屬公開 API 變更。

## 3. 語音辨識的選項

文字模型伺服器**不能**當語音辨識用（它不收音訊）。Linux 上可以選：

| 方案 | 說明 | 狀態 |
|------|------|------|
| SenseVoice-Small（CPU，本機） | 需要 sherpa-onnx 共享庫與約 160 MB 模型 | **未驗證**（這台機器沒有下載，也沒有麥克風） |
| 遠端相容語音辨識端點 | 任何提供 `POST /v1/audio/transcriptions`（OpenAI 格式）的伺服器，例如 whisper.cpp `whisper-server`（見 `docs/lan-server-guide.md`） | 已實測（英文合成語音，見下方）；**中文辨識未驗證** |
| Breeze-ASR-25（GPU） | 需要 NVIDIA＋CUDA | 這台機器不可用 |

### 遠端語音辨識端點

```toml
[route]
asr = "lan-asr"
cloud_fallback_to_local = true

[timeouts]
asr_cloud_ms = 8000             # 預設 5000；實測這台伺服器一句約 5.6 秒，見下方

[[cloud]]
id = "lan-asr"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8002/v1"
asr_model = "breeze-asr-25"     # 伺服器的模型名稱（GET /v1/models 列出的 id）；自訂端點沒有預設值，不填則不會使用這個服務
```

2026-11-10 實測（Breeze-ASR-25 伺服器，需要金鑰；音訊是 3.1 秒的英文合成語音 "Please send the meeting notes to the team by Friday."，不是真人錄音）：

| 項目 | 結果 |
|------|------|
| 辨識結果 | 與原句相同（忽略大小寫與標點，字元錯誤率 0） |
| 每次辨識耗時 | 約 5.6～5.7 秒（連跑 3 次都一樣，不是冷啟動） |
| 預設逾時 `asr_cloud_ms = 5000` | **每次都逾時**；要用這台伺服器，請把 `asr_cloud_ms` 調到 8000 左右 |
| 端到端（辨識 → 潤飾 → 注入計畫） | 辨識約 5.7 秒；潤飾在思考開啟時 1 秒預算逾時並退回去贅字（總計約 6.7 秒），關閉思考（0.1.3）時約 0.4～0.6 秒成功（總計約 6.1～6.3 秒） |

中文辨識沒有驗證：這台機器的合成語音工具（flite）只能產生英文，也沒有麥克風可以錄中文。

- v2c 送出 multipart：`file`（16 kHz 單聲道 WAV）、`model`、`response_format=json`，有個人詞彙時加 `prompt`；預期回應 `{"text": "..."}`。
- 伺服器要求金鑰時，在設定視窗「雲端服務」頁貼上（存在系統金鑰庫）。金鑰錯誤會顯示「金鑰錯誤」（HTTP 401），端點不支援語音會顯示「這個端點不支援語音辨識」（404／405／501）。
- 失敗時改用本機語音辨識的動作由桌面 App 執行（`cloud_fallback_to_local = true`）；本機沒有可用的語音辨識模型時，這次輸入會顯示錯誤。

## 4. 完整範例：兩台遠端服務（語音辨識 :8002、文字模型 :8000）

```toml
[route]
asr = "lan-asr"
polish = "lan-llm"
translate = "lan-llm"
cloud_fallback_to_local = true

[polish]
enabled = true

[timeouts]
asr_cloud_ms = 8000    # 見第 3 節實測：5000 對這台語音伺服器太短
llm_cloud_ms = 3000

[gpu]
enabled = false

[[cloud]]
id = "lan-asr"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8002/v1"
asr_model = "breeze-asr-25"

[[cloud]]
id = "lan-llm"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8000/v1"
llm_model = "Qwen3.8-27B-UD-Q6_K_L"
```

這份範例由測試 `two_remote_services_config_routes_every_purpose`（`crates/v2c-platform/tests/linux_asr.rs`）確認三種用途都會導向對應的服務。

## 5. 自己測試連線（開發者）

自動測試預設不連任何真實伺服器。要對真實伺服器跑一次（只送合成測試句或合成音訊，輸出只有長度與耗時，不印出文字）：

```sh
# 文字模型
V2C_TEST_LLM_BASE_URL=http://192.168.1.50:8000/v1 \
V2C_TEST_LLM_MODEL=Qwen3.8-27B-UD-Q6_K_L \
cargo test -p v2c-platform --test linux_llm -- --ignored llm_real --nocapture --test-threads=1

# 語音辨識（金鑰放在檔案裡，測試不會印出；不給 WAV 時送 1 秒合成音，只檢查連線與格式）
V2C_TEST_ASR_BASE_URL=http://192.168.1.50:8002/v1 \
V2C_TEST_ASR_MODEL=breeze-asr-25 \
V2C_TEST_ASR_KEY_FILE="$PWD/tests/tmp/asr-key" \
V2C_TEST_ASR_WAV="$PWD/tests/tmp/sample.wav" \
V2C_TEST_ASR_EXPECT="Please send the meeting notes to the team by Friday." \
cargo test -p v2c-platform --test linux_asr -- --ignored asr_real --nocapture

# 端到端（真實語音辨識 → 管線 → 真實潤飾 → 注入計畫）：同時設定上面兩組變數
cargo test -p v2c-platform --test linux_e2e_sim -- --ignored e2e_real --nocapture
```

選填：`V2C_TEST_LLM_TIMEOUT_MS`（預設 15000）、`V2C_TEST_ASR_TIMEOUT_MS`（預設為設定檔預設值 5000）、`V2C_TEST_ASR_RUNS`（預設 2）。`V2C_TEST_ASR_EXPECT` 只用來在測試裡比對，輸出只有「是否相同」與字元錯誤率。

## 6. 桌面 App（T3.6）

### 6.1 檔案位置

v2c 自己決定路徑（`apps/desktop/src-tauri/src/paths.rs`），不使用 Tauri 的 `app_config_dir()`：

| 內容 | 位置 |
|------|------|
| 設定 `config.toml`、個人詞庫 `vocab.txt`、輸入紀錄 `history.db`、日誌 `logs/` | `$XDG_CONFIG_HOME/v2c/`，沒設定時為 `~/.config/v2c/` |
| 語音模型 `models/` | `$XDG_DATA_HOME/v2c/models/`，沒設定時為 `~/.local/share/v2c/models/` |
| 全部改到同一個資料夾（測試、可攜版） | 環境變數 `V2C_DATA_DIR=/某個/資料夾` |
| 額外的模型搜尋資料夾 | 環境變數 `V2C_MODEL_DIR` |
| API 金鑰、輸入紀錄的加密金鑰 | 系統金鑰庫（Secret Service，GNOME 是「登入」鑰匙圈）；紀錄金鑰的項目是 service `v2c-history`／user `history-key` |
| 網頁介面（WebKitGTK）的快取 | 由 Tauri／WebKit 決定，預計在 `~/.local/share/tw.v2c.desktop/`、`~/.cache/tw.v2c.desktop/`（**未實測**） |

第一次啟動時建立的 `config.toml` 在 Linux 上預設 `[gpu] enabled = false`（Linux 不走 Breeze GPU；語音辨識用設定的遠端端點或 SenseVoice CPU）。Windows 的預設值不變。

### 6.2 熱鍵與文字輸出：IBus 優先，否則備援

| 情況 | 熱鍵 | 文字輸出 |
|------|------|----------|
| v2c 輸入法元件已安裝（`/usr/share/ibus/component/v2c.xml`，T3.7 的 .deb 會裝），而且連得上 ibus-daemon | IBus 引擎收到的右 Ctrl（**只在輸入來源切到 v2c、且有輸入框焦點時**） | IBus `CommitText`；沒有可輸入的框時改用下一列的剪貼簿方式（有的話） |
| 元件沒安裝、IBus 連不上、或 `V2C_IBUS=off` | GlobalShortcuts portal（GNOME ≥ 48）→ evdev（要加入 `input` 群組） | 剪貼簿＋Ctrl+V（Clipboard portal，GNOME ≥ 45）；這台 GNOME 42 沒有，文字只會保留（見 6.3） |

- IBus 連不上時會跳出通知「v2c 輸入法無法啟動」，並改用備援。
- ibus-daemon 重新啟動（例如 `ibus restart`、重新登入）時，v2c 會跳出「輸入法連線中斷」並在背景重新連線（1、2、5、10 秒，之後每 30 秒）；連上後通知自動關閉。
- 環境變數 `V2C_IBUS`：`off`＝完全不碰 IBus；`launched`＝一律以已安裝元件的名稱連上；`dynamic`＝開發用，在執行時動態註冊元件（**注意**：ibus 1.5.26 移除動態元件時，可能把目前輸入框的全域引擎拔掉，所以 v2c 不會自動這樣做）。不設定＝元件已安裝就連，沒安裝就用備援。

#### `v2c --ibus` 與單一執行個體

元件 XML 的 `<exec>` 是 `/usr/bin/v2c --ibus`：當你切到 v2c 輸入來源而 v2c 還沒在執行，ibus-daemon 會啟動它。

- 這個行程就是一般的 v2c（在背景執行、**不開主視窗**），並以元件名稱 `org.freedesktop.IBus.V2c` 連上 IBus。
- v2c 已經在執行時（例如開機自動啟動），新行程由單一執行個體機制交給既有的 v2c 後立刻結束；既有的 v2c 只會（重新）連上 IBus，不會打開視窗。所以同時只有一個引擎、一個熱鍵來源，不會重複輸入。
- 你自己從選單開 v2c 時，若元件已安裝，v2c 會直接以元件名稱連上，之後 ibus-daemon 不需要再啟動另一個。

### 6.3 文字沒有輸入時

IBus 只會把文字送進「輸入來源是 v2c、有焦點、而且焦點後收到過按鍵」的輸入框；不符合時（或備援方式也不支援、權限被拒）文字**不會丟掉**：

1. 寫進加密的輸入紀錄（紀錄開啟、金鑰庫可用時）；
2. 否則只保留在記憶體（最多 20 筆，結束 v2c 就消失，不寫入硬碟），列在主視窗「輸入紀錄」頁最上方，標示「未輸入」；
3. 兩種情況都會跳出通知「文字沒有輸入」，主畫面顯示原因（「沒有使用 v2c 輸入法的輸入框」或「v2c 輸入法連線中斷」）。

「輸入紀錄」頁的「複製」在 Linux 上由視窗本身複製到剪貼簿（GNOME 42 背景程式不能設定剪貼簿）；「重新輸出」在 Linux 尚未支援。日誌只記字數與原因，不記文字。

### 6.4 狀態通知

Wayland 下不顯示浮動標籤，改用桌面通知（同一則通知依序更新：錄音中 → 辨識中 → 潤飾中 → 完成）。錯誤（麥克風、辨識失敗、熱鍵）會另外顯示；潤飾退回、雲端退回等提示只顯示在主視窗。X11 工作階段仍使用浮動標籤，只有 IBus 與「文字沒有輸入」的通知。

