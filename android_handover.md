# Android 交接：Linux → Windows（2026-11-13）

> Phase 2 的程式全部合併進 `main`，並在 Linux（Ubuntu 22.04）上**建置成功、跑過單元測試與 Android 16 模擬器冒煙測試**。接下來到 Windows 主機 `git pull`，用真的簽署金鑰建 release APK，再側載手機實測。
> 需求與決策：`v2cplan.md` 第 3 節（Android 各列）與第 5 節。設計：`tasks/plan.md` 第 8 節。分派流程：`tasks/cards/README.md`〈分派流程〉。

## 1. 現況總表

| 卡 | 內容 | 狀態 |
|----|------|------|
| T2.0 | Phase 2 文件改寫 | ✅ 已合併 |
| T2.1 | 語音鍵盤最小路徑（Typeless 式大麥克風＋系統辨識） | ✅ 已合併；🧑 手機實測 |
| T2.2 | 退格（長按加速）與換行 | ✅ 已合併；模擬器上退格正常；🧑 LINE／Chrome／Gmail 實測 |
| T2.3 | `v2c-ffi`（Rust）＋ Gradle 接線（`cargoNdk`、`generateUniffiKotlin`、`copyPresets`、JNA 5.19.1 `@aar`、`proguard-rules.pro`） | ✅ 已合併；Linux 建置通過，模擬器上 FFI 呼叫正常 |
| T2.6 | 本機模型（sherpa-onnx、SenseVoice、下載器、比較工具） | ✅ 已合併；模擬器上下載＋SHA-256 驗證＋辨識正常；🧑 手機比較、定案預設模型 |
| T2.4 | 區網偵測、辨識路由、來源小圖示、中翻英鍵、`network_security_config` | ✅ 已合併；模擬器上區網 TEA／Breeze／中翻英都通；🧑 手機網路情境實測 |
| T2.5 | `tools/build-android.sh`＋`tools/build-android.ps1`、release 簽署、`docs/android-install.md` | ✅ 已合併；`.sh` 在 Linux 用拋棄式金鑰完整跑通（APK 已刪）；`.ps1` 還沒在 Windows 跑過；🧑 產生正式簽署金鑰、建 APK、側載 |

## 2. Linux 上驗證了什麼（2026-11-13）

工具全部裝在 `tests/tmp/`（git 已忽略、不用 sudo）：可攜式 JDK 17.0.21、Android cmdline-tools、`platforms;android-37.0`（這版 sdkmanager 的套件名，不是 `android-37`）、`android-36`、`build-tools;36.0.0`、NDK 28.2.13676358、emulator、`system-images;android-36;google_apis;x86_64`；cargo-ndk 4.1.2 與兩個 Android rustup target 裝在 `~/.cargo`。環境變數範本見第 5 節。

| 項目 | 結果 |
|------|------|
| `./gradlew assembleDebug lint testDebugUnitTest --rerun-tasks` | ✅ BUILD SUCCESSFUL；單元測試 **146 項全過**；lint 0 錯誤、10 個警告（targetSdk 不是最新、Gradle 有新版、`UseKtx` 等建議類） |
| APK 內原生函式庫 | `libv2c_ffi.so`、`libjnidispatch.so`、`libsherpa-onnx-jni.so`、`libonnxruntime.so` 都在（arm64-v8a＋x86_64）；`zipalign -c -P 16` 通過；ELF LOAD 段全是 0x4000（16 KB） |
| `bash tools/build-android.sh`（拋棄式金鑰） | ✅ `--check` 全部找到；release 建置、16 KB 對齊、`apksigner verify`（v2）、複製到 `releases/`、寫 `SHA256SUMS.txt` 全部通過。這個 APK 用的是測試金鑰，已刪除 |
| 模擬器（Android 16、x86_64、KVM） | 設定頁、啟用鍵盤、鍵盤畫面正常；來源小圖示長按選單（自動／TEA／Breeze／系統／本機）、選擇在重開鍵盤後保留、強制「本機」未下載時顯示原因與下載詢問 |
| 區網偵測 | 模擬器經主機網路連得到兩個區網，偵測 68 ms 內完成；兩邊都通時誰先回應就用誰（TN、RD 都出現過） |
| 區網 TEA | 修正金鑰後（見第 3 節）：錄音 → TEA → `to_zh_tw` → 文字送進輸入框，約 0.4–0.6 秒 |
| 區網 Breeze | 強制 Breeze：帶預設金鑰成功，無聲時顯示「沒有聽到聲音，請再試一次」 |
| 中翻英 | 中翻英鍵 → TEA → Qwen → 英文送進輸入框，翻譯 518 ms |
| 本機模型 | 鍵盤錯誤列「下載本機模型」→ 詢問 → 下載 237 MB → SHA-256 通過；比較工具用 `tests/fixtures/audio` 10 段錄音：**CER 7.7%**、延遲中位數 815 ms、RTF 0.12（模擬器跑在主機 CPU，不代表手機速度） |
| 當機 | 整段測試 logcat 沒有 FATAL／AndroidRuntime |

