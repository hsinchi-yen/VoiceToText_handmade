# 地端模型設定：顯示卡記憶體（VRAM）

v2c 預設全部在你自己的電腦上執行：語音辨識用 Breeze-ASR-25（NVIDIA 顯示卡）或 SenseVoice-Small（CPU），潤飾用你自己開的 LM Studio。這份文件說明 v2c 怎麼和畫面輸出、遊戲、剪片軟體分享顯示卡記憶體，以及 LM Studio 和 NVIDIA 驅動的建議設定。

> **注意：本文的 VRAM 數字都是估計值**，要等 T0.1 用 `nvidia-smi` 實測後回填（見 `tasks/spike-results.md`）。實測前請以設定頁的即時使用條為準。

## 三種方案

在設定檔 `[gpu] profile`（或設定頁的「顯示卡」）切換：

| 方案 | 設定值 | Breeze-ASR-25 | LM Studio 語言模型 | 預估 VRAM | 適用情境 |
|------|--------|---------------|--------------------|-----------|----------|
| **均衡（預設）** | `balanced` | GPU，q5_0（約 1.5 GB） | GPU，3B Q4_K_M、context 2048（約 2.3 GB） | 約 3.8 GB | 日常文書、瀏覽、開會 |
| 省顯存 | `low_vram` | GPU，q5_0（約 1.5 GB） | 1.5B 模型，或 CPU 執行（0～1.2 GB） | 1.5～2.7 GB | 同時開較多吃顯卡的程式；潤飾慢約 1～2 秒 |
| 不用顯卡 | `cpu_only` | CPU，SenseVoice-Small | CPU 執行或改用雲端 | 0 | 玩遊戲、剪片、3D 繪圖 |

- `[local] breeze_quant = "q8_0"` 辨識稍準，但多用約 0.6 GB（Breeze 約 2.1 GB）。
- 「均衡」與「省顯存」對 v2c 本身沒有差別（都用 GPU 跑 Breeze）；差別在你替 LM Studio 選的模型與 GPU offload，見下方 LM Studio 一節。
- 預算原則：保留約 **600 MB** 給畫面輸出與其他程式，v2c＋LM Studio 合計約 **4 GB** 以內（以 6 GB 的 RTX 2060 為例；實測 Breeze q5_0 約 1.7 GB、LM Studio 3B（context 8192）約 2.4 GB，兩者同時載入後剩約 860 MB）。
- 如果拖曳視窗或播影片時畫面卡頓，把 `vram_reserve_mb` 調高（例如 1024）；代價是開著 LM Studio 時 Breeze 可能放不下而改用 CPU。
- **啟動順序**：先開 v2c（Breeze 先載入）、再在 LM Studio 載入模型，兩者就能同時在 GPU 上。反過來的話，Breeze 載入時要多一段約 256 MB 的緩衝，可能差一點放不下而先用 CPU，直到可用量足夠。

## v2c 怎麼保護畫面用的顯存

v2c 每 5 秒讀一次顯示卡的可用記憶體（透過 NVIDIA 驅動附的 NVML；沒有 NVIDIA 顯示卡時這些功能自動停用，直接用 CPU）。

1. **載入前檢查**：可用 VRAM 扣掉保留額度（`vram_reserve_mb`，預設 600 MB）後，要放得下 Breeze 再加約 256 MB 緩衝才會載入，否則改用 SenseVoice（CPU），浮動標籤提示一次。Breeze 實際用多少，v2c 會在每次載入時自己量（載入前後的可用量差），之後以實測值判斷，不只靠上表的估計值。
2. **執行中監看**：Breeze 載入後，連續兩次讀到可用 VRAM 低於保留額度（兩次讀取相隔 5 秒，所以從變低到卸載約 5～10 秒），就卸載 Breeze 改用 CPU；正在辨識的那句話會先做完。偶爾讀不到顯示卡資料不會觸發卸載。可用量恢復並穩定約 20 秒後自動載回；如果剛載回不久又因顯存不足被卸載，等待時間會加倍（最長 10 分鐘），避免反覆載入、卸載。
3. **閒置卸載**：超過 `idle_unload_minutes`（預設 15 分鐘）沒有使用就卸載；下次按熱鍵時重新載入，第一句會多 1～2 秒。設成 0 表示不因閒置卸載。
4. **重度程式自動讓路**：前景程式的執行檔在 `heavy_apps` 名單內，或前景是全螢幕程式（包含獨佔全螢幕的遊戲與簡報模式，`yield_on_fullscreen = true` 時），立刻改用 CPU；離開這些程式約 20 秒後恢復。注意：全螢幕看影片（瀏覽器按 F11）也算全螢幕；不想這樣可把 `yield_on_fullscreen` 設成 `false`，只靠 `heavy_apps` 名單。
5. **whisper.cpp 參數**：預設開啟 flash attention、一次只處理一段，壓低辨識時的暫存記憶體。

`keep_loaded` 的三種值：

| 值 | 行為 |
|----|------|
| `auto`（預設） | 啟動時載入；上面 1～4 全部生效 |
| `always` | 一旦載入就常駐：不因閒置、低顯存或重度程式卸載（載入前仍會檢查）。適合顯存很充裕、想要每句都最快的人 |
| `on_demand` | 不預先載入；按熱鍵才載入，閒置約 1 分鐘就卸載。最省顯存，但幾乎每句都多 1～2 秒 |

設定範例：

