# 🧑 需要你親自做的事

agent 做完自己的部分後會在這裡新增項目，然後停下來等你。完成後打勾，並把結果告訴主 agent（或寫在項目下方）。

## 開始前
- [x] 檢查並執行 `tools/setup-dev.ps1`（T0.0 產出）——2026-11-09 完成，workspace 已可編譯
  - 2026-11-08 檢查本機：缺 Rust、缺 MSVC C++ Build Tools、缺 LLVM；CUDA 只有 11.0（太舊，腳本會並列安裝 12 以上）；Node.js v24、Git、Python 已有。
  - 以「系統管理員」身分開 PowerShell，在專案根目錄執行：`powershell -ExecutionPolicy Bypass -File tools\setup-dev.ps1`
  - 裝完後**開新的終端機**，執行 `cargo test --workspace` 確認可以編譯
- [x] 同意建立第一個 git commit（agent 的 worktree 需要它）——2026-11-09 同意
- [ ] 依 `tests/fixtures/audio/README.md` 錄製 10 段測試音檔
- [x] 審查 T0.0 的核心介面並核准合併——2026-11-09 核准

## 建議的系統設定（一次性）
- [ ] 系統環境變數 `CUDA_PATH` 目前指向 v11.0：改成 `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5`，並新增 `CUDA_PATH_V13_5`（同值）。GPU 版編譯需要。
  - **同時修正 `Path`**（2026-11-09 發現）：系統 `Path` 只有 `CUDA\v11.0\bin` 與 `CUDA\v11.0\libnvvp`，CMake 會抓到 v11 的 nvcc 與標頭而編譯失敗（「unsupported Microsoft Visual Studio version」）。移除這兩項，加入 `C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v13.5\bin` 與 `...\v13.5\bin\x64`（`cublas64_13.dll` 在 `bin\x64`，少了它 v2c-cli 會以代碼 53 直接結束）。改完重開終端機。
- [ ] 把 VS Build Tools 內建的 CMake 加入 PATH：`C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`
- [ ] 每次開 PowerShell 編譯前（或設成使用者環境變數）：
  ```powershell
  $env:CARGO_TARGET_DIR = "$env:USERPROFILE\.v2ct"      # 避免 Windows 路徑過長
  $env:CMAKE_CUDA_ARCHITECTURES = '75'                  # RTX 2060，大幅縮短編譯時間
  ```

## Phase 0（Phase 1 已依你同意平行開發；以下實測若失敗，agent 會回頭修）
- [x] T0.1 錄音：依 `tests/fixtures/audio/README.md` 錄 10 段 WAV（2026-11-09 完成，m4a 由 agent 轉成 WAV）
- [x] （2026-11-09 agent 代為執行，結果在 `spike-results.md`）T0.1 轉檔：`powershell -ExecutionPolicy Bypass -File tools\convert-breeze\convert-breeze.ps1`，問 `Proceed? [y/N]` 時輸入 `y`（約 20～40 分鐘、下載約 3 GB）；把印出的檔案大小與 SHA-256 填進 `tasks/spike-results.md`
- [x] （2026-11-09 agent 代為執行：q5_0 平均 860 ms、CER 2.9%，通過）T0.1 量測（設好上面的環境變數）：
  ```powershell
  cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model models\ggml-breeze-asr-25-q5_0.bin tests\fixtures\audio
  cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model models\ggml-breeze-asr-25-q8_0.bin tests\fixtures\audio
  cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model models\ggml-breeze-asr-25-q5_0.bin --lang zh tests\fixtures\audio
  ```
  記錄每段 ASR ms、CER、偵測語言；輸出是否繁體、夾雜句英文是否保留、`en-01/en-02` 是否被翻成中文。驗收：10 秒音檔 ≤ 1200 ms。
