# Android 手機本機語音模型（T2.6）

手機離線辨識用 sherpa-onnx 的 Kotlin API（不經 Rust）。模型不包進 APK，第一次切到本機模型時由鍵盤詢問是否下載。本文記錄【實作前確認】的結果、建置方式、下載流程與比較工具用法。確認日期：2026-11-12。

## 1. 【實作前確認】結果

資料來源：k2-fsa/sherpa-onnx 的 GitHub Releases（API 回傳的 asset 大小與 sha256）、原始碼（tag `v2.0.1`）、Hugging Face 模型頁（檔案清單與 LFS sha256）。

### 1.1 sherpa-onnx Android 發佈方式

| 項目 | 結果 |
|------|------|
| 最新版本 | `v2.0.1`（2026-10-26）。GitHub 的「latest」標籤目前指到 `fa-models`，不是版本發佈，要用 tag `v2.0.1`。 |
| Maven／AAR | **沒有**。`repo1.maven.org/maven2/com/github/k2-fsa/sherpa-onnx/` 回 404；官方文件只提供預編 APK 與自行編譯。 |
| 預編庫 | Release asset `sherpa-onnx-v2.0.1-android.tar.bz2`（45,300,379 位元組，sha256 `39003753ec293425ed56723ea9b972b98841a490af3311670c3060548d654330`，與 GitHub 公布值及本機下載計算值一致）。內含 `jniLibs/{arm64-v8a,armeabi-v7a,x86,x86_64}/`，每個 ABI 有 `libonnxruntime.so`（arm64 約 22 MB）、`libsherpa-onnx-jni.so`（約 4.6 MB），另有 C／C++ API 用的 `.so`（JNI 用不到，README 也說明）。 |
| Kotlin API | 不在 AAR 內，是原始碼：`sherpa-onnx/kotlin-api/*.kt`（package `com.k2fsa.sherpa.onnx`；JNI 函式名稱綁定這個 package，所以不能改名）。離線辨識要用 `OfflineRecognizer.kt`（含所有 `Offline*ModelConfig` 與 `OfflineRecognizerConfig`）、`OfflineStream.kt`、`FeatureConfig.kt`、`QnnConfig.kt`、`HomophoneReplacerConfig.kt`。 |
| API 用法 | `OfflineRecognizer(config = OfflineRecognizerConfig(...))` → `createStream()` → `stream.acceptWaveform(FloatArray, sampleRate)` → `decode(stream)` → `getResult(stream).text` → `stream.release()`。`acceptWaveform` 會自行重取樣。 |
| 16 KB 頁面對齊 | **是**。直接解析 `arm64-v8a`、`x86_64` 各 `.so` 的 ELF `PT_LOAD` 對齊，全部是 16384。實際建出的 debug APK 用 `zipalign -c -P 16 -v 4` 驗證，`Verification successful`（含 `libonnxruntime.so`、`libsherpa-onnx-jni.so`）。 |
| 授權 | sherpa-onnx：Apache-2.0（上游專案授權，未在本次逐字確認 LICENSE 檔）。 |

### 1.2 候選模型

下載網址是 `https://huggingface.co/csukuangfj/<repo>/resolve/<commit>/<檔名>`，**釘在 commit 上**（不用會移動的 `main`：上游一更新，所有人的檔案雜湊就會不符，每次重試都白下載約 240 MB）。commit：SenseVoice int8 2025-09-09 → `355f4d4884d8afd08aef04b9007a8556d7b463b2`；paraformer-zh-2023-09-14 → `def027084691107096b5ebba69785756d63de6c5`；whisper-small → `8f3c18b358db4d1f2fc1eae49d75cd20989e4309`。雜湊與大小來自 Hugging Face 檔案清單的 LFS sha256；`tokens` 檔沒有 LFS 雜湊，雜湊是實際下載後計算的。

**1. SenseVoice-Small int8（預設）**，repo `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09`

