# v2c 語音輸入

按住 **右 Ctrl** 說中文（台灣）或英文，放開後整理好的文字會直接輸入到游標所在的位置。
按 **右 Ctrl + Shift** 說中文，輸出英文。

- **地端優先**：語音辨識在你的電腦上執行，安裝後不需要帳號、不需要任何 API 金鑰、不用付費。
- 有 NVIDIA 顯示卡時使用 **Breeze-ASR-25**（台灣中文最準）；沒有時使用 **SenseVoice-Small**（CPU）。
- 潤飾（去贅字、全形標點、確保繁體）與中翻英使用你自己的 **LM Studio**（選用）；也可以選擇串接雲端服務（選用）。

目前版本：0.1（Windows 第一版，未簽章）。

---

## 系統需求

| 項目 | 需求 |
|------|------|
| 作業系統 | Windows 10 或 11（64 位元） |
| 顯示卡 | 選用。NVIDIA 顯示卡（建議 6 GB 顯示記憶體以上、已安裝 NVIDIA 驅動程式）才能用 Breeze；沒有的話自動使用 CPU 版的 SenseVoice |
| 磁碟空間 | 安裝檔約 440 MB；安裝後程式約 650 MB（大部分是 NVIDIA cuBLAS 函式庫）＋模型：SenseVoice 約 230 MB、Breeze q5_0 約 1.1 GB |
| 其他 | 麥克風；WebView2 執行環境（Windows 11 內建，Windows 10 安裝程式會自動下載） |

## 安裝

1. 下載 `v2c_0.1.0_x64-setup.exe`。
2. 執行安裝檔。因為第一版**沒有數位簽章**，Windows 會跳出藍色的「**Windows 已保護您的電腦**」（SmartScreen）：
   按「**其他資訊**」→「**仍要執行**」。
3. 安裝只裝在你自己的帳號底下（`%LOCALAPPDATA%\v2c`），**不需要系統管理員權限**，也會在「開始」功能表建立捷徑。

### 防毒軟體（Windows Defender）

開發者在建置機上以 Windows Defender（病毒碼 1.463.203.0）掃描 0.1.0 安裝檔與 `v2c.exe`，未發現威脅；你的電腦掃描結果可能不同。
v2c 會攔截鍵盤（只為了偵測右 Ctrl，按鍵不會被記錄或擋下）並模擬打字，這類行為偶爾會被防毒軟體誤判。如果被攔下：

1. 開啟「Windows 安全性」→「病毒與威脅防護」→「保護歷程記錄」，確認被攔下的是 v2c 的檔案。
2. 若確定檔案是從可信任的來源下載，可以選「允許在裝置上」，或把 v2c 的安裝資料夾加入「排除項目」。
3. 也歡迎把檔案回報給 Microsoft 做誤判分析：<https://www.microsoft.com/wdsi/filesubmission>。

## 第一次啟動

第一次開啟時會自動進入「初次設定」：

1. **偵測顯示卡**：有 NVIDIA 顯示卡就建議 Breeze-ASR-25 q5_0，否則使用 SenseVoice。
2. **下載語音模型**：顯示進度、下載中斷可以接著續傳，完成後會用 SHA-256 驗證檔案。
   模型存放在 `%LOCALAPPDATA%\v2c\models\`。
   - SenseVoice 從 sherpa-onnx 官方 GitHub Release 下載。
   - Breeze 目前還沒有公開的下載位置：請用「**從本機檔案匯入**」選擇自己用 `tools/convert-breeze` 轉好的 `.bin` 檔。
3. **測試麥克風**：說幾句話，音量條會跟著動。
4. **試試看**：在頁面上的文字框按住右 Ctrl 說話。
5. 完成。v2c 之後會在背景執行（系統列圖示）；關掉視窗不會結束程式。

下載失敗不影響 v2c 的其他功能，隨時可以從「設定 → 地端語音模型」或「初次設定」頁重新下載。

## 熱鍵

| 操作 | 功能 |
|------|------|
| 按住 **右 Ctrl** 說話、放開 | 輸入文字到游標處 |
| 快速按兩下 **右 Ctrl** | 免持模式（不用一直按著），再按一下結束 |
| 按住 **右 Ctrl + Shift** 說中文 | 輸出英文（需要 LM Studio 或雲端服務） |
| **Esc** | 取消這段錄音 |

按住的判定時間、雙擊間隔可以在「設定 → 熱鍵」調整；第一版的按鍵固定為右 Ctrl。

## 開機自動啟動

在「設定 → 啟動」勾選後，登入 Windows 時 v2c 會在背景啟動（只顯示系統列圖示）。v2c 同一時間只會執行一份：再從「開始」功能表開啟時，會叫出已在執行的那一份視窗。

- 已知限制：若 Windows 使用者名稱含有空白，開機啟動登錄項目（`HKCU\...\Run`）中的程式路徑不會加上引號（上游 auto-launch 套件的限制）；受影響的只會是你自己帳號底下的資料夾。

## 潤飾與中翻英：LM Studio（選用）

沒有 LM Studio 也能用：潤飾會改用規則式去贅字，只是中翻英無法使用。
安裝 [LM Studio](https://lmstudio.ai/)、載入 3B～4B 的小模型並開啟 Local Server（預設 `http://localhost:1234/v1`），
v2c 就會自動使用。建議設定與顯示卡記憶體的分配請看 [docs/local-setup.md](docs/local-setup.md)
（安裝後也在 `安裝資料夾\docs\local-setup.md`）。

