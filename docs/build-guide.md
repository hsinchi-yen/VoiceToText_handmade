# 建置指南：從原始碼產生 v2c.exe 與安裝檔

適用 Windows 10／11 x64。需要的工具與下載位置見 [download-guide.md](download-guide.md) 的「開發工具」。

## 1. 一次性準備

1. 安裝開發工具。可以用 `tools/setup-dev.ps1`，請先讀過腳本，再以系統管理員身分執行：
   ```powershell
   powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1
   ```
2. 確認 CUDA 的環境變數只指向你要用的版本（GPU 版才需要）。
   - `CUDA_PATH` 要是 `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5`。
   - 系統 `Path` 裡如果還有舊版（例如 `CUDA\v11.0\bin`），CMake 可能抓到舊的 nvcc，出現「unsupported Microsoft Visual Studio version」。請在系統管理員 PowerShell 移除舊版路徑：
     ```powershell
     $p = [Environment]::GetEnvironmentVariable('Path','Machine') -split ';' | Where-Object { $_ -notmatch 'CUDA\\v11\.0' }
     [Environment]::SetEnvironmentVariable('Path', ($p -join ';'), 'Machine')
     ```
   - 改完後要**重開終端機**（或重新登入）才會生效。
3. 準備模型（執行和量測要用，建置本身不需要）：見 [download-guide.md](download-guide.md)。

## 2. 一鍵建置安裝檔（建議）

在 repo 根目錄：

```powershell
powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1           # GPU（CUDA）版
powershell -ExecutionPolicy Bypass -File tools\build-installer.ps1 -CpuOnly  # 不含 CUDA，只用 SenseVoice（CPU）
```