| 檔案 | 位元組 | sha256 |
|------|--------|--------|
| `model.int8.onnx` | 237,115,547 | `12ca1a2ae7ecf3e0019ef2822307ee0b5cadc9196569e379b4c4026f8205276d` |
| `tokens.txt` | 315,894 | `f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc` |

- 合計約 237 MB（卡片寫的「約 230 MB」成立）。
- 語言：中、英、日、韓、粵。授權：上游 FunAudioLLM/SenseVoiceSmall 標示 `license: other`，連到 FunASR 的 MODEL_LICENSE（`github.com/modelscope/FunASR/blob/main/MODEL_LICENSE`）；sherpa 轉換版的 Hugging Face 頁沒有另外標示授權。**散布前請人工讀過 MODEL_LICENSE**（本專案是使用者自己下載，不隨 APK 散布）。
- **與桌面實測版本不同**：桌面（`docs/linux-setup.md` 的 7.9%／5.4% 數字）用的是 `2024-07-17` int8。這個版本在 Hugging Face 沒有 int8 repo（API 回 `Invalid username or password`），只有 GitHub 的 `asr-models` release 壓縮檔（`sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2`，163,002,883 位元組，sha256 `7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e`，GitHub 公布值）。該格式是 `.tar.bz2`，在手機上需要解壓縮函式庫，所以預設改用有單檔下載的 `2025-09-09` 版。兩版的辨識品質差異未驗證，比較工具量到的數字才算數。
- GitHub release 另有 `2025-09-09` 的 `.tar.bz2`（165,783,878 位元組，sha256 `7305f7905bfcf77fa0b39388a313f3da35c68d971661a65475b56fb2162c8e63`），沒有採用。

**2. Paraformer 中英雙語**，repo `sherpa-onnx-paraformer-zh-2023-09-14`

| 檔案 | 位元組 | sha256 |
|------|--------|--------|
| `model.int8.onnx` | 243,371,218 | `f36a0433bcf096bd6d6f11b80a3ac8bed110bdca632fe0d731df8d1a84475945` |
| `tokens.txt` | 75,756 | `59aba8873a2ed1e122c25fee421e25f283b63290efbde85c1f01a853d83cb6e6` |

- 合計約 243 MB。Hugging Face 頁標示 `apache-2.0`。
- 「中英雙語」未逐字確認：sherpa 文件對同系列 `zh-2023-03-28` 寫明支援中英；`2023-09-14` 的文件段落因頁面被截斷沒讀到，只看到 repo 內有 `6-zh-en.wav` 測試檔。未知，待比較工具實測。
- 另有較新的 `sherpa-onnx-paraformer-zh-int8-2025-10-07.tar.bz2`（GitHub，228,262,632 位元組，sha256 `a071ee5419e14adb34d7f970ab98105a45e6608018b168f023ca2e4810744abe`），是 `.tar.bz2`，未採用；也沒有確認它是否支援英文。

**3. Whisper small int8**，repo `sherpa-onnx-whisper-small`（多語言版）

| 檔案 | 位元組 | sha256 |
|------|--------|--------|
| `small-encoder.int8.onnx` | 112,442,483 | `4cbe7b22fa9026b843b60a68640c747de05bafb1a11b57edc0e66c232d9f33a9` |
| `small-decoder.int8.onnx` | 262,226,114 | `acad50b5c782696e91b55914cc5ab4f756f1532f76e22aa6fc615f39fb69a8ee` |
| `small-tokens.txt` | 816,730 | `b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126` |

- 合計約 375 MB。Hugging Face 頁沒有標示授權；上游 openai/whisper 為 MIT（未在本次確認）。
- **Whisper turbo int8**（repo `sherpa-onnx-whisper-turbo`）：`turbo-encoder.int8.onnx` 674,716,297 位元組（sha256 `b02dcdf54f348741e93fe732b67d933c8dcb6735655f710640143081db38878b`）、`turbo-decoder.int8.onnx` 361,080,764 位元組（sha256 `20accd02388482eb3a46bd615631adfdc85e1eb2c7db9ea3f02a40ffe6b81547`）、`turbo-tokens.txt` 816,730 位元組（雜湊未計算）。合計約 1.04 GB，對手機太大，沒有放進比較清單。