## 雲端服務（選用）

可以在「雲端服務」頁用精靈串接 Gemini、OpenAI、Claude 或自訂的 OpenAI 相容端點，並指定把語音辨識／潤飾／中翻英哪些交給雲端。
雲端服務會依用量向你收費。步驟與各家取得金鑰的方法請看 [docs/cloud-setup.md](docs/cloud-setup.md)，各家推薦的模型與設定請看 [docs/cloud-models.md](docs/cloud-models.md)。
系統列選單的「全部改用地端」可以隨時一鍵切回。

想把模型放在區網裡另一台 Ubuntu 電腦（Breeze 語音辨識＋Qwen／GLM 文字模型，CPU 或 NVIDIA GPU），讓多台電腦共用，請看 [docs/lan-server-guide.md](docs/lan-server-guide.md)。

## 隱私

- **預設全部在你的電腦上處理**，錄音不會上傳。麥克風「常開」模式只在記憶體中保留最近 0.5 秒，從不存檔；可在設定改成「按下才開」。
- **輸入紀錄**（預設保留 7 天、最多 200 筆）以 Windows DPAPI 加密後存放，只有你的 Windows 帳號能解開；不存錄音。
  密碼管理器（1Password、Bitwarden、KeePass…）在前景時不記錄；名單可在設定修改，也可以整個關閉。
- **日誌**（`%APPDATA%\v2c\logs\`）只記錄長度、耗時、使用的服務名稱與錯誤代碼，**永遠不記錄辨識或潤飾後的文字、詞庫內容**。
- **API 金鑰**存在 Windows 認證管理員，不寫進設定檔。
- **啟用雲端服務後**：交給該服務的用途會把資料送過去——語音辨識送出這段錄音（WAV）與你的詞庫；潤飾／中翻英送出辨識後的文字與詞庫。未指定給雲端的用途維持地端。
- 首次設定只會連線到模型下載網址（GitHub／你設定的下載位置），不會送出任何個人資料。

## 檔案位置

| 內容 | 位置 |
|------|------|
| 程式 | `%LOCALAPPDATA%\v2c\`（安裝資料夾） |
| 設定檔 | `%APPDATA%\v2c\config.toml` |
| 詞庫、輸入紀錄、日誌 | `%APPDATA%\v2c\`（`vocab.txt`、`history.db`、`logs\`） |
| 語音模型 | `%LOCALAPPDATA%\v2c\models\` |

## 解除安裝

「設定 → 應用程式 → 已安裝的應用程式」找到 **v2c** → 解除安裝。
解除安裝**一律保留**你的設定、詞庫、輸入紀錄與模型（解除安裝畫面的「刪除應用程式資料」只會清掉介面的暫存資料）；要完全清除，請再手動刪除：

- `%APPDATA%\v2c`
- `%LOCALAPPDATA%\v2c\models`（模型檔，可能超過 1 GB）
- `%LOCALAPPDATA%\tw.v2c.desktop`（介面的暫存資料）

開機自動啟動若有開啟，請先在「設定 → 啟動」關閉。

## 疑難排解

| 狀況 | 處理方式 |
|------|----------|
| 看不到螢幕下方的浮動標籤 | 少數顯示卡驅動程式不支援透明視窗。結束 v2c，設定環境變數 `V2C_OVERLAY_LAYERED_ALPHA=1` 後重新開啟（PowerShell：`[Environment]::SetEnvironmentVariable('V2C_OVERLAY_LAYERED_ALPHA','1','User')`） |
| 在某個程式裡按右 Ctrl 沒有反應，顯示「目前視窗以系統管理員身分執行」 | Windows 不允許一般程式輸入到以系統管理員身分執行的視窗。請改用一般權限開那個程式，或不要在那個視窗使用 |
| LINE、Teams、Slack 等聊天程式，換行時訊息被送出 | 在「設定 → App 規則」把該程式的換行鍵設為 Shift+Enter（常見聊天程式已內建） |
| 虛擬機（VMware、VirtualBox）裡打不出字 | 這些視窗收不到模擬打字；「App 規則」已預設改用貼上，並請在虛擬機開啟剪貼簿共用 |
| 「還沒有語音模型」 | 到「初次設定」下載 SenseVoice，或匯入 Breeze |
| 顯示「已改用 CPU」 | 顯示記憶體不足、或遊戲／剪片程式在前景，v2c 讓出顯示卡；離開後約 20 秒自動恢復。見「設定 → 顯示卡記憶體」 |
| 「請啟動 LM Studio」 | 中翻英需要 LM Studio（或雲端服務）；確認 LM Studio 的 Local Server 已啟動 |
| 麥克風沒聲音 | 「設定 → 麥克風」選擇正確的裝置並測試；確認 Windows「設定 → 隱私權 → 麥克風」允許桌面應用程式使用麥克風 |

## 從原始碼建置

完整步驟見 [docs/build-guide.md](docs/build-guide.md)（一鍵腳本 `tools\build-installer.ps1`）；模型、安裝檔等不進 git 的大檔怎麼取得，見 [docs/download-guide.md](docs/download-guide.md)。

需要：Visual Studio 2022 Build Tools（C++ 工作負載）、Rust（MSVC 工具鏈）、LLVM（libclang）、Node.js LTS；
要用顯示卡還需要 NVIDIA CUDA Toolkit。`tools/setup-dev.ps1` 會安裝這些（請先讀過腳本，再自己以系統管理員身分執行）：

```powershell
powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1
```

環境變數（每個新的終端機視窗）：

```powershell
$env:Path = "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;" + $env:Path
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
# CUDA 建置另外需要：
$env:CUDA_PATH = "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5"
$env:CMAKE_CUDA_ARCHITECTURES = "75"   # 你的顯示卡架構，例如 RTX 20 系列 = 75、RTX 30 = 86、RTX 40 = 89
```

常用指令：

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p v2c-cli -- bench --asr breeze tests/fixtures/audio
cd apps/desktop && npm install && npm run tauri dev     # 開發模式
cd apps/desktop && npm run tauri:build:cuda             # 發行用 NSIS 安裝檔（含 CUDA）
cd apps/desktop && npm run tauri build                  # 不含 CUDA 的安裝檔（只用 SenseVoice／CPU）
```

