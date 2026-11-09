# v2c 實作計畫

> 需求與決策見 `../v2cplan.md`（衝突時以它為準）；本檔說明「怎麼做」；每個任務的工作範圍見 `cards/`；進度見 `todo.md`；agent 規範見 `../AGENTS.md`。
> 標註 **【實作前確認】** 的項目是我依目前所知寫的，動工前要先查證（crate 是否仍維護、API 是否變動）。

---

## 0. 總體設計原則

1. **OS 相關的部分只做薄薄一層**：熱鍵掛勾、文字注入、浮動視窗是唯一依平台分開的程式碼；判斷邏輯（熱鍵狀態機、注入計畫）寫成純函式放在共用 crate，可單元測試。
2. **先有命令列工具，再有桌面 App**：`v2c-cli` 能直接對 wav 檔跑完整管線，Phase 0 的量測與之後的回歸測試都用它。
3. **Provider 一律走 trait**：地端、雲端、自訂端點都實作同一組介面，管線不知道背後是誰。
4. **設定、金鑰、紀錄分開存**：設定 TOML（可手改）、金鑰在 OS 金鑰庫、紀錄在 SQLite。
5. **地端優先，雲端選用**：安裝後不需任何金鑰就能用（地端模型）；雲端只在使用者走完「串接雲端服務」流程（第 6.2 節）後才會啟用，而且依用途個別開啟。

---

## 1. 專案結構

```
VoiceToText_handmade/
├─ Cargo.toml                  # workspace
├─ crates/
│  ├─ v2c-core/                # 無 OS 依賴，可編到 Android
│  │  └─ src/
│  │     ├─ audio.rs           # AudioClip、重取樣、WAV 編碼（送雲端用）
│  │     ├─ pipeline.rs        # Pipeline::run(clip, mode) -> Output
│  │     ├─ asr/{mod,openai,gemini,compat}.rs
│  │     ├─ llm/{mod,openai_compat,gemini,claude}.rs
│  │     ├─ postprocess/{zh_tw,fillers,newline}.rs
│  │     ├─ prompts.rs         # 潤飾 / 翻譯 / ASR initial prompt
│  │     ├─ vocab.rs           # 個人詞庫
│  │     ├─ history.rs         # SQLite 輸入紀錄
│  │     ├─ config.rs          # TOML 設定 + 預設值
│  │     ├─ trigger.rs         # 熱鍵狀態機（純邏輯）
│  │     └─ inject_plan.rs     # 文字 → 注入動作序列（純邏輯）
│  ├─ v2c-asr-local/           # whisper-rs(Breeze) + sherpa(SenseVoice)，只給桌面
│  ├─ v2c-platform/            # OS 相關：{windows,linux}/{hotkey,inject,foreground}.rs；cli 與 desktop 共用
│  ├─ v2c-ffi/                 # uniffi，給 Android
│  └─ v2c-cli/                 # 開發工具；每個子命令一個檔案 src/cmd/{transcribe,bench,hotkey_test,inject_test}.rs
├─ apps/
│  ├─ desktop/                 # Tauri v2
│  │  ├─ src-tauri/src/
│  │  │  ├─ main.rs, app_state.rs, controller.rs   # 串起錄音→管線→注入
│  │  │  ├─ recorder.rs        # cpal 錄音
│  │  │  └─ overlay.rs, tray.rs   # OS 相關功能一律呼叫 crates/v2c-platform
│  │  └─ ui/                   # Vite + Svelte（設定、紀錄、浮動標籤）
│  └─ android/                 # Gradle 專案，Kotlin
├─ tools/convert-breeze/       # Breeze → GGML 轉檔腳本（只開發者執行一次）
└─ tests/fixtures/audio/       # 測試音檔 + 期望文字
```

---

## 2. 核心介面（v2c-core）

```rust
pub struct AudioClip { pub samples: Vec<f32>, pub sample_rate: u32 } // 一律 16 kHz mono

pub enum Mode { Dictate, TranslateZhToEn }

pub struct AsrContext { pub vocabulary: Vec<String>, pub prompt: String }
pub struct Transcript { pub text: String, pub lang: Lang /* Zh | En | Mixed */ }

#[async_trait]
pub trait AsrProvider: Send + Sync {
    async fn transcribe(&self, clip: &AudioClip, ctx: &AsrContext) -> Result<Transcript>;
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
}

pub struct Output { pub text: String, pub timings: Timings /* asr_ms, llm_ms, total_ms */ }

impl Pipeline {
    pub async fn run(&self, clip: AudioClip, mode: Mode) -> Result<Output>;
}
```

