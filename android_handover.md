# Android 交接：Phase 2 進度與後續（2026-11-12）

> 在 Windows 上做到一半，使用者要求停下，改到 Linux 主機（Ubuntu 22.04）`git pull` 後繼續。
> 需求與決策：`v2cplan.md` 第 3 節（Android 各列）與第 5 節（模型分工）。設計：`tasks/plan.md` 第 8 節。分派流程：`tasks/cards/README.md`〈分派流程〉。

## 1. 現況總表

| 卡 | 內容 | 狀態 | 在哪裡 |
|----|------|------|--------|
| T2.0 | Phase 2 文件改寫（只有語音輸入＋中翻英、三種辨識來源融合、Typeless 式操作、Android 16／17） | ✅ 已合併 | `main` |
| T2.1 | 語音鍵盤最小路徑：Typeless 式大麥克風＋系統辨識、`SetupActivity` | ✅ 已合併（審查 3 輪，26 項單元測試） | `main` |
| T2.3 | `v2c-ffi`：uniffi 0.32、`choose_asr`、`load_lan_services`、`transcribe_pcm(ByteArray)`、`translate`、`to_zh_tw` | ✅ Rust 端已合併（21 項測試）；**還沒接進 Gradle**（缺 cargo-ndk） | `main`；接線步驟在 `crates/v2c-ffi/ANDROID.md` |
| T2.2 | 退格（送 `KEYCODE_DEL`、長按加速、換輸入框就停）與換行（依 `imeOptions`，放開才觸發） | 🟡 審查通過、測試通過（37 項）；**等使用者核准合併** | 分支 `t2.2-android-keys`（`e10b995`） |
| T2.6 | 手機本機模型：sherpa-onnx v2.0.1、SenseVoice-Small int8（預設候選）、下載器、比較工具 | 🟡 第 2 輪審查**通過**（`85a57f5`）；之後在修 5 個小建議，修到一半停下 | 分支 `t2.6-android-local-model`，最新 `49abeb5`（**WIP，未建置、未測試**） |
| T2.4 | 區網偵測、辨識路由接線、來源小圖示（長按切換）、中翻英鍵 | ⏳ 未開始；要等 T2.3 接線（需 cargo-ndk）與 T2.6 合併 | — |
| T2.5 | release APK、簽署、`tools/build-android.ps1`、`docs/android-install.md` | ⏳ 未開始 | — |

本機 `main` 與兩個分支都已 push 到 `origin`。

## 2. 下一步（依序）

1. **在 Linux 準備環境**（第 3 節）。
2. **T2.6 收尾**：
   - `git checkout t2.6-android-local-model`。
   - 最新 commit `49abeb5` 是 WIP，內容是第 2 輪審查的 5 個建議 A–E：
     - A：下載收尾時才到的 `delete()` 不會漏掉。
     - B：sherpa-onnx 快取改用 `Files.move(REPLACE_EXISTING)`，避免 Windows 上快取損壞後建置一直失敗。
     - C：`start()` 後立刻 `cancel()` 不會先連線。
     - D：`response.close()` 拋的 IOException 不算「儲存空間」錯誤，另補寫入錯誤的測試。
     - E：`cancel()` 後立刻 `start()` 會被記住，舊的一輪結束後接著續傳。
   - 先跑 `cd apps/android && ./gradlew assembleDebug assembleRelease lint testDebugUnitTest --rerun-tasks`。失敗就修。
   - 通過後依流程交給 Haiku 測試、Opus 審查（只審 `85a57f5..HEAD`）。
3. **合併 T2.2 與 T2.6**：使用者已同意「T2.6 修完 5 個小建議後，T2.2、T2.6 一起合併進 main」。
   - 合併後在 `tasks/human-checklist.md` Phase 2 補上 T2.2 實機項目：在 LINE、Chrome、Gmail 測退格長按、選取刪除、emoji、換行與送出，LINE 的「Enter 送出」開、關兩種都要測。
   - 更新 `tasks/todo.md`。
   - 是否 push 先問使用者。
4. **T2.3 接線**（要先有 cargo-ndk）：照 `crates/v2c-ffi/ANDROID.md` 做下列幾件事：
   - 把 cargo-ndk 任務、Kotlin 綁定產生（`app/build/generated/uniffi`）、TN／RD preset 複製到 assets 接進 `apps/android/app/build.gradle.kts`。
   - 加入 JNA `@aar`，版本要查證。
   - release 加 R8 keep 規則。
   - 驗證：`.so` 在 APK 內並 16 KB 對齊，Kotlin 呼叫 `toZhTw` 成功。
   - 屬於 T2.3 卡片的「受阻」項目，做完更新 todo。
5. **W10 T2.4**：卡片 `tasks/cards/T2.4-android-translate.md`。要接的東西：
   - `choose_asr`。
   - T2.6 的 `LocalAsr`、`LocalModelPrompt`：把 `ZhTwConverter` 換成 FFI 的 `toZhTw`。
   - `AudioRecord` 錄音；Whisper 一次最多 30 秒，超過要切段。
   - 主 manifest 的 `INTERNET` 與 `network_security_config`。T2.6 只在 debug manifest 加了 `INTERNET`，release 目前不能下載模型。
6. **W10b T2.5**：release APK。Linux 上的建置腳本請用 `tools/build-android.sh`，或兩個平台各一份；卡片寫的是 `.ps1`，要改卡片時先跟使用者確認。
7. 全部完成後：**檢查點 2**（手機日常使用一週）。

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
