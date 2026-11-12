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

**TEA-ASR-1.1（GPU，手動選用；T1.12）**
- 模型：`JacobLinCool/TEA-ASR-1.1`（MIT，以 Qwen3-ASR-1.7B 微調，台灣華語＋中英夾雜，輸出繁體台灣用語）。用社群轉好的 GGUF：`dorajim15/TEA-ASR-1.1-transcribe-cpp` 的 `TEA-ASR-1.1-Q8_0.gguf`（2.18 GB）。
- 推論：`transcribe-cpp` crate（transcribe.cpp 的 Rust 綁定），feature `cuda`＋`shared`（DLL，避免與 whisper-rs 的 ggml 符號衝突）。**【實作前確認】** Windows＋CUDA 建置。
- 輸入 16 kHz 單聲道、每段 ≤30 秒；語言固定 `zh`；輸出移除 U+E000–U+F8FF；不做簡轉繁；幾乎沒有標點，由潤飾補上。
- 選擇邏輯：只有 `asr = "tea_asr"` 才用；GPU 不允許、無 CUDA、模型不存在時改用 SenseVoice。`auto` 不變。VRAM 管理規則與 Breeze 相同（5.1）。
- 作者自測 MER（%，CommonVoice zh-TW／ASCEND 中英夾雜）：TEA-ASR-1.1 3.58／9.60，Breeze-ASR-25 8.03／17.53。尚未在本機實測。

**同一時間只載入一個辨識模型**（2026-11-11 你決定；T1.12 一併實作）
- 引擎只有一個「目前模型」槽位：Breeze、TEA-ASR、SenseVoice 三者同時最多一個在記憶體。
- 切換（設定變更、自動判斷、讓路、恢復）一律先卸載舊模型、確認釋放後再載入新模型；切換期間按熱鍵的那段錄音等新模型載入完再辨識（浮動標籤顯示「切換模型中」），不得用舊模型與新模型並行。
- GPU 模型因顯存不足、全螢幕或 `heavy_apps` 讓路而卸載後，載入 SenseVoice（CPU）暫代；GPU 恢復後卸載 SenseVoice、載回使用者選的 GPU 模型。
- `asr = "auto"` 保留，只決定要載入哪一個；不再為了備援預先常駐 SenseVoice。

### 5.1 顯示卡記憶體（VRAM）管理

**你這台電腦的現況**（2026-11-08 以 `nvidia-smi` 查得）：RTX 2060 Max-Q 共 6144 MiB，**正在負責畫面輸出**（2560×1440，display_active = Enabled）；平常桌面程式（Chrome、Teams、Edge、Docker Desktop、Telegram 等）已經用掉約 1.4 GB，剩約 4.5 GB。另有 AMD 內顯與 Trigger 6 USB 顯示卡。

**預算原則**：保留約 **600 MiB** 給畫面輸出與一般程式（原訂 2 GB；2026-11-10 實測 Breeze q5_0 約 1.7 GB＋LM Studio 3B 約 2.4 GB 後剩約 860 MiB，保留 2 GB 會讓兩者無法同時在 GPU 上，你決定改為 600 MiB，畫面是否卡頓於 dogfood 觀察），v2c 與 LM Studio 合計上限約 **4 GB**。

| 方案 | Breeze-ASR-25 | LM Studio 語言模型 | 預估 VRAM | 適用情境 |
|------|---------------|--------------------|-----------|----------|
| **均衡（預設）** | GPU，q5_0（約 1.5 GB） | GPU，3B Q4_K_M、context 2048（約 2.3 GB） | 約 3.8 GB | 日常文書、瀏覽、開會 |
| 省顯存 | GPU，q5_0（約 1.5 GB） | 1.5B 模型或 CPU 執行（0～1.2 GB） | 1.5～2.7 GB | 同時開較多吃顯卡的程式；潤飾變慢約 1～2 秒 |
| 不用顯卡 | CPU，SenseVoice-Small | CPU 執行或改用雲端 | 0 | 玩遊戲、剪片、3D 繪圖 |

