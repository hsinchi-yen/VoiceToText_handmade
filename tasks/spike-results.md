# Phase 0 量測結果

## T0.1 Breeze-ASR-25

### 【實作前確認】結果（2026-11-09 查證）

| 項目 | 結果 | 來源 |
|------|------|------|
| 授權 | **Apache-2.0**（模型卡 metadata `license: apache-2.0`，repo 附 `LICENSE`）。訓練資料授權為 ODC-By＋Apache-2.0（合成中文）、CC0、MIT 等寬鬆授權。可轉檔、量化並放到你自己的 HF repo 散布（附 LICENSE 與出處）。 | https://huggingface.co/MediaTek-Research/Breeze-ASR-25 |
| 基底模型 | `openai/whisper-large-v2` 微調（`config.json`：`WhisperForConditionalGeneration`、d_model 1280、32+32 層、80 mel、vocab 51865） | https://huggingface.co/MediaTek-Research/Breeze-ASR-25/blob/main/config.json |
| repo 檔案 | `model.safetensors` 3.09 GB（16-bit 權重）、tokenizer 檔齊全（`vocab.json`、`added_tokens.json`、`merges.txt`…）；另有 `optimizer.bin` 3.6 GB 與兩個 OpenAI 格式 `.pt`（3.1 GB / 6.2 GB），轉檔不需要，腳本不下載 | HF API `api/models/MediaTek-Research/Breeze-ASR-25` |
| whisper.cpp 轉檔 | **支援**。`models/convert-h5-to-ggml.py` 專給 HF 微調的 Whisper 用：讀 `vocab.json`、`added_tokens.json`、`config.json`，以 `WhisperForConditionalGeneration.from_pretrained` 載入，加上 openai/whisper 的 `mel_filters.npz`。Breeze 的檔案與欄位都符合。v1.8.3 與 master 的腳本內容相同。注意：`transformers` 須用 4.x（5.x 預設以 bf16 載入，腳本的 `.numpy()` 會失敗）。 | https://github.com/ggml-org/whisper.cpp/blob/v1.8.3/models/convert-h5-to-ggml.py |
| 量化 | whisper.cpp `examples/quantize`（CMake target `whisper-quantize`），用法 `whisper-quantize in.bin out.bin q5_0\|q8_0` | https://github.com/ggml-org/whisper.cpp/tree/v1.8.3/examples/quantize |
| whisper-rs | 最新 **0.16.0**（2026-03-12），內建 whisper.cpp **1.8.3**（`whisper-rs-sys` 0.15.0）。features：`cuda`、`vulkan`、`log_backend`/`tracing_backend` 等 | https://crates.io/crates/whisper-rs 、https://docs.rs/whisper-rs/0.16.0 |

**whisper-rs 0.16 API 重點**（依 crate 原始碼確認）
- `WhisperContext::new_with_params(path, WhisperContextParameters)`；`WhisperContextParameters { use_gpu, flash_attn, gpu_device, dtw_parameters }` —— **有 flash attention 開關**，已預設開啟。
- `ctx.create_state()` → `WhisperState`（`Send + Sync`）；`state.pcm_to_mel()` + `state.lang_detect(offset_ms, threads)` 回傳 `(lang_id, probs)`；`whisper_rs::get_lang_str(id)` 轉成 `"en"`、`"zh"`。
- `state.full(FullParams, &samples)`；`FullParams::set_language(Some("zh"))`、`set_initial_prompt`（字串含 NUL 會 panic，已先移除）、`set_no_context`、`set_single_segment`、`set_audio_ctx`。
- 輸出：`state.as_iter()` 逐段 `segment.to_str_lossy()`。
- `whisper_rs::install_logging_hooks()`：沒開 `log_backend`／`tracing_backend` 時等於把 whisper.cpp 的日誌全部丟掉 —— 已在載入模型時呼叫，避免 whisper.cpp 的 debug 輸出帶出 token 文字（遵守「日誌不記錄辨識文字」）。

### 實作決策

