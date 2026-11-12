# Android 語音鍵盤：建置、安裝與使用

v2c 的 Android 版是一個語音輸入法（鍵盤）：點一下麥克風說話，辨識後的文字直接輸入到目前的輸入框；另有「中→英」鍵，說中文、輸出英文。沒有設定頁、沒有輸入紀錄、沒有潤飾。需求是 Android 9（API 28）以上，建議 Android 16 或 17。APK 只含 arm64-v8a（現在的手機都是）。

APK 不放進 git，請自己建置，成品在 `releases/v2c_<版本>_android.apk`，雜湊在 `releases/SHA256SUMS.txt`。

## 1. 建置 APK

兩個平台各有一支腳本，做的事一樣：檢查工具 → 編譯 Rust 的 `.so` → `assembleRelease` → 驗證 16 KB 對齊與簽章 → 複製到 `releases/` 並更新 SHA-256。**腳本不會安裝任何東西**；缺什麼會列出要你執行的指令。

| 平台 | 只檢查 | 檢查並建置 |
|------|--------|------------|
| Linux／macOS | `bash tools/build-android.sh --check` | `bash tools/build-android.sh` |
| Windows | `powershell -ExecutionPolicy Bypass -File tools\build-android.ps1 -Check` | `powershell -ExecutionPolicy Bypass -File tools\build-android.ps1` |

### 1.1 需要的工具（一次性）

| 項目 | 版本／指令 |
|------|------------|
| JDK | 17（設定 `JAVA_HOME`） |
| Android SDK | `platforms;android-37`、`platforms;android-36`、`build-tools;36.0.0`、`platform-tools`（設定 `ANDROID_HOME`） |
| NDK | `ndk;28.2.13676358`（預設 16 KB 對齊；`ANDROID_NDK_HOME` 沒設時用 `<SDK>/ndk/28.2.13676358`） |
| Rust | `rustup target add aarch64-linux-android x86_64-linux-android`、`cargo install cargo-ndk` |

用 cmdline-tools 安裝 SDK 與 NDK 的例子：

```sh
sdkmanager "platforms;android-37" "platforms;android-36" "build-tools;36.0.0" "platform-tools" "ndk;28.2.13676358"
```

第一次建置需要網路：Gradle 會下載 sherpa-onnx v2.0.1 的 Android 函式庫並驗證 SHA-256（快取在 `apps/android/.gradle/sherpa-onnx-cache`，`clean` 不會刪）。若環境變數 `CARGO_TARGET_DIR` 有設定，Rust 建置也會用它（建置快取，不是成品）。

### 1.2 產生簽署金鑰（一次性，由你執行）

沒有金鑰時，腳本會印出下面的指令然後停止，**不會產生未簽署的 APK**。金鑰檔和密碼請自己備份：弄丟了就無法用同一個簽章更新已安裝的 App。金鑰與密碼檔都已加入 `.gitignore`，不要提交。

```sh
keytool -genkeypair -v -keystore apps/android/v2c-release.jks -alias v2c \
  -keyalg RSA -keysize 4096 -validity 36500
```

（`keytool` 在 JDK 的 `bin` 裡。Windows 請把 `\` 換行改成同一行，或用反引號 `` ` ``。）

接著建立 `apps/android/keystore.properties`：

```properties
storeFile=v2c-release.jks
storePassword=你輸入的金庫密碼
keyAlias=v2c
keyPassword=你輸入的金鑰密碼
```

`storeFile` 用相對路徑時以 `apps/android` 為基準；寫絕對路徑請用正斜線（例如 `C:/keys/v2c-release.jks`）。

### 1.3 建置結果

成功時腳本會顯示：

- `zipalign -c -P 16 -v 4` 通過（`.so` 檔 16 KB 對齊，Android 15+ 的 16 KB 頁面裝置才能載入）。
- `apksigner verify` 通過。
- APK 內有 `lib/arm64-v8a/` 的 `libv2c_ffi.so`、`libjnidispatch.so`、`libsherpa-onnx-jni.so`、`libonnxruntime.so`。
- 檔案在 `releases/v2c_<版本>_android.apk`（版本取自 `apps/android/app/build.gradle.kts` 的 `versionName`），SHA-256 寫在 `releases/SHA256SUMS.txt`（同名檔案的舊一行會被取代，其他行保留）。

也可以手動驗證：

```sh
<SDK>/build-tools/36.0.0/zipalign -c -P 16 -v 4 releases/v2c_<版本>_android.apk
```

開發時 `cd apps/android && ./gradlew assembleDebug` 會一併編出 arm64 與 x86_64（模擬器）的 `.so`。只想跑 lint 而不編 Rust 時可加 `-Pv2c.skipRust=true`（沿用上次產生的結果；沒有綁定檔時 FFI 相關 Kotlin 無法編譯）。

## 2. 安裝到手機（側載）

1. 把 APK 傳到手機（USB 線、雲端硬碟、`adb install releases/v2c_<版本>_android.apk` 都可以）。
2. 用檔案管理員點 APK。第一次會要求允許「安裝不明應用程式」：在跳出的畫面把該來源（例如「檔案」或瀏覽器）的開關打開，再回來按安裝。
3. 若手機啟用 Play 安全防護而警告「未知的開發人員」，選「仍要安裝」。
4. 想確認檔案沒被改過：比對 `releases/SHA256SUMS.txt` 的雜湊（Linux `sha256sum`，Windows `Get-FileHash`）。