**Pipeline::run 流程**
1. 太短（< 0.3 秒）或靜音 → 回傳空，不輸出。
2. `asr.transcribe`（帶詞庫與 initial prompt）。
3. `postprocess::zh_tw`：簡轉台灣繁體（含台灣用語）；英文不動。
4. `Mode::Dictate`：潤飾開啟且有 LLM → `llm.complete(POLISH_PROMPT)`；失敗或逾時（預設 1.0 秒）→ 退回規則式 `fillers` 去贅字，並回報警告。
   **延遲預算**（10 秒語句，放開熱鍵起算）：ASR ≤ 1.2 秒＋後處理與注入 ≤ 0.3 秒 → 不含潤飾 ≤ 1.5 秒；再加潤飾 ≤ 1.0 秒 → 含潤飾 ≤ 2.5 秒。`Output.timings` 記錄每段耗時，`v2c-cli bench` 依此判定。
   `Mode::TranslateZhToEn`：`llm.complete(TRANSLATE_PROMPT)`；沒有 LLM → 回報錯誤「未設定翻譯模型」。
5. LLM 輸出再過一次 `zh_tw`（防模型吐簡體）。
6. 寫入 history，回傳 Output。

**提示詞（prompts.rs）重點**
- 潤飾：只做去贅字（嗯、那個、就是）、處理自我更正（「不對，我是說…」只留更正後內容）、補全形標點、依語意自然換行；**不改寫、不增刪內容、不回答問題**；中英夾雜保留原樣；只輸出結果文字。附上詞庫，要求專有名詞照詞庫拼寫。
- 翻譯：中文（可能夾雜英文）→ 自然流暢的英文；專有名詞與詞庫保留；只輸出譯文。
- ASR initial prompt：`以下是台灣繁體中文與 English 夾雜的口語句子。` + 詞庫詞彙（截斷到模型上限）。

**依賴（暫定，【實作前確認】）**：`tokio`、`reqwest`（`rustls-tls`，避免 Android 上的 OpenSSL）、`serde`/`toml`、`rusqlite`（`bundled`）、`ferrous-opencc`（純 Rust 實作的 OpenCC，設定 `s2twp`，含台灣用語：软件→軟體、视频→影片、内存→記憶體；T1.1 已確認。`zhconv` 0.4 的 zh-TW 不轉 视频／内存／信息／服务器 等常用詞，故不採用；「台」保留原字不轉「臺」）、`hound`（WAV）、`rubato`（重取樣）、`async-trait`、`thiserror`、測試用 `wiremock`。

---

## 3. 熱鍵狀態機（trigger.rs，純邏輯）

輸入事件：`KeyDown(k, t)`、`KeyUp(k, t)`、`OtherKey(t)`、`Tick(t)`。
輸出指令：`StartRecording(mode)`、`StopAndProcess`、`Cancel`。

| 狀態 | 事件 | 轉移 / 指令 |
|------|------|-------------|
| Idle | RCtrl 按下 | → Pressed(t0, mode)；Shift 同時按著則 mode=Translate；**立即** StartRecording（避免吃掉開頭） |
| Pressed | 其他鍵按下 | → Idle，Cancel（使用者其實在按 Ctrl+C 之類） |
| Pressed | 放開，按住 ≥ 350 ms | → Idle，StopAndProcess（按住說話） |
| Pressed | 放開，按住 < 350 ms | → TapWait(t1)，暫停但保留錄音 |
| TapWait | 300 ms 內再按 RCtrl | → HandsFree，繼續錄音 |
| TapWait | 逾時 | → Idle，Cancel（單擊不算數） |
| HandsFree | 按 RCtrl | → Idle，StopAndProcess |
| HandsFree | 錄音達上限（5 分鐘） | → Idle，StopAndProcess，並提示 |
| 任何錄音狀態 | Esc | → Idle，Cancel |

門檻值都放設定檔。單元測試直接餵帶時間戳的事件序列驗證輸出。

---

## 4. 文字注入（inject_plan.rs + crates/v2c-platform/src/*/inject.rs）

**inject_plan（純邏輯）**：`plan(text, app_profile) -> Vec<Action>`
- `Action::Text(String)`、`Action::NewLine`、`Action::Paste(String)`。
- 換行：一般 App 用 Enter；**聊天類 App（LINE、Teams、Slack、Discord、瀏覽器）用 Shift+Enter**，避免換行變成送出訊息。App 規則依執行檔名稱比對，可在設定覆寫。
- App 規則標記 `always_paste` → 整段改成 `Paste`。