- **語言策略**：`LanguageMode::Detect`（預設）先跑 `lang_detect`；偵測為 `en` → 以 `en` 解碼，其他一律 `zh`。`bench`／`transcribe` 可用 `--lang zh|en` 強制語言做對照。
- **`Lang` 對應**：依輸出文字的字元類別判斷（只看字元類別，不保存文字）：同時有中文字與拉丁字母 → `Mixed`；只有中文 → `Zh`；只有拉丁字母 → `En`；都沒有 → 依解碼語言。
- **initial prompt**：`AsrContext.prompt` 有值就用，否則用 `prompts::ASR_BASE_PROMPT`（`prompts::asr_initial_prompt` 尚未實作，T1.8 由呼叫端組好放進 `ctx.prompt`）。英文解碼也帶同一個 prompt。
- **解碼參數**：greedy（best_of 1）、`no_context`、`single_segment`、`no_timestamps`；其餘（溫度 fallback 等）用 whisper.cpp 預設值。
- **`cuda` feature 預設關閉**：`v2c-asr-local` 與 `v2c-cli` 各有 `cuda` feature（CLI 轉發給 asr-local，asr-local 轉發給 `whisper-rs/cuda`）。理由：開 CUDA 需要 CUDA Toolkit 且 nvcc 編譯 whisper.cpp 要數分鐘，預設關閉讓其他開發者與 CI 的 `cargo build --workspace` 不需 CUDA 也能過。正式桌面版（T1.x）在 `apps/desktop` 依賴時開 `features = ["cuda"]`。沒開 cuda 時會在 stderr 提醒「running on CPU」。
- `AsrProvider::transcribe`：在 Tokio runtime 內用 `spawn_blocking` 跑推論；不在 runtime 內就直接在呼叫端執行緒跑。單一 `WhisperState` 常駐（GPU 暫存只配置一次），推論以 `Mutex` 序列化。
- **錯誤**：找不到／載不了模型 → `V2cError::Config`（含模型路徑，不含文字）；取樣率不對／空音檔 → `V2cError::Audio`；推論失敗 → `V2cError::Provider { stage: Asr, provider: "breeze", kind: Server }`。錯誤訊息都不含辨識文字。

### CER 計算方式

`v2c-cli bench` 的 CER（字元錯誤率）＝ `Levenshtein(辨識結果, 期望文字) / 期望文字長度`，兩邊都先做以下正規化：
1. 全形 ASCII（U+FF01–U+FF5E）轉半形；全形空白轉一般空白。
2. ASCII 字母轉小寫。
3. 移除所有「非字母、非數字」的字元（空白、標點、符號；判斷用 Rust `char::is_alphanumeric`）。中文字算字母，所以**每個中文字、每個英文字母、每個數字各算一個單位**。
4. 不做簡繁轉換 —— 輸出簡體字會被算成錯字（這正是要驗收的「輸出繁體」）。
5. 不做數字正規化 —— 「三點」辨識成「3點」會算 1 個錯字。

期望文字：有 `<name>.raw.txt` 就用它（逐字稿，例如 `filler-01.raw.txt`），否則用 `<name>.txt`。期望文字為空時，辨識也為空記 0%，否則 100%。

### 建置筆記（這台電腦）

| 項目 | 結果 |
|------|------|
| CPU 版 `cargo build/test/clippy --workspace` | ✅ 通過 |
| `--features cuda`（CUDA 13.5、RTX 2060 = sm_75） | ✅ 編譯成功；用 whisper `ggml-base` 測試模型實測 GPU 推論正常（`CUDA : ARCHS = 750`） |

需要的環境設定（只在當次 PowerShell 有效，不改系統設定）：

```powershell
# 1. CMake：不在 PATH 上，用 VS Build Tools 內建的
$env:Path = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;$env:Path"
# 2. bindgen 需要 libclang（你的使用者環境變數已有，新開的視窗會自動帶入）
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
# 3. 短的 target 目錄：whisper.cpp 的 CMake/MSBuild 在太深的路徑會失敗（MSB4018，Windows 路徑長度上限）
$env:CARGO_TARGET_DIR = "$env:USERPROFILE\.v2ct"
# 4. 只有 --features cuda 需要：
$env:CUDA_PATH = 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5'   # 目前系統值指向舊的 v11.0
$env:CUDA_PATH_V13_5 = $env:CUDA_PATH       # CUDA 13.5 的 MSBuild 整合靠這個找 Toolkit
$env:CMAKE_CUDA_ARCHITECTURES = '75'         # 只編 RTX 2060 (Turing)，省很多編譯時間
```

