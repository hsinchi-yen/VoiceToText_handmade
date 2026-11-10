# 大檔下載指南

GitHub repo 只放原始碼與文件。下面這些檔案太大、或是你本人的錄音，**一律不進 git**（見 `.gitignore`），要另外下載或自己產生。

| 檔案 | 大小 | 用途 | 取得方式 |
|------|------|------|----------|
| SenseVoice-Small（int8） | 約 163 MB（壓縮檔） | CPU 語音辨識（備援） | App 初次設定自動下載，或手動下載（見下方） |
| Breeze-ASR-25 GGML q5_0 | 約 1.08 GB | GPU 語音辨識（預設） | 自己轉檔（見下方） |
| Breeze-ASR-25 GGML q8_0 | 約 1.66 GB | GPU 語音辨識（稍準、多用約 0.5 GB 顯存） | 自己轉檔 |
| LM Studio 語言模型 | 約 2 GB | 潤飾、中翻英（選用） | 在 LM Studio 裡下載 |
| Windows 安裝檔 `v2c_<版本>_x64-setup.exe` | 約 439 MB | 安裝 v2c | 自己建置，見 [build-guide.md](build-guide.md) |
| 測試錄音 `tests/fixtures/audio/*.wav` | 每個約 0.1～0.3 MB | `v2c-cli bench` 量測 | 自己錄，見 [tests/fixtures/audio/README.md](../tests/fixtures/audio/README.md) |

## 模型放哪裡

安裝版 v2c 從 `%LOCALAPPDATA%\v2c\models\` 讀模型：

```
%LOCALAPPDATA%\v2c\models\
├─ ggml-breeze-asr-25-q5_0.bin
├─ ggml-breeze-asr-25-q8_0.bin                                   （選用）
└─ sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17\      （SenseVoice 解壓後的資料夾）
   ├─ model.int8.onnx
   └─ tokens.txt
```

也可以在 App 的「初次設定」或「設定 → 語音模型」用「匯入」選檔案，v2c 會先驗證 SHA-256 再複製進去。

開發時（`cargo run`、`v2c-cli`），模型放在 repo 根目錄的 `models\`（不進 git）。

## SenseVoice-Small

- 來源：k2-fsa/sherpa-onnx 官方 release
- 網址：<https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2>
- 大小：163,002,883 bytes
- SHA-256：`7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e`

手動安裝：

```powershell
$dst = "$env:LOCALAPPDATA\v2c\models"
New-Item -ItemType Directory -Force $dst | Out-Null
curl.exe -L -o "$dst\sv.tar.bz2" https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2
(Get-FileHash "$dst\sv.tar.bz2" -Algorithm SHA256).Hash   # 要和上面的 SHA-256 一樣
tar -xjf "$dst\sv.tar.bz2" -C $dst
Remove-Item "$dst\sv.tar.bz2"
```

## Breeze-ASR-25（GGML）

原始模型是 Hugging Face 上的 [MediaTek-Research/Breeze-ASR-25](https://huggingface.co/MediaTek-Research/Breeze-ASR-25)（Apache-2.0）。官方沒有提供 whisper.cpp 用的 GGML 檔，所以要自己轉檔：

```powershell
powershell -ExecutionPolicy Bypass -File tools\convert-breeze\convert-breeze.ps1
```

- 需要 Python 3.12、git、VS 2022 Build Tools，約 12 GB 磁碟空間，20～40 分鐘。細節見 [tools/convert-breeze/README.md](../tools/convert-breeze/README.md)。
- 輸出在 repo 的 `models\`，再複製或「匯入」到 `%LOCALAPPDATA%\v2c\models\`。

2026-11-09 在這台電腦轉出的檔案（同版本工具應得到相同結果，可用來核對）：

| 檔案 | 大小（bytes） | SHA-256 |
|------|---------------|---------|
| `ggml-breeze-asr-25-q5_0.bin` | 1,080,732,108 | `9be65cda66ee424a261cae9e9864e5aceb2f1be63cc01e5b670e57d022fdf335` |
| `ggml-breeze-asr-25-q8_0.bin` | 1,656,129,708 | `669eb226a0e23b42465a6d2f60ce1902fbd534e19faab59511730420eb25e90d` |

> 待辦：把這兩個檔案上傳到你自己的 Hugging Face repo，再把網址、大小、SHA-256 填進 `apps/desktop/src-tauri/models.json`（目前是 `placeholder`），之後 App 的初次設定就能直接下載 Breeze。

## LM Studio 語言模型（選用）

v2c 不代裝 LM Studio，也不下載語言模型。

1. 從 <https://lmstudio.ai> 安裝 LM Studio。
2. 下載 3B～4B、**Q4_K_M** 量化的指令模型。這台電腦目前用 `qwen2.5-coder-3b-instruct`（約 1.9 GB）：
   ```powershell
   & "$env:USERPROFILE\.lmstudio\bin\lms.exe" get qwen2.5-coder-3b-instruct
   ```
3. 啟動 Local Server（預設 `http://localhost:1234/v1`），載入模型：
   ```powershell
   & "$env:USERPROFILE\.lmstudio\bin\lms.exe" server start
   & "$env:USERPROFILE\.lmstudio\bin\lms.exe" load qwen2.5-coder-3b-instruct --gpu max --context-length 8192
   ```

顯存設定與啟動順序（先開 v2c 再載入模型）見 [local-setup.md](local-setup.md)。

## 開發工具（建置才需要）

| 工具 | 版本（這台電腦） | 下載 |
|------|------------------|------|
| Visual Studio 2022 Build Tools（C++ 工作負載，內含 CMake） | 2022 | <https://visualstudio.microsoft.com/visual-cpp-build-tools/> |
| Rust（MSVC 工具鏈） | 1.99.1 | <https://rustup.rs> |
| LLVM（libclang） | — | <https://github.com/llvm/llvm-project/releases> |
| Node.js LTS | — | <https://nodejs.org> |
| NVIDIA CUDA Toolkit（GPU 版才需要） | 13.5 | <https://developer.nvidia.com/cuda-downloads> |
| ffmpeg（把手機錄音轉成 WAV，選用） | 9.0 | `winget install Gyan.FFmpeg` |

`tools/setup-dev.ps1` 會安裝前五項，請先讀過腳本，再自己以系統管理員身分執行。