模擬器做不到的（要手機）：真的對麥克風說話（模擬器以 `-no-audio` 執行，錄到的是靜音）、系統辨識（`SpeechRecognizer`）、行動網路／VPN 切換、手機上的效能、LINE／Gmail 等 App 的退格與送出行為。

## 3. 這次在 Linux 修掉的問題

1. **TEA 每句都 401（模擬器實測發現）**：TN、RD 的 TEA-ASR `:8001` 跟 Breeze 一樣要金鑰（`tn8227` 可用），但共用預設檔只給 Breeze 金鑰。你決定改共用預設檔：`TN_／RD_CUSTOM_LLM_ADDR.toml` 的 `needs_key`、`default_keys` 都加上 `*-tea-asr`。
   - **Windows／Linux 桌面也受影響**：套用預設組態的對話框，TEA 也會出現「API 金鑰」欄（留空就用內建金鑰）。之前已套用過 TEA 的桌面，要**重新套用一次預設組態**，TEA 才會有金鑰。
   - 桌面端 `presets.rs` 測試與 `v2c-ffi` 測試已跟著改：`cargo test -p v2c-desktop` 245 項、`-p v2c-ffi` 21 項通過，clippy、fmt 乾淨。
2. 比較工具接上 `to_zh_tw`，CER 不再把簡繁字形算錯（22.6% → 7.7%）；上方留白加大，第一列按鈕不再被狀態列擋住。

## 4. Windows 上的下一步（依序）

1. `git pull`。
2. 確認工具（Windows 那台已有 SDK、NDK 28.2、JDK 17）：
   ```powershell
   rustup target add aarch64-linux-android x86_64-linux-android
   cargo install cargo-ndk
   powershell -ExecutionPolicy Bypass -File tools\build-android.ps1 -Check
   ```
   `-Check` 會列出缺什麼與安裝指令。SDK 平台若是用新版 sdkmanager 裝，名稱是 `platforms;android-37.0`。
3. （建議）先跑一次 debug 與測試：`cd apps\android; .\gradlew.bat assembleDebug lint testDebugUnitTest --rerun-tasks`。
4. 產生**正式**簽署金鑰（`-Check` 沒找到 `keystore.properties` 時會印出完整的 `keytool` 指令與檔案範本；金鑰與密碼請自己保管，不進 git，路徑不要有反斜線）。
5. `powershell -ExecutionPolicy Bypass -File tools\build-android.ps1` → `releases\v2c_0.1.0_android.apk`，SHA-256 寫進 `releases\SHA256SUMS.txt`。這是 `.ps1` 第一次實跑，有錯請把輸出貼回來。
6. 側載到手機，照 `docs/android-install.md` 操作，並做 `tasks/human-checklist.md` Phase 2 的實測項目。
7. **檢查點 2**（手機日常使用一週）。

## 5. 在 Linux 重現這次測試