上表的數字是估計值，T0.1 會實測後回填。

**v2c 程式內的保護機制**
1. **載入前檢查**：用 NVML（`nvml-wrapper` crate【實作前確認】）讀可用 VRAM；可用量扣掉保留額度（`vram_reserve_mb`，預設 600）後放不下 Breeze，就改用 SenseVoice（CPU），並在浮動標籤提示一次。
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
vram_reserve_mb = 600         # 保留給畫面輸出與其他程式
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
| OpenAI | `gpt-transcribe`（multipart，帶 `prompt`；`gpt-4o-transcribe` 已標示停用，2027-02-26 關閉，T1.7 查證） | `/chat/completions` | |
| Gemini | `generateContent` 內嵌 WAV（base64） | 同左 | |
| Claude | — | Messages API | 不收音訊；預設 `claude-haiku-5-5`（速度優先），可改 |

- 模型名稱全部放設定檔，不寫死。
- 逾時：ASR 5 秒、LLM 3 秒；失敗時浮動標籤顯示錯誤，紀錄中保留 ASR 原文以便重送。
- 金鑰：`v2c-core` 定義 `SecretStore` trait，`keyring` feature 只依賴與 OS 無關的 `keyring-core`；桌面 App 啟動時以 `windows-native-keyring-store` 安裝 Windows 認證管理員作為預設 store（T1.7 查證：`keyring` 4.x 建議應用程式改用 `keyring-core`）；Android 另接 Keystore。設定檔只存 provider 名稱。
- 雲端 HTTP client 一律不跟隨轉址（避免 `x-api-key` 等自訂金鑰標頭被帶到其他主機）；官方服務的 base_url 必須是 https（loopback 除外）。

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

- **ASR 一定有地端可用**：首次啟動引導下載地端模型（第 5 節）；這一步完成前不主動提示雲端選項，避免使用者以為必須付費。例外（2026-11-11 你決定）：主視窗選單一律顯示「狀態、設定、詞庫、紀錄、雲端服務、初次設定」六個頁面（初次設定畫面也顯示選單）；模型步驟提供「略過，改用雲端服務」。沒有地端模型就離開初次設定（略過、稍後再設定或點選單）後，啟動時不再自動開啟初次設定。
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

**自訂相容端點的別名**（T1.13，2026-11-11 你決定）：自訂相容端點可在精靈或服務清單填選填的「名稱」（`[[cloud]] name`，例如「區網 Qwen」），所有顯示服務的地方都用它；沒填就顯示代號（`custom`、`custom-2`…）。代號 `id` 不變，改名不影響 `[route]` 與金鑰。Gemini／OpenAI／Claude 不提供別名。

**每個服務各自的語音辨識逾時**（T1.15，2026-11-11 你決定）：做語音辨識的服務（Gemini／OpenAI，或填了語音辨識模型的自訂相容端點）可填選填的 `[[cloud]] asr_timeout_ms`（整數毫秒 1000～60000）；有效逾時 = 該值，沒填就用 `[timeouts] asr_cloud_ms`（`CloudService::asr_timeout()`），實際辨識與「測試連線」都用它。精靈第 4 步「語音辨識逾時（毫秒，選填）」提供快選：Breeze-ASR-25 建議 8000（區網 `:8002` 實測每句 5.5～6.3 秒）、TEA-ASR 建議 8000（暫定）、用預設；建議值集中在 `apps/desktop/ui/cloud-setup/guides.js` 的 `asrTimeoutSuggestions`。服務清單顯示「辨識逾時 N ms」。

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
  - （T1.14，2026-11-11 你決定）變更先暫存，按「儲存」才寫入 config.toml 並套用，可「取消變更」，未儲存就離開會先詢問。最上方「目前使用中」顯示引擎實際狀態（實際使用的辨識引擎與執行位置、潤飾／中翻英實際走的服務與模型、改用備援的原因）。語音辨識用途選單直接列出各地端引擎。地端語音模型有「測試」。設定頁不再顯示 LM Studio 區塊（「地端」潤飾仍用 config.toml 的 `[local] llm_*`，預設本機 LM Studio；要用別台或別的模型就在雲端服務新增自訂相容端點）。
