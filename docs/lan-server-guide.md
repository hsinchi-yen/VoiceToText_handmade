# 區網地端模型架設手冊（Ubuntu 伺服器）

這份手冊說明怎麼把 v2c 用的模型改放到**區網裡的另一台 Ubuntu 電腦**，讓一台或多台 Windows 上的 v2c 共用，不必每台電腦都有好的顯示卡。

- **語音辨識**：Breeze-ASR-25，用 whisper.cpp 的 `whisper-server` 提供服務。
- **文字模型**（潤飾、中翻英）：Qwen 或 GLM，用 Ollama 提供服務。GPU 主機另外提供 vLLM 的做法。
- 每一種都分 **CPU 方案**（沒有顯示卡）和 **NVIDIA GPU 方案**。

> 本文的延遲與記憶體數字，除了標示「實測」的以外，都是**估計值**（2026-11 依同類模型推估，未在 Ubuntu 主機上量過）。架好後請用第 7 節的方法實測。

## 1. 架構

```
Windows 電腦（v2c）                          Ubuntu 伺服器（區網，例如 192.168.1.50）
┌──────────────────────┐   HTTP（區網）   ┌──────────────────────────────────────┐
│ 錄音 → 送出 WAV ─────┼────────────────▶│ whisper-server :8080                  │
│                      │                  │   Breeze-ASR-25（GGML）              │
│ 辨識結果 → 潤飾 ─────┼────────────────▶│ Ollama :11434（或 vLLM :8000）        │
│                      │◀────────────────┼   Qwen / GLM                          │
│ 打字到游標處          │                  └──────────────────────────────────────┘
└──────────────────────┘
```

v2c 端不用改程式：兩個服務都提供 **OpenAI 相容 API**，在 v2c 的「新增雲端服務」精靈選「自訂相容端點」，填伺服器的 IP 即可（第 6 節）。

- 語音辨識：`POST /v1/audio/transcriptions`（multipart：`file`＝16 kHz WAV、`model`、`response_format=json`、選填 `prompt`），回應 `{"text": "..."}`。
- 文字模型：`POST /v1/chat/completions`，模型清單 `GET /v1/models`。
- 伺服器連不上或出錯時，v2c 會自動退回這台電腦自己的地端模型（`cloud_fallback_to_local = true`，預設開啟）。

## 2. 選方案

| 角色 | 方案 | 硬體建議 | 預估延遲（一句 5～10 秒的話） | 適用 |
|------|------|----------|------------------------------|------|
| 語音辨識 | **A. CPU** | 8 核以上（近 5 年的 Ryzen 7／Core i7）、記憶體 8 GB 以上 | 約 3～10 秒（估計） | 只用潤飾、或不在意延遲；建議辨識仍留在 Windows 本機 |
| 語音辨識 | **B. GPU** | NVIDIA，VRAM 4 GB 以上（Breeze q5_0 約 1.8 GB，**實測於 RTX 2060**） | 約 0.6～1 秒（RTX 2060 實測辨識 0.58 秒，另加網路約 0.05 秒） | 多台電腦共用、本機沒有好顯示卡 |
| 文字模型 | **C. CPU** | 8 核以上、記憶體 8 GB 以上 | 潤飾一句約 1.5～4 秒（3B 模型，估計） | 潤飾可以慢一點 |
| 文字模型 | **D. GPU** | NVIDIA，VRAM 8 GB 以上（7B～9B）；4 GB 可跑 3B | 潤飾一句約 0.3～1 秒（估計） | 日常使用 |

常見組合：

- **一台 GPU 主機（建議）**：B＋D，同一張卡。Breeze q5_0（約 1.8 GB）＋ 7B Q4（約 5～6 GB），VRAM 12 GB 的卡放得下；8 GB 的卡改用 3B 模型。
- **一台沒有顯示卡的主機**：只做 C（潤飾、中翻英），辨識留在各台 Windows 本機（Breeze 或 SenseVoice）。不建議用 A，太慢。

## 3. 共同準備（兩種方案都要做）

以 **Ubuntu 24.04 LTS** 為例（22.04 指令相同）。

```bash
sudo apt update
sudo apt install -y build-essential cmake git curl ufw
```

### 3.1 固定伺服器 IP

讓伺服器每次開機都用同一個 IP，v2c 才不用改設定。做法二選一：

- 在路由器的 DHCP 設定裡，把這台主機的 MAC 位址綁定固定 IP（建議）。
- 或在 Ubuntu 設定固定 IP（「設定 → 網路 → IPv4 → 手動」）。

