# Linux 設定說明（Phase 3，開發中）

這份文件說明在 Linux（目前以 Ubuntu 22.04／GNOME 42 Wayland 開發）上，怎麼把 v2c 的**潤飾**、**中翻英**接到你已經架好的 OpenAI 相容文字模型伺服器（例如 llama.cpp `llama-server`、vLLM、Ollama、另一台電腦的 LM Studio），以及語音辨識有哪些選項。

> 狀態：Linux 版桌面 App 已可在這台機器編譯、啟動（T3.6，見第 6 節），也已產生 .deb 與 AppImage 安裝包（T3.7，安裝步驟見第 7 節），但**還沒有實機安裝、按住說話驗證**（沒有麥克風、.deb 尚未安裝）。下面的設定格式和 Windows 版共用同一份 `config.toml`，已用自動測試驗證（`crates/v2c-platform/tests/linux_*.rs`）。Linux 上設定檔在 `~/.config/v2c/config.toml`（見第 6.1 節）。手動改設定檔後要重新啟動 v2c 才會生效。

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

- 自訂端點可以用 `http://`（區網伺服器）；需要金鑰時在主視窗的「服務」頁貼上，存在系統金鑰庫，不寫進設定檔。不需要金鑰就留空。
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
| SenseVoice-Small（CPU，本機） | 需要 sherpa-onnx 共享庫（.deb 已內含）與約 230 MB 模型 | 已用 10 段真人錄音實測（見下方「Linux 建議方案」）；還沒在安裝好的 App 上用麥克風試過 |
| 遠端相容語音辨識端點 | 任何提供 `POST /v1/audio/transcriptions`（OpenAI 格式）的伺服器，例如 whisper.cpp `whisper-server`（見 `docs/lan-server-guide.md`） | 已用 10 段真人錄音實測（見下方「兩種模型比較」） |
| Breeze-ASR-25（GPU） | 需要 NVIDIA＋CUDA | 這台機器不可用 |

### 遠端語音辨識端點

```toml
[route]
asr = "lan-asr"
cloud_fallback_to_local = true

[[cloud]]
id = "lan-asr"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8002/v1"
asr_model = "breeze-asr-25"     # 伺服器的模型名稱（GET /v1/models 列出的 id）；自訂端點沒有預設值，不填則不會使用這個服務
asr_timeout_ms = 8000           # 0.1.6 起：只放寬這個服務的逾時（實測一句約 5.6～6.1 秒）；不填用 [timeouts] asr_cloud_ms（預設 5000）
```

0.1.6 起也可以在「服務 → 新增服務」精靈的「選擇模型」步驟填「語音辨識逾時」，不用手改設定檔。

2026-11-10 實測（Breeze-ASR-25 伺服器，需要金鑰；音訊是 3.1 秒的英文合成語音 "Please send the meeting notes to the team by Friday."，不是真人錄音）：

| 項目 | 結果 |
|------|------|
| 辨識結果 | 與原句相同（忽略大小寫與標點，字元錯誤率 0） |
| 每次辨識耗時 | 約 5.6～5.7 秒（連跑 3 次都一樣，不是冷啟動） |
| 預設逾時 `asr_cloud_ms = 5000` | **每次都逾時**；要用這台伺服器，請把這個服務的 `asr_timeout_ms` 設成 8000 左右（0.1.6 以前只能調全域的 `asr_cloud_ms`） |
| 端到端（辨識 → 潤飾 → 注入計畫） | 辨識約 5.7 秒；潤飾在思考開啟時 1 秒預算逾時並退回去贅字（總計約 6.7 秒），關閉思考（0.1.3）時約 0.4～0.6 秒成功（總計約 6.1～6.3 秒） |

中文辨識沒有驗證：這台機器的合成語音工具（flite）只能產生英文，也沒有麥克風可以錄中文。

- v2c 送出 multipart：`file`（16 kHz 單聲道 WAV）、`model`、`response_format=json`，有個人詞彙時加 `prompt`；預期回應 `{"text": "..."}`。
- 伺服器要求金鑰時，在主視窗「服務」頁貼上（存在系統金鑰庫）。金鑰錯誤會顯示「金鑰錯誤」（HTTP 401），端點不支援語音會顯示「這個端點不支援語音辨識」（404／405／501）。
- 失敗時改用本機語音辨識的動作由桌面 App 執行（`cloud_fallback_to_local = true`）；本機沒有可用的語音辨識模型時，這次輸入會顯示錯誤。