- 雲端服務：已串接的服務清單（測試連線、修改、移除）與「新增雲端服務」精靈（第 6.2 節）。
- （Phase 4，2026-11-12 你決定）頁面重整為 4 項選單：首頁（目前狀態＋套用預設組態＋常用開關）、服務、詞庫與紀錄、設定（常用＋進階）；設計 token 與共用元件見第 13 節。
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

## 9. Linux（Phase 3）

> T3.0 查證日期 2026-11-10。Linux 開發機：使用者指定「目前執行 agent 的這台電腦」（查證指令與結果見 `cards/T3.0-linux-planning.md`）。檢查點 2 尚未通過，是使用者授權提前進行 Phase 3。

### 9.1 本機實測能力（唯讀查證）

| 項目 | 結果 | 影響 |
|------|------|------|
| OS／桌面 | Ubuntu 22.04.5、核心 6.8、GNOME Shell 42.9、Wayland（`XDG_SESSION_TYPE=wayland`） | 原計畫假設的 portal 多數不存在 |
| xdg-desktop-portal | 1.14.4；後端 xdg-desktop-portal-gnome 42.1 | — |
| GlobalShortcuts portal | **沒有**（GNOME 48 才加入） | 熱鍵只能退回 evdev |
| Clipboard portal | **沒有**（xdg-desktop-portal-gnome 45 才加入） | 背景程式無法設定剪貼簿 |
| RemoteDesktop portal | 有，**version 1**，AvailableDeviceTypes=7 | 可送按鍵，但沒有 `persist_mode`，每次建立工作階段都會跳出同意對話框；Wayland 下 mutter 的 `NotifyKeyboardKeysym` 不送不在鍵盤配置裡的字元（中文無法逐字輸入） |
| evdev | `/dev/input/event*` 屬 `root:input` 0660；使用者**不在** `input` 群組；`/dev/uinput` 為 root 0600 | evdev 熱鍵需要人工把使用者加入 `input` 群組 |
| 鍵盤裝置 | `/proc/bus/input/devices` 只有電源鍵、Video Bus、HDMI 音訊，**沒有實體鍵盤**（T3.1 實測；推測經遠端桌面操作） | 即使加入 `input` 群組，evdev 熱鍵在本機也收不到按鍵 |
| 音訊 | PulseAudio 伺服器；**沒有任何擷取裝置**（`arecord -l` 為空，唯一 source 是 HDMI monitor） | 無法實機錄音；ASR 只能用 mock 或 WAV |
| GPU | Intel Iris Xe，**沒有 NVIDIA** | 不用 CUDA／Breeze；第 5.1 節 VRAM 管理在 Linux 上停用（`gpu.enabled=false`） |
| 工具鏈 | rustc／cargo 1.99.1、clippy、rustfmt、gcc、Node；**沒有 cmake、clang、libclang、gtk／webkit2gtk-4.1／libsoup-3 開發檔** | `whisper-rs-sys` 編不過 → `v2c-asr-local`、`v2c-cli`、Tauri 桌面 App 在本機無法建置；`v2c-core`、`v2c-platform` 可以 |
| 實錄音 ASR（2026-11-10） | 你的 10 段錄音（4.4～7.7 秒）經 8002：每段約 5.6～6.1 秒；CER：zh 0～5.9%、en 0～2%、mix 0～4.3%、filler（對 raw）10.5%、vocab 41.9%（未帶詞庫）；加潤飾後 zh-01 0%、mix-01 4.3%、filler-01 20%，總耗時約 6.4～6.7 秒 | 準確度可用；延遲遠超過 ≤ 3 秒目標，需在 ASR 主機端改善 |
| 既有 ASR | `http://10.20.30.23:8002/v1`（使用者提供）：OpenAI 相容 `/audio/transcriptions`，需要金鑰，模型 `breeze-asr-25`；英文合成句實測約 5.6 秒，超過預設 `asr_cloud_ms=5000` | 用 breeze-asr-25 時需 `asr_cloud_ms = 8000`；2026-11-11 已改用同主機 `:8001` 的 `tea-asr-1`（見下一列） |
| Linux ASR 定案（2026-11-11） | 使用者決定：遠端 `tea-asr-1`（`:8001`，10 段錄音平均 1.7 秒、CER 1.8%）＋本機 SenseVoice CPU 備援（約 0.4 秒、CER 7.9%）；本機 CPU 跑 TEA-ASR-1 每段約 157 秒，不採用 | `asr_cloud_ms = 5000`、`[local] asr = "sense_voice"`、`cloud_fallback_to_local = true`；設定範例見 `docs/linux-setup.md` 第 4 節 |
| 既有 LLM | `http://10.20.30.23:8000/v1`：llama.cpp（build b10752）、模型 `Qwen3.8-27B-UD-Q6_K_L`、n_ctx 65536、不需金鑰；`/health` ok；`modalities.audio=false`；`/v1/audio/transcriptions` 回 501 | 只用於潤飾／翻譯，**不能做 ASR**；預設開啟思考，一句約 2.7 秒，關閉思考（`chat_template_kwargs.enable_thinking=false`）約 0.23 秒 |