查目前 IP：`hostname -I`。下面的例子都假設是 `192.168.1.50`。

### 3.2 建立服務用的帳號與資料夾

```bash
sudo useradd --system --create-home --home-dir /opt/v2c --shell /usr/sbin/nologin v2c
sudo mkdir -p /opt/v2c/models
sudo chown -R v2c:v2c /opt/v2c
```

## 4. 語音辨識：Breeze-ASR-25（whisper-server）

### 4.1 取得 Breeze GGML 模型檔

二選一：

- **從 Windows 複製**（已照 [download-guide.md](download-guide.md) 轉好檔案時）：在 Windows PowerShell 執行
  ```powershell
  scp "$env:LOCALAPPDATA\v2c\models\ggml-breeze-asr-25-q5_0.bin" 你的帳號@192.168.1.50:/tmp/
  ```
  再到 Ubuntu 上：`sudo mv /tmp/ggml-breeze-asr-25-q5_0.bin /opt/v2c/models/ && sudo chown v2c:v2c /opt/v2c/models/*`
- **在 Ubuntu 上自己轉檔**：見 4.5 節。

核對檔案（應和 download-guide 的表格一致）：

```bash
sha256sum /opt/v2c/models/ggml-breeze-asr-25-q5_0.bin
# 9be65cda66ee424a261cae9e9864e5aceb2f1be63cc01e5b670e57d022fdf335
```

### 4.2 方案 A：CPU 版 whisper-server

```bash
cd /opt/v2c
sudo -u v2c git clone --depth 1 --branch v1.8.3 https://github.com/ggml-org/whisper.cpp
cd whisper.cpp
sudo -u v2c cmake -B build -DCMAKE_BUILD_TYPE=Release
sudo -u v2c cmake --build build -j"$(nproc)" --config Release --target whisper-server
ls build/bin/whisper-server
```

用 v1.8.3，是因為它和 v2c 內建的 whisper.cpp 版本相同，Breeze 的轉檔也是用這一版驗證的。

### 4.3 方案 B：NVIDIA GPU 版 whisper-server

1. 安裝顯示卡驅動，重開機後確認看得到顯示卡：
   ```bash
   sudo ubuntu-drivers install
   sudo reboot
   nvidia-smi
   ```
2. 安裝 CUDA Toolkit（編譯用）。最簡單是 Ubuntu 套件庫的版本：
   ```bash
   sudo apt install -y nvidia-cuda-toolkit
   nvcc --version
   ```
   想要較新的 CUDA，改用 NVIDIA 官方套件庫：<https://developer.nvidia.com/cuda-downloads>（選 Linux → x86_64 → Ubuntu → deb (network)）。
3. 編譯（`CMAKE_CUDA_ARCHITECTURES` 換成你的顯示卡架構：RTX 20 系列＝75、RTX 30＝86、RTX 40＝89；查詢：`nvidia-smi --query-gpu=compute_cap --format=csv`，例如 7.5 就填 75）：
   ```bash
   cd /opt/v2c
   sudo -u v2c git clone --depth 1 --branch v1.8.3 https://github.com/ggml-org/whisper.cpp
   cd whisper.cpp
   sudo -u v2c cmake -B build -DCMAKE_BUILD_TYPE=Release -DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=75
   sudo -u v2c cmake --build build -j"$(nproc)" --config Release --target whisper-server
   ```
   第一次編譯 CUDA 核心約 10～30 分鐘。

### 4.4 啟動與開機自動執行

先手動試跑（兩種方案指令相同；CPU 版的 `-t` 設成實體核心數）：

```bash
sudo -u v2c /opt/v2c/whisper.cpp/build/bin/whisper-server \
  -m /opt/v2c/models/ggml-breeze-asr-25-q5_0.bin \
  --host 0.0.0.0 --port 8080 \
  --inference-path /v1/audio/transcriptions \
  -l zh -t 8
```

- `--inference-path /v1/audio/transcriptions`：讓網址和 OpenAI 一樣，v2c 才找得到。
- `-l zh`：固定用中文解碼。v2c 本機版也是這樣設定：實測 Breeze 的語言偵測會把純英文判成中文，而用中文解碼時英文字會照原樣保留、不會被翻譯，固定語言還能省下偵測的約 0.3 秒。
- 看到 `whisper server listening at http://0.0.0.0:8080` 就是成功了，按 Ctrl+C 結束。

