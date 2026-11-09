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
- [ ] 把 VS Build Tools 內建的 CMake 加入 PATH：`C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin`
- [ ] 每次開 PowerShell 編譯前（或設成使用者環境變數）：
  ```powershell
  $env:CARGO_TARGET_DIR = "$env:USERPROFILE\.v2ct"      # 避免 Windows 路徑過長
  $env:CMAKE_CUDA_ARCHITECTURES = '75'                  # RTX 2060，大幅縮短編譯時間
  ```

## Phase 0（Phase 1 已依你同意平行開發；以下實測若失敗，agent 會回頭修）
- [ ] T0.1 錄音：依 `tests/fixtures/audio/README.md` 錄 10 段 WAV
- [ ] T0.1 轉檔：`powershell -ExecutionPolicy Bypass -File tools\convert-breeze\convert-breeze.ps1`，問 `Proceed? [y/N]` 時輸入 `y`（約 20～40 分鐘、下載約 3 GB）；把印出的檔案大小與 SHA-256 填進 `tasks/spike-results.md`
- [ ] T0.1 量測（設好上面的環境變數）：
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
- [ ] T1.8 個人詞庫
  1. `npm run tauri dev` →「詞庫」頁輸入 5 個常用人名或產品名 → 儲存，確認 `%APPDATA%\v2c\vocab.txt` 有內容
  2. 錄一段含這些詞的音檔（可另存為 `tests/fixtures/audio/vocab-02.wav` 並寫期望文字），比較 CER：
     `cargo run -p v2c-cli --release --features cuda -- bench --asr breeze --model <breeze.bin> tests/fixtures/audio` 與加上 `--vocab "$env:APPDATA\v2c\vocab.txt"` 的結果
- [ ] T1.9 輸入紀錄
  1. 說幾句話後，用 DB Browser for SQLite 開 `%APPDATA%\v2c\history.db`：`text_sealed` 是亂碼，搜尋句子找不到
  2. 在 KeePassXC／Bitwarden 的欄位說話 → 紀錄頁沒有新增
  3. 先點記事本，再從系統列開「輸入紀錄」按「重新輸出」→ 文字打進記事本；只點過桌面時按 → 顯示「找不到要輸入的視窗」，不輸入
  4. 「複製」後貼上、「刪除」、「全部清除」（兩段確認）
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
- [ ] 各卡的實機驗收（見各卡「🧑 需人工」）
- [ ] 檢查點 1：日常使用一週，問題記在 `tasks/dogfood-windows.md`

## Phase 2
- [ ] 側載 APK 到手機實測；回報手機型號與 Android 版本
- [ ] 檢查點 2：手機日常使用一週
- [ ] 決定 Linux 用哪台電腦（同一台雙系統或另一台），並提供規格