### 9.2 設計（依查證結果調整）

- **能力偵測先行**（T3.1）：啟動時以 D-Bus 讀 portal 介面與 `version`，結果是純資料結構 `LinuxCaps`；後端選擇是純函式，可單元測試。
- **熱鍵**（T3.1）：
  1. GlobalShortcuts portal（GNOME ≥ 48）：`Activated`／`Deactivated` 對應 `KeyDown`／`KeyUp`，可做按住說話。
  2. 否則 evdev：讀鍵盤裝置，KEY_RIGHTCTRL／Shift／Esc 對應狀態機事件；`EACCES` 時回報「需要加入 input 群組」，不自動提權。
  3. 都沒有：明確回報不可用（不得默默失效）。
- **注入**（T3.2）：中文不能靠 keysym 逐字輸入，Linux 一律走「剪貼簿＋Ctrl+V」：
  - 按鍵：RemoteDesktop portal `NotifyKeyboardKeycode`（Ctrl=29、V=47、Shift+Enter 換行）；使用者在對話框按取消 → `InjectError::PermissionDenied`，改為只把文字留在剪貼簿並提示。
  - 剪貼簿：Clipboard portal（GNOME ≥ 45）；本機（GNOME 42）沒有可用的背景剪貼簿方法 → 回報 `Unsupported`，不假裝成功。
  - 等修飾鍵放開、備份／還原剪貼簿（被別人改過就不還原）的邏輯與 Windows 相同，寫成可注入 mock 後端的純流程。
- **既有 LLM 串接與端到端模擬**（T3.3）：用現有 `OpenAiCompatLlm`，以設定指定 endpoint／model／timeout（`[[cloud]]` 自訂端點或 `[local] llm_base_url`）；mock 測試涵蓋連線失敗、逾時、HTTP 錯誤、`reasoning_content` 與 `<think>` 回應解析；可用時以合成文字做最小真實連線。關閉思考需要在請求中加入額外欄位，屬於 `v2c-core` 公開介面變更，**須你核准**後另外處理。
- **提示與打包**（T3.4）：Wayland 不能自訂置頂視窗位置，提示改用 `org.freedesktop.Notifications`（D-Bus）；打包為 .deb／AppImage，需要 Tauri 的 Linux 開發套件（本機沒有，由你安裝後執行）。
- **ASR**：本機無 NVIDIA、無麥克風。Phase 3 流程以 mock ASR 驗證；可行方案為 SenseVoice CPU（需 sherpa-onnx 共享庫與約 160 MB 模型，未下載）或遠端相容 ASR 端點（目前的 LLM 主機不支援）。
- **建議**：Ubuntu 24.04（GNOME 46）才有 Clipboard portal 與可記住同意的 RemoteDesktop v2；GNOME 48 以上（Ubuntu 25.04 起）才有 GlobalShortcuts。本機維持 22.04 時，只能「evdev 熱鍵＋僅複製到剪貼簿」或改用 IBus 引擎輸出（與 `v2cplan.md`「非輸入法框架」的決策衝突，需你決定）。