- [ ] T0.1 VRAM：另開視窗 `nvidia-smi --query-gpu=memory.used,memory.total --format=csv -l 1`，同時 `bench ... --repeat 20`；四種組合（Breeze q5/q8 × LM Studio 3B/4B）各量一次最高用量；拖曳視窗、播影片看畫面會不會卡頓
- [ ] T0.1 確認筆電能否把內建螢幕切給 AMD 內顯（MUX 開關 / Advanced Optimus）
- [ ] T0.2 `cargo run -p v2c-cli -- hotkey-test --seconds 60`，逐項確認：
  1. 按住右 Ctrl 約 1 秒再放開 → `StartRecording(Dictate)`，放開時 `StopAndProcess`
  2. 快速雙擊右 Ctrl → 只有 `StartRecording(Dictate)`；再單按一次 → `StopAndProcess`
  3. 右 Ctrl+C → `StartRecording` 後立刻 `Cancel`；並確認其他程式的複製仍正常（按鍵沒被吞）
  4. 先按住 Shift 再按住右 Ctrl 說話、放開 → `StartRecording(TranslateZhToEn)`、`StopAndProcess`
  5. 按住右 Ctrl 時按 Esc → `Cancel`；免持模式中按 Esc → `Cancel`
  6. 單擊右 Ctrl 一次 → `StartRecording(Dictate)`，約 0.3 秒後 `Cancel`
  7. （選做）免持模式等 5 分鐘 → `StopAtLimit`（`--seconds 330`）
- [ ] T0.3 每個 App 執行 `cargo run -p v2c-cli -- inject-test --delay 5 "這個 meeting 我們 delay 一下\n第二行 👍𠮷"`，5 秒內點進輸入框、手離開鍵盤：
  1. 記事本：兩行正確；CLI 印 `Notepad.exe`、`Enter`
  2. Chrome（Gmail 撰寫或 GitHub 留言框）：`ShiftEnter`，換行不送出表單
  3. LINE 聊天框：**換行不能送出訊息**，兩行留在框內（測完手動刪掉）
  4. VS Code：兩行正確、無自動完成亂字，👍𠮷 完整
  5. Word：**開注音輸入法中文模式**，文字原樣出現、不被組字攔截（其他 4 個 App 也各測一次注音開啟）
  6. 倒數結束時故意按住右 Ctrl → 顯示 `modifier keys still held after 500 ms`，沒有字被打出
  7. （選做）以系統管理員開記事本 → 顯示 `foreground window is elevated`
- [ ] 檢查點 0 核准

