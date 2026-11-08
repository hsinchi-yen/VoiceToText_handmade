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
- [ ] 在 LM Studio 載入 3B 模型並啟動伺服器（T1.5 前）
- [ ] 各卡的實機驗收（見各卡「🧑 需人工」）
- [ ] 檢查點 1：日常使用一週，問題記在 `tasks/dogfood-windows.md`

## Phase 2
- [ ] 側載 APK 到手機實測；回報手機型號與 Android 版本
- [ ] 檢查點 2：手機日常使用一週
- [ ] 決定 Linux 用哪台電腦（同一台雙系統或另一台），並提供規格