用 `tools\build-installer.ps1` 建置時，安裝檔會放在 repo 的 `releases\`（不進 git）；手動執行 `npm run tauri build` 則在 `<CARGO_TARGET_DIR 或 target>\release\bundle\nsis\`。
Breeze 模型的轉檔方式見 [tools/convert-breeze/README.md](tools/convert-breeze/README.md)。

## Linux（測試中）

Linux 版（Ubuntu 22.04／GNOME，透過 IBus 輸入）還沒有在實機上按住說話驗證過。這一版只有 CPU 版：語音辨識用區網的遠端服務或 SenseVoice，不支援 Breeze／CUDA。

- 安裝：`sudo apt install ./v2c_0.1.6_amd64.deb`，再登出並重新登入（或執行 `ibus restart`，會讓所有 App 的輸入法短暫中斷），然後在「設定 → 鍵盤 → 輸入來源」加入「v2c 語音輸入」（「設定」裡加不進去時，用 [docs/linux-setup.md](docs/linux-setup.md) 第 7.2.1 節的 `gsettings` 指令）。.deb 會一併安裝 IBus 輸入法元件。
- 也會產生免安裝的 `v2c_0.1.6_amd64.AppImage`，但它不含 IBus 元件，建議用 .deb。
- 設定檔在 `~/.config/v2c/config.toml`，模型在 `~/.local/share/v2c/models/`。
- 安裝檔不放進 git，請自己建置：`bash releases/linux/build.sh --check` 會列出缺少的套件；`bash releases/linux/build.sh` 會產生 .deb 與 AppImage，放在 `releases/linux/`，雜湊寫在 `releases/linux/SHA256SUMS.txt`（見 [releases/linux/README.md](releases/linux/README.md)）。

完整步驟（加入輸入來源、設定語音辨識與文字模型兩個遠端服務）見 [docs/linux-setup.md](docs/linux-setup.md) 第 7 節。

## 授權

v2c 本身以 MIT 授權釋出。使用與隨附的第三方元件：

| 元件 | 授權 | 說明 |
|------|------|------|
| [Breeze-ASR-25](https://huggingface.co/MediaTek-Research/Breeze-ASR-25)（MediaTek Research） | Apache-2.0 | 地端語音辨識（GPU）；模型檔由使用者自行轉檔或下載，不隨安裝檔附上 |
| [SenseVoice-Small](https://github.com/FunAudioLLM/SenseVoice)（阿里巴巴 FunAudioLLM） | FunASR Model License | 地端語音辨識（CPU）；首次啟動時下載。依授權需標示出處：本程式使用 SenseVoice-Small，模型版權屬原作者 |
| [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) | Apache-2.0 | SenseVoice 推論（隨附 `sherpa-onnx-*.dll`） |
| [ONNX Runtime](https://github.com/microsoft/onnxruntime) | MIT | 隨附 `onnxruntime*.dll` |
| [whisper.cpp](https://github.com/ggml-org/whisper.cpp) | MIT | Breeze 推論 |
| NVIDIA CUDA 函式庫（cuBLAS：`cublas64_13.dll`、`cublasLt64_13.dll`） | NVIDIA CUDA Toolkit EULA | 依 EULA 附件 A（可隨應用程式散布的檔案）隨附；僅供 v2c 使用 |
| Microsoft Visual C++ 執行階段（`msvcp140.dll`、`vcruntime140*.dll`） | Visual Studio 可散布程式碼 | 隨附於安裝資料夾 |
| [Tauri](https://tauri.app/) | MIT／Apache-2.0 | 桌面程式框架 |