改成開機自動執行（systemd）：

```bash
sudo tee /etc/systemd/system/v2c-asr.service > /dev/null <<'EOF'
[Unit]
Description=v2c speech recognition (whisper.cpp server, Breeze-ASR-25)
After=network-online.target
Wants=network-online.target

[Service]
User=v2c
ExecStart=/opt/v2c/whisper.cpp/build/bin/whisper-server -m /opt/v2c/models/ggml-breeze-asr-25-q5_0.bin --host 0.0.0.0 --port 8080 --inference-path /v1/audio/transcriptions -l zh -t 8
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now v2c-asr
systemctl status v2c-asr --no-pager
```

看日誌：`journalctl -u v2c-asr -f`。注意：whisper-server 自己的日誌可能包含辨識出的文字；若伺服器多人共用，請限制日誌的讀取權限，或用 `StandardOutput=null` 關掉輸出。

### 4.5 在 Ubuntu 上自己轉 Breeze 模型（選用）

不想從 Windows 複製時才需要。約需 12 GB 磁碟、Python 3.10 以上：

```bash
sudo apt install -y python3-venv
mkdir -p ~/breeze && cd ~/breeze
python3 -m venv .venv && . .venv/bin/activate
pip install "transformers<5" torch --index-url https://download.pytorch.org/whl/cpu
pip install safetensors numpy huggingface_hub
# 只下載轉檔需要的檔案（不下載約 13 GB 的 optimizer 與 .pt 檢查點）
hf download MediaTek-Research/Breeze-ASR-25 \
  --include "*.json" "*.txt" "model.safetensors" --local-dir Breeze-ASR-25
git clone --depth 1 --branch v1.8.3 https://github.com/ggml-org/whisper.cpp
git clone --depth 1 https://github.com/openai/whisper openai-whisper
python whisper.cpp/models/convert-h5-to-ggml.py Breeze-ASR-25 openai-whisper .
cmake -S whisper.cpp -B whisper.cpp/build -DCMAKE_BUILD_TYPE=Release
cmake --build whisper.cpp/build -j"$(nproc)" --target whisper-quantize
whisper.cpp/build/bin/whisper-quantize ggml-model.bin ggml-breeze-asr-25-q5_0.bin q5_0
sha256sum ggml-breeze-asr-25-q5_0.bin
```

步驟和 Windows 的 `tools/convert-breeze/convert-breeze.ps1` 相同，細節（為什麼限定 transformers 4.x 等）見 [tools/convert-breeze/README.md](../tools/convert-breeze/README.md)。

## 5. 文字模型：Qwen／GLM

### 5.1 選模型

| 模型 | Ollama 名稱 | 下載大小（Q4） | 方案 | 說明 |
|------|-------------|----------------|------|------|
| Qwen2.5 3B Instruct | `qwen2.5:3b` | 約 1.9 GB | C（CPU）、D（4 GB VRAM） | 最輕；中文潤飾夠用 |
| Qwen2.5 7B Instruct | `qwen2.5:7b` | 約 4.7 GB | D（8 GB VRAM 以上） | **建議**：中英翻譯明顯較好 |
| GLM-4 9B Chat | `glm4:9b` | 約 5.5 GB | D（8 GB VRAM 以上） | 中文能力好，可和 Qwen 比較後擇一 |

注意：

- **選「Instruct／Chat」版本**，不要選 Base 版。
- **不要選會先「思考」的模型**（例如預設開啟思考模式的 Qwen3）。v2c 會把 `<think>…</think>` 去掉，但思考要多花好幾秒，常常超過潤飾的時間上限，結果每句都退回基本整理。真的要用，請選該模型的「instruct／非思考」版本。
- 新模型推出很快，上表是 2026-11 確認過、在 Ollama 上有的名稱；換新模型時，用第 7 節的方法實測延遲。

### 5.2 方案 C／D：Ollama（CPU 與 GPU 共用同一套安裝）

Ollama 會自動偵測 NVIDIA 顯示卡：有裝驅動（4.3 節的第 1 步）就用 GPU（方案 D），沒有就用 CPU（方案 C），**不必另外編譯**。

1. 安裝（官方腳本，會建立 `ollama` 系統服務）。建議先下載下來讀過再執行：
   ```bash
   curl -fsSL https://ollama.com/install.sh -o ollama-install.sh
   less ollama-install.sh
   sh ollama-install.sh
   ```