### 9.2a IBus 引擎（2026-11-10 你決定 Linux 改走 IBus，取代 9.2 的「剪貼簿＋Ctrl+V」為主要路線）

- **為什麼**：本機 IBus 1.5.26 由 GNOME 啟動（`ibus-daemon --panel disable`）；GNOME 在 Wayland 下透過 IBus 處理所有輸入框。IBus 引擎可直接 `CommitText` 任意 Unicode（中文、emoji），不需要剪貼簿、RemoteDesktop 同意對話框，也不需要 `input` 群組。
- **熱鍵**：引擎啟用時，焦點輸入框的每個按鍵都會先經過引擎的 `ProcessKeyEvent(keyval, keycode, state)`，包含放開（`IBUS_RELEASE_MASK`）；右 Ctrl（`Control_R`）、Shift、Esc 轉成 `trigger::Event`，其他鍵原樣放行（回傳 false）。限制：只在「v2c 輸入來源啟用且有輸入框焦點」時有效。
- **注入目標**：輸入框失焦時 daemon 會把引擎移到內部的 fake context，送到那裡的 CommitText 會被默默丟掉；所以只輸出到「已啟用、有焦點，且 FocusIn 之後收到過按鍵」的引擎，否則回報錯誤，由呼叫端保留文字（T3.5 實測）。`ibus list-engine` 不列出動態註冊的引擎，要看 `ActiveEngines`。
- **注入**：`Action::Text` → `CommitText`；`NewLine` → `ForwardKeyEvent`（Return，ShiftEnter 帶 Shift）；`Paste` 也改為 `CommitText`。
- **註冊**：.deb 安裝 `/usr/share/ibus/component/v2c.xml`；開發時用 IBus 的 `RegisterComponent` 動態註冊（是否會出現在 GNOME 設定的清單需實測）。使用者要在「設定 → 鍵盤 → 輸入來源」加入 v2c。
- **與中文輸入法並存**：v2c 引擎啟用時，一般打字只送英文（按鍵放行）；要用注音等輸入法時以 Super+Space 切換。
- **9.2 的 portal 熱鍵與剪貼簿注入**（T3.1、T3.2）保留為「引擎未啟用時」的備援，不再是主要路線。
- 【實作前確認】IBus D-Bus 介面（`org.freedesktop.IBus`、`org.freedesktop.IBus.Factory`、`org.freedesktop.IBus.Engine`）與 `IBusText` 的序列化格式，對照 ibus 1.5.26 原始碼。

### 9.3 來源（2026-11-10 查證）

- GNOME 48 開發者新功能（GlobalShortcuts portal）：https://release.gnome.org/48/developers/
- xdg-desktop-portal-gnome GlobalShortcuts 需求追蹤：https://gitlab.gnome.org/GNOME/xdg-desktop-portal-gnome/-/issues/85
- xdg-desktop-portal-gnome 45.beta NEWS（Clipboard 後端、還原遠端桌面工作階段）：https://download.gnome.org/sources/xdg-desktop-portal-gnome/45/xdg-desktop-portal-gnome-45.beta.news
- IBus Component（`/usr/share/ibus/component`、`IBUS_COMPONENT_PATH`）：https://ibus.github.io/docs/ibus-1.5/IBusComponent.html ；ibus(1)：https://man.archlinux.org/man/extra/ibus/ibus.1.en
- RemoteDesktop portal 規格（`persist_mode` 自 version 2）：https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.RemoteDesktop.html
- Wayland 下 RemoteDesktop 無法注入任意 Unicode：https://discourse.gnome.org/t/injecting-arbitrary-unicode-characters/24799 ；mutter issue 2111：https://gitlab.gnome.org/GNOME/mutter/-/issues/2111
- ashpd（0.13.x，`global_shortcuts`／`remote_desktop`／`clipboard` feature）：https://docs.rs/crate/ashpd/latest/features
- 本機能力：`gdbus introspect`、`pactl`、`arecord -l`、`lspci`、`pkg-config`、`curl` 唯讀查詢（指令見 `cards/T3.0-linux-planning.md`）

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
apps/desktop/ui 重構＋presets（不改 v2c-core） ← Phase 4（第 13 節）
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