### 兩種遠端語音辨識模型比較（2026-11-11，10 段真人錄音，同一台文字模型潤飾）

| 指標 | `tea-asr-1` | `breeze-asr-25` |
|------|-------------|-----------------|
| 純辨識平均耗時 | 約 1.7 秒 | 約 5.9 秒 |
| 辨識＋潤飾平均總耗時 | 約 2.4 秒（10 段中 9 段 ≤ 3 秒） | 約 6.9 秒（0 段 ≤ 3 秒） |
| 純辨識平均 CER | 1.8% | 7.0% |
| 預設逾時 5000 ms 可用 | 可以 | 不行（這個服務的 `asr_timeout_ms` 要 ≥ 8000） |

建議 Linux 用 `tea-asr-1`（`asr_cloud_ms` 可維持 5000；第 4 節範例已是這個設定）。兩者都沒有帶詞庫；測試是用 v2c 的處理流程程式直接餵音檔，不是在安裝好的 App 上用麥克風說話。

### Linux 建議方案：遠端 TEA-ASR-1，本機 SenseVoice 備援

日常用遠端 `tea-asr-1`；遠端失敗（網路斷線、伺服器沒回應、超過 `asr_cloud_ms`、金鑰錯誤）時，這一句自動改用本機 SenseVoice 辨識（`cloud_fallback_to_local = true`），主視窗會顯示「雲端服務失敗，已改用地端」。

同一組 10 段真人錄音的實測（2026-11-11，這台 i7-1165G7 筆電，潤飾都用同一台 Qwen 文字模型）：

| 方案 | 每段辨識平均耗時 | 純辨識平均 CER | 加潤飾後平均總耗時 | 潤飾後平均 CER |
|------|------------------|----------------|--------------------|----------------|
| 遠端 `tea-asr-1` | 約 1.7 秒 | 1.8% | 約 2.4 秒 | 1.5% |
| 本機 SenseVoice（CPU） | 約 0.4 秒 | 7.9% | 約 1.0 秒 | 5.4% |

SenseVoice 的錯誤集中在中英夾雜與專有名詞；潤飾能修掉大部分中英夾雜錯誤，專有名詞仍容易錯（之後可用個人詞庫改善）。不建議把 TEA-ASR-1 放在這類筆電的 CPU 上跑：實測每段約 157 秒（mini 版約 12 秒）。

#### 放入 SenseVoice 模型

v2c 在 `~/.local/share/v2c/models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/` 找模型（也可用環境變數 `V2C_MODEL_DIR` 指定上一層資料夾）：

```sh
mkdir -p ~/.local/share/v2c/models
cd ~/.local/share/v2c/models
curl -L -o sv.tar.bz2 https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2
tar xjf sv.tar.bz2 && rm sv.tar.bz2
```

找不到模型時，遠端失敗的那一句會顯示錯誤，不會退回。v2c 剛啟動（或改設定後重新載入）、SenseVoice 還在載入時，那一句也不會退回，會直接顯示遠端的錯誤。

## 4. 完整範例：遠端 TEA-ASR-1（:8001）＋ 遠端文字模型（:8000）＋ 本機 SenseVoice 備援

```toml
[route]
asr = "lan-asr"
polish = "lan-llm"
translate = "lan-llm"
cloud_fallback_to_local = true

[polish]
enabled = true

[timeouts]
asr_cloud_ms = 5000    # tea-asr-1 平均約 1.7 秒；超過就改用本機 SenseVoice
llm_cloud_ms = 3000

[local]
asr = "sense_voice"    # 遠端失敗時的本機備援（CPU）

[gpu]
enabled = false

[[cloud]]
id = "lan-asr"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8001/v1"
asr_model = "tea-asr-1"

[[cloud]]
id = "lan-llm"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8000/v1"
llm_model = "Qwen3.8-27B-UD-Q6_K_L"
```