## 3. 第一次設定

安裝完成後開啟「v2c 語音輸入」App，它會帶你做三步，每完成一步會打勾：

1. **授權麥克風**：按「授權」，在系統對話框選「使用應用程式時」。鍵盤自己無法要求麥克風權限，所以一定要在這裡授權。
2. **啟用 v2c 鍵盤**：按「開啟輸入法設定」，在清單把「v2c 語音鍵盤」打開，系統會警告輸入法可能收集輸入內容，選「確定」。v2c 只把聲音辨識成文字，辨識文字不寫進日誌。
3. **切換到 v2c 鍵盤**：按「選擇鍵盤」，選「v2c 語音鍵盤」。

三步都打勾後會顯示「全部完成」。

## 4. 日常使用

### 切換鍵盤

在任何輸入框點一下叫出鍵盤後，按畫面底部導覽列右下角的**切換鍵盤按鈕**（小鍵盤圖示，Android 16／17 的三鍵或手勢導覽列都有），選「v2c 語音鍵盤」；要換回原本的鍵盤同樣用這個按鈕。v2c 鍵盤上沒有「切回」鍵。

### 說話

- **點一下說話**：點一下大麥克風開始錄音（畫面顯示波形與計時），說完**再點一下**結束並送出，辨識後的文字直接輸入。
- **按住說話**：按住麥克風超過約 0.3 秒就是按住模式，說話期間一直按著，**放開**就送出。
- 中文輸出一律是繁體中文（臺灣用字），英文夾雜在句子裡沒問題。

### 中→英鍵

按「中→英」鍵，操作和麥克風完全一樣（點一下或按住），說中文，輸入的是英文翻譯。需要連上公司網路（見第 5 節）；連不到時這個鍵會變灰，點了會顯示「需要連上公司網路」。

### 退格與換行

退格鍵長按會連續刪除。換行鍵依輸入框的設定決定是換行還是送出（搜尋框、聊天室「Enter 送出」等）。

### 長按來源小圖示：選擇辨識方式

麥克風旁的小圖示顯示目前的辨識來源（例如「TEA」「系統」「本機」）。**長按**它會跳出選單：

| 選項 | 意思 |
|------|------|
| 自動（預設） | 連得上公司區網就用區網的 TEA-ASR；連不到就用手機的系統辨識；失敗時退回本機模型（已下載的話） |
| TEA | 強制用區網的 TEA-ASR |
| Breeze | 強制用區網的 Breeze-ASR |
| 系統 | 強制用手機內建的語音辨識（Google 語音服務） |
| 本機 | 強制用手機上的本機模型，離線可用 |

選了「自動」以外的項目，就**只用**那個來源；它不可用時會顯示原因，不會偷偷改用別的。選擇會記住，下次開鍵盤沿用。系統辨識若提示「請安裝或更新 Google 語音服務」，按旁邊的按鈕到 Play 商店更新。

### 本機模型下載

第一次切到「本機」（或自動模式需要退回本機、但還沒下載）時，鍵盤會問你要不要下載模型：

- 預設模型 SenseVoice-Small（int8），大小約 237 MB，建議連 Wi-Fi 下載。
- 檔案下載到 App 私有目錄，並比對 SHA-256，不符會重新下載；可以隨時取消，下次再問。
- 下載完成後完全離線可用（飛航模式也行）。細節與模型比較見 [android-local-model.md](android-local-model.md)。

## 5. 哪些功能需要公司網路

| 功能 | 網路需求 |
|------|----------|
| 區網 TEA／Breeze 語音辨識 | 要在公司 Wi-Fi 或 VPN 內（TN 或 RD 區網） |
| 中→英 | 同上（用區網的 Qwen 模型） |
| 系統辨識 | 看手機：裝置端辨識不需網路；否則需要一般網路 |
| 本機模型辨識 | 不需要（只有第一次下載模型需要網路） |

離開公司網路時，自動模式會改用系統辨識或本機模型，不會卡住；只有中→英用不了。

## 6. 疑難排解

| 狀況 | 處理 |
|------|------|
| 按麥克風顯示「尚未授權麥克風」 | 開啟 v2c App 重做第 3 節第 1 步，或到「設定 → 應用程式 → v2c → 權限」開啟麥克風 |
| 鍵盤清單找不到 v2c | 確認第 3 節第 2 步已打開；沒有就重新安裝 APK |
| 「此裝置沒有語音辨識服務」 | 安裝或更新 Google 語音服務，或長按來源小圖示改選「本機」 |
| 第一句提示「已切換辨識方式，請再說一次」 | 裝置端辨識不支援中文，已改用一般系統辨識；再說一次即可 |
| 安裝時顯示「套件似乎無效」或版本衝突 | 簽署金鑰和已安裝的版本不同：先解除安裝舊版（或用同一把金鑰建置） |
| 想重新建置但腳本說缺工具 | 照腳本印出的指令安裝後，再跑一次 `--check`／`-Check` |