遇到的錯誤與解法：
1. `error MSB4018 ... GetItemSpecModifier`（CMake TryCompile 失敗）：worktree 路徑太深，超過 Windows 260 字元上限 → 設 `CARGO_TARGET_DIR` 為短路徑。（或你自行在 Windows 開啟長路徑支援，這是系統設定，agent 不動。）
2. `CUDA 13.5.targets(688,9): error : The CUDA Toolkit directory '' does not exist.` → 系統的 `CUDA_PATH` 指向 v11.0、且沒有 `CUDA_PATH_V13_5` → 依上面第 4 點設定即可。**建議你**把系統環境變數 `CUDA_PATH` 改成 v13.5 並新增 `CUDA_PATH_V13_5`（之後桌面 App 的 release build 也需要）。
3. 首次 CUDA 編譯約 5～8 分鐘（nvcc 編 ggml-cuda）。

其他觀察（以 whisper `ggml-base` 測試模型＋Windows 內建 TTS 產生的英文／中文音檔做端到端驗證，**不是 Breeze 的數據**）：
- GPU：4～5 秒音檔 ASR 130～150 ms（語言偵測約 35 ms＋解碼約 100 ms），偵測 `en` p=0.99、`zh` p=1.00，策略運作正常。
- CPU（同一個 base 模型）：約 19～20 秒 —— Windows MSVC 版 whisper.cpp 的 CPU 推論非常慢，large-v2 等級的 Breeze 在 CPU 上不可行，CPU 備援必須是 SenseVoice（與計畫一致）。
- 語言偵測會多跑一次 encoder：Breeze 上這段的耗時就是 `detect` 欄位，若總時間超標，可比較 `--lang zh` 的耗時判斷偵測成本。

### 你要執行的步驟（🧑）

1. **錄音**：依 `tests/fixtures/audio/README.md` 錄 10 段 WAV。
2. **轉檔**：在 repo 根目錄
   ```powershell
   powershell -ExecutionPolicy Bypass -File tools\convert-breeze\convert-breeze.ps1
   ```
   問 `Proceed? [y/N]` 時輸入 `y`。完成後把印出的檔案大小與 SHA-256 填到下方「模型檔」表。
3. **量測**（先設好上面「建置筆記」的環境變數 1～4）：
   ```powershell
   cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model models\ggml-breeze-asr-25-q5_0.bin tests\fixtures\audio
   cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model models\ggml-breeze-asr-25-q8_0.bin tests\fixtures\audio
   # 對照「強制 zh」會不會把純英文翻成中文：
   cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model models\ggml-breeze-asr-25-q5_0.bin --lang zh tests\fixtures\audio
   ```
   每段會印出辨識文字、期望文字、CER、ASR 毫秒（偵測＋解碼）、音檔長度；最後有總表。把數字填進下方「延遲與準確度」表，並檢查：輸出是繁體、夾雜句的英文有保留、`en-01`/`en-02` 沒被翻成中文。
   超標時可試 `--audio-ctx 768`（encoder 只看約 15 秒，較快但可能較不準）。
4. **VRAM**：平常的使用狀態（Chrome、Teams 等照開），另開一個視窗執行
   ```powershell
   nvidia-smi --query-gpu=memory.used,memory.total --format=csv -l 1
   ```
   再用 `--repeat 20` 讓模型常駐一段時間：
   ```powershell
   cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model models\ggml-breeze-asr-25-q5_0.bin --repeat 20 tests\fixtures\audio
   ```
   記錄「執行前」與「執行中最高」的 `memory.used`，相減就是 Breeze 用量。LM Studio 載入 3B／4B（Q4_K_M、context 2048）後再各量一次組合。量測中拖曳視窗、播 YouTube，觀察 2560×1440 畫面是否卡頓。
5. **AMD 內顯**：查筆電 BIOS／廠商工具（MUX 開關、Advanced Optimus），或 NVIDIA 控制面板「管理顯示模式」，看內建螢幕能不能交給 AMD 內顯。

### 模型檔（🧑 轉檔後填）

| 檔案 | 大小 (MB) | SHA-256 |
|------|-----------|---------|
| ggml-breeze-asr-25-q5_0.bin | 1031（1,080,732,108 bytes） | `9be65cda66ee424a261cae9e9864e5aceb2f1be63cc01e5b670e57d022fdf335` |
| ggml-breeze-asr-25-q8_0.bin | 1579（1,656,129,708 bytes） | `669eb226a0e23b42465a6d2f60ce1902fbd534e19faab59511730420eb25e90d` |

（2026-11-09 由 agent 執行 `convert-breeze.ps1 -Yes` 轉檔；whisper.cpp v1.8.3 quantize）