（T3.0 於 2026-11-10 重新查證後改為）T3.1 能力偵測＋熱鍵 → T3.2 剪貼簿注入（依賴 T3.1）；T3.3 既有 LLM 串接＋端到端模擬（可與 T3.1 平行，最後接上 T3.2）→ T3.4 通知提示＋打包。卡片見 `cards/T3.*.md`。

### Phase 4 — 介面精簡、預設組態檔、新圖示（0.2.0）

| 任務 | 主要檔案 | 驗收 |
|------|----------|------|
| T4.0 設計系統＋導覽外殼 | `ui/app.css`、`ui/lib/`、`ui/App.svelte`、`ui/dev-mock.js`、`package.json` | 4 項導覽、舊網址轉址；淺色／深色；所有舊功能照常；`npm run check`／`build` 通過 |
| T4.1 預設組態檔 | `src-tauri/presets/`、`commands/presets.rs`、`ui/presets/` | 第 13.1 節的套用規則全部有單元測試；兩組來回套用不掉金鑰、不動使用者自訂服務 |
| T4.2 首頁 | `ui/routes/Home.svelte`、`ui/home/` | 狀態卡、預設組態卡、常用開關即時生效 |
| T4.3 設定頁 | `ui/routes/Settings.svelte`、`ui/settings/` | 常用＋進階；草稿／儲存行為不變；原有欄位都找得到 |
| T4.4 服務頁、詞庫與紀錄、初次設定、浮動標籤 | `ui/routes/CloudSetup.svelte`、`ui/cloud-setup/` 等 | 精靈照常；依預設組分組；樣式一致 |
| T4.5 圖示候選 | `src-tauri/icons/src/` | 3 個 SVG 候選，16 px 可辨識；🧑 選定 |
| T4.6 交付 | 圖示、`docs/`、README、版本 0.2.0、`releases/` | 全部檢查通過；安裝檔與 SHA-256 在 `releases/` |

**檢查點 4**：🧑 安裝 0.2.0，TN、RD 各套用一次並實際說話；確認介面是否夠簡潔。卡片見 `cards/T4.*.md`。

---

## 12. 測試與驗證

- **單元測試**（`cargo test`）：`trigger`、`inject_plan`、`postprocess`、`prompts` 組裝、各 provider 的請求格式與錯誤處理（`wiremock`）、`Pipeline` 搭配 mock provider（逾時退回、LLM 吐簡體再轉換）。
- **回歸量測**：`cargo run -p v2c-cli -- bench --asr <provider> [--llm <provider>] tests/fixtures/audio` 輸出 CER 與延遲表；每次改模型、提示詞後都跑一次並與上次比較。
- **單一測試**：`cargo test -p v2c-core trigger::` 這類寫法只跑某模組。
- **手動驗收**：每個檢查點以 Chrome、LINE、VS Code、Word、記事本實際操作。
- **前端**（Phase 4 起）：`cd apps/desktop && npm run check && npm test && npm run build`；`npm run dev` 在一般瀏覽器以 `ui/dev-mock.js` 的假資料開啟各頁，截圖淺色／深色供審查。

---

## 13. 介面精簡與預設組態檔（Phase 4，2026-11-12）

背景：使用者回饋介面「看起來很複雜」。現況是 6 項平鋪選單、設定頁 9 個區塊攤平、沒有設計 token（顏色寫死、瀏覽器預設控制項）、加一個區網端點要走 7 步精靈，公司兩組環境各 3 個端點要走三次。**不變更 `v2c-core` 公開介面。**

### 13.1 預設組態檔

