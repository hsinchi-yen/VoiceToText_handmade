# Android 交接：Phase 2 進度與後續（2026-11-12）

> 在 Windows 上做到一半，使用者要求停下，改到 Linux 主機（Ubuntu 22.04）`git pull` 後繼續。
> 需求與決策：`v2cplan.md` 第 3 節（Android 各列）與第 5 節（模型分工）。設計：`tasks/plan.md` 第 8 節。分派流程：`tasks/cards/README.md`〈分派流程〉。

## 1. 現況總表（2026-11-12 晚上在 Linux 更新）

| 卡 | 內容 | 狀態 |
|----|------|------|
| T2.0 | Phase 2 文件改寫 | ✅ 已合併 |
| T2.1 | 語音鍵盤最小路徑（Typeless 式大麥克風＋系統辨識） | ✅ 已合併；🧑 手機實測 |
| T2.2 | 退格（長按加速）與換行 | ✅ 已合併；🧑 LINE／Chrome／Gmail 實測 |
| T2.3 | `v2c-ffi`（Rust）＋ Gradle 接線（`cargoNdk`、`generateUniffiKotlin`、`copyPresets`、JNA 5.19.1 `@aar`、`proguard-rules.pro`） | ✅ 已合併；接線**未經建置驗證** |
| T2.6 | 本機模型（sherpa-onnx、SenseVoice、下載器、比較工具） | ✅ 已合併（第 3 輪審查通過，含 A–E 與審查後的 2 項修正）；🧑 手機比較、定案預設模型 |
| T2.4 | 區網偵測、辨識路由、來源小圖示（長按切換）、中翻英鍵、`network_security_config` | ✅ 已合併（審查 2 輪通過）；**未編譯**；🧑 網路情境實測 |
| T2.5 | `tools/build-android.sh`＋`tools/build-android.ps1`、release 簽署、`docs/android-install.md` | ✅ 已合併（審查 2 輪通過）；腳本只跑過 `bash -n` 與 `--check`；🧑 產生簽署金鑰、建 APK、側載 |

這台 Linux 沒有 JDK 與 Android SDK，所以 T2.4、T2.5、T2.3 接線，以及 T2.6 在 Linux 上的最後修正（`d0e4d0b`、`4d13518`）**都沒有編譯、也沒有跑過單元測試**，只經過獨立審查（逐行閱讀）。曾下載可攜式 JDK 17＋kotlinc 到 `tests/tmp/jvm`（SHA-256 已驗證），但 agent 的權限設定不允許執行外部下載的程式，所以沒有用上；要用的話由你決定是否放行，或直接在 Windows 那台跑 Gradle。

## 2. 下一步（依序）

1. 在有 Android 工具的機器（Windows 那台已有 SDK／NDK／JDK 17）：
   ```
   git pull
   rustup target add aarch64-linux-android x86_64-linux-android
   cargo install cargo-ndk
   cd apps/android && ./gradlew assembleDebug lint testDebugUnitTest --rerun-tasks
   ```
   第一次建置是接線的第一次實跑，失敗就修 `apps/android/app/build.gradle.kts`。
2. 產生簽署金鑰後建 release：Windows `powershell -ExecutionPolicy Bypass -File tools\build-android.ps1`，Linux `bash tools/build-android.sh`（先用 `-Check`／`--check` 看缺什麼）。APK 會在 `releases/v2c_<版本>_android.apk`，SHA-256 寫進 `releases/SHA256SUMS.txt`。詳見 `docs/android-install.md`。
3. 側載實測（`tasks/human-checklist.md` Phase 2）。
4. **檢查點 2**（手機日常使用一週）。

### 審查時決定、實作者沒寫進文件的行為

- 區網偵測：網路真的換了（不同 `Network`，或目前的網路斷了）才清快取；鍵盤每次開啟只重測、不先清快取。
- 探測 `GET <base>/models`，任何 < 500 的 HTTP 狀態都算「伺服器在」。
- 多段（>30 秒）辨識結果直接相接，不加空白。
- `network_security_config` 只管 Java 的 HTTP；Rust 的 `transcribe_pcm`／`translate` 用自己的連線，不受它限制。
- 原生函式庫載入失敗（`LinkageError`）時鍵盤不會當掉，改成區網／本機不可用。

## 3. Linux 環境準備（agent 不安裝系統軟體，以下由使用者執行）