2. 讓區網連得到，並讓模型保持載入（避免每次閒置後第一句要多等幾秒載入）：
   ```bash
   sudo systemctl edit ollama
   ```
   在編輯器的空白處貼上，存檔離開：
   ```ini
   [Service]
   Environment="OLLAMA_HOST=0.0.0.0:11434"
   Environment="OLLAMA_KEEP_ALIVE=30m"
   ```
   然後：
   ```bash
   sudo systemctl daemon-reload
   sudo systemctl restart ollama
   ```
3. 下載模型（依 5.1 節選一個）：
   ```bash
   ollama pull qwen2.5:7b      # GPU 8 GB 以上
   ollama pull qwen2.5:3b      # CPU 或小顯卡
   ollama pull glm4:9b         # 想試 GLM 時
   ollama list
   ```
4. 確認用的是 GPU 還是 CPU：先 `ollama run qwen2.5:7b "你好"`，再執行 `ollama ps`，`PROCESSOR` 欄顯示 `100% GPU` 就是全部在顯示卡上。

OpenAI 相容網址是 `http://192.168.1.50:11434/v1`，模型名稱就是 `ollama list` 顯示的名稱（例如 `qwen2.5:7b`）。

### 5.3 方案 D 的另一種選擇：vLLM（GPU，追求速度或多人共用）

vLLM 同時處理多人請求的效率比 Ollama 好，但只支援 NVIDIA GPU，設定也較多。需要 Python 3.10 以上與驅動（不需另外裝 CUDA Toolkit）：

```bash
sudo apt install -y python3-venv
sudo -u v2c python3 -m venv /opt/v2c/vllm
sudo -u v2c /opt/v2c/vllm/bin/pip install vllm
sudo -u v2c /opt/v2c/vllm/bin/vllm serve Qwen/Qwen2.5-7B-Instruct-AWQ \
  --host 0.0.0.0 --port 8000 \
  --max-model-len 4096 --gpu-memory-utilization 0.6 \
  --api-key 換成你自己的一串密碼
```

- `--gpu-memory-utilization 0.6`：vLLM 預設會佔掉 90% 的 VRAM；和 whisper-server 共用同一張卡時一定要調低，留 2 GB 以上給 Breeze。
- `--api-key`：vLLM 支援簡單的金鑰檢查，在 v2c 精靈的「金鑰」欄貼上同一串即可。
- AWQ 是 4-bit 量化版，7B 約佔 5～6 GB。GLM 可改用 `zai-org/GLM-4-9B-0414` 之類的模型（請先到 Hugging Face 確認名稱、授權與 vLLM 支援狀況）。
- 開機自動執行：仿照 4.4 節寫一個 `v2c-llm.service`，把 `ExecStart` 換成上面的 `vllm serve …` 指令。

## 6. 在 v2c 加入區網模型（Windows 端）

每台要用的 Windows 電腦都做一次。

### 6.1 用精靈加入（建議）

1. 打開 v2c 的「設定 → 雲端服務 → 新增雲端服務」。
2. **語音辨識服務**：
   - 服務：「自訂相容端點」
   - 端點網址：`http://192.168.1.50:8080/v1`（要包含 `/v1`）
   - 金鑰：留空（whisper-server 沒有金鑰檢查）
   - 語音辨識模型：填 `breeze`（whisper-server 不看這個名稱，但欄位不能空白）
   - 語音辨識逾時：伺服器每句要超過 5 秒時按「Breeze-ASR-25 建議 8000」（見下方 6.2 節的說明）；CPU 版辨識（方案 A）可填 15000
   - 文字模型：留空
   - 測試連線 → 用途勾「語音辨識」→ 儲存
3. **文字模型服務**：
   - 服務：「自訂相容端點」
   - 端點網址：`http://192.168.1.50:11434/v1`（vLLM 則是 `:8000/v1`）
   - 金鑰：Ollama 留空；vLLM 貼 `--api-key` 的那串
   - 文字模型：`qwen2.5:7b`（從下拉清單選，或照 `ollama list` 輸入）
   - 測試連線 → 用途勾「潤飾」與「中翻英」→ 儲存

精靈每一步的說明見 [cloud-setup.md](cloud-setup.md)。

### 6.2 手動改設定檔（等效做法）

`%APPDATA%\v2c\config.toml`（改完存檔後，從系統列結束 v2c 再重新開啟才會生效；v2c 不會自動偵測手動修改。在設定頁或精靈裡改的則立刻生效）：

