# Linux 交接：0.2.0 新介面與預設組態（Phase 4）

> 2026-11-12 在 Windows 完成 Phase 4，已 push 到 `origin/main`。這份文件讓你切到 Linux（Ubuntu 22.04／GNOME 42 Wayland，IBus 引擎路線）後，能把同一套新介面與功能完整跑起來並驗收。
> 需求與決策：`v2cplan.md` 第 2 節「預設組態檔」「介面」兩列；設計細節：`tasks/plan.md` 第 13 節。

## 1. 結論先講

**這次的介面與功能幾乎全是跨平台的共用程式**（Svelte 前端＋`apps/desktop/src-tauri` 的共用指令），Linux **不需要另外實作**，`git pull` 後重新建置就有。需要在 Linux 上處理的只有第 4 節列的幾項：

| 項目 | Linux 狀態 |
|------|------------|
| 4 項選單、首頁、設定（常用＋進階）、服務頁分組、詞庫與紀錄 | 共用，直接可用 |
| 預設組態 TN／RD 一鍵套用、從檔案載入 | 共用；金鑰存 GNOME 鑰匙圈（Secret Service） |
| 新設計系統（Material 3 Expressive token 與元件） | 共用；WebKitGTK 需目視確認（第 4.2 節） |
| 浮動標籤：錄音＝黑底綠色心電圖波形、辨識／潤飾＝進度條、無狀態文字 | X11 才有浮動標籤；Wayland 正常說話不跳通知，只在出錯與免持時提示（第 4.1 節，T4.9 已做） |
| 新圖示（方案 B）與系統列圖示隨工作列主題切換 | Linux 固定用「深色工作列」版（淺色字形），需目視確認（第 4.3 節） |
| 版本 0.2.0（含 `tools/ibus/v2c.xml`） | 已改；`build-linux.sh` 的版本檢查應通過 |

## 2. 這次改了什麼（main 上的合併順序）

| 卡 | 內容 | 主要檔案 |
|----|------|----------|
| T4.0 | 設計 token（`--md-*`，淺色／深色）、共用元件、4 項導覽（舊網址轉址）、`dev-mock.js`、`npm run check`／`npm test` | `apps/desktop/ui/app.css`、`ui/lib/**`（元件說明 `ui/lib/README.md`）、`ui/App.svelte`、`ui/lib/nav.js`、`ui/routes/Library.svelte` |
| T4.1 | 預設組態檔 TN／RD、`presets_list`／`presets_preview`／`presets_apply`／`presets_pick_file` 指令、套用對話框 | `apps/desktop/src-tauri/presets/*.toml`、`src-tauri/src/commands/presets.rs`、`ui/presets/**` |
| T4.5 | 圖示候選，選定方案 B（對話氣泡＋麥克風＋黃色 V 托架） | `src-tauri/icons/src/**` |
| T4.2 | 首頁：狀態卡、目前使用中、預設組態卡、常用設定（潤飾、語音辨識由誰處理，立即儲存） | `ui/routes/Home.svelte`、`ui/home/logic.js`、`ui/settings/RuntimeStatus.svelte` |
| T4.3 | 設定頁：常用 4 項＋收合的「進階」；底部「重新執行初次設定」 | `ui/routes/Settings.svelte`、`ui/settings/**` |
| T4.4 | 服務頁依 TN／RD／其他分組；詞庫與紀錄、初次設定、浮動標籤換新樣式 | `ui/routes/CloudSetup.svelte`、`ui/cloud-setup/**`、`ui/routes/{Vocab,History,Onboarding}.svelte` |
| T4.6a | 套用圖示 B、系統列淺／深兩版、導覽列標誌、版本 0.2.0 | `src-tauri/icons/**`、`src-tauri/src/tray.rs`、`ui/lib/NavRail.svelte` |
| T4.6b | `docs/`、README 改寫（預設組態、新選單、新浮動標籤） | `docs/*.md`、`README.md` |
| T4.7 | 浮動標籤：綠色心電圖波形（`<canvas>`）、估算進度條、完成不顯示文字（`DONE_MS` 800→250） | `ui/routes/Overlay.svelte`、`ui/overlay/**`、`src-tauri/src/overlay.rs` |
| 收尾 | 免持時波形右側顯示「按右 Ctrl 結束」；介面殘留的「雲端服務」「狀態頁」改名 | `ui/routes/Overlay.svelte` 等 |

