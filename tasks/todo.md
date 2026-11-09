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
  - [x] 程式已合併（2026-11-09）：Tauri v2＋Svelte 5 桌面 App、controller、recorder（0.5 秒預錄、裝置重開）、`Pipeline::run`、zh_tw（ferrous-opencc s2twp，保護已是繁體的文字）、CSP、日誌不含文字的測試
  - [x] 整合：`run_with_fallback`＋結構化注入錯誤、SenseVoice 自動切換、GPU guard 輪詢、LLM 接上、麥克風 watchdog、設定熱重載、正常結束（整合，2026-11-09 合併）
  - [ ] 🧑 實機說話測試與延遲（見 human-checklist）
- [ ] W3 T1.2 剪貼簿貼上＋還原
  - [x] 程式已合併（2026-11-09）：原生 Win32 備份／還原所有 HGLOBAL 格式、不進 Win+V 歷史、`run_with_fallback`、VM 類 App 預設走貼上
  - [ ] 🧑 實機剪貼簿測試（見 human-checklist）
- [ ] W3 T1.3 浮動標籤＋系統列
  - [x] 程式已合併（2026-11-09）：不搶焦點、滑鼠穿透的浮動標籤（直接以 Win32 顯示）、系統列選單、主視窗關閉改為隱藏
  - [x] 整合：overlay 載入後重送、設定即時生效、結束時收尾（整合，2026-11-09 合併）
  - [ ] 🧑 實機確認（見 human-checklist；若標籤完全看不見，回報給 agent：WS_EX_LAYERED 問題）
- [ ] W3 T1.5 LM Studio 輕度潤飾（含潤飾 ≤ 2.5 秒）
  - [x] 程式已合併（2026-11-09）：`OpenAiCompatLlm`、潤飾／翻譯提示詞（逐字稿包標籤防注入）、規則式去贅字；pipeline 已接上
  - [x] 整合：桌面建立 LLM、LM Studio 每 10 秒存活檢查（沒開時直接去贅字）、長篇口述逾時依字數放寬（整合，2026-11-09 合併）
  - [ ] 🧑 LM Studio 實測（見 human-checklist）
- [ ] W4 T1.4 雙擊免持模式
  - [x] 接上 controller：雙擊開始、單擊結束、上限自動結束並提示、Esc 取消、標籤顯示「免持」（整合，2026-11-09 合併）
  - [ ] 🧑 口述 1 分鐘以上實測
- [ ] W4 T1.7 雲端 providers（≤ 3 秒，失敗退回地端）
  - [x] 程式已合併（2026-11-09）：OpenAI／Gemini／相容端點 ASR、Gemini／Claude LLM、`SecretStore`（keyring-core）、`route::resolve`、`test_connection`、不轉址的 `CloudHttp`
  - [x] 整合：依 route 建立 provider、雲端失敗退回地端、啟動時安裝 Windows 認證管理員 store（整合，2026-11-09 合併）
  - [ ] 🧑 用自己的金鑰實測至少一家（可選）
- [ ] W4 T1.10 SenseVoice-Small CPU 備援
  - [x] 程式已合併（2026-11-09）：`SenseVoiceAsr`（`sensevoice` feature，sherpa-onnx 2.0.1 shared）、`select::pick`；CPU 約 0.4 秒／10 秒音檔（TTS 測試）
  - [x] bench 對 ASR 輸出先過 `zh_tw` 再算 CER（overlay follow-ups，2026-11-09）
  - [ ] 🧑 真人錄音 bench
- [ ] W5 T1.6 中翻英
  - [x] 右 Ctrl+Shift 走 `TranslateZhToEn`；沒有 LLM 時提示「中翻英需要 LM Studio 或雲端服務」（整合，2026-11-09 合併）
  - [ ] 🧑 實機說 5 句中文確認譯文
- [x] W5 T1.9 輸入紀錄（DPAPI 加密、7 天、不記錄的 App、日誌不含文字）— 2026-11-09 合併：SQLite＋DPAPI、200 筆、每小時清除、只有主視窗能呼叫、重新輸出只打進先前的視窗
- [ ] W5 T1.10b VRAM 管理
  - [x] 後端已合併（2026-11-09）：`GpuGuard`（遲滯、實測用量、退避）、NVML `GpuMonitor`、全螢幕偵測、`docs/local-setup.md`
  - [x] 接上 controller（每 5 秒輪詢、讓路／恢復提示）（整合，2026-11-09 合併）
  - [ ] 設定頁即時使用條（T1.11）
  - [ ] 🧑 實機讓路測試（見 human-checklist）
- [ ] W6 T1.7b 「新增雲端服務」精靈＋`docs/cloud-setup.md`
  - [x] 程式已合併（2026-11-09）：七步精靈、服務列表（測試／修改／移除）、金鑰只顯示末 4 碼、換主機不沿用舊金鑰、寫檔失敗還原金鑰、`docs/cloud-setup.md`
  - [ ] 🧑 用真實金鑰照 `docs/cloud-setup.md` 走一遍（可選，見 human-checklist）
- [ ] W6 T1.8 個人詞庫
  - [x] 程式已合併（2026-11-09）：`Vocab` 檔案、ASR initial prompt（詞彙在前、基本句在後、72 字上限）、潤飾／翻譯 1000 字上限與清理、詞庫頁、CLI `--vocab`
  - [x] pipeline 讀入詞庫、監聽 `v2c:vocab-changed`（整合，2026-11-09 合併）
  - [ ] 🧑 加入 5 個常用詞實測
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