```toml
[route]
asr = "lan-asr"
polish = "lan-llm"
translate = "lan-llm"
cloud_fallback_to_local = true   # 伺服器沒開時退回本機

[[cloud]]
id = "lan-asr"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:8080/v1"
asr_model = "breeze"
asr_timeout_ms = 8000  # 只給這個服務：區網 Breeze-ASR-25 每句約 5.5～6.3 秒；不填就用 [timeouts] asr_cloud_ms

[[cloud]]
id = "lan-llm"
kind = "open_ai_compat"
base_url = "http://192.168.1.50:11434/v1"
llm_model = "qwen2.5:7b"

[timeouts]
asr_cloud_ms = 5000    # 沒設 asr_timeout_ms 的雲端服務都用這個值；建議改在慢的那個服務設 asr_timeout_ms
llm_cloud_ms = 3000    # 會先「思考」的模型或 CPU 版文字模型（方案 C）建議 6000

[polish]
timeout_ms = 1000      # 潤飾的基本時間上限（依字數放寬，最多到 llm_cloud_ms）；伺服器較慢時調到 5000
```

注意：潤飾有自己的時間上限 `[polish] timeout_ms`，只調 `llm_cloud_ms` 不夠。中翻英只看 `llm_cloud_ms`。

**每個服務各自的語音辨識逾時**（`[[cloud]] asr_timeout_ms`）：有設就用它，沒設才用 `[timeouts] asr_cloud_ms`。範圍 1000～60000 毫秒。精靈第 4 步的「語音辨識逾時（毫秒，選填）」寫的就是這個欄位，並提供快選：

| 模型 | 建議值 | 依據 |
|------|--------|------|
| Breeze-ASR-25 | 8000 | 2026-11-11 實測區網 `:8002`：每句 asr_ms 約 5.5～6.3 秒（連 2 秒短句也要 5.5 秒），6000 會偶爾逾時並退回本機 |
| TEA-ASR | 8000 | 暫定，之後依日誌 `asr_ms` 實測調整 |

只放寬慢的那個服務，其他雲端服務不受影響；精靈的「測試連線」也用同一個值。

實例（2026-11-10 實測一台區網伺服器）：語音辨識每句固定約 5.6 秒、會思考的 27B 文字模型約 3～4 秒，預設上限下兩者都逾時；改成語音辨識服務的 `asr_timeout_ms = 8000`（當時是全域 `asr_cloud_ms = 8000`）、`llm_cloud_ms = 6000`、`[polish] timeout_ms = 5000` 後即可使用（較慢，但不會退回本機）。

需要金鑰的服務（例如 vLLM）請用精靈貼金鑰：金鑰只存在 Windows 認證管理員，不會寫進這個檔案。

### 6.3 另一種接法：只把文字模型換成區網的

如果只想讓「地端潤飾」改用區網伺服器，也可以不新增服務，直接改 `[local]`：

```toml
[local]
llm_base_url = "http://192.168.1.50:11434/v1"
llm_model = "qwen2.5:7b"
```

這樣設定時，v2c 把區網伺服器當成「本機的 LM Studio」：每 10 秒檢查一次是否開著，沒開就只做規則式整理。差別是地端潤飾的時間上限較短（`[polish] timeout_ms`，預設 1000 毫秒，會依字數放寬）；CPU 方案請把它調到 3000 以上。

## 7. 驗證

在 **Windows** 的 PowerShell 測試（換成你的 IP）：

```powershell
# 語音辨識：用任何一個 16 kHz WAV（例如 tests\fixtures\audio 裡你自己的錄音）
curl.exe http://192.168.1.50:8080/v1/audio/transcriptions -F "file=@tests\fixtures\audio\mix-01.wav" -F model=breeze -F response_format=json

# 文字模型：列出模型、送一句測試
curl.exe http://192.168.1.50:11434/v1/models
curl.exe http://192.168.1.50:11434/v1/chat/completions -H "Content-Type: application/json" -d "{\"model\":\"qwen2.5:7b\",\"messages\":[{\"role\":\"user\",\"content\":\"把這句改成通順的繁體中文：嗯那個我們明天開會\"}]}"
```

在 v2c 裡驗證：

