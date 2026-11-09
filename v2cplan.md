# v2cplan.md — 語音轉文字輸入工具：需求與決策

> 本檔只寫「做什麼、為什麼」。「怎麼做」見 `tasks/plan.md`；任務卡見 `tasks/cards/`；進度見 `tasks/todo.md`；agent 工作規範見 `AGENTS.md`。
> 三者衝突時：**本檔的決策 > `tasks/plan.md` > 任務卡**。要改決策，先改本檔。

## 1. 背景與目標

打造一個類 Typeless 的語音輸入工具，以「個人使用習慣」為準、可分享給他人自行安裝（各自填 API 金鑰，不做帳號/計費/後端）。

- 只支援 **繁體中文（台灣）** 與 **英文**。
- 一般輸入：自動判斷語言，中英夾雜照原樣保留（例：「這個 meeting 我們 delay 一下」）。
- 中翻英：另一個觸發鍵，說中文 → 直接輸出英文。**不做英翻中**。
- 預設 **地端免費方案**；雲端（Gemini / OpenAI / Claude / OpenAI 相容自訂端點）為選用。
- 平台交付順序：**Windows → Android → Linux（Ubuntu GNOME Wayland，最後交付）**。

## 2. 產品決策

| 項目 | 決定 |
|------|------|
| 使用對象 | 自用為主、可分享；使用者自填金鑰 |
| 平台順序 | Windows → Android → Linux；Linux 用哪台電腦**待定**（Phase 2 結束時決定） |
| 桌面整合 | 全域熱鍵＋輸出到游標處（非 TSF/fcitx 輸入法） |
| 中翻英 | 獨立觸發鍵，說中文輸出英文 |
| 語言判斷 | 自動判斷，保留夾雜英文 |
| 潤飾 | 輕度（去贅字、自我更正、全形標點、自動換行、確保繁體），可關閉，不改語意；**不做語音指令** |
| 觸發行為 | 按住說話；快速雙擊進入免持模式，再按一下結束 |
| 預設熱鍵 | 右 Ctrl＝輸入；右 Ctrl+Shift＝中翻英（可改；你的筆電鍵盤有右 Ctrl） |
| 麥克風 | 預設串流常開，只在記憶體保留最近 0.5 秒（不存檔），按下熱鍵時不吃字；可在設定改成「按下才開」 |
| 文字輸出 | 優先模擬逐字輸出；不適用時改用「剪貼簿貼上＋還原」 |
| 畫面提示 | 系統列圖示＋不搶焦點的浮動小標籤（音量/處理中/錯誤） |
| 第一版附加功能 | 個人詞庫、輸入紀錄（不存錄音） |
| 速度目標 | 10 秒內語句，放開到出字：地端不含潤飾 ≤ 1.5 秒、含潤飾 ≤ 2.5 秒；雲端 ≤ 3 秒 |

## 3. 模型決策

| 項目 | 決定 |
|------|------|
| 預設策略 | **地端優先**：安裝後不填金鑰即可使用；雲端需使用者透過「新增雲端服務」精靈主動串接（選服務 → 取得金鑰說明 → 貼金鑰 → 選模型 → 測試連線 → 指定用途），附使用說明文件 |
| 桌面地端 ASR | Breeze-ASR-25（GPU）為主，SenseVoice-Small（CPU）備援 |
| 桌面地端 LLM | 使用者自行以 **LM Studio** 起模型；本工具只走 OpenAI 相容 API（預設 `http://localhost:1234/v1`），不代裝任何軟體 |
| 雲端 | 官方 API（Gemini、OpenAI、Claude）＋ OpenAI 相容自訂端點（可填自架服務）；雲端失敗自動退回地端 |
| 顯示卡記憶體 | RTX 2060 同時負責畫面輸出：保留約 600 MiB（2026-11-10 實測後你決定：保留 2 GB 時 Breeze 與 LM Studio 3B 無法同時在 GPU 上），v2c＋LM Studio 上限約 4 GB；三種方案（均衡／省顯存／不用顯卡），顯存不足或遊戲、剪片時自動改用 CPU |
| Android ASR | Android 內建 `SpeechRecognizer`；不可用時改用已串接的雲端 ASR，都沒有則提示安裝 Google 語音服務 |
| Android 潤飾 | 不做 |
| Android 中翻英 | 設定雲端/相容端點後才顯示翻譯鍵 |