## 2. 建置方式（沒有大型檔案進 git）

`apps/android/app/build.gradle.kts` 的 `fetchSherpaOnnx` 工作：

- 從 `v2.0.1` tag 下載 5 個 Kotlin 原始檔（每個檔案的 sha256 寫死在 `KOTLIN_FILES`）與 `sherpa-onnx-v2.0.1-android.tar.bz2`（sha256 寫死在 `ANDROID_ARCHIVE_SHA256`），不符就讓建置失敗。
- 只取 `arm64-v8a`、`x86_64` 的 `libonnxruntime.so` 與 `libsherpa-onnx-jni.so`。
- 輸出到 `apps/android/app/build/sherpa-onnx/`（git 已忽略），以產生的 Kotlin／jniLibs 來源目錄接進每個 variant。
- 第一次建置需要網路；下載的檔案快取在 `apps/android/.gradle/sherpa-onnx-cache/`（git 已忽略，`clean` 不會清掉）。不需要安裝任何系統軟體。
- 升級版本：改 `tag`，並更新六個雜湊（算法與本文相同：下載後 `sha256sum`，再與 GitHub release asset 的 `digest` 比對）。
- ABI：debug 打包 `arm64-v8a` 與 `x86_64`（模擬器用）；release 只打包 `arm64-v8a`（`buildTypes.release.ndk.abiFilters`）。debug APK 約 87 MB。
- 開啟 minify 時要保留 `com.k2fsa.sherpa.onnx.**`（JNI 以名稱呼叫）。目前 `isMinifyEnabled = false`。

## 3. 程式結構（`app.v2c.ime.asr.local`）

| 檔案 | 內容 |
|------|------|
| `LocalModelSpec.kt` | 模型規格（檔名、網址、大小、SHA-256）與候選清單 `LocalModels` |
| `DownloadState.kt` | 下載狀態：未下載、下載中（進度）、檢查中、可用、失敗（中斷／雜湊不符／回應異常／儲存空間） |
| `LocalModelDownloader.kt` | 下載（開始、進度、取消、刪除）；寫到 App 私有目錄 `files/models/<id>/`，先存成 `.part`，雜湊相符才改名；中斷後從 `.part` 的長度用 HTTP Range 續傳，伺服器不支援就重頭；純 Kotlin，網路用 `ModelFetcher` 介面隔開 |
| `LocalAsr.kt` | `LocalAsr`（`isReady()`、`transcribe(pcm, rate)`）、`SherpaLocalAsr`（延遲建立 recognizer）、`ZhTwConverter` 介面 |
| `LocalModelPrompt.kt` | 鍵盤用的下載詢問／進度／錯誤 Compose 元件，與可單元測試的 `LocalModelPromptPolicy`（詢問只出現一次） |
| `Cer.kt` | 字錯率與 WAV 讀取（比較工具用，放在 main 以便單元測試） |