- 位置：`apps/desktop/src-tauri/presets/{TN,RD}_CUSTOM_LLM_ADDR.toml`，以 `include_str!` 編進程式（同 `models.json`），不改打包設定。也可從使用者選的檔案載入。
- 格式：一份只寫部分內容的 `config.toml`，加一個 `[preset]` 區段（`Config` 解析器忽略未知區段）：

```toml
[preset]
title = "TN 區網服務"
id_prefix = "tn-"
asr_choices = ["tn-tea-asr", "tn-breeze-asr"]   # 套用時讓使用者選一個當 route.asr
needs_key = ["tn-breeze-asr"]                   # 需要金鑰的服務（金鑰存進系統金鑰庫）
default_keys = { "tn-breeze-asr" = "tn8227" }  # 沒輸入時存這把共用金鑰（T4.8）
detect_llm_model = true                         # 套用時查 /v1/models

[route]
polish = "tn-qwen"
translate = "tn-qwen"
cloud_fallback_to_local = true

[[cloud]]
id = "tn-tea-asr"
name = "TN TEA-ASR"
kind = "open_ai_compat"
base_url = "http://10.20.30.23:8001/v1"
asr_model = "tea-asr-1"

[[cloud]]
id = "tn-breeze-asr"
name = "TN Breeze-ASR"
kind = "open_ai_compat"
base_url = "http://10.20.30.23:8002/v1"
asr_model = "breeze-asr-25"
asr_timeout_ms = 8000

[[cloud]]
id = "tn-qwen"
name = "TN Qwen3.8 27B"
kind = "open_ai_compat"
base_url = "http://10.20.30.23:8000/v1"
llm_model = "Qwen3.8-27B-UD-Q6_K_L"
```

  RD 檔相同，主機改 `10.88.95.2`（三個端點都是；2026-11-12 你更正），前綴 `rd-`，名稱前綴 `RD `。
- 套用規則：
  - 新增或更新檔內的服務（依 id），並設定 `[route]` 的 polish／translate／cloud_fallback_to_local；`route.asr` = 使用者在 `asr_choices` 中選的那個。
  - 其他服務（另一組、使用者自己加的）與它們的金鑰不動，所以 TN ↔ RD 來回切換不用重貼金鑰。
  - 白名單：只採用 `[[cloud]]`、`[route]`、`[preset]`；其他區段一律忽略且不寫入。
  - 驗證沿用 `commands/cloud.rs` 既有規則（名稱、逾時範圍、`route::effective_base_url`）；服務 id 必須以 `id_prefix` 開頭，`route` 只能指向檔內服務或 `local`。任何一項不過就整份拒絕，`config.toml` 不變。
  - 既有服務被更新且主機改變時，照 `save_service` 的規則刪掉舊金鑰（不把金鑰送到新主機）；主機不變時保留金鑰。
  - 文字模型：`detect_llm_model = true` 時向端點查模型清單（重用 `cloud_models` 的查詢），只有一個模型就用它，否則（包含查詢失敗、逾時 3 秒）用檔內的 `llm_model`。
  - 一次原子寫入（持有 `CONFIG_LOCK`、toml_edit 保留註解），寫完發 `v2c:config-changed`。
  - 金鑰（T4.8）：存進系統金鑰庫（`SecretStore`）；寫設定失敗時還原金鑰（同 `save_service`）。`[preset] default_keys = { "<id>" = "<key>" }` 可為 `needs_key` 內的服務附預設金鑰（id 必須在 `needs_key`；值去頭尾空白、非空、無控制字元、≤256 字元，否則整份拒絕）。內建 TN、RD 各附 Breeze 的共用金鑰 `tn8227`（2026-11-12 使用者決定，入版控）。每個 `needs_key` 服務依序選金鑰：(a) 對話框輸入的 → (b) 已存且端點未變的舊金鑰 → (c) `default_keys` → (d) 都沒有就列入 `key_missing`；未被選為語音辨識的 `needs_key` 服務也會補上預設金鑰（之後切到 Breeze 不用再貼）。端點改變時舊金鑰被 (a) 或 (c) 取代，都沒有才刪除。預覽只回 `has_default_key`，金鑰值不進 UI 與日誌。
  - 日誌只記預設檔名稱、服務數、錯誤碼、耗時。