這份範例由測試 `two_remote_services_config_routes_every_purpose`（`crates/v2c-platform/tests/linux_asr.rs`）確認三種用途都會導向對應的服務，而且都會在失敗時退回本機。要改用 `breeze-asr-25`：`base_url` 換成該服務的埠號、`asr_model = "breeze-asr-25"`，並在這個服務加上 `asr_timeout_ms = 8000`（見第 3 節；只影響這個服務，其他服務仍用 `asr_cloud_ms`）。

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
| v2c 輸入法元件已安裝（`/usr/share/ibus/component/v2c.xml`，T3.7 的 .deb 會裝），而且連得上 ibus-daemon | IBus 引擎收到的右 Ctrl（**只在輸入來源切到 v2c、且有輸入框焦點時**） | IBus `CommitText`；沒有可輸入的框時**不會**改用剪貼簿貼到別處，文字照 6.3 保留 |
| 曾連上 IBus 但連線中斷、正在重新連線 | 原本的 IBus 熱鍵（重新連上後恢復） | 下一列的剪貼簿方式（有的話），否則照 6.3 保留 |
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
2. 否則只保留在記憶體（最多 20 筆，結束 v2c 就消失，不寫入硬碟），列在主視窗「詞庫與紀錄 → 輸入紀錄」最上方，標示「未輸入」；
3. 兩種情況都會跳出通知「文字沒有輸入，已保留在 v2c 的「輸入紀錄」」（只在記憶體時註明「結束 v2c 後消失」），主畫面顯示原因（「沒有使用 v2c 輸入法的輸入框」或「v2c 輸入法連線中斷」）。

已知限制（刻意保留）：v2c 連上 IBus、但目前的輸入來源不是 v2c（例如切到注音、英文鍵盤）時，右 Ctrl 不會被 v2c 收到，也不會改用備援熱鍵，按了沒有任何反應。原因是 GNOME Wayland 只把按鍵交給目前選取的輸入法；備援熱鍵在這台機器上也收不到全域按鍵。要說話時先用 Super+空白鍵切到「v2c 語音輸入」。打字和說話可以都用 v2c 輸入來源：沒按右 Ctrl 時，按鍵照常送給應用程式（英文鍵盤配置）。

「詞庫與紀錄 → 輸入紀錄」的「複製」在 Linux 上由視窗本身複製到剪貼簿（GNOME 42 背景程式不能設定剪貼簿）；「重新輸出」在 Linux 尚未支援。日誌只記字數與原因，不記文字。

### 6.4 狀態通知

Wayland 下不顯示浮動指示條，也比照 Windows 版的浮動指示條「不顯示狀態文字」：正常說一句話（錄音、辨識、潤飾、完成）**不跳任何通知**，文字直接出現在游標處。只有需要你注意時才跳通知：

- 錯誤（麥克風、辨識失敗、熱鍵）；下一次開始錄音時自動關閉。
- 進入免持模式（快速按兩下右 Ctrl）時跳出「免持模式：按右 Ctrl 結束」，錄音結束時自動關閉。
- IBus 與「文字沒有輸入」的通知（見上節）。

潤飾退回、雲端退回等提示只顯示在主視窗。X11 工作階段仍使用浮動指示條，只有 IBus 與「文字沒有輸入」的通知。


## 7. 安裝（T3.7）

安裝包放在 repo 的 `releases/linux/`，雜湊寫在 `releases/linux/SHA256SUMS.txt`。安裝檔不放進 git，clone 或 pull 原始碼後用 `bash releases/linux/build.sh` 自己建置（`--check` 只列出缺少的套件；不會自動安裝）。資料夾只保留最新版，舊版會被刪掉：

| 檔案 | 內容 | 適合 |
|------|------|------|
| `v2c_<版本>_amd64.deb` | `/usr/bin/v2c`、SenseVoice 函式庫 `/usr/lib/v2c/*.so`、說明文件 `/usr/lib/v2c/docs/`、**IBus 輸入法元件** `/usr/share/ibus/component/v2c.xml`、選單捷徑 | Ubuntu／Debian，**建議使用** |
| `v2c_<版本>_amd64.AppImage` | 同一個程式與函式庫，免安裝 | 不能裝 .deb 的發行版；**不含 IBus 元件**（見 7.4） |

這一版只有 CPU 版（沒有 Breeze／CUDA）；語音辨識用遠端服務或 SenseVoice。

### 7.1 安裝 .deb