**Windows 實作**
1. **等所有修飾鍵放開**（RCtrl、Shift），最多等 500 ms；否則注入的字會變成 Ctrl+字母快捷鍵。這一步是正確性關鍵。
2. `GetForegroundWindow` → 取得行程執行檔名 → 選 app_profile。
3. `Text`：`SendInput` + `KEYEVENTF_UNICODE`，以 UTF-16 code unit 送出（surrogate pair 要成對送），每批 ~50 字，批次間不延遲；`SendInput` 回傳值小於送出數 → 判定失敗，改走 Paste。
4. `Paste`：以原生 Win32 剪貼簿 API 備份所有 HGLOBAL 格式（文字、圖片 DIB、HTML、RTF、複製的檔案等；`arboard` 只支援文字與圖片且會重新編碼，T1.2 實作時改用原生 API）→ 設定文字（標記不進 Win+V 歷史與雲端剪貼簿）→ 送 Ctrl+V → 等 150 ms → 還原（期間若剪貼簿被別人改過則不還原）。已知限制：GDI handle 類格式（EMF 等）、OLE 私有格式、超大的延遲渲染格式不還原；還原後剪貼簿擁有者變成 v2c。
5. 前景視窗是系統管理員權限（UIPI 擋住）→ 浮動標籤提示「無法輸入到系統管理員視窗，已複製到剪貼簿」。

---

## 5. 地端 ASR（v2c-asr-local）

**Breeze-ASR-25（GPU）**
- 轉檔（`tools/convert-breeze/`，開發者執行一次）：從 Hugging Face 下載 `MediaTek-Research/Breeze-ASR-25` → 用 whisper.cpp 的 `models/convert-h5-to-ggml.py` 轉 GGML → `quantize` 成 q5_0 與 q8_0 兩版。**【實作前確認】** 授權條款與轉檔腳本是否支援此模型。
- 推論：`whisper-rs`（feature `cuda`），程式啟動時載入並常駐 VRAM。
- **語言設定陷阱**：Whisper 若強制 `zh`，純英文句子可能被「翻譯」成中文。做法：先用模型的語言偵測，偵測到 `en` → 以 `en` 解碼，其他一律 `zh`（中英夾雜在 `zh` 下會保留英文）。T0.1 用測試音檔驗證這個策略。
- 參數：greedy、`no_context`、`single_segment`（短句）、initial prompt 如上。

**SenseVoice-Small（CPU 備援）**
- `sherpa-rs` 或 sherpa-onnx 官方 C API 綁定 **【實作前確認】**；輸出簡體，靠 `zh_tw` 轉換。
- 選擇邏輯：設定 `asr = "auto"` 時，偵測到 CUDA 且 Breeze 模型存在 → Breeze，否則 SenseVoice。

### 5.1 顯示卡記憶體（VRAM）管理

**你這台電腦的現況**（2026-11-08 以 `nvidia-smi` 查得）：RTX 2060 Max-Q 共 6144 MiB，**正在負責畫面輸出**（2560×1440，display_active = Enabled）；平常桌面程式（Chrome、Teams、Edge、Docker Desktop、Telegram 等）已經用掉約 1.4 GB，剩約 4.5 GB。另有 AMD 內顯與 Trigger 6 USB 顯示卡。

**預算原則**：保留至少 **2 GB** 給畫面輸出與一般程式，v2c 與 LM Studio 合計上限約 **4 GB**。

| 方案 | Breeze-ASR-25 | LM Studio 語言模型 | 預估 VRAM | 適用情境 |
|------|---------------|--------------------|-----------|----------|
| **均衡（預設）** | GPU，q5_0（約 1.5 GB） | GPU，3B Q4_K_M、context 2048（約 2.3 GB） | 約 3.8 GB | 日常文書、瀏覽、開會 |
| 省顯存 | GPU，q5_0（約 1.5 GB） | 1.5B 模型或 CPU 執行（0～1.2 GB） | 1.5～2.7 GB | 同時開較多吃顯卡的程式；潤飾變慢約 1～2 秒 |
| 不用顯卡 | CPU，SenseVoice-Small | CPU 執行或改用雲端 | 0 | 玩遊戲、剪片、3D 繪圖 |

上表的數字是估計值，T0.1 會實測後回填。