Windows 這邊用的版本，Linux 請對齊：

| 項目 | 版本 |
|------|------|
| JDK | 17（`JAVA_HOME`） |
| Android SDK | `platforms;android-37`（compileSdk 37）、`platforms;android-36`、`build-tools;36.0.0`、`platform-tools` |
| NDK | `ndk;28.2.13676358`（預設 16 KB 對齊） |
| Gradle | 用 wrapper（9.6.0，已附 SHA-256），不用另外安裝 |
| AGP／Kotlin／Compose | 寫在 `apps/android/gradle/libs.versions.toml`（AGP 9.5.1、Kotlin 2.4.21、Compose BOM 2026.09.00） |
| Rust | `rustup target add aarch64-linux-android x86_64-linux-android`；`cargo install cargo-ndk` |

```bash
# 例：用 Android cmdline-tools 安裝（路徑依你的 SDK 位置）
sdkmanager "platforms;android-37" "platforms;android-36" "build-tools;36.0.0" "platform-tools" "ndk;28.2.13676358"
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/28.2.13676358
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk
```

- 第一次建置 `apps/android` 需要網路：`fetchSherpaOnnx` 會下載 sherpa-onnx v2.0.1 的 Android 函式庫，並驗證寫死的 SHA-256。快取在 `apps/android/.gradle/sherpa-onnx-cache`（git 已忽略，`clean` 不會刪）。
- `whisper-rs-sys` 需要 `cmake`；`cargo test --workspace` 要先有 cmake。也可以只測 `cargo test -p v2c-core -p v2c-ffi -p v2c-platform`。
- `AGENTS.md` 規則 5 寫的 `CARGO_TARGET_DIR=C:\Users\lance.tn\.v2ct` 是 Windows 路徑；Linux 請照該機器原本的慣例。

## 4. 在 Windows 上學到、Linux 也適用的事

- **分派流程**：主 agent（Opus）分派；實作用 Sonnet；測試用 Haiku，只跑卡片驗證指令、不改檔；審查用 Opus，只看 diff 和卡片。退回 3 次就停下問使用者。含 🧑 的卡與 FFI／`v2c-core` 介面變更，合併前要使用者核准。
- **worktree**：Agent 工具的 `isolation: "worktree"` 在 Windows 上會失敗，改成手動 `git worktree add .claude/worktrees/<branch> -b <branch> main`（已被 git 忽略）。Linux 可以先試內建隔離。
- **Gradle 快取**：Haiku 測試要加 `--rerun-tasks`，否則測試結果可能是從快取還原，不是真的有跑。
- **T2.1 的坑**：手機的裝置端辨識器不一定有 zh-TW。遇到語言錯誤時，改用一般系統辨識並請使用者重說，不要顯示「安裝 Google 語音服務」。`releaseRecognizer()` 必須在 `createRecognizer()` 之前呼叫，否則判斷不到目前用的是哪一種辨識器。

## 5. 留給使用者（🧑，全部也在 `tasks/human-checklist.md` Phase 2）

1. 在 Linux 安裝上表的 Android／Rust 工具。
2. 側載 debug APK 到 Android 16／17 手機實測 T2.1，回報機型與版本。若裝置端辨識沒有中文，第一句應提示「請再說一次」，第二句起正常。
3. T2.2 實機測試（見第 2 節第 3 步）。
4. T2.6：
   - 讀 SenseVoice 的 FunASR MODEL_LICENSE。
   - 等 T2.4 接上 `to_zh_tw` 後，用 `tests/fixtures/audio` 在手機跑比較工具，再決定預設模型。
5. T2.4 網路情境測試；T2.5 簽署金鑰。

## 6. 文件差異提醒

- T2.3 卡片的 `transcribe_pcm` 簽名已改為 `transcribe_pcm(pcm_le: ByteArray, rate, service_id)`（審查建議，避免 Kotlin 端逐一裝箱）。
- 本機模型預設候選是 SenseVoice `2025-09-09` int8（Hugging Face 單檔）。Linux 桌面實測用的是 `2024-07-17` 版，兩版品質差異還沒驗證。
- 翻譯逾時寫死 8000 ms（`TRANSLATE_TIMEOUT_MS`），刻意比桌面的 `llm_cloud_ms = 3000` 長，因為 Android 沒有設定頁。