```sh
sha256sum -c --ignore-missing SHA256SUMS.txt     # 在 releases/linux/ 資料夾裡執行，確認檔案沒有損壞
sudo apt install ./v2c_0.3.0_amd64.deb            # 會一併安裝相依套件（ibus、libwebkit2gtk-4.1-0、libayatana-appindicator3-1…）
ibus restart                                      # 讓 ibus-daemon 讀到新的 v2c 元件（或登出再登入）
```

`ibus restart` 會重新啟動所有 App 共用的輸入法服務：執行當下各 App 的輸入法會中斷一下，目前選用的輸入來源也可能被重設。比較穩妥的做法是登出再登入。

要用 `apt install ./檔名`（前面有 `./`），不要用 `dpkg -i`，否則相依套件不會自動安裝。解除安裝：`sudo apt remove v2c`（設定與紀錄在 `~/.config/v2c/`，不會被刪除）。

### 7.2 加入 v2c 輸入來源（GNOME）

1. 「設定」→「鍵盤」→「輸入來源」→「＋」→ 選中文（台灣）語言（元件的語言是 `zh_TW`；GNOME 顯示的名稱依介面語言而定，也可以在搜尋框輸入「v2c」）→「**v2c 語音輸入**」→「新增」。
   清單裡沒有 v2c：執行 `ibus restart` 或重新登入後再試；`ibus list-engine | grep v2c` 應該看得到。
   「設定」裡加不進去（找不到、按鈕沒反應、遠端連線沒有「設定」視窗）：改用下方 7.2.1 的指令。
2. 要說話時，用 Super+空白鍵切到 v2c 輸入來源，在輸入框裡按住右 Ctrl 說話（見第 6.2 節）。切到 v2c 時 v2c 還沒執行，ibus-daemon 會自動啟動它（`/usr/bin/v2c --ibus`，不開主視窗）。
3. 主視窗從應用程式選單的「v2c」打開。

#### 7.2.1 用指令加入輸入來源

GNOME 的輸入來源清單存在 `org.gnome.desktop.input-sources sources`，「設定」視窗改的也是它，所以用 `gsettings` 改的效果一樣。以下指令都在**你自己登入桌面的帳號**下、一般權限執行（不要加 sudo）；從 SSH 連進來時，要在已登入桌面的同一個帳號下執行。

1. 確認 ibus-daemon 認得 v2c 引擎（裝完 .deb 後要先 `ibus restart` 或重新登入）：

   ```sh
   ibus list-engine | grep v2c          # 應該看到：  v2c - v2c 語音輸入
   ```

   沒有輸出：確認 `/usr/share/ibus/component/v2c.xml` 存在，再執行 `ibus restart`。

2. 先看目前的清單，記下來（要還原時用得到）：

   ```sh
   gsettings get org.gnome.desktop.input-sources sources
   # 例如：[('xkb', 'us')]
   ```

3. 把 v2c 加到清單最後，原本的輸入來源都保留（已經有 v2c 就不會重複加）：

   ```sh
   python3 - <<'EOF'
   import ast, subprocess
   key = ["org.gnome.desktop.input-sources", "sources"]
   cur = subprocess.run(["gsettings", "get", *key], capture_output=True, text=True, check=True).stdout.strip()
   if cur.startswith("@"):            # 清單是空的時，gsettings 會印出 "@a(ss) []"
       cur = cur.split(" ", 1)[1]
   src = ast.literal_eval(cur)
   if ("ibus", "v2c") not in src:
       src.append(("ibus", "v2c"))
   subprocess.run(["gsettings", "set", *key, str(src)], check=True)
   print("輸入來源：", src)
   EOF
   ```

   清單原本只有英文鍵盤時，也可以直接整個設定（**會覆蓋**原本的清單，有注音等其他輸入法時不要用這行）：

   ```sh
   gsettings set org.gnome.desktop.input-sources sources "[('xkb', 'us'), ('ibus', 'v2c')]"
   ```

4. 確認：

   ```sh
   gsettings get org.gnome.desktop.input-sources sources    # 應該包含 ('ibus', 'v2c')
   ```

   畫面右上角會出現輸入來源選單。按 Super+空白鍵切到「v2c 語音輸入」（選單上顯示「語」），切到後執行 `ibus engine` 應該印出 `v2c`。

不需要登出；設定一改，GNOME 就會更新輸入來源選單。

**移除 v2c 輸入來源**（解除安裝前，或想還原時）：