**v2c 程式內的保護機制**
1. **載入前檢查**：用 NVML（`nvml-wrapper` crate【實作前確認】）讀可用 VRAM；可用量扣掉保留額度（`vram_reserve_mb`，預設 2048）後放不下 Breeze，就改用 SenseVoice（CPU），並在浮動標籤提示一次。
2. **執行中監看**：每 5 秒讀一次可用 VRAM；低於保留額度（例如開了遊戲）就卸載 Breeze、改用 CPU，可用量恢復後再載回。
3. **閒置卸載**：超過 `idle_unload_minutes`（預設 15 分鐘）沒用就卸載；下次按熱鍵時重新載入，第一句會多 1～2 秒。
4. **重度程式自動讓路**：前景是全螢幕程式，或執行檔在 `heavy_apps` 名單（遊戲、Premiere、Blender…）時，自動切到「不用顯卡」方案。
5. **whisper.cpp 參數**：開啟 flash attention、一次只處理一段，壓低運算時的暫存記憶體。
6. **介面不用 NVIDIA**：v2c 的設定視窗與浮動標籤（WebView2）在 Windows「設定 → 顯示器 → 圖形」指定為「省電（AMD 內顯）」，不佔 NVIDIA 顯存。

設定檔：

```toml
[gpu]
enabled = true
profile = "balanced"          # balanced | low_vram | cpu_only
vram_reserve_mb = 2048        # 保留給畫面輸出與其他程式
keep_loaded = "auto"          # always | auto（閒置卸載＋低顯存讓路）| on_demand
idle_unload_minutes = 15
yield_on_fullscreen = true
heavy_apps = ["blender.exe", "Adobe Premiere Pro.exe"]

[local]
breeze_quant = "q5_0"         # q5_0 | q8_0（q8 較準、多約 0.6 GB）
```

設定視窗顯示即時 VRAM 使用條（v2c、其他程式、可用量），以及三種方案的預估用量。

**LM Studio 端的建議設定**（由你自行設定，寫進 `docs/local-setup.md`；項目名稱【實作前確認】）
- 模型：3B～4B、Q4_K_M 量化；context length 設 2048（潤飾句子很短，不需要更長）。
- GPU offload：均衡方案全部層數上 GPU；省顯存方案調低層數或設 0。
- 開啟 flash attention；KV cache 量化設 Q8。
- 開啟 JIT 載入與閒置自動卸載（TTL，例如 15 分鐘），與 v2c 的閒置卸載一致。

**NVIDIA 驅動與 Windows 的建議**
- NVIDIA 控制面板 →「管理 3D 設定 → 程式設定」，對 v2c 與 LM Studio 設定「CUDA - 系統記憶體備援原則」=「偏好無系統記憶體備援」：顯存不足時直接報錯，由 v2c 改用 CPU，而不是偷用系統記憶體，讓整台電腦和畫面一起變慢。只針對這兩個程式設定，不改全域設定。
- 若你的筆電支援切換畫面輸出的顯卡（MUX 開關或 Advanced Optimus），把內建螢幕交給 AMD 內顯，NVIDIA 就能完整留給模型。這要看機型，T0.1 時順便確認。
- 已知影響：辨識的那 1 秒左右顯卡滿載，玩遊戲時可能有短暫掉幀；所以遊戲時建議用「不用顯卡」方案（第 4 點會自動切換）。

**模型存放**：`%LOCALAPPDATA%\v2c\models\`；首次啟動引導下載（顯示進度、SHA-256 驗證）。轉好的 GGML 檔放在你自己的 Hugging Face repo 供下載。

---

## 6. 雲端與相容端點（v2c-core/asr、llm）

| Provider | ASR | LLM | 備註 |
|----------|-----|-----|------|
| OpenAI 相容（LM Studio / 自架） | `/audio/transcriptions`（若端點支援） | `/chat/completions` | base_url + 選填金鑰；LM Studio 預設 `http://localhost:1234/v1` |
| OpenAI | `gpt-4o-transcribe`（multipart，帶 `prompt`） | `/chat/completions` | |
| Gemini | `generateContent` 內嵌 WAV（base64） | 同左 | |
| Claude | — | Messages API | 不收音訊；預設 `claude-haiku-5-5`（速度優先），可改 |

- 模型名稱全部放設定檔，不寫死。
- 逾時：ASR 5 秒、LLM 3 秒；失敗時浮動標籤顯示錯誤，紀錄中保留 ASR 原文以便重送。
- 金鑰：`keyring` crate（Windows 認證管理員）；設定檔只存 provider 名稱。

### 6.1 預設模型（地端優先）

安裝完成、未做任何設定時的預設值：

```toml
[route]                 # 每個用途各自選 provider
asr       = "local"     # Breeze-ASR-25（有 CUDA）→ 否則 SenseVoice-Small
polish    = "local"     # LM Studio（OpenAI 相容，http://localhost:1234/v1）
translate = "local"     # 同上
cloud_fallback_to_local = true   # 雲端失敗時自動改用地端

[polish]
enabled = true
```