- **zh_tw 後處理**：`ZhTwConverter` 介面，預設 `Identity`。T2.3 把 `v2c-ffi` 的 `to_zh_tw` 接進 Gradle 後，由 T2.4 在建立 `SherpaLocalAsr` 時傳入。SenseVoice 輸出簡體，所以**接上之前**比較工具量到的中文 CER 會偏高（簡繁字形算成錯字）。
- `LocalModelDownloader` 的 `start`／`cancel`／`delete` 都立刻返回、可在主執行緒呼叫，實際工作在下載執行緒。`cancel()` 之後立刻 `start()`（舊的一輪還在收尾）會被記住，舊的一輪結束後馬上續傳；`start()` 後立刻 `cancel()` 不會連線；收尾時才到的 `delete()` 也不會漏掉。下載前先檢查可用空間（剩餘位元組＋64 MB），不足回報「儲存空間」；寫檔錯誤是「儲存空間」、讀取／連線錯誤是「中斷」；HTTP 206 的 `Content-Range` 起點必須等於要求的偏移。
- Whisper 一次只能處理 30 秒以內的音訊；T2.4 接進鍵盤時，超過 30 秒的錄音必須先切段再逐段辨識。
- 日誌只記模型 id、取樣數、字數、毫秒，不記辨識文字。
- 鍵盤要用的東西：`LocalModelDownloader.state`／`addListener` 提供進度；`LocalModelPrompt(...)` 放進鍵盤版面（T2.4）。「已問過」旗標由 T2.4 存在 App 私有偏好檔。
- 需要 `android.permission.INTERNET`：`AndroidManifest.xml` 不在本卡允許修改的範圍，主 manifest **還沒有**這個權限（T2.4 的卡片有寫），所以 release 版目前下載不會成功；debug 版在 `src/debug/AndroidManifest.xml` 加了，供比較工具使用。

## 4. 比較工具（只有 debug build）

`app.v2c.ime.asr.local.debug.CompareActivity`，輸出只有數字（字錯率、延遲、即時倍率），不記錄、不顯示、不寫入辨識文字。

1. 準備錄音：`tests/fixtures/audio` 裡每段 `名稱.wav`（16-bit PCM）與 `名稱.txt`（期望文字）。
   （若 `名稱.raw.txt` 存在，工具以它為期望文字；有些 `.txt` 是潤飾後的文字，不是實際說的話。）
2. 安裝 debug APK，放進手機：
   ```sh
   adb push tests/fixtures/audio/. /sdcard/Android/data/app.v2c.ime/files/compare/
   ```
   （目錄不存在時先 `adb shell mkdir -p /sdcard/Android/data/app.v2c.ime/files/compare`，或先開一次工具讓系統建立。）
3. 啟動：`adb shell am start -n app.v2c.ime/.asr.local.debug.CompareActivity`。每個模型有「Download」「Run」「Delete」三顆按鈕；下載需要網路，之後可開飛航模式。
4. 結果顯示在畫面上，也寫到 `files/compare/result-<模型 id>.csv`（欄位：檔名、秒數、延遲 ms、RTF、CER、參考字數、錯字數，最後一行是彙總）；用 `adb pull` 取回。
5. 延遲不含模型載入（暖機另外記為 `load_ms`）。

### 比較表（待填，需要人工在手機上跑）

注意：SenseVoice 輸出簡體字。**必須等 `to_zh_tw`（T2.3 FFI）接進 `SherpaLocalAsr` 之後，這張表的 CER 才能當作決定預設模型的依據**；在那之前簡繁字形會被算成錯字，只能參考延遲。

| 模型 | 手機型號 | CER（接 zh_tw 後） | 延遲中位數 ms | 最大延遲 ms | RTF | 載入 ms | 下載大小 |
|------|----------|--------------------|---------------|-------------|-----|---------|----------|
| SenseVoice-Small int8 | | | | | | | 237 MB |
| Paraformer zh int8 | | | | | | | 243 MB |
| Whisper small int8 | | | | | | | 375 MB |

桌面 CPU 參考（`docs/linux-setup.md`）：SenseVoice 約 0.4–1.0 秒、CER 7.9%／5.4%（`2024-07-17` 版，十段真人錄音）。

## 5. 尚未完成

- 🧑 在你的手機跑比較工具，填上面的表並回報手機型號；依表決定預設模型。
- 🧑 閱讀 SenseVoice 的 FunASR MODEL_LICENSE，確認個人使用沒問題。
- 還沒在實機測過：本卡在沒有手機的環境完成，只驗證了建置、APK 的 16 KB 對齊與單元測試；JNI 載入與實際辨識、「飛航模式下按住麥克風能輸入中、英、夾雜句子」要到實機確認，且需要 T2.4 把本機模型接進鍵盤與 `AudioRecord`。