```sh
python3 - <<'EOF'
import ast, subprocess
key = ["org.gnome.desktop.input-sources", "sources"]
cur = subprocess.run(["gsettings", "get", *key], capture_output=True, text=True, check=True).stdout.strip()
if cur.startswith("@"):
    cur = cur.split(" ", 1)[1]
src = [s for s in ast.literal_eval(cur) if s != ("ibus", "v2c")]
subprocess.run(["gsettings", "set", *key, str(src)], check=True)
print("輸入來源：", src)
EOF
```

或用第 2 步記下的原始清單：`gsettings set org.gnome.desktop.input-sources sources "<原本的清單>"`。

不是 GNOME 的桌面（Xfce、KDE 等用 IBus 的環境）：GNOME 的 `gsettings` 設定對它們無效。改用 `ibus-setup` 的「輸入法」分頁加入 v2c，或執行 `ibus engine v2c` 直接切到 v2c（只對這次登入有效）。這個做法沒有實測過。

### 7.3 設定兩個遠端服務

**方法一（最快）：一鍵套用預設組態。** 不用手改 `config.toml`：開啟主視窗 →「首頁 → 套用預設組態」。公司的 TN／RD 伺服器直接選 TN 或 RD，選 TEA-ASR 或 Breeze-ASR →「套用」，語音辨識、潤飾、中翻英一次設好，立刻生效、不用重新啟動。自己的伺服器可以寫成預設組態檔（`.toml`，範例見 [cloud-setup.md](cloud-setup.md#預設組態一鍵設好區網服務)）後按「從檔案載入」。金鑰在套用畫面輸入，存進 GNOME「登入」鑰匙圈。套用預設組態不會設定本機 SenseVoice 備援模型，請照第 3 節「放入 SenseVoice 模型」另外放好。

**方法二：手動改設定檔**（和方法一等效，下面是原本的步驟）：

（設定檔的欄位與手寫規則見 [config-file.md](config-file.md)；設好的服務也可以用「設定 → 進階 → 匯出／匯入設定 → 匯出設定」存成檔案，在另一台「匯入設定」。）

1. 先啟動一次 v2c，讓它建立 `~/.config/v2c/config.toml`（Linux 預設 `[gpu] enabled = false`）。
2. 結束 v2c（系統匣圖示 → 結束），用文字編輯器打開 `~/.config/v2c/config.toml`，照第 4 節的範例加入 `[route]`、`[timeouts]`、`[local]` 與兩個 `[[cloud]]`（語音辨識 `:8001` 的 `tea-asr-1`、文字模型 `:8000`），把 IP 換成你的伺服器。並照第 3 節「放入 SenseVoice 模型」放好本機備援模型。
   也可以在主視窗「服務」頁用「新增服務 → 自訂相容端點」分別設定文字與語音端點。
3. 伺服器需要金鑰時，在「服務」頁貼上；金鑰存在 GNOME「登入」鑰匙圈，不寫進設定檔。
4. 重新啟動 v2c（手動改設定檔後一定要重新啟動）。

### 7.4 AppImage

```sh
chmod +x v2c_0.3.0_amd64.AppImage
./v2c_0.3.0_amd64.AppImage
```

- 需要 FUSE 2（Ubuntu 22.04：`sudo apt install libfuse2`；Ubuntu 24.04：`libfuse2t64`）。沒有 FUSE 時可改用 `./v2c_0.3.0_amd64.AppImage --appimage-extract-and-run`。
- AppImage 已內含 WebKit、系統匣（ayatana-appindicator）與 D-Bus 函式庫；系統只需要有 ibus（輸入法服務）與 gnome-keyring（金鑰庫）。
- AppImage **不能安裝 IBus 元件**，所以預設走備援熱鍵／輸出（第 6.2 節；GNOME 42 上文字只會保留在紀錄裡）。要用 IBus 請改裝 .deb。若一定要用 AppImage：把 AppImage 放到固定位置（例如 `~/Applications/v2c.AppImage`），複製 repo 的 `tools/ibus/v2c.xml` 到 `/usr/share/ibus/component/v2c.xml`（需要 sudo），把其中的 `<exec>` 改成 `/home/你的帳號/Applications/v2c.AppImage --ibus`，然後 `ibus restart`。這個做法未經測試。