- **ASR 一定有地端可用**：首次啟動引導下載地端模型（第 5 節）；這一步完成前不提示任何雲端選項，避免使用者以為必須付費。
- **LM Studio 未啟動**：潤飾改用規則式去贅字；中翻英顯示「請啟動 LM Studio，或串接雲端服務」，並附「串接雲端服務」按鈕。
- **Android**：辨識預設 Android 內建（不需金鑰）；手機沒有地端 LLM，中翻英鍵要等串接雲端服務，或在相容端點填入同一個網路中電腦的 LM Studio 位址後才出現。

### 6.2 串接雲端服務的操作方式

設定視窗的「雲端服務」分頁 → 「新增雲端服務」按鈕，進入逐步精靈：

1. **選擇服務**：Gemini／OpenAI／Claude／自訂（OpenAI 相容端點，例如你自己架的服務、別台電腦的 LM Studio）。每個選項旁標示可用的用途（Claude：只能潤飾與中翻英）。
2. **取得金鑰的說明**：顯示該服務取得 API 金鑰的步驟與官方頁面連結（點擊用預設瀏覽器開啟），並提醒「雲端服務會依用量向你收費，錄音會上傳到該服務」。**【實作前確認】** 各家金鑰頁面的網址：Google AI Studio、OpenAI Platform、Anthropic Console。自訂端點則改填 base_url，金鑰可留空。
3. **貼上金鑰**：存入 OS 金鑰庫，畫面上只顯示末 4 碼。
4. **選擇模型**：預填建議模型；若服務提供模型清單 API（`/models`），改為下拉選單。
5. **測試連線**：辨識用途送出內建的 3 秒測試音檔，LLM 用途送一句測試文字；顯示結果與延遲。失敗時顯示可理解的原因（金鑰錯誤、額度不足、網路不通、端點不支援音訊）。
6. **指定用途**：勾選要把語音辨識／潤飾／中翻英哪些交給這個服務；未勾選的維持地端。
7. **完成**：系統列選單可隨時一鍵切回「全部地端」。

另外提供 `docs/cloud-setup.md`：給使用者看的圖文說明，涵蓋上述步驟與各家取得金鑰的流程；精靈第 2 步的「詳細說明」連到這份文件。

Android 的設定頁使用同一套流程（第 1～7 步），以 Compose 畫面實作；Rust 核心提供共用的 `test_connection()`。

---

## 7. 桌面 App（apps/desktop）

**controller.rs**（核心協調者，一個 tokio task）
`trigger 指令` → `recorder` 開始/停止 → `pipeline.run` → `inject` → 更新 `overlay`、寫入 `history`。同一時間只處理一段；處理中又按熱鍵 → 排隊一段。

**recorder.rs**：`cpal` 預設輸入裝置 → 轉 mono → `rubato` 重取樣到 16 kHz；即時算 RMS 給浮動標籤畫音量。麥克風裝置可在設定選擇。
- **預錄緩衝**（`mic_mode = "always_on"`，預設）：程式啟動即開啟串流，寫入 0.5 秒的環形緩衝（只在記憶體）；按下熱鍵時把緩衝內容接在錄音開頭，避免吃掉第一個字。`mic_mode = "on_press"` 時按下才開串流。
- 裝置拔除或切換時自動重新開啟串流，並在浮動標籤提示。

**overlay.rs**：Tauri 視窗設 `transparent`、`decorations: false`、`always_on_top`、`skip_taskbar`、`focus: false`；Windows 上再對 HWND 加 `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT`（滑鼠穿透、不搶焦點）。位置：主螢幕下方中央。狀態：錄音中（音量條）、辨識中、潤飾中、完成（0.5 秒後淡出）、錯誤（3 秒）。

**tray.rs**：開啟設定、開啟紀錄、潤飾開關、切換 provider、結束。

