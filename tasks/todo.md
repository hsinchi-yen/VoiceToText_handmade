# v2c 待辦清單

> 每項對應 `cards/` 中的一張任務卡；波次與平行規則見 `cards/README.md`。完成就勾選；檢查點未通過不進下一階段。🧑 項目見 `human-checklist.md`。

## Phase 0 — 技術驗證
- [x] W0 T0.0 開發環境腳本、workspace、`v2c-core` 介面與設定格式、測試音檔說明（🧑 需你核准）
  - [x] `tools/setup-dev.ps1`（已通過 PowerShell 語法檢查，未執行）
  - [x] workspace 與 5 個 crate 骨架、`v2c-core` 介面、`config.rs` 完整設定與預設值、單元測試
  - [x] `tests/fixtures/audio/README.md` 與 11 個期望文字檔
  - [x] 2026-11-09 驗證：rustc 1.99.1、CUDA 13.5；`cargo build`、`cargo test`（14 項通過）、`cargo clippy -D warnings`、`cargo fmt --check` 全部通過
  - [x] 🧑 你審查介面後合併（2026-11-09 核准）
- [ ] W1 T0.1 Breeze-ASR-25 轉 GGML＋量測（≤ 1.2 秒、繁體、英文保留、純英不被翻譯）＋ VRAM 實測
  - [x] 程式已合併（2026-11-09）：`BreezeAsr`、轉檔腳本、`transcribe`/`bench`（含 PASS/FAIL 判定、簡體字偵測）；CPU 與 CUDA 版皆可編譯
  - [ ] 🧑 轉檔、錄音、量測、VRAM（見 human-checklist）
- [ ] W1 T0.2 熱鍵狀態機＋Windows 掛勾
  - [x] 程式已合併（2026-11-09）：狀態機 30 個測試涵蓋第 3 節每一列；掛勾忽略 v2c 自己注入的按鍵
  - [ ] 🧑 `hotkey-test` 實機按鍵
- [ ] W1 T0.3 注入計畫＋SendInput
  - [x] 程式已合併（2026-11-09）：`plan()`、`SendInputInjector`（注入標記、部分失敗時放開 Shift）、前景 App 偵測
  - [ ] 🧑 5 個 App 實機 `inject-test`
- [ ] **檢查點 0**：結果寫入 `spike-results.md`（你已同意 Phase 1 平行開發，實測失敗時回頭修）

## Phase 1 — Windows MVP
- [ ] W2 T1.1 最小可用路徑（≤ 1.5 秒、預錄緩衝）
- [ ] W3 T1.2 剪貼簿貼上＋還原
- [ ] W3 T1.3 浮動標籤＋系統列
- [ ] W3 T1.5 LM Studio 輕度潤飾（含潤飾 ≤ 2.5 秒）
- [ ] W4 T1.4 雙擊免持模式
- [ ] W4 T1.7 雲端 providers（≤ 3 秒，失敗退回地端）
- [ ] W4 T1.10 SenseVoice-Small CPU 備援
- [ ] W5 T1.6 中翻英
- [ ] W5 T1.9 輸入紀錄（DPAPI 加密、7 天、不記錄的 App、日誌不含文字）
- [ ] W5 T1.10b VRAM 管理
- [ ] W6 T1.7b 「新增雲端服務」精靈＋`docs/cloud-setup.md`
- [ ] W6 T1.8 個人詞庫
- [ ] W7 T1.11 設定視窗、首次啟動、NSIS 安裝檔、README（Defender 實測）
- [ ] **檢查點 1**：日常使用一週，問題記在 `dogfood-windows.md`

## Phase 2 — Android
- [ ] W8 T2.1 語音鍵盤 IME 最小路徑
- [ ] W8 T2.3 v2c-ffi（uniffi＋cargo-ndk）
- [ ] W9 T2.2 功能鍵
- [ ] W9 T2.4 中翻英鍵＋雲端 ASR 備援
- [ ] W10 T2.5 設定頁、串接雲端精靈、release APK
- [ ] **檢查點 2**：手機使用一週；決定 Linux 用哪台電腦

## Phase 3 — Linux（Ubuntu GNOME Wayland）
- [ ] W11 T3.0 重新查證並拆出 T3.1～T3.4 任務卡
- [ ] **檢查點 3**：第一版交付
