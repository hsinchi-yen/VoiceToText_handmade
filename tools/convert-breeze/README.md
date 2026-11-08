# Breeze-ASR-25 轉檔（GGML q5_0 / q8_0）

把 Hugging Face 上的 [MediaTek-Research/Breeze-ASR-25](https://huggingface.co/MediaTek-Research/Breeze-ASR-25) 轉成 whisper.cpp 用的 GGML 檔，並量化成 `q5_0` 與 `q8_0` 兩版。**這一步由你自己執行**（要下載約 3 GB）。

- 授權：Breeze-ASR-25 為 **Apache-2.0**，可轉檔、量化、再散布（散布時附上 LICENSE 與來源說明）。
- 輸出（不進 git）：`models\ggml-breeze-asr-25-q5_0.bin`（約 1.1 GB）、`models\ggml-breeze-asr-25-q8_0.bin`（約 1.7 GB）
- 暫存（不進 git）：`models\.work\`；Python 虛擬環境：`tools\convert-breeze\.venv\`

## 需要的東西

- Python 3.12（`py -3.12` 或 `python`）、git、VS 2022 Build Tools（C++，內含 CMake）
- 約 12 GB 可用磁碟空間；網路
- 不需要系統管理員權限；不會安裝任何系統軟體。Python 套件只裝進 `.venv`：
  `torch`（CPU 版，約 200 MB）、`transformers`（4.x）、`safetensors`、`numpy`、`huggingface_hub`

## 執行

在 repo 根目錄開一般的 PowerShell：

```powershell
powershell -ExecutionPolicy Bypass -File tools\convert-breeze\convert-breeze.ps1
```

腳本會先列出要做的事與磁碟空間，問你 `Proceed? [y/N]`，輸入 `y` 才會開始。約 20～40 分鐘（主要是下載與轉檔）。

選項：
- `-Yes`：不詢問直接執行
- `-KeepF16`：保留未量化的 f16 檔（約 3.1 GB，可用來比較準確度）
- `-Clean`：完成後刪除 `models\.work\`（下載的原始模型、whisper.cpp 原始碼與建置檔）
- `-ModelsDir <路徑>`、`-WorkDir <路徑>`：改輸出／暫存位置

## 它做了什麼

1. 建立 `.venv` 並安裝上述套件。
2. 從 Hugging Face 只下載轉檔需要的檔案（`model.safetensors`、`config.json`、tokenizer 檔等）。該 repo 裡的 `optimizer.bin` 與 `.pt` 檢查點（合計約 13 GB）不下載。
3. 取得 whisper.cpp **v1.8.3**（與 `whisper-rs` 0.16 內建的版本相同），並下載 openai/whisper 的 `mel_filters.npz`（轉檔腳本需要）。
4. 用 whisper.cpp 的 `models/convert-h5-to-ggml.py` 轉成 f16 GGML。
5. 用 CMake 建置 whisper.cpp 的 `whisper-quantize`（只用 CPU），量化成 `q5_0`、`q8_0`。
6. 印出每個檔案的大小與 SHA-256 —— 請抄進 `tasks/spike-results.md`。

## 常見問題

- **`transformers` 版本**：限定 4.x。transformers 5 預設以模型原始精度（bf16）載入，轉檔腳本的 `tensor.numpy()` 不支援 bf16 會失敗。
- **路徑太長**：whisper.cpp 的建置在很深的資料夾裡失敗（MSBuild `MSB4018`）時，用 `-WorkDir C:\v2cwork` 之類的短路徑重跑。
- **下載中斷**：直接重跑，已完成的步驟會跳過，`huggingface_hub` 會續傳。

## 轉完之後

量測步驟見 `tasks/spike-results.md` 的「你要執行的步驟」。