每張卡都經過獨立審查（Opus）才合併。Windows 上 `cargo test --workspace`（桌面 206 項）、clippy、fmt、`npm run check`（0／0）、`npm test`（40 項）、`npm run build` 全部通過。

### 預設組態重點（細節見 `docs/cloud-setup.md`、`presets.rs` 模組說明）
- TN＝`10.20.30.23`、RD＝`10.88.95.2`；各有 TEA-ASR `:8001`、Breeze-ASR `:8002`（需金鑰、內建預設金鑰 `tn8227` 套用時自動存入、逾時 8000）、Qwen3.8 27B `:8000`（潤飾／中翻英）。
- 套用時選 TEA 或 Breeze 當語音辨識；文字模型向 `/v1/models` 自動偵測，只有一個模型才採用，否則用檔內 `Qwen3.8-27B-UD-Q6_K_L`。
- 只寫 `[[cloud]]` 與 `[route]`；**不改 `[local]`**。TN↔RD 切換兩組服務與金鑰都保留，使用者自己的服務不動。
- 端點（kind／scheme／host／port／path）一改就刪舊金鑰；網路路徑（`\\`、`//`）與 > 64 KiB 的檔案一律拒絕。

## 3. 在 Linux 上的步驟

```bash
cd ~/<repo>
git pull                                   # 取得 0.2.0（main）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # Windows 上無法驗證 Linux 的 cfg 分支，這一步很重要
cargo fmt --all --check
cd apps/desktop && npm ci && npm run check && npm test && npm run build && cd ../..
bash tools/build-linux.sh --check           # 依賴檢查
bash tools/build-linux.sh                   # 產出 releases/linux/v2c_0.2.0_amd64.deb、.AppImage、SHA256SUMS.txt
```

前端可以不開 Tauri 直接看畫面：`cd apps/desktop && npm run dev`，瀏覽器開 `http://localhost:1420`（`dev-mock.js` 提供假資料；`#/overlay` 可在 console 用 `__v2cMock.emit("v2c:overlay", {view:{kind:"recording",level:0.6,hands_free:true},hide_after_ms:null})` 測浮動標籤）。

**Windows 端沒驗證過、Linux 上要特別看的程式：**
- `apps/desktop/src-tauri/src/tray.rs` 的 `#[cfg(not(windows))]`：`current_variant()` 回傳 `tray_variant(None)`（深色工作列版），`watch_taskbar_theme` 為空。clippy 在 Linux 上必須沒有 dead-code 警告。
- `apps/desktop/src-tauri/Cargo.toml` 為 `windows-sys` 新增的 `Win32_System_Registry` feature 在 `[target.'cfg(windows)'.dependencies.windows-sys]`，Linux 不會編到（已確認）。

## 4. Linux 需要你決定或實機確認的事

### 4.1 浮動標籤在 Wayland（已決定：選項 1，T4.9，2026-11-12）

> 使用者指示「Windows 所優化的功能，Linux 比照辦理」，Linux 端採用選項 1：正常說話不跳通知，只在出錯、文字沒輸入時通知；免持開始時跳「免持模式：按右 Ctrl 結束」，錄音結束自動關閉。

- Linux 的浮動標籤只在非 Wayland（X11）開啟（`overlay_enabled()` 依 `XDG_SESSION_TYPE` 判斷，見 `src-tauri/src/main.rs`）；**Wayland 改用桌面通知**（`linux_app/notes.rs` → `crates/v2c-platform/src/linux/notify.rs` 的 `notification_for`），通知內容仍是「錄音中／辨識中／潤飾中」文字。
- 這和 0.2.0「錄音／辨識／潤飾不顯示文字、只用波形與進度條」的設計不一致。通知無法畫波形，可選：
  1. **（建議）Wayland 只在出錯或提示時通知**：錄音、辨識、潤飾、完成都不跳通知（最接近「直接出字、操作流暢」）。改 `notes.rs` 的狀態對應即可，`notify.rs` 的純函式與測試一併調整。
  2. 保留現狀（每句話一則通知原地更新）。
  3. 嘗試在 Wayland 也開浮動標籤（GNOME Wayland 下透明、不搶焦點、置頂的視窗限制多，需要先查證，風險高）。
- 免持模式在 Wayland 沒有浮動標籤可顯示「按右 Ctrl 結束」；若選 1，建議免持開始時發一則「按右 Ctrl 結束」的短通知。

