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

## Phase 0
- [ ] T0.1：執行 Breeze 轉檔；記錄 VRAM 組合實測與畫面是否卡頓；確認筆電能否把畫面切到 AMD 內顯 → `tasks/spike-results.md`
- [ ] T0.2：執行 `v2c-cli hotkey-test`，依情境按鍵並確認結果
- [ ] T0.3：在記事本、Chrome、LINE、VS Code、Word 執行 `v2c-cli inject-test`
- [ ] 檢查點 0 核准

## Phase 1
- [ ] 在 LM Studio 載入 3B 模型並啟動伺服器（T1.5 前）
- [ ] 各卡的實機驗收（見各卡「🧑 需人工」）
- [ ] 檢查點 1：日常使用一週，問題記在 `tasks/dogfood-windows.md`

## Phase 2
- [ ] 側載 APK 到手機實測；回報手機型號與 Android 版本
- [ ] 檢查點 2：手機日常使用一週
- [ ] 決定 Linux 用哪台電腦（同一台雙系統或另一台），並提供規格