### 延遲與準確度（🧑 填，release＋cuda）

驗收：10 秒音檔 ASR ≤ 1200 ms；輸出繁體；夾雜句英文保留；純英文不被翻成中文。

| 段落 | 音檔 ms | q5_0 ASR ms（偵測＋解碼） | q5_0 CER | q8_0 ASR ms | q8_0 CER | 偵測語言 | 繁體？ | 英文保留／未被翻譯？ |
|------|---------|---------------------------|----------|-------------|----------|----------|--------|----------------------|
| zh-01 | 6975 | 824（288＋536） | 0.0% | 799 | 0.0% | zh 0.99 | ✅ | — |
| zh-02 | 6592 | 908（293＋615） | 5.3% | 905 | 5.3% | zh 0.99 | ✅ | — |
| zh-03 | 6272 | 890（291＋599） | 0.0% | 893 | 0.0% | zh 0.99 | ✅ | — |
| en-01 | 6719 | 835（300＋535） | 2.0% | 776 | 2.0% | **zh 0.95** | — | ✅ 未被翻譯（slides→sliders） |
| en-02 | 4863 | 770（286＋484） | 0.0% | 764 | 0.0% | **zh 0.95** | — | ✅ |
| mix-01 | 4416 | 800（286＋514） | 4.3% | 789 | 4.3% | zh 0.98 | ✅ | ✅ meeting、delay |
| mix-02 | 6976 | 842（289＋553） | 0.0% | 811 | 0.0% | zh 0.99 | ✅ | ✅ code、push、review |
| mix-03 | 5312 | 875（292＋583） | 0.0% | 818 | 0.0% | zh 0.99 | ✅ | ✅ feature、deadline（QA→qa） |
| filler-01 | 7680 | 896（292＋604） | 10.5% | 823 | 21.1% | zh 0.99 | ✅ 後天 | q8 漏掉「嗯」「呃」 |
| vocab-01 | 7168 | 961（301＋660） | 6.5% | 936 | 3.2% | zh 0.99 | ✅ | q5 開頭「請把」→「錢包」（bench 未帶詞庫） |
| **平均／最大** | | **860／961** | **2.9%** | **831／936** | **3.6%** | | 無簡體字 | |

2026-11-09 由 agent 以你的錄音實測（release＋cuda，RTX 2060，CUDA 13.5，sm_75，flash attention 開）。模型載入約 2.3 秒、暖機 781 ms。

`--lang zh`（q5_0，跳過語言偵測）：平均 **577 ms**、最大 669 ms、CER 2.9%（與偵測時相同）；en-01／en-02 仍輸出原本的英文，**沒有被翻成中文**。

觀察與決定：
- **速度通過**：每段實際 ASR 時間都 ≤ 961 ms（目標 1200 ms）。bench 原本把短音檔「換算成每 10 秒」，4.4 秒音檔 800 ms 會被算成 1813 ms 而判 FAIL；Whisper 固定編碼 30 秒視窗、成本幾乎固定，這樣換算不合理。已改為「10 秒以內的句子看實際時間，超過 10 秒才換算」（`bench.rs` `per_10s`）。
- **語言偵測沒有用**：Breeze 把純英文也判成 zh（p≈0.95），而以 zh 解碼英文照樣保留、不翻譯，偵測只多花約 290 ms。已把 `BreezeOptions` 預設改為直接以 zh 解碼（桌面 App 跟著生效；CLI 的 `--lang` 預設仍是 `detect`，`--lang zh` 等於 App 行為）。
- **預設量化：q5_0**。q8_0 速度只快約 30 ms、CER 沒有比較好（3.6% vs 2.9%），卻多 550 MB 顯存與硬碟。
- **Breeze 不輸出標點**（例：「今天下午三點要開會記得把報告準備好」）。開 LM Studio 潤飾時會補標點；潤飾關閉或 LM Studio 沒開時，輸出沒有標點 —— 列入 dogfood 觀察，必要時再加規則式補句號。
- 數字會輸出阿拉伯數字（10 點），CER 算錯字，但實際使用可接受。

### VRAM（🧑 填，`nvidia-smi` memory.used，MiB）

執行前（平常狀態）基準：**1267 MiB**／6144 MiB（Windows 桌面＋其他程式；卸載後回到 1004 MiB）