- 指令：`presets_list() -> Vec<PresetSummary>`、`presets_preview(source) -> PresetPreview`、`presets_apply(source, asr_choice, keys) -> PresetApplied`；`source` = `{ builtin: "TN" }` 或 `{ file: <path> }`。

### 13.2 介面

- 選單 4 項（左側導覽列，圖示＋文字；視窗窄時改成底部導覽列）：

| 頁面 | 網址 | 內容 |
|------|------|------|
| 首頁 | `#/` | 目前使用中（原 RuntimeStatus）、套用預設組態、常用開關（潤飾、語音辨識由誰處理） |
| 服務 | `#/services` | 各用途由誰處理、服務清單（依預設組分組）、新增服務精靈 |
| 詞庫與紀錄 | `#/library`（分頁 `vocab`／`history`） | 原詞庫頁與紀錄頁 |
| 設定 | `#/settings` | 常用（麥克風、用途選擇、潤飾、開機自動啟動）＋「進階」（預設收合：熱鍵時間、地端語音模型、顯示卡記憶體、App 規則、輸入紀錄設定、麥克風模式、雲端失敗改用地端）；底部「重新執行初次設定」 |

  舊網址 `#/cloud-setup` → `#/services`、`#/vocab` → `#/library?tab=vocab`、`#/history` → `#/library?tab=history` 轉址；`#/onboarding` 仍可直接開（首次啟動與設定頁底部的入口）。
- 視覺：Material 3 Expressive 風格。`ui/app.css` 定義 token（色彩角色 primary／on-primary／secondary-container／surface、surface-container-{lowest…highest}／outline／error 等，淺色與深色兩組；圓角 xs 4／sm 8／md 12／lg 16／xl 28／full；字級 display／headline／title／body／label；間距 4 的倍數；動態曲線與時間，`prefers-reduced-motion` 時關閉）。只用系統字型（不下載字型，符合 CSP）；不使用 Google 商標或圖檔，圖示自繪成行內 SVG。
- 共用元件 `ui/lib/`：Button（filled／tonal／outlined／text）、IconButton、Switch、Select、TextField、Card、ListItem、Dialog、SegmentedButton、Tabs、Banner、LinearProgress、NavRail、Icon（行內 SVG 圖庫）。各頁用元件取代重複的 `.hint`／`.bad` 樣式。
- 開發輔助：`ui/dev-mock.js` 在沒有 Tauri 時（`window.__TAURI_INTERNALS__` 不存在）提供假的 `invoke`／`listen`，只在 `npm run dev` 使用，不進正式版。`package.json` 新增 `check`（svelte-check）、`test`（vitest，只測純 JS）。

### 13.3 圖示

- 概念：聲波流向游標／AI 節點（聲音 → 電腦／代理人），外形帶出「V」。先出 3 個 SVG 候選與 16／24／32 px、淺色／深色預覽，🧑 你選定後以 `npm run tauri icon` 產生全部尺寸，導覽列與安裝檔同步使用。

### 13.4 依賴

```
T4.0 設計系統＋導覽外殼 ─┬─> T4.2 首頁 ───┐
T4.1 預設組態檔 ─────────┤                  │
                         ├─> T4.3 設定頁 ──┼─> T4.6 交付 0.2.0 ─> 檢查點 4
                         └─> T4.4 服務等頁 ┤
T4.5 圖示候選 ─(🧑 選定)───────────────────┘
```

### 13.5 注意

- 兩個預設檔含內網位址 `10.20.30.23`、`10.88.95.2`，會進 git（`tasks/plan.md` 已有前者）。若不想公開，改成預設檔 git-ignore、只在建置安裝檔時放入（🧑 T4.1 合併前決定）。
- RD 主機（`10.88.95.2`）沒有實測過；模型名稱與 `:8002` 需要金鑰是比照 TN 假設。