```toml
[gpu]
enabled = true
profile = "balanced"          # balanced | low_vram | cpu_only
vram_reserve_mb = 600         # 保留給畫面輸出與其他程式
keep_loaded = "auto"          # always | auto | on_demand
idle_unload_minutes = 15
yield_on_fullscreen = true
heavy_apps = ["blender.exe", "Adobe Premiere Pro.exe"]

[local]
breeze_quant = "q5_0"         # q5_0 | q8_0
```

`heavy_apps` 填執行檔名稱（不分大小寫），可在工作管理員 →「詳細資料」頁籤查到，例如 `blender.exe`、`DaVinci Resolve`（`Resolve.exe`）、各遊戲的 `.exe`。

## LM Studio 建議設定

以下由你在 LM Studio 裡自行設定（v2c 不會改 LM Studio）。名稱以 LM Studio 0.3.x 介面為準，新版可能略有不同。

| 項目 | 位置 | 均衡 | 省顯存 |
|------|------|------|--------|
| 模型 | 下載模型時選 | 3B～4B，**Q4_K_M** 量化 | 1.5B，Q4_K_M |
| Context Length | 載入模型時的設定 | **2048**（潤飾的句子很短） | 2048 |
| GPU Offload | 載入模型時的設定 | 拉到最大（全部層數上 GPU） | 調低層數，或設 0（全部 CPU） |
| Flash Attention | 載入設定 → 進階（Show Advanced） | 開 | 開 |
| K / V Cache Quantization Type | 載入設定 → 進階（需先開 Flash Attention） | **Q8_0** | Q8_0 |
| JIT 載入 | Developer 分頁 → Server Settings | 開（有請求時才載入模型） | 開 |
| Idle TTL | Developer 分頁 → Server Settings（JIT 載入的模型預設 60 分鐘） | 改成 **15 分鐘**，和 v2c 的閒置卸載一致 | 15 分鐘 |
| Auto-Evict | Developer 分頁 → Server Settings | 開（JIT 載入時只保留最後一個模型） | 開 |

說明：
- Idle TTL 與 Auto-Evict 只對「JIT 載入」的模型有效；你手動在 LM Studio 載入的模型不會被自動卸載。想讓 LM Studio 閒置時也釋放顯存，就不要手動預先載入，讓 v2c 第一次潤飾時觸發載入。
- 「不用顯卡」方案時，請把 LM Studio 的 GPU Offload 設 0，或在 v2c 改用雲端潤飾。

## NVIDIA 控制面板：不要偷用系統記憶體

顯存不夠時，新版 NVIDIA 驅動預設會把 CUDA 的資料搬到系統記憶體，結果整台電腦和畫面一起變慢。建議只針對 v2c 和 LM Studio 關掉這個行為：

1. 在桌面按右鍵 →「NVIDIA 控制面板」→「管理 3D 設定」→「程式設定」分頁。
2. 「選取要自訂的程式」→「新增」，加入 v2c 的執行檔（`v2c.exe`），再加入 LM Studio（`LM Studio.exe`）。
3. 找到 **「CUDA - Sysmem Fallback Policy」（CUDA - 系統記憶體備援原則）**，設為 **「Prefer No Sysmem Fallback」（偏好無系統記憶體備援）**，按「套用」。
4. **不要**改「全域設定」分頁，以免影響遊戲和其他程式。

這樣顯存不足時程式會直接報錯，由 v2c 改用 CPU，而不是讓整台電腦變慢。系統記憶體備援是 536.40 版驅動開始的行為，這個開關則在 546.01 之後的驅動才有；看不到就先更新驅動。

## Windows 圖形設定：介面不要用 NVIDIA

v2c 的設定視窗與浮動標籤是網頁介面（WebView2），不需要 NVIDIA 顯示卡：

1. 「設定」→「系統」→「顯示器」→「圖形」。
2. 新增應用程式 → 瀏覽，選 v2c 的執行檔 → 「選項」→ **「省電」**（會顯示 AMD 內顯的名稱）→ 儲存。
3. WebView2 的繪圖實際在 `msedgewebview2.exe` 裡進行。如果設定後 `nvidia-smi` 仍看得到 `msedgewebview2.exe`，也把它加入並設為「省電」：路徑在 `C:\Program Files (x86)\Microsoft\EdgeWebView\Application\<版本>\msedgewebview2.exe`（WebView2 更新後版本資料夾會變，需要重設）。

辨識引擎（Breeze／CUDA）不受這個設定影響，仍然使用 NVIDIA 顯示卡。

## 讓 NVIDIA 完全留給模型：MUX 開關與 Advanced Optimus

目前這台筆電的 RTX 2060 同時負責 2560×1440 的畫面輸出。若機型支援切換畫面輸出的顯示卡：

- **MUX 開關**（通常在廠商的控制軟體，例如 Armoury Crate、Lenovo Vantage 的「GPU 模式／混合模式」）：選「混合模式／Optimus」，內建螢幕由 AMD 內顯輸出，NVIDIA 只做運算。
- **Advanced Optimus**：驅動會自動切換，一般不用設定。

切換後通常要重新開機。外接螢幕接在哪個連接埠，決定由哪張顯示卡輸出（有些機型的 HDMI 直連 NVIDIA）。是否支援要看機型，T0.1 時會順便確認。

## 已知影響

- 辨識的那 1 秒左右顯示卡滿載，玩遊戲時可能短暫掉幀；所以遊戲時會自動讓路（上面第 4 點），或直接選「不用顯卡」方案。
- 從 CPU 切回 GPU、或閒置卸載後的第一句，會多 1～2 秒載入時間。