註：Claude API 不接受音訊，只用於潤飾/翻譯；雲端語音辨識選項為 Gemini、OpenAI 及相容端點。

## 4. 技術決策

| 項目 | 決定 |
|------|------|
| 桌面技術 | **Rust + Tauri v2**，Windows 與 Linux 共用同一套程式 |
| Android 架構 | **Kotlin**（InputMethodService + Jetpack Compose）外殼＋ **Rust 共用核心**（uniffi） |
| Android 鍵盤 | 純語音鍵盤：大麥克風鍵、中翻英鍵、退格、換行、切回原輸入法 |
| 隱私 | 輸入紀錄以 Windows DPAPI 加密、預設保留 7 天；「不記錄的 App」清單（預設：密碼管理器）；**日誌永不記錄辨識文字** |
| 散布 | 第一版不簽章、不做自動更新；README 說明 SmartScreen 與防毒誤判處理；要正式分享時再加簽章與 Tauri 自動更新 |

## 5. 開發方式決策（AI agent 分工）

| 項目 | 決定 |
|------|------|
| Agent | Claude Code：主 agent 負責分派，實作 agent 各在自己的 git worktree 開發；共用規範寫在 `AGENTS.md`（日後可加入 Codex） |
| 並行數 | 最多 3 個實作 agent＋1 個審查 agent |
| 介面先行 | T0.0 由單一 agent 完成 workspace 骨架與 `v2c-core` 介面，經你審查合併後才平行開發 |
| 版本控制 | 本機 git；一任務一分支；合併門檻：`cargo test`、`cargo clippy` 通過＋審查 agent 通過；Phase 1 結束再決定是否推 GitHub |
| 合併權限 | 一般任務由主 agent 自動合併；**介面變更、檢查點、含「🧑 需人工」項目的任務需你核准** |
| 任務卡 | 每個任務一張卡（`tasks/cards/`），限定可修改的範圍 |
| 人工項目 | 標「🧑 需人工」，彙整在 `tasks/human-checklist.md`；agent 做完自己的部分就停下等你回報 |
| 測試音檔 | 你本人錄製，放 `tests/fixtures/audio/` 但不進 git；repo 只放期望文字與錄製說明 |
| 開發環境 | agent 寫 `tools/setup-dev.ps1`，你看過後自行執行 |
| 語言 | 需求、任務卡、報告用繁體中文；程式碼、註解、commit 訊息、`AGENTS.md` 用英文 |

## 6. 風險

| 風險 | 處理 |
|------|------|
| Breeze-ASR-25 轉 GGML 的相容性與量化後準確度 | T0.1 驗證；備案：Whisper large-v3-turbo |
| 6 GB VRAM 同時負責畫面、ASR、LM Studio | 保留額度＋自動讓路（T1.10b）；T0.1 實測 |
| Rust 端簡繁轉換、sherpa-onnx、NVML 綁定 crate 的成熟度 | 各任務動工前查證 |
| 系統管理員權限的視窗無法注入（UIPI） | 偵測後提示並改放剪貼簿 |
| 鍵盤掛勾＋模擬輸入被防毒誤判為鍵盤側錄程式；SmartScreen 警告 | T1.11 以 Windows Defender 實測；README 說明 |
| 輸入紀錄含私人內容 | DPAPI 加密、7 天保留、不記錄的 App、日誌不記文字 |
| Android 內建辨識不可用或自動語言判斷有限 | 以 zh-TW 為主；退回雲端 ASR |
| GNOME Wayland 的熱鍵與注入限制 | Phase 3 動工前重新查證，風險最高 |
| 多 agent 平行開發造成介面分歧與合併衝突 | 介面先行、任務卡限定範圍、介面變更需你核准 |