## Phase 1
- [ ] T1.1 桌面 App 第一次實機說話（需要先完成 Breeze 轉檔）
  1. 把 `ggml-breeze-asr-25-q5_0.bin` 放到 `%LOCALAPPDATA%\v2c\models\`（或設 `V2C_MODEL_DIR`）
  2. 設好 CUDA 環境變數（見上方「建議的系統設定」），在 `apps\desktop` 執行 `npm install`，再執行 `npm run tauri dev -- --release --features cuda`（不加 cuda 是 CPU 版，很慢，只能確認流程）
  3. 主視窗顯示「speech recognition ready」後，開記事本，按住右 Ctrl 說話、放開；中文、英文、中英夾雜各 3 句
  4. 延遲：主視窗顯示「放開到出字 X ms」，或看 `%APPDATA%\v2c\logs\v2c.log.<日期>` 的 `latency_ms`（目標 ≤ 1500）
  5. 第一個字有沒有被吃掉；說話中拔掉／切換麥克風，看是否自動恢復（主視窗顯示「麥克風已重新連線」）
- [ ] T1.10 SenseVoice（模型已下載到 `%LOCALAPPDATA%\v2c\models\sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17\`；同層的 `dl-sensevoice\sv.tar.bz2` 可刪）：錄好音檔後執行 `cargo run -p v2c-cli --release --features sensevoice -- bench --asr sensevoice --model "$env:LOCALAPPDATA\v2c\models\sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17" tests/fixtures/audio`
- [ ] T1.2 剪貼簿貼上＋還原（指令：`cargo run -p v2c-cli -- inject-test --paste "測試\n第二行"`，3 秒內切到目標視窗）
  1. `cargo test -p v2c-platform -- --ignored`（兩個實機剪貼簿測試，會短暫改動剪貼簿；應全部通過）；`inject-test` 結束時應印 `clipboard: restored`
  2. 小畫家框選並複製 → 對記事本執行指令，出現兩行 → 回小畫家 Ctrl+V，原圖仍在
  3. 先複製一段文字（再用 Word 帶格式文字試一次）→ 重複 → 原本內容與格式仍在
  4. 檔案總管複製一個檔案 → 執行 → 回檔案總管 Ctrl+V 能貼出檔案
  5. LINE 輸入框：換行不送出訊息，事後剪貼簿已還原
  6. 系統管理員身分開記事本 → 顯示 elevated 錯誤，手動 Ctrl+V 能貼出文字
  7. Win+V：測試文字不在剪貼簿歷史裡
  8. （若有 VMware/VirtualBox）不加 `--paste` 也會自動走貼上；確認 150 ms 是否足夠
- [ ] T1.3 浮動標籤＋系統列（`cd apps/desktop && npm run tauri dev`；只想看外觀可先設 `$env:V2C_OVERLAY_DEMO="1"` 自動輪播各狀態）
  1. 記事本打字到一半按住右 Ctrl 說話：記事本游標持續閃爍不失焦；標籤在主螢幕下方中央；滑鼠點標籤會穿透；標籤不在工作列、不在 Alt+Tab
  2. 各狀態外觀與時間：錄音音量條、辨識中、完成約 0.8 秒消失、錯誤停留 3 秒；不同縮放比例下也看一下
  3. 系統列：設定／輸入紀錄打開對應頁面；潤飾勾選會改 config.toml 的 `polish.enabled`；「全部改用地端」讓 `[route]` 三項變 `"local"`；「切換服務」是灰色；「結束」真的關閉
  4. 主視窗按 ✕ 只是隱藏、背景繼續執行，可從系統列再打開
  5. 冷啟動時故意讓模型缺少（或 config.toml 格式錯誤）→ 標籤顯示錯誤約 3 秒，不是空白
  6. 上一句還在處理時就開始錄下一句 → 錄音標籤不會閃到「辨識中／完成」；放開後約 0.2 秒內出現「辨識中」
  7. 若標籤完全看不見：設 `$env:V2C_OVERLAY_LAYERED_ALPHA="1"` 再啟動，確認看得見且仍可點穿、不搶焦點，並回報給 agent
  8. 在含註解的 config.toml 上用系統列切「潤飾」與「全部改用地端」→ 註解仍在；故意弄壞格式 → 顯示「設定檔格式錯誤，未變更」
  9. 免持模式下標籤顯示「免持」徽章與「按右 Ctrl 結束」，沒有被截斷
- [ ] T1.8 個人詞庫
  1. `npm run tauri dev` →「詞庫」頁輸入 5 個常用人名或產品名 → 儲存，確認 `%APPDATA%\v2c\vocab.txt` 有內容
  2. 錄一段含這些詞的音檔（可另存為 `tests/fixtures/audio/vocab-02.wav` 並寫期望文字），比較 CER：
     `cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model <breeze.bin> tests/fixtures/audio` 與加上 `--vocab "$env:APPDATA\v2c\vocab.txt"` 的結果
- [ ] T1.9 輸入紀錄
  1. 說幾句話後，用 DB Browser for SQLite 開 `%APPDATA%\v2c\history.db`：`text_sealed` 是亂碼，搜尋句子找不到
  2. 在 KeePassXC／Bitwarden 的欄位說話 → 紀錄頁沒有新增
  3. 先點記事本，再從系統列開「輸入紀錄」按「重新輸出」→ 文字打進記事本；只點過桌面時按 → 顯示「找不到要輸入的視窗」，不輸入
  4. 「複製」後貼上、「刪除」、「全部清除」（兩段確認）
- [ ] 整合後的端到端測試（`cd apps/desktop && npm run tauri dev`；量延遲用 `-- --release --features cuda`）
  1. 免持（T1.4）：雙擊右 Ctrl，連續講 1 分鐘以上，單擊結束 → 文字完整；免持中按 Esc → 不輸出
  2. 中翻英（T1.6）：先不開 LM Studio，按住右 Ctrl+Shift 說話 → 提示「中翻英需要 LM Studio 或雲端服務」；開 LM Studio 後說 5 句中文 → 檢查譯文自然、專有名詞保留
  3. LM Studio 開／關：關著時說一句 → 第一次出現「LM Studio 未啟動，已改用基本整理」；開啟約 10 秒後日誌出現 `LM Studio status up=true`，之後的句子有潤飾
  4. 系統列「全部改用地端」或「潤飾」開關 → 立即生效，不用重開
  5. 詞庫頁存檔後，下一句就用到新詞彙
  6. （有雲端金鑰時）把 route 指到雲端服務、斷網說話 → 出現「雲端失敗，已改用地端」並照常輸出
- [ ] T1.7b 雲端精靈（可選，需要你自己的金鑰）
  1. 「雲端服務」→「新增雲端服務」，照 `docs/cloud-setup.md` 走完 7 步：測試結果與延遲、末 4 碼正確；`config.toml` 有 `[[cloud]]` 與 `[route]`；Windows 認證管理員有 `<id>.v2c` 項目
  2. 故意貼錯金鑰 → 顯示「金鑰錯誤…」
  3. 在列表按「移除」→ route 改回 local、認證管理員項目被刪除
  4. 系統列「全部改用地端」→ 雲端服務頁自動更新為「未使用」
  5. 說明文件看得懂
- [ ] T1.10b 顯示卡記憶體管理（等桌面 App 整合完成後測；以預設 `auto` 啟動，另開 `nvidia-smi -l 1` 觀察）
  1. 啟動後 Breeze 已載入
  2. Blender（在 `heavy_apps` 名單）放到前景 → 5 秒內改用 CPU、浮動標籤提示一次、顯存降約 1.5 GB
  3. 關掉 Blender → 約 20～25 秒後載回 Breeze
  4. 全螢幕遊戲或吃顯存的程式讓可用量低於 2 GB → 約 10 秒後卸載；結束後恢復
  5. 閒置 15 分鐘 → 卸載；再按熱鍵第一句稍慢
  6. profile 切到 `cpu_only` 再切回 → 立即卸載、切回後重新載入
  7. 依 `docs/local-setup.md` 設定 NVIDIA 控制面板、Windows 圖形設定、LM Studio
- [ ] 在 LM Studio 載入 3B 模型並啟動伺服器（T1.5 前）；模型的 Context Length 設 **8192**（預設 4096 時，長篇免持口述會超出而退回規則式去贅字）
- [ ] T1.5 潤飾實測：`cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model <breeze.bin> --llm lmstudio tests/fixtures/audio`（還沒錄音時 `filler-01` 會直接拿 `.raw.txt` 做純文字潤飾）
  - polished 那行沒有改變語意、沒有多出或少掉內容、問句沒被回答、「幫我寫一封信…」這類請求沒被執行
  - filler-01 應得到「我們後天早上十點見面。」；沒有簡體字
  - 每段的 `PASS/FAIL asr+polish (… per 10 s, target <= 2500)` 與摘要的 `polish verdict`
- [ ] T1.11 安裝檔與初次設定（安裝檔：`C:\Users\lance.tn\AI Project\VoiceToText_handmade\releases\v2c_0.1.3_x64-setup.exe`，2026-11-10 建置的 0.1.3（0.1.1：放開右 Ctrl 的訊號遺失時不再卡在錄音中；0.1.2：雲端服務頁可分別設定文字模型與語音辨識端點；0.1.3：相容端點關閉模型的「思考」、閒置連線被伺服器關掉時自動重送），約 439 MiB，SHA-256 `00281BED9289AD889916370703607EF083CAF7AF78DBADEAD6ECAC7C83F31B8B`，建置機 Defender 掃描無威脅；重建方式見 `docs/build-guide.md`）
  1. **授權確認**：安裝檔隨附 VC++ 執行階段（`msvcp140.dll` 等），前提是你有有效的 Visual Studio（含 Build Tools／Community）授權；確認後打勾
  2. **Windows Sandbox（模擬沒有 NVIDIA 的乾淨電腦）**：把安裝檔複製進 Sandbox 執行 → SmartScreen 按「其他資訊 → 仍要執行」→ 不需要系統管理員權限即可安裝 → 啟動時沒有缺 DLL 的錯誤 → 自動進入初次設定
     - 偵測顯示卡應顯示「沒有偵測到 NVIDIA」→ 下載 SenseVoice（約 160 MB；試暫停與繼續）→ 自動重新啟動並從「麥克風」步驟接續 → 測試麥克風（Sandbox 需先開麥克風）→ 在文字框按住右 Ctrl 說一句
     - 解除安裝後 `%APPDATA%\v2c` 仍在
  3. **實機安裝（有 NVIDIA）**：初次設定用「從本機檔案匯入」選 `ggml-breeze-asr-25-q5_0.bin` → 日誌出現 Breeze active；設定頁 VRAM 條約每 2 秒更新；打開「開機自動啟動」後重開機 → v2c 只出現在系統列、不跳出主視窗；這時再從開始功能表開 v2c → 只叫出原本那一份的視窗，說一句不會打兩次
  4. **設定頁**：改按住判定時間、潤飾開關、route 後立即生效；麥克風與輸入紀錄設定會提示需重新啟動；`config.toml` 原有註解保留
  5. **Defender**：在測試機執行 `"%ProgramFiles%\Windows Defender\MpCmdRun.exe" -Scan -ScanType 3 -File <安裝檔>`；有誤報就記下來（README 已寫處理方式）
  6. **Breeze 下載網址**：把轉好的 GGML 上傳到你的 Hugging Face repo 後，把 url、sha256、size 填進 `apps/desktop/src-tauri/models.json`（目前是 `PLACEHOLDER-HF-USER`，App 會顯示「尚未提供下載」）
  7. 要分享給 RTX 30／40 系列使用者時，告訴 agent 加入更多 CUDA 架構（目前只建 sm_75，新顯示卡第一次載入較慢）
- [ ] T1.12 TEA-ASR-1.1（用新建的 CUDA 安裝檔；`tools\build-installer.ps1` 已會設定 `/utf-8` 並檢查 `transcribe.dll` 等 DLL 有被打包）
  1. 設定頁「地端語音模型 → TEA-ASR-1.1 模型 → 下載」（約 2.18 GB；或照 `docs/local-setup.md` 手動放 `TEA-ASR-1.1-Q8_0.gguf`）
  2. 「潤飾與用途 → 語音辨識」選「地端：TEA-ASR-1.1（NVIDIA 顯示卡）」（或「地端語音模型 → 辨識引擎」選「TEA-ASR-1.1（NVIDIA 顯示卡）」），按「儲存」，不用重開；幾秒後日誌（`%APPDATA%\v2c\logs`）出現 `GPU ASR active asr="tea_asr"`
  3. 按住右 Ctrl 實際聽寫中文、英文、中英夾雜各數句；日誌每句出現 `providers for utterance ... asr="tea_asr"`；開著潤飾時有標點，關掉潤飾時照原樣（幾乎沒有標點）
  4. 辨識時畫面不卡頓（TEA 約佔 2.0～2.6 GB 顯存；開著 LM Studio 3B 時 6 GB 顯示卡可能很緊，必要時把 LM Studio 改小模型或 CPU）
  5. 全螢幕程式或 `heavy_apps` 在前景時改用 SenseVoice（浮動標籤提示），離開後約 20 秒載回 TEA
  6. 切回「自動」或 Breeze → TEA 立即卸載、改載 Breeze；切換的那幾秒內說話，浮動標籤顯示「切換模型中…」，那句話等新模型載入後才出字
  7. 同一時間只有一個模型：用 `nvidia-smi -l 1` 看，讓路時 GPU 用量降回來（SenseVoice 在 CPU）；工作管理員裡 v2c 的記憶體不會同時有兩個模型的量
- [ ] T1.14 設定頁儲存按鈕、目前使用中、地端引擎選單與測試（安裝新版後）
  1. 「潤飾與用途」的語音辨識選「地端：Breeze-ASR-25」→ 上方出現「有 1 項變更尚未儲存」（從雲端切回地端時為 2 項）、欄位有「未儲存」標示，`config.toml` 還沒變 → 按「儲存」→ `config.toml` 變成 `route.asr = "local"`、`local.asr = "breeze"`，「地端語音模型」頁同步顯示 Breeze；有雲端服務時改選雲端再切回地端也正確
  2. 「地端語音模型 → 測試」：有顯存時顯示 Breeze／NVIDIA 顯示卡、載入與辨識耗時、顯存用量；開吃顯存的程式讓可用量不足、辨識引擎選「自動」時再測 → 顯示改用 SenseVoice 與原因；按住熱鍵說話的同時按「測試」→ 顯示「正在聽寫中」
  3. 改幾項設定後按「取消變更」→ 回到已儲存的值；改了不儲存就點上方「詞庫」→ 先詢問（儲存後離開／捨棄並離開／留在設定頁）；改了不儲存就關視窗再從系統列打開 → 上方提示上次關閉時有未儲存變更
  4. 有未儲存變更時從系統列切換「潤飾」→ 設定頁提示「設定已在別處變更」，可選重新載入或保留草稿
  5. 「目前使用中」顯示的引擎與服務和日誌 `providers for utterance`（debug 等級）一致；顯存不足改用 SenseVoice 時有一句說明；設定頁沒有 LM Studio 區塊，選「地端（本機 LM Studio）」潤飾時仍走 `config.toml` 的 `[local] llm_base_url`
  6. 把視窗縮窄，欄位會換行、不會超出畫面
- [ ] T1.13 自訂相容端點別名（安裝含 T1.13 的新版後）
  1. 雲端服務頁 → 現有的 `custom` 按「重新命名」→ 第 2 步「名稱（選填）」填例如「區網 Qwen」→ 走完測試與儲存；`custom-2` 同樣取另一個名字
  2. 服務清單、「目前由誰處理」、設定頁「潤飾與用途」的下拉選單都顯示新名稱；`%APPDATA%\v2c\config.toml` 出現 `name = "…"`，`id` 仍是 `custom`／`custom-2`，原有註解還在
  3. 改名後說一句：潤飾、辨識（交給該端點的用途）照常運作；金鑰末 4 碼不變
  4. 把名稱清空再儲存 → 畫面改回顯示 `custom`，設定檔的 `name` 被移除
- [ ] T1.15 每個雲端服務各自的語音辨識逾時（安裝含 T1.15 的新版後）
  1. 雲端服務頁 → Breeze-ASR（`:8002`）按「修改」→ 第 4 步按「Breeze-ASR-25 建議 8000」→ 測試連線通過 → 儲存；服務清單出現「辨識逾時 8000 ms」，`config.toml` 該服務下有 `asr_timeout_ms = 8000`，原有註解還在
  2. 說幾句話（含 2 秒左右的短句），確認不再出現「已改用地端」；日誌 `asr_ms` 約 5.5～6.3 秒
  3. TEA-ASR 架在伺服器後：依日誌 `asr_ms` 回報實測秒數，再調整 `guides.js` 的 `asrTimeoutSuggestions`（目前暫定 8000）
- [ ] 各卡的實機驗收（見各卡「🧑 需人工」）
- [ ] 檢查點 1：日常使用一週，問題記在 `tasks/dogfood-windows.md`

## Phase 2
- [ ] Android Rust 工具（T2.3 開工前；agent 不安裝工具，請你執行）：
  ```powershell
  rustup target add aarch64-linux-android x86_64-linux-android
  cargo install cargo-ndk
  $env:ANDROID_NDK_HOME = "$env:LOCALAPPDATA\Android\Sdk\ndk\28.2.13676358"   # 建議設成使用者環境變數
  ```
  已確認存在：Android SDK（platform 36、37，build-tools 36.0.0）、NDK 28.2、JDK 17（`JAVA_HOME`）。
- [ ] 側載 APK 到手機實測；回報手機型號與 Android 版本（16 或 17）
- [ ] T2.2 退格與換行（合併後）：在 LINE、Chrome、Gmail 測退格長按、選取刪除、emoji 刪除、換行與送出；LINE 的「Enter 送出」開、關兩種都要測
- [ ] T2.4 網路情境：公司 Wi-Fi 或 VPN 下自動走 TEA；長按小圖示切 Breeze／系統／本機；關 Wi-Fi 後自動改系統辨識；區網伺服器關閉時不卡頓；中翻英輸出英文
- [ ] T2.6 本機模型：用 `tests/fixtures/audio` 的錄音在手機跑比較工具，依比較表決定預設模型；飛航模式下能輸入
- [ ] T2.5 產生 APK 簽署金鑰（`keytool`，說明見 `docs/android-install.md`）；照說明側載安裝一遍
- [ ] 檢查點 2：手機日常使用一週
- [ ] 決定 Linux 用哪台電腦（同一台雙系統或另一台），並提供規格

## Phase 3（Linux：目前執行 agent 的這台電腦，Ubuntu 22.04／GNOME 42 Wayland）

> mock 測試通過只代表模擬流程正確，**不代表**真實 GNOME Wayland 端到端可用。檢查點 2（手機）尚未通過。

- [x] 核准 T3.0（2026-11-10）：`tasks/plan.md` 第 9 節與 `tasks/cards/T3.*.md`
- [x] 決定本機的輸出路線（2026-11-10：Linux 改做 IBus 引擎）（GNOME 42 沒有 Clipboard portal，背景程式無法設定剪貼簿、也無法逐字送中文）：
  1. 升級到 Ubuntu 24.04（GNOME 46：Clipboard portal＋可記住同意的 RemoteDesktop），熱鍵仍用 evdev；或 GNOME 48 以上（Ubuntu 25.04 起）才有 GlobalShortcuts
  2. 維持 22.04，只做「辨識後複製到剪貼簿＋通知」，由你手動 Ctrl+V
  3. 改用 IBus 引擎輸出文字（與 `v2cplan.md`「非輸入法框架」的決策衝突，要先改決策）
- [x] （2026-11-10 決定不做：0.1.3 已關閉思考）核准是否在 `v2c-core` 新增「LLM 額外請求欄位」設定（給 llama.cpp 的 `chat_template_kwargs.enable_thinking=false`；開著思考一句約 2.7 秒，會超過潤飾 1 秒的逾時而退回規則式去贅字）
- [ ] evdev 熱鍵：`sudo usermod -aG input $USER`，登出再登入（這會讓同一使用者的程式都能讀鍵盤，請自行評估）
  - 本機目前**沒有接任何實體鍵盤**（`/proc/bus/input/devices` 只有電源鍵等），加入群組後還要接上實體鍵盤才能實測按住右 Ctrl
- [ ] 接上麥克風（本機目前沒有任何擷取裝置），之後才能做實機語音輸入
- [ ] Linux 建置依賴（本機缺少；Tauri 桌面 App 與 whisper-rs 需要）：`sudo apt install cmake clang libclang-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev`（Ubuntu 22.04 是否提供 `libwebkit2gtk-4.1-dev` 請在安裝時確認）
- [x] ASR：2026-11-11 決定 Linux 用遠端 `tea-asr-1`（`:8001`）＋本機 SenseVoice 備援
- [ ] T3.7 設定：在主視窗「雲端服務」頁貼上 `:8001` 語音辨識端點的金鑰（存進 GNOME 登入鑰匙圈）；確認 `~/.local/share/v2c/models/` 有 SenseVoice 模型；拔網路說一句，打開主視窗查看，應顯示「雲端服務失敗，已改用地端」且文字仍輸出
- [ ] 檢查點 3：實機 GNOME Wayland 按住說話 → 文字出現在 gedit／Firefox／終端機
- [ ] T3.1 熱鍵實機：加入 `input` 群組並接上鍵盤後，按住右 Ctrl 說話、雙擊免持、右 Ctrl+C 不誤觸、Shift 翻譯、Esc 取消；在 GNOME ≥ 48 的機器測 GlobalShortcuts（綁定的取消鍵會被系統攔截，不要選 Esc）
- [ ] T3.2 注入實機（需 GNOME ≥ 45）：貼上到 gedit／Firefox／終端機，換行用 Shift+Enter 不送出；RemoteDesktop 同意對話框按「取消」時顯示權限不足；原剪貼簿文字被還原
- [ ] T3.3 中文語音辨識：把一段自己錄的中文 WAV 放到 `tests/tmp/`，依 `docs/linux-setup.md` 第 5 節執行 `asr_real`（目前只用英文合成句驗證過）
- [ ] T3.4：依 `bash tools/build-linux.sh --check` 安裝缺少的套件後執行 `bash tools/build-linux.sh`；`apps/desktop` 尚未支援 Linux（金鑰庫只接 Windows、`build.rs` 只放 Windows DLL、controller 未接通知），預期會有編譯錯誤，需另開卡並核准 `tauri.conf.json` 的 Linux 設定
- [ ] T3.4 通知實機：`cargo test -p v2c-platform -- --ignored linux_real_notify`（會在桌面跳出幾則測試通知）
- [x] 核准合併 `t3-linux-integration` 到 `main`（2026-11-10，本機 fast-forward，未 push）（含 T3.0～T3.4；本機只合併到整合分支，沒有 push）
- [ ] T3.5：同意 agent 在你的 ibus-daemon 上動態註冊暫時的 `v2c-test` 引擎做實測（不改 GNOME 輸入來源設定，測完移除）
- [ ] T3.5 之後：「設定 → 鍵盤 → 輸入來源」加入 v2c，按 Super+Space 切換，實機按住右 Ctrl 說話
- [ ] T3.5 換行順序：在 gedit／Firefox／終端機輸出多行文字（文字與 Enter 交錯），確認不會變成「ab⏎」（IBus 的 CommitText 立即處理、ForwardKeyEvent 排隊）
- [ ] T3.5 右 Ctrl：Wayland 下 v2c 輸入來源啟用時，右 Ctrl+C 仍能複製（引擎吞掉右 Ctrl 的按下；若失效，改成不吞觸發鍵需你決定）
- [ ] T3.5（選做）在你正在用的 ibus-daemon 上做動態註冊測試：ibus 1.5.26 移除動態引擎時可能拔掉目前的全域引擎，所以 agent 只在私人 daemon 上測；要做需你本人同意
- [ ] T3.6 Secret Service 寫入測試：`V2C_SECRET_REAL_WRITE=1 cargo test -p v2c-platform -- --ignored secret_real_write`（會在 login 鑰匙圈建立再刪除 `v2c-test` 項目）
- [ ] T3.7 安裝 .deb：在 `releases/linux/` 執行 `sha256sum -c --ignore-missing SHA256SUMS.txt`，再 `sudo apt install ./v2c_0.1.6_amd64.deb`，`ibus restart`，加入「v2c 語音輸入」輸入來源，實機按住右 Ctrl 說話（含切到 v2c 時由 ibus-daemon 啟動 `v2c --ibus`）。步驟見 `docs/linux-setup.md` 第 7 節（「設定」視窗加不進輸入來源時用 7.2.1 的指令）
- [ ] T3.7（選做）AppImage：`./v2c_0.1.6_amd64.AppImage` 能否啟動（需要 libfuse2）
## Phase 4（0.2.0：介面精簡、預設組態、新圖示、新浮動標籤）
- [ ] 安裝 `releases\v2c_0.2.0_x64-setup.exe`（核對 `releases\SHA256SUMS.txt`）
- [ ] 首頁 → 套用預設組態 → TN → 選「TN TEA-ASR」→ 套用：三個端點測試連線通過，首頁「預設組態」卡顯示「TN 區網服務」；再選一次「TN Breeze-ASR」：金鑰欄顯示「已內建預設金鑰，可不填」，不用貼金鑰，套用後 Breeze 測試連線通過（預設金鑰 `tn8227` 自動存入）
- [ ] RD（`10.88.95.2`）沒實測過：套用 RD，確認模型名稱自動偵測正確、`:8002` 是否接受內建預設金鑰 `tn8227`（不用貼）；切回 TN 時 TN 的金鑰仍在
- [ ] 按住右 Ctrl 說話：浮動標籤是黑底綠色波形 → 進度條 → 淡出，沒有「錄音中／完成」文字；文字輸出的感覺比 0.1.6 順
- [ ] 雙擊右 Ctrl 免持：波形左邊綠點、右邊「按右 Ctrl 結束」；單擊結束
- [ ] 浮動標籤外圍是透明的（淺色與深色 Windows 主題都看）
- [ ] 系統列圖示：Windows「設定 → 個人化 → 色彩」切換工作列淺／深色，約 3 秒內圖示跟著換
- [ ] 設定頁預設只看到常用 4 項；「進階」可展開；改一項不儲存就切頁會詢問
- [ ] 從系統列「輸入紀錄」直接開到「詞庫與紀錄 → 輸入紀錄」分頁
- [ ] 整體是否比 0.1.6 簡潔；有覺得複雜的地方記下來
- [ ] Linux：照 `linux_handover.md` 第 3、4 節；決定 Wayland 通知是否只在出錯時跳出（第 4.1 節）

- [ ] 錄音檔已推上 GitHub（`34fcbe2`）：`tests/fixtures/audio/README.md` 寫的是「不會進 git」，若要公開 repo 請確認