1. 「試試看」頁面按住右 Ctrl 說一句話，看是否正常出字。
2. 看延遲：v2c 的日誌 `%APPDATA%\v2c\logs\` 裡每句會記一行 `Timings`，包含 `asr_ms`、`llm_ms`、`latency_ms`（只記時間與字數，不記內容）。
3. 測退回：在伺服器暫停服務（`sudo systemctl stop v2c-asr`），再說一句話，浮動標籤應顯示「雲端服務失敗，已改用地端」，文字仍然會出現。測完記得 `sudo systemctl start v2c-asr`。

## 8. 安全與隱私

- **只開放給區網**：用防火牆限制只有你的電腦能連（把 `192.168.1.0/24` 換成你的網段，或單一 IP）：
  ```bash
  sudo ufw allow OpenSSH
  sudo ufw allow from 192.168.1.0/24 to any port 8080 proto tcp
  sudo ufw allow from 192.168.1.0/24 to any port 11434 proto tcp
  sudo ufw enable
  sudo ufw status
  ```
  不要在路由器上把這些連接埠轉發到網際網路：whisper-server 和 Ollama 都沒有登入機制。
- **區網內是未加密的 HTTP**：你的語音與文字會以明碼在區網傳送。家用網路通常可以接受；在公司或公共網路請改用 VPN（例如 WireGuard、Tailscale），把端點網址換成 VPN 內的 IP。
- **伺服器端的日誌**：v2c 自己的日誌永遠不記辨識文字，但伺服器軟體可能會記。多人共用時請限制伺服器日誌的權限。

## 9. 交給其他人：要提供的檔案

要讓別人照這份手冊自己架一台，給他們：

| 項目 | 怎麼給 |
|------|--------|
| 這份手冊 | `docs/lan-server-guide.md`（或整個 GitHub repo 的連結） |
| Breeze GGML 模型檔 | `ggml-breeze-asr-25-q5_0.bin`（約 1.08 GB）。用隨身碟、NAS 或你自己的 Hugging Face repo 傳；附上 SHA-256 讓對方核對。Breeze-ASR-25 是 Apache-2.0 授權，轉發時請一併附上原始授權與來源說明（MediaTek-Research/Breeze-ASR-25） |
| 文字模型 | 不用傳檔案：對方用 `ollama pull` 自己下載 |
| v2c 安裝檔 | 照 [build-guide.md](build-guide.md) 建置的 `v2c_<版本>_x64-setup.exe` |
| 你的設定（選用） | 第 6.2 節的 `[route]`、`[[cloud]]` 段落，請對方把 IP 換成自己的伺服器 |

**不要**給：你的 `config.toml` 整份（裡面有你的個人設定）、`history.db`（加密的輸入紀錄）、你的錄音檔、任何 API 金鑰。

## 10. 疑難排解

| 狀況 | 原因與處理 |
|------|------------|
| 精靈顯示「網路不通」 | 伺服器沒開、IP 錯、防火牆擋住。在 Windows 用 `Test-NetConnection 192.168.1.50 -Port 8080` 檢查；在伺服器用 `ss -ltnp` 確認服務聽在 `0.0.0.0` 而不是 `127.0.0.1` |
| 「這個端點不支援語音辨識」 | whisper-server 少了 `--inference-path /v1/audio/transcriptions`，或網址少了 `/v1` |
| 「逾時：服務回應太慢」 | CPU 方案常見。語音辨識：在精靈修改該服務的「語音辨識逾時」（`asr_timeout_ms`）；文字模型：調高 `[timeouts] llm_cloud_ms`；或改用 GPU 方案、較小的模型 |
| 每句都顯示「潤飾逾時，已改用基本整理」 | 文字模型太慢：換小一號的模型、確認 `ollama ps` 是 GPU、設 `OLLAMA_KEEP_ALIVE` 讓模型保持載入，或改用不會思考的模型 |
| 辨識出簡體字 | v2c 會在本機把辨識結果轉成台灣繁體，畫面上不應出現簡體；若仍出現，請回報 |
| 英文被翻成中文 | 確認 whisper-server 的啟動參數沒有 `--translate`（`-tr`）；語言用 `-l zh` 即可，英文字會照原樣保留 |
| `nvidia-smi` 找不到顯示卡 | 驅動沒裝好。`sudo ubuntu-drivers install` 後重開機；開機畫面有 Secure Boot 的 MOK 註冊時要照指示完成 |
| whisper-server 編譯時找不到 CUDA | 確認 `nvcc --version` 有輸出；用官方套件庫安裝時要把 `/usr/local/cuda/bin` 加進 PATH |
| GPU 記憶體不夠，模型載入失敗 | 兩個服務共用一張卡時：Breeze 改用 q5_0、文字模型改小，或 vLLM 調低 `--gpu-memory-utilization` |