### 4.2 WebKitGTK 顯示（實機目視）
Linux 的 webview 是 WebKitGTK（Windows 是 WebView2／Chromium），開 App 逐頁看：
- 字型：token 的字型堆疊含 `"Noto Sans CJK TC"`，確認中文字型正確、粗細正常。
- 深色模式跟隨 GNOME「外觀 → 深色」；淺色／深色都沒有白底白字。
- 圓角、膠囊按鈕、`color-mix()`、`:where()`、原生 `<dialog>`（預設組態對話框）在 WebKitGTK 版本上正常；Esc 可關對話框。
- X11 下的浮動標籤：黑色膠囊外圍透明、綠色波形平順捲動（`<canvas>` + `requestAnimationFrame`）、波形兩端漸隱（CSS `mask`）。若外圍出現黑底或白底，檢查 `app.css` 的 `html.overlay` 與 `ui/main.js` 在掛載前加的 `overlay` class。
- 窄視窗（< 600 px）改為底部導覽列，沒有水平捲軸。

### 4.3 圖示與系統列
- App 圖示由 `src-tauri/icons/{32x32,128x128,128x128@2x}.png`、`icon.png` 產生；確認 .deb 的 `/usr/share/icons/...` 與應用程式選單顯示新圖示（方案 B）。
- 系統列（AppIndicator）固定用深色工作列版（`#C9C2FF` 淺色字形）；GNOME 42 頂列是深色，應清楚。若你用淺色頂列的主題，告訴 agent 改成依 GNOME 設定切換（可讀 `org.gnome.desktop.interface color-scheme`）。

### 4.4 預設組態在 Linux
- `docs/linux-setup.md` 第 7.3 節：方法一「首頁 → 套用預設組態」、方法二手動改 `config.toml`。
- **注意**：預設組態不改 `[local]`。Linux 定案是遠端 `tea-asr-1`＋本機 SenseVoice 備援，所以仍要確認 `~/.local/share/v2c/models/` 有 SenseVoice 模型，且 `[local] asr = "sense_voice"`（初次設定或設定 → 進階 → 地端語音模型）。若希望套用預設組態時在 Linux 自動設 `local.asr = sense_voice`，需另開卡（會動到 `presets.rs` 的白名單規則）。
- 套用時選 Breeze-ASR 不用貼金鑰：內建預設金鑰會自動存進 GNOME 登入鑰匙圈（要換別的金鑰才在欄位輸入）；TEA-ASR `:8001` 不需要金鑰。

### 4.5 實機驗收清單（Linux）
1. `git pull` 後第 3 節的指令全部通過；`bash tools/build-linux.sh` 產出 0.2.0 的 .deb／AppImage。
2. `sudo apt install ./releases/linux/v2c_0.2.0_amd64.deb`、`ibus restart`、切到 v2c 輸入來源。
3. 首頁 → 套用預設組態 → TN → 選「TN TEA-ASR」→ 套用 → 三個端點測試連線通過；首頁「預設組態」卡顯示「TN 區網服務」。
4. 按住右 Ctrl 說一句：文字輸出；（X11）浮動標籤是綠色波形 → 進度條 → 淡出，沒有狀態文字；（Wayland）依第 4.1 節的決定。
5. 雙擊右 Ctrl 進免持：（X11）波形右側有「按右 Ctrl 結束」。
6. 切到 RD 再切回 TN：服務頁 TN、RD 兩組都在；Breeze 金鑰沒被要求重貼（本來就不用貼，內建預設金鑰會自動存入）。
7. 設定頁預設只看到常用 4 項，「進階」收合；改一項不儲存就切頁會詢問。
8. 詞庫與紀錄：從系統列「輸入紀錄」直接開到紀錄分頁。
9. 結果記到 `tasks/human-checklist.md` Phase 4 與 Phase 3 的對應項目。

## 5. 給在 Linux 上接手的 agent
- 先讀 `AGENTS.md`、本文件、`tasks/plan.md` 第 13 節、`apps/desktop/ui/lib/README.md`。
- 不要重做前端；只處理第 4 節列出的 Linux 差異，且每項先停下請使用者決定（4.1、4.3、4.4 都牽涉設計選擇）。
- `tasks/cards/` 沒有 T4.* 卡片（這次規格直接寫在 agent 指示裡，計畫在 `tasks/plan.md` 第 13 節與 `tasks/todo.md` Phase 4）；Linux 的後續工作請開新卡（例如 T4.8 Wayland 通知精簡）並更新 `tasks/cards/README.md` 波次表。
- Windows 安裝檔：`releases/v2c_0.2.0_x64-setup.exe`（不進 git）。