**ui/**（Svelte）
- 設定：熱鍵（錄製按鍵）、各用途使用地端或哪個雲端服務、地端模型（Breeze / SenseVoice、LM Studio 位址與模型）、潤飾開關、App 規則、麥克風、開機自動啟動。
- 雲端服務：已串接的服務清單（測試連線、修改、移除）與「新增雲端服務」精靈（第 6.2 節）。
- 詞庫：一行一詞的編輯器。
- 紀錄：最近 200 筆（預設保留 7 天），可複製、重新輸出、刪除、清空。

**隱私規則（history.rs 與全程式）**
- 紀錄的文字欄位以 Windows DPAPI（`CryptProtectData`，綁定目前使用者）加密後才寫入 SQLite；Linux 改用 Secret Service 存的金鑰加密，Android 用 Keystore。
- `history.retention_days = 7`；啟動時與每小時清除過期紀錄。
- `history.exclude_apps`：前景 App 在清單內就不寫紀錄（預設：1Password、Bitwarden、KeePassXC 等密碼管理器）。
- **日誌（`tracing`）永不記錄辨識文字、潤飾結果、詞庫內容**，只記長度、耗時、provider、錯誤碼。由單元測試檢查日誌輸出不含測試句子。

**檔案位置**：設定 `%APPDATA%\v2c\config.toml`、紀錄 `%APPDATA%\v2c\history.db`。

**打包**：`tauri build` 產出 NSIS 安裝檔；CUDA 執行階段 DLL 隨附或要求使用者安裝 **【實作前確認】** 授權與大小。

---

## 8. Android（apps/android）

- `V2cImeService : InputMethodService`；`onCreateInputView` 回傳 `ComposeView`（必須手動設定 `ViewTreeLifecycleOwner` 與 `SavedStateRegistryOwner`，否則 Compose 會當掉）。
- 鍵盤：大麥克風鍵（按住說話 / 點一下開始、再點一下結束）、中翻英鍵（只在設定了 LLM 時顯示）、退格、換行、切回原輸入法（`switchToPreviousInputMethod()`）。
- 辨識：`SpeechRecognizer`，`EXTRA_LANGUAGE = "zh-TW"`、`EXTRA_PARTIAL_RESULTS`（顯示在鍵盤上方預覽）、`EXTRA_PREFER_OFFLINE`；API 31+ 優先 `createOnDeviceSpeechRecognizer`。
- 權限：IME 無法自己請求 `RECORD_AUDIO`，由設定 Activity 引導授權並啟用鍵盤。
- 內建辨識不可用（`SpeechRecognizer.isRecognitionAvailable()` 為 false，或回傳語言不支援錯誤）：若已串接雲端 ASR（Gemini / OpenAI / 相容端點），改由 Kotlin 用 `AudioRecord` 錄 16 kHz PCM，經 `v2c-ffi` 送雲端辨識；都沒有則在鍵盤上顯示「請安裝或更新 Google 語音服務」並附前往 Play 商店的按鈕。
- 輸出：結果經 `v2c-ffi` 的 `zh_tw` 轉換 → `currentInputConnection.commitText()`。
- Rust 核心：`cargo-ndk` 編 `arm64-v8a`（必要時加 `x86_64` 給模擬器）；uniffi 產生 Kotlin 綁定；FFI 提供**同步**函式（內部自帶 tokio runtime），Kotlin 在 `Dispatchers.IO` 呼叫，避開 uniffi async 的複雜度。
- 金鑰：Android Keystore 加密後存 DataStore。
- 最低版本：Android 9（API 28），以 `switchToPreviousInputMethod` 為準。

---

## 9. Linux（Phase 3，摘要）

- 熱鍵：`ashpd`（XDG portal）GlobalShortcuts 的 Activated/Deactivated 做按住說話；GNOME 版本不支援時退回 `evdev`（需將使用者加入 `input` 群組）。**【實作前確認】** 你的 Ubuntu 版本的 GNOME 是否支援此 portal。
- 注入：以剪貼簿＋Ctrl+V 為主（RemoteDesktop portal 送 Ctrl+V）。**【實作前確認】** GNOME Wayland 下背景程式設定剪貼簿的方式（GNOME 不支援 wlr-data-control）。
- 浮動標籤：Wayland 不允許 App 自訂視窗位置與置頂，可能改用系統通知或 GNOME 擴充；Phase 3 再決定。

---

## 10. 依賴圖

```
v2c-core ─┬─ postprocess / prompts / trigger / inject_plan（純邏輯，先做）
          ├─ asr/llm 雲端 providers
          └─ history / vocab / config
v2c-asr-local ──(依賴 core 的 trait)
v2c-platform/windows ──(core 的 inject_plan、trigger)
v2c-cli ──(core + asr-local + platform)         ← Phase 0 量測工具
apps/desktop ──(core + asr-local + platform)    ← Phase 1
v2c-ffi ──(core) → apps/android                 ← Phase 2
v2c-platform/linux → apps/desktop              ← Phase 3
```

---

## 11. 各階段細部任務

每個任務：**做什麼 → 主要檔案 → 驗收 → 驗證方式**。

### Phase 0 — 技術驗證

**T0.0 開發環境、workspace 與核心介面（介面先行，單一 agent）**
- `tools/setup-dev.ps1`：以 winget 安裝 Rust（stable, MSVC）、CUDA Toolkit、LLVM/Clang（whisper-rs 編譯需要）、Node.js（Tauri UI）；🧑 由你檢查後執行。
- 建立 workspace 與各 crate 空殼；`v2c-core` 寫好第 2 節的 trait、型別、`Mode`、`Output/Timings`、錯誤型別，以及 `config.rs` 的完整設定格式與預設值（含 `[route]`、`[gpu]`、`[history]`、`mic_mode`）。只有介面與預設值，不寫實作。
- `tests/fixtures/audio/README.md`：錄製說明（10 段：純中 3、純英 2、夾雜 3、含贅字與自我更正 1、含詞庫詞 1；16 kHz mono WAV）與每段的期望文字 `.txt`；🧑 由你錄音。
- 驗收：`cargo build`、`cargo test`、`cargo clippy -- -D warnings` 通過；🧑 你審查介面後合併，之後才開始平行任務。

**T0.1 Breeze 轉檔與量測**
- 檔案：`tools/convert-breeze/`、`crates/v2c-asr-local/src/breeze.rs`、`v2c-cli bench`。
- `v2c-cli bench --asr breeze --model <path> tests/fixtures/audio` 輸出每段的辨識文字、字元錯誤率（CER）、耗時。
- 驗收：10 秒音檔辨識 ≤ 1.2 秒；輸出繁體；夾雜句英文保留；純英文句不被翻成中文。
- **VRAM 實測**：在平常開著 Chrome、Teams 等程式的狀態下，用 `nvidia-smi` 記錄 Breeze q5_0 / q8_0 與 LM Studio 3B / 4B 各組合的實際用量，回填第 5.1 節的方案表；確認「均衡」方案合計 ≤ 4 GB，且辨識時 2560×1440 畫面拖曳視窗、播影片沒有明顯卡頓。順便確認筆電能否把畫面輸出切到 AMD 內顯。
- 不通過的備案：改 q8 / q5 / 換 Whisper large-v3-turbo，再量一次。

**T0.2 熱鍵狀態機與 Windows 掛勾**
- 檔案：`v2c-core/src/trigger.rs`（含單元測試）、`v2c-cli hotkey-test`（Windows 掛勾，印出狀態機指令）。
- 驗收：單元測試涵蓋第 3 節表格每一列；實機：按住、雙擊、Ctrl+C 不誤觸、右 Ctrl+Shift 進翻譯模式。

**T0.3 注入**
- 檔案：`v2c-core/src/inject_plan.rs`（含單元測試）、`v2c-cli inject-test "這個 meeting 我們 delay 一下\n第二行"`（倒數 3 秒後注入前景視窗）。
- 驗收：在記事本、Chrome（Gmail 與一般網頁輸入框）、LINE、VS Code、Word 正確輸出；注音輸入法開著時不受干擾；LINE 換行不會送出訊息。

**檢查點 0**：三項通過，記錄量測數據到 `tasks/spike-results.md`，再進 Phase 1。

### Phase 1 — Windows MVP

| 任務 | 主要檔案 | 驗收 |
|------|----------|------|
| T1.1 最小可用路徑 | `apps/desktop` 骨架、`controller.rs`、`recorder.rs`、`pipeline.rs`、`postprocess/zh_tw.rs` | 記事本中按住右 Ctrl 說中/英/夾雜，正確輸出；放開到出字 ≤ 1.5 秒（不含潤飾）；預錄緩衝讓第一個字不被吃掉 |
| T1.2 剪貼簿備援 | `crates/v2c-platform/src/windows/inject.rs`、App 規則 | 黑名單 App 走貼上；原剪貼簿文字與圖片被還原；系統管理員視窗有提示 |
| T1.3 浮動標籤＋系統列 | `overlay.rs`、`tray.rs`、`ui/overlay` | 錄音/處理/錯誤狀態正確；打字中的視窗不失焦 |
| T1.4 雙擊免持 | `trigger.rs` 接上 controller | 雙擊開始、單擊結束、5 分鐘上限、Esc 取消 |
| T1.5 LM Studio 潤飾 | `llm/openai_compat.rs`、`prompts.rs`、`postprocess/fillers.rs` | 贅字與自我更正處理正確；不改語意；LM Studio 未啟動 → 規則式去贅字＋提示；含潤飾總時間 ≤ 2.5 秒 |
| T1.6 中翻英 | `Mode::TranslateZhToEn` | 右 Ctrl+Shift 說中文輸出英文；未設定 LLM 時提示 |
| T1.7 雲端 providers | `asr/{openai,gemini,compat}.rs`、`llm/{gemini,claude}.rs`、`keyring`、`route` 設定 | 各 provider 用 `wiremock` 單元測試；實機各跑一次，雲端 ≤ 3 秒；雲端失敗時自動退回地端 |
| T1.7b 串接雲端精靈 | `ui/cloud-setup`、`test_connection()`、`docs/cloud-setup.md` | 從零開始依精靈串接 Gemini、OpenAI、Claude、自訂端點各一次皆成功；金鑰錯誤時顯示明確原因；一鍵切回全部地端 |
| T1.8 個人詞庫 | `vocab.rs`、`ui/vocab` | 詞庫詞在 initial prompt 與潤飾中生效；含詞庫詞的測試音檔 CER 下降 |
| T1.9 輸入紀錄 | `history.rs`、`ui/history` | 最近 200 筆可複製、重新輸出、刪除；不存音訊；DPAPI 加密（直接開 SQLite 看不到明文）；7 天自動清除；不記錄的 App 生效；日誌不含辨識文字 |
| T1.10 SenseVoice 備援 | `v2c-asr-local/src/sensevoice.rs` | 停用 GPU 時自動改用；輸出繁體 |
| T1.10b VRAM 管理 | `v2c-asr-local/src/gpu_guard.rs`、`[gpu]` 設定、`ui/settings` 使用條、`docs/local-setup.md` | 可用 VRAM 低於保留額度時不載入／自動卸載 Breeze 並改用 CPU；開啟 `heavy_apps` 名單內的程式時自動讓路、關掉後恢復；閒置 15 分鐘卸載；三種方案可切換 |
| T1.11 設定與安裝包 | `ui/settings`、`config.rs`、`tauri.conf.json` | 熱鍵可改；首次啟動下載地端模型；NSIS 安裝檔在另一台乾淨的 Windows 上，**不填任何金鑰**即可安裝並使用；Windows Defender 掃描不誤報（誤報則記錄並在 README 說明處理方式）；README 說明 SmartScreen「仍要執行」步驟 |

**檢查點 1**：日常使用一週，問題記在 `tasks/dogfood-windows.md`，修完再進 Phase 2。

### Phase 2 — Android

| 任務 | 主要檔案 | 驗收 |
|------|----------|------|
| T2.1 語音鍵盤最小路徑 | `V2cImeService.kt`、`KeyboardView.kt`、`SetupActivity.kt` | 在 LINE、Chrome 按住麥克風輸入中/英/夾雜；內建辨識不可用時顯示安裝提示 |
| T2.2 功能鍵 | `KeyboardView.kt` | 點擊模式、退格、換行、切回原輸入法 |
| T2.3 Rust 核心接入 | `crates/v2c-ffi`、Gradle 的 cargo-ndk 任務 | `zh_tw` 轉換與紀錄可用；APK 內含 `.so` |
| T2.4 中翻英鍵＋雲端 ASR 備援 | FFI `translate()`、`transcribe_pcm()`、`AudioRecord` | 設定 LLM 後才顯示翻譯鍵；內建辨識不可用且已串接雲端 ASR 時改走雲端 |
| T2.5 設定頁與 APK | `SettingsActivity.kt`、Keystore、串接雲端精靈（同第 6.2 節） | 不填金鑰即可語音輸入；依精靈串接雲端後出現中翻英鍵；側載 APK 在你的手機安裝並使用 |

**檢查點 2**：手機日常使用一週。

### Phase 3 — Linux

T3.1 portal 熱鍵 → T3.2 剪貼簿注入 → T3.3 浮動提示 → T3.4 AppImage/.deb。Phase 2 結束後，先重新驗證第 9 節的【實作前確認】項目，再細化任務。

---

## 12. 測試與驗證

- **單元測試**（`cargo test`）：`trigger`、`inject_plan`、`postprocess`、`prompts` 組裝、各 provider 的請求格式與錯誤處理（`wiremock`）、`Pipeline` 搭配 mock provider（逾時退回、LLM 吐簡體再轉換）。
- **回歸量測**：`cargo run -p v2c-cli -- bench --asr <provider> [--llm <provider>] tests/fixtures/audio` 輸出 CER 與延遲表；每次改模型、提示詞後都跑一次並與上次比較。
- **單一測試**：`cargo test -p v2c-core trigger::` 這類寫法只跑某模組。
- **手動驗收**：每個檢查點以 Chrome、LINE、VS Code、Word、記事本實際操作。