```bash
R=$PWD   # repo 根目錄
export JAVA_HOME=$R/tests/tmp/jvm/jdk-17.0.21+9
export ANDROID_HOME=$R/tests/tmp/android-sdk ANDROID_SDK_ROOT=$R/tests/tmp/android-sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/28.2.13676358
export ANDROID_USER_HOME=$R/tests/tmp/android-user ANDROID_AVD_HOME=$R/tests/tmp/android-user/avd
export GRADLE_USER_HOME=$R/tests/tmp/gradle-home
export PATH=$JAVA_HOME/bin:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$HOME/.cargo/bin:$PATH
cd apps/android && ./gradlew assembleDebug lint testDebugUnitTest --rerun-tasks
emulator -avd v2c-test -no-window -no-audio -no-snapshot -no-boot-anim -gpu swiftshader_indirect -port 5584 &
adb -s emulator-5584 install -r app/build/outputs/apk/debug/app-debug.apk
adb -s emulator-5584 shell pm grant app.v2c.ime android.permission.RECORD_AUDIO
adb -s emulator-5584 shell ime enable app.v2c.ime/.V2cImeService
adb -s emulator-5584 shell ime set app.v2c.ime/.V2cImeService
adb -s emulator-5584 shell settings put secure show_ime_with_hard_keyboard 1   # 模擬器有實體鍵盤時仍顯示螢幕鍵盤
```

- 重新 `adb install` 後系統會把輸入法切回預設，要再 `ime enable`／`ime set` 一次。
- 截圖在 `tests/tmp/android-smoke/`（git 已忽略）。

## 6. 審查與實測時決定、實作者沒寫進文件的行為

- 區網偵測：網路真的換了（不同 `Network`，或目前的網路斷了）才清快取；鍵盤每次開啟只重測、不先清快取。
- 探測 `GET <base>/models`，任何 < 500 的 HTTP 狀態都算「伺服器在」（所以要金鑰的 401 也算在）。
- 多段（>30 秒）辨識結果直接相接，不加空白。
- `network_security_config` 只管 Java 的 HTTP；Rust 的 `transcribe_pcm`／`translate` 用自己的連線，不受它限制。
- 原生函式庫載入失敗（`LinkageError`）時鍵盤不會當掉，改成區網／本機不可用。
- 下載詢問只在第一次切到「本機」時自動出現；按過「稍後」之後，要從錯誤列的「下載本機模型」按鈕叫出。
- 無聲錄音送 TEA 會得到「嗯」這類字（模型行為）；Android 沒有潤飾，所以會直接輸出。

## 7. 已知的小問題（不擋實測）

- 比較工具（只有 debug 版）版面很擠：按鈕被擠到換行、Paraformer 的 Delete 鍵被擠出畫面。
- lint 建議：`allowBackup` 改用 `dataExtractionRules`、`SharedPreferences.edit` 改 KTX 寫法、`R.string.source_system` 沒用到。

## 8. 留給使用者（🧑，全部也在 `tasks/human-checklist.md` Phase 2）

1. Windows：正式簽署金鑰、跑 `.ps1` 建 release APK。
2. 側載到 Android 16／17 手機實測 T2.1，回報機型與版本。若裝置端辨識沒有中文，第一句應提示「請再說一次」，第二句起正常。
3. T2.2：在 LINE、Chrome、Gmail 測退格長按、選取刪除、emoji、換行與送出；LINE 的「Enter 送出」開、關兩種都要測。
4. T2.4：公司 Wi-Fi／VPN 下自動走 TEA；長按切 Breeze／系統／本機；關 Wi-Fi 改系統辨識；區網伺服器關閉時不卡頓；中翻英輸出英文。
5. T2.6：讀 SenseVoice 的 FunASR MODEL_LICENSE；在手機跑比較工具（模擬器數字見第 2 節），決定預設模型。
6. 桌面（Windows／Linux）：重新套用一次 TN 或 RD 預設組態，讓 TEA 也存到金鑰。

## 9. 文件差異提醒

- T2.3 卡片的 `transcribe_pcm` 簽名是 `transcribe_pcm(pcm_le: ByteArray, rate, service_id)`。
- 本機模型預設候選是 SenseVoice `2025-09-09` int8；Linux 桌面實測用的是 `2024-07-17` 版。模擬器上 `2025-09-09` 的 CER 7.7%，與桌面 7.9% 相近。
- 翻譯逾時寫死 8000 ms（`TRANSLATE_TIMEOUT_MS`），刻意比桌面的 `llm_cloud_ms = 3000` 長，因為 Android 沒有設定頁。
- `AGENTS.md` 規則 5 的 `CARGO_TARGET_DIR=C:\Users\lance.tn\.v2ct` 是 Windows 路徑；Linux 用 `<repo>/target`。