腳本會：
1. 設好 PATH（VS 內建的 CMake、cargo、Node、指定版本的 CUDA，並排除其他版本的 CUDA）與 `LIBCLANG_PATH`、`CUDA_PATH`、`CMAKE_CUDA_ARCHITECTURES`。
2. 第一次執行時在 `apps/desktop` 跑 `npm ci`。
3. 執行 `npm run tauri:build:cuda`（或 `-CpuOnly` 時的 `npm run tauri build`）。
4. 把安裝檔搬到 repo 的 `releases\`，SHA-256 寫進 `releases\SHA256SUMS.txt`。
5. 印出安裝檔與 `v2c.exe` 的大小、SHA-256，並用 Windows Defender 掃描。

**檔案整理規則：** 交付用的檔案（安裝檔）一律放在 repo 的 `releases\`；`%USERPROFILE%\.v2ct\` 只放編譯快取，可以隨時刪掉重建（下次建置會很久）。`releases\` 已列在 `.gitignore`，不會進 git。手動建置（第 5 節）時，也請把安裝檔搬到 `releases\`。

參數：

| 參數 | 預設 | 說明 |
|------|------|------|
| `-Jobs` | `1` | 同時編譯的工作數。記憶體 16 GB 又開著瀏覽器時用 1；記憶體充裕可設 4 |
| `-CudaPath` | `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5` | CUDA Toolkit 位置 |
| `-CudaArch` | `75` | 顯示卡架構：RTX 20 系列＝75、RTX 30＝86、RTX 40＝89 |
| `-TargetDir` | `%USERPROFILE%\.v2ct` | Cargo 的建置輸出資料夾（`CARGO_TARGET_DIR`） |

時間：第一次完整建置約 30～60 分鐘（whisper.cpp 的 CUDA 核心最久）；之後只改 Rust 程式約 4～5 分鐘。

## 3. 產出的檔案

| 檔案 | 位置 |
|------|------|
| 安裝檔 | repo 的 `releases\v2c_<版本>_x64-setup.exe`（約 439 MiB；腳本建好後從 `.v2ct\release\bundle\nsis\` 搬過來） |
| 雜湊清單 | `releases\SHA256SUMS.txt`（每個版本一行，給別人核對下載的檔案） |
| 主程式 | `%USERPROFILE%\.v2ct\release\v2c.exe` |
| 開發工具 | `%USERPROFILE%\.v2ct\release\v2c-cli.exe`（用 `cargo build --release -p v2c-cli` 產生） |

安裝檔是 NSIS 格式，只裝給目前使用者（`%LOCALAPPDATA%\v2c\`），不需要系統管理員權限。裡面附了 cuBLAS 和 VC++ 執行階段的 DLL，所以大小大多來自 `cublasLt64_13.dll`（約 476 MiB，壓縮後變小）。安裝檔與模型**不進 git**。

## 4. 改版本號

版本號要同時改這三個地方，安裝檔檔名與 App 顯示的版本才會一致：

| 檔案 | 欄位 |
|------|------|
| `Cargo.toml`（workspace 根目錄） | `[workspace.package] version` |
| `apps/desktop/src-tauri/tauri.conf.json` | `"version"` |
| `apps/desktop/package.json` | `"version"`（再到 `apps/desktop` 跑一次 `npm install`，同步 `package-lock.json`） |

## 5. 手動建置（了解細節或除錯用）

每開一個新的 PowerShell 視窗都要先設環境變數：

```powershell
$cuda = 'C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5'
$env:Path = "$cuda\bin\x64;$cuda\bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;$env:USERPROFILE\.cargo\bin;" + $env:Path
$env:LIBCLANG_PATH = 'C:\Program Files\LLVM\bin'
$env:CARGO_TARGET_DIR = "$env:USERPROFILE\.v2ct"
$env:CARGO_BUILD_JOBS = '2'
$env:CUDA_PATH = $cuda
$env:CUDAToolkit_ROOT = $cuda
$env:CMAKE_CUDA_ARCHITECTURES = '75'
# GPU 版會一起建 TEA-ASR 的 transcribe.cpp；中文版 Windows 的 MSVC 預設用 950 字碼頁讀原始碼，
# 不加這兩行會編譯失敗（見第 6 節）。在 Git Bash 裡要寫成 -utf-8。
$env:CFLAGS_x86_64_pc_windows_msvc = '/utf-8'
$env:CXXFLAGS_x86_64_pc_windows_msvc = '/utf-8'
```

常用指令：

```powershell
cargo build --workspace                                   # 不含 CUDA
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build --release -p v2c-cli --features cuda          # 量測工具（GPU 版）
cargo build --release -p v2c-cli --features tea,cuda      # 量測工具（含 TEA-ASR）
cd apps\desktop; npm ci; npm run tauri dev                # 開發模式（熱重載介面）
cd apps\desktop; npm run tauri:build:cuda                 # 安裝檔（GPU 版）
```

量測（模型與錄音見 download-guide）：

```powershell
& "$env:USERPROFILE\.v2ct\release\v2c-cli.exe" bench --asr breeze --lang zh --model models\ggml-breeze-asr-25-q5_0.bin tests\fixtures\audio
& "$env:USERPROFILE\.v2ct\release\v2c-cli.exe" hotkey-test --seconds 30
```

## 6. 常見問題

| 狀況 | 原因與處理 |
|------|------------|
| `unsupported Microsoft Visual Studio version! Only the versions between 2015 and 2019` | CMake 抓到舊版 CUDA（v11）。照第 1 節移除舊路徑，並刪掉舊的 CMake 快取：`Remove-Item -Recurse "$env:USERPROFILE\.v2ct\release\build\whisper-rs-sys-*"` |
| 建置 `transcribe-cpp-sys` 時出現一連串 `error C2065`、`C2146`（`canary\model.cpp`） | MSVC 用 950 字碼頁讀 UTF-8 原始碼，註解裡的特殊字元吃掉換行。設定 `CFLAGS_x86_64_pc_windows_msvc` 與 `CXXFLAGS_x86_64_pc_windows_msvc` 為 `/utf-8`（見第 5 節；`tools\build-installer.ps1` 已自動設定）後重建 |
| `is cmake not installed?` | PATH 沒有 CMake。把 VS Build Tools 內建的 CMake 路徑（見第 5 節）放進 PATH |
| `v2c-cli.exe` 結束代碼 53、沒有任何輸出 | 找不到 `cublas64_13.dll`。把 `CUDA\v13.5\bin\x64` 放進 PATH（安裝版已附 DLL，不受影響） |
| 找不到 `libclang.dll` | 設定 `LIBCLANG_PATH = C:\Program Files\LLVM\bin` |
| 建置到一半被中止、或電腦變很慢 | 記憶體不夠，連結階段最吃記憶體。用 `-Jobs 1`，先關掉 LM Studio 模型（`lms unload --all`）、WSL（`wsl --shutdown`）和不用的瀏覽器分頁。在前景執行，不要放背景 |
| 辨識比預期慢很多（每句約 1.5 秒以上） | 筆電顯示卡被限制功率（例如 Armoury Crate 的靜音模式只給 30 W）。改成「效能」或「加速」模式；可用 `nvidia-smi -q -d POWER` 檢查 `Current Power Limit` |
| Defender 把 `v2c.exe` 判成可疑 | v2c 有鍵盤掛勾與模擬輸入，可能被誤判。見 README 的「防毒軟體」一節 |