2026-11-10 由 agent 實測：`nvidia-smi` 每 0.3 秒取樣取最高值；LM Studio 載入 `qwen2.5-coder-3b-instruct`（`--gpu max`、context 8192）；Breeze 用 `bench --lang zh --repeat 10`（100 次辨識）。你只有 3B 模型，4B 兩列沒測。

| 組合 | Breeze 用量 | LM Studio 用量 | 合計（v2c＋LM Studio） | 剩餘 | ASR 平均／最大 | 畫面卡頓？（拖曳視窗／播影片） |
|------|-------------|----------------|------------------------|------|----------------|------------------------------|
| Breeze q5_0 單獨 | 1779 | — | 1779 | 3098 | — | 🧑 |
| Breeze q5_0 ＋ LM Studio 3B | 1645 | 2373 | 4018 | **859** | 599／740 ms | 🧑 |
| Breeze q5_0 ＋ LM Studio 4B | 未測 | | | | | |
| Breeze q8_0 ＋ LM Studio 3B | 2137 | 2373 | 4510 | **367** | 614／997 ms | 🧑 |
| Breeze q8_0 ＋ LM Studio 4B | 未測 | | | | | |

「均衡」方案合計 ≤ 4 GB？ q5_0＋3B：**約 4.0 GB（剛好在邊界）**；q8_0＋3B：否（4.4 GB）。

**重要發現：預設的 `vram_reserve_mb = 2048` 和「Breeze＋LM Studio 3B 同時在顯卡上」互斥。**
- LM Studio 3B 載入後可用約 2504 MiB；`GpuGuard::can_load` 需要 `2048（保留）＋約 1700（Breeze）＋margin`，所以 Breeze 不會載入，App 會改用 CPU 的 SenseVoice。
- 若 Breeze 先載入、LM Studio 後載入，可用量掉到約 859 MiB < 2048，約 10 秒後 Breeze 會被卸載。
- 也就是說，預設設定下「開著 LM Studio 3B」＝「辨識改用 SenseVoice」。要兩者同時在 GPU 上，保留額度需降到約 700 MiB 以下（剩 859 MiB 時畫面是否卡頓要你實際確認），或讓 LM Studio 用 CPU／較少 GPU 層。待你決定。

### 筆電能否把畫面輸出切到 AMD 內顯（🧑 填）

- 機型：＿＿＿＿
- MUX 開關／Advanced Optimus：有／沒有
- 結果：＿＿＿＿

## T1.10 SenseVoice-Small 真人錄音（2026-11-09，CPU，release＋sensevoice）

指令：`cargo run -p v2c-cli --release --features sensevoice -- bench --asr sensevoice --model "%LOCALAPPDATA%\v2c\models\sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17" tests/fixtures/audio`（WAV 由你的 m4a 以 ffmpeg 轉成 16 kHz mono s16）

| 段落 | 音檔 ms | ASR ms | CER | 偵測 | 主要差異 |
|------|---------|--------|-----|------|----------|
| zh-01 | 6975 | 236 | 0.0% | Zh | — |
| zh-02 | 6592 | 235 | 5.3% | Zh | 漏「再」 |
| zh-03 | 6272 | 207 | 0.0% | Zh | — |
| en-01 | 6719 | 235 | 3.9% | En | — |
| en-02 | 4863 | 177 | 0.0% | En | — |
| mix-01 | 4416 | 170 | 17.4% | Mixed | `meetinging`（模型重複字尾）、「下個禮拜」 |
| mix-02 | 6976 | 243 | 3.7% | Mixed | `coded` |
| mix-03 | 5312 | 168 | 17.2% | Mixed | `featureature`（模型重複字尾）、`qa` 小寫 |
| filler-01 | 7680 | 280 | 15.8% | Zh | 「后天」沒轉成「後天」（已修，見下）、「10點」 |
| vocab-01 | 7168 | 242 | 22.6% | Mixed | `lMt`、`v to c`（SenseVoice 不吃詞庫 prompt） |
| **平均／最大** | | **219／280** | **8.6%** | | |

- 速度：全部 PASS（約 0.35 秒／10 秒音檔，RTF 0.035），沒有簡體字警告。
- 「后」修正：`zh_tw` 原本把「后」視為簡繁共用字而保留（皇后），SenseVoice 輸出繁體句中夾一個「后」就不會被轉。已改為「后」除了皇后、王后、太后、天后、影后、后妃等用法外一律轉「後」（`postprocess::zh_tw::resolve_shared`，有測試）。
- 英文字尾重複（`meetinging`、`featureature`）是 SenseVoice 模型本身的輸出，`clean_output` 沒有改動文字；不做規則式刪除（會誤傷 singing、ringing）。開潤飾（LM Studio）時通常會被修正；主力 Breeze 量測後再比較。

### 結論（量測後填）

- 預設量化：**q5_0**（2026-11-09 實測）
- 是否需要備案（Whisper large-v3-turbo）：**否**（速度與準確度都達標；VRAM 待你實測）

## T4.10 辨識加速與啟動提前（2026-11-13，agent 實測）

**量測條件（重要）**：你的 v2c.exe 正在執行並占用 GPU，所以 bench 的 GPU 是**與執行中的 App 共用**，數字偏慢且波動大（第一組前後差 2 倍以上）。要驗收請關掉 v2c 後重跑。App 內「啟動到 speech recognition ready」**沒有量**（不能關你的 App，dev 版也會跟它搶單一實例與熱鍵）。

指令（q8_0、`--lang zh` 同 App、每組 3 輪共 30 次；改前加 `--audio-ctx 1500` = 完整 30 秒視窗，改後不加 = 依錄音長度）：
`v2c-cli bench --asr breeze --model models\ggml-breeze-asr-25-q8_0.bin --lang zh [--audio-ctx 1500] --repeat 3 tests\fixtures\audio`

| 組別 | 改前 ASR 平均／最大 | 改後 ASR 平均／最大 | 變化 | 改前／改後 平均 CER |
|------|---------------------|---------------------|------|---------------------|
| 第 1 組（GPU 被占用，波動大） | 3785／6745 ms | 1819／5841 ms | −52% | 3.6%／3.3% |
| 第 2 組（較穩定） | 2013／2499 ms | 1459／1881 ms | −27.5% | 3.6%／3.3% |

- 第 2 組逐輪平均：改前 2047／2013／1978，改後 1482／1464／1432 ms（中位數 1962 → 1422，−27.5%）。
- 錯字率沒有變差（3.6% → 3.3%，同上次 q8 ctx 768 的結果）；10 段音檔都在 14 秒內，實際使用 ctx 768。
- 驗收要求 ≥ −30%：第 2 組 −27.5% 略低，第 1 組與先前調查（−37%）高於；GPU 共用時分母被其他程式的負載墊高，**需關掉 App 後重量**才能下定論。
- 暖機（bench 內，含載入後第一次推論）：改前 1.77～1.83 秒，改後 1.21～1.27 秒（ctx 768）；App 的暖機另外限制 8 個 token。
- 模型載入 9～36 秒（讀檔 I/O，受磁碟快取與其他程式影響很大），與改動無關。

### T4.10 (GPU free, 2026-11-13)

Measured with v2c closed (RTX 2060, q8_0, `--lang zh`, 3 interleaved rounds, `--repeat 3` = 30 clips per run). Before = `--audio-ctx 1500`, after = no flag.

| Round | Before ASR mean/max (ms) | After ASR mean/max (ms) | Before/After CER | Before/After warm-up (ms) |
|-------|--------------------------|-------------------------|------------------|---------------------------|
| 1 | 1968 / 2511 | 1312 / 1590 | 3.6% / 3.3% | 1497 / 840 |
| 2 | 1939 / 2254 | 1311 / 1596 | 3.6% / 3.3% | 1435 / 854 |
| 3 | 1927 / 2228 | 1297 / 1584 | 3.6% / 3.3% | 1415 / 835 |
| Median | 1939 | 1311 | 3.6% / 3.3% | 1435 / 840 |

- ASR mean change: -32.4% (threshold -30%): PASS. CER not worse: PASS.

App startup ("v2c starting" to "speech recognition ready", seconds; installed 0.2.0 vs T4.10 release build with the same DLLs, alternating, one instance at a time):

| Launch | 0.2.0 baseline | T4.10 | overlay ready (base/new) |
|--------|----------------|-------|--------------------------|
| 1 | 5.22 | 3.77 | 1.20 / 0.83 |
| 2 | 4.60 | 3.62 | 0.67 / 0.69 |
| 3 | 4.88 | 3.64 | 0.69 / 0.69 |
| Median | 4.88 | 3.64 | |

- Startup gain: 1.24 s (threshold 1.5 s): FAIL (short by 0.26 s). Model files were warm in the page cache for all launches.
