# Linux 交接：0.3.0（加速、組態檔匯出／匯入）

> 2026-11-13 在 Windows 完成，已 push 到 `origin/main`。這份文件讓你切到 Linux（Ubuntu 22.04／GNOME 42 Wayland，IBus 引擎路線）後，把 0.3.0 建置、驗證並交付。
> 需求：`v2cplan.md` 第 2 節「組態檔匯出／匯入」列；設計：`tasks/plan.md` 第 13 節（0.2.0）、第 14 節（匯出／匯入）；加速量測：`tasks/spike-results.md`「T4.10」。
> 0.2.0 的交接內容（Wayland 通知、WebKitGTK、圖示、預設金鑰）仍有效，濃縮在第 5 節；細節看 git 歷史中的上一版本文件（`git show 11569d0:linux_handover.md`）。

## 1. 結論先講

0.3.0 的改動幾乎都是**跨平台共用程式**，Linux 不需要另外實作，`git pull` 後重新建置就有。但有兩處 **Linux 專屬程式只在 Windows 上讀過、沒編譯過**，Linux 上一定要跑 clippy 和實機啟動（第 3、4 節）。

| 項目 | Linux 狀態 |
|------|------------|
| 版本 0.3.0（`Cargo.toml`、`tauri.conf.json`、`package.json`、`tools/ibus/v2c.xml`） | 已改；`build-linux.sh` 的版本檢查應通過 |
| T4.10 啟動加速：模型在建立視窗前開始載入（在單一實例檢查之後）、早期狀態事件先訂閱 | 共用；**Linux 通知橋（`linux_app/mod.rs` 的 `notes_rx`）沒編譯過** |
| T4.10 Breeze `audio_ctx` 依錄音長度、背景暖機 | 只影響 Breeze（GPU）；Linux 用遠端 TEA＋本機 SenseVoice，不受影響 |
| T4.10 錄音掉樣本（Xrun）計數記在 `Timings` 行 | 共用；Linux 的音訊後端（ALSA／PipeWire）會不會回報 Xrun 未知 |
| T4.11 Claude 潤飾提示詞快取 | 共用（`v2c-core`），不用另外處理 |
| T5.1～T5.2 組態檔匯出／匯入（設定頁底部）、手寫規則 `docs/config-file.md` | 共用；**檔案對話框是 GTK3（行程內），AppImage 要實測** |
| 首頁「從檔案載入預設組態」改走匯入預覽；`presets_pick_file` 指令已移除 | 共用 |
| T2.7／T2.8／T5.3 Android 雲端服務與匯出／匯入 | 不影響 Linux 桌面；這台若要建 APK 見第 6 節 |

## 2. 這次改了什麼（main 上的合併順序，0.2.0 之後）

| 卡 | 內容 | 主要檔案 |
|----|------|----------|
| T2.7 | FFI 雲端服務：`set_cloud_services`、`test_connection`、`choose_asr` 加雲端與 `fallback2` | `crates/v2c-ffi/**` |
| T2.8 | Android 雲端設定（Keystore 金鑰）、路由 區網 → 雲端 → 手機本地、雲端中翻英 | `apps/android/**` |
| T4.11 | Claude 潤飾提示詞快取（`cache_control`）、HTTP 連線池參數明確固定 | `crates/v2c-core/src/llm/claude.rs`、`asr/http.rs` |
| T5.1 | `v2c-core::portable`：組態檔解析、驗證、合併計畫、匯出；`docs/config-file.md` 與範例檔 | `crates/v2c-core/src/portable.rs`、`tests/fixtures/config/**` |
| T4.10 | Breeze `audio_ctx` 依錄音長度（下限 768，> 14 秒用 1500）、提早載入、背景暖機（暖機後才量顯示卡記憶體）、浮動標籤延後建立、Xrun 計數 | `crates/v2c-asr-local/src/breeze.rs`、`apps/desktop/src-tauri/src/{main,engine,local_asr,overlay,recorder,controller}.rs`、`linux_app/mod.rs` |
| T5.2 | 桌面匯出／匯入：`config_pick_open`／`config_pick_save`／`config_export`／`config_import_preview`／`config_import_apply`；合併後檢查三個用途、預覽後檔案被改就拒絕（SHA-256） | `src-tauri/src/commands/config_io.rs`、`ui/config-io/**`、`ui/routes/{Settings,Home}.svelte`、`build.rs`、`capabilities/default.json` |
| T5.3 | Android 匯出／匯入（FFI `parse_config_file`、`export_config_file`） | `crates/v2c-ffi/src/config_file.rs`、`apps/android/.../cloud/ConfigFile*.kt` |
| 0.3.0 | 版本號 | 見第 1 節表格 |

每張卡都經 Opus 獨立審查才合併。Windows 上 `cargo test --workspace`（696 項）、clippy、fmt、`npm run check`（202 檔 0／0）、`npm test`（49 項）、Android 208 項單元測試全部通過。

Windows 量測（GPU 空閒，`tasks/spike-results.md`）：Breeze 辨識 1939 → 1311 ms（−32%，錯字率 3.6% → 3.3%）；啟動到可以說話 4.88 → 2.72 秒。

## 3. 在 Linux 上的步驟

```bash
cd ~/<repo>
git pull                                   # 取得 0.3.0（main）
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # 必跑：Linux 的 cfg 分支在 Windows 上沒編譯過
cargo fmt --all --check
cd apps/desktop && npm ci && npm run check && npm test && npm run build && cd ../..
bash tools/build-linux.sh --check           # 依賴檢查
bash tools/build-linux.sh                   # 產出 releases/linux/v2c_0.3.0_amd64.deb、.AppImage、SHA256SUMS.txt
```

**Windows 端沒編譯過、Linux 上要特別看的程式：**
- `apps/desktop/src-tauri/src/main.rs`：`EarlyStatus`（在 `main()` 先訂閱 forwarder／overlay／notes 三個接收端）、`#[cfg(not(target_os = "linux"))] let _ = notes;`、`v2c-asr` plugin（註冊在 single-instance 之後）。
- `apps/desktop/src-tauri/src/linux_app/mod.rs`：`start_controller` 新增參數 `notes_rx: Option<Receiver>`，取代原本在 setup 內 `state.status.subscribe()`。
- clippy 不可有 unused／dead-code 警告；有的話只修 Linux cfg 分支，不改 Windows 行為。

## 4. Linux 需要實機確認的事

### 4.1 啟動與早期事件（T4.10）
1. 從開始選單／`v2c` 啟動，看 `~/.config/v2c/logs/`（`docs/linux-setup.md` 第 6 節）：`v2c starting` 到 `speech recognition ready` 的時間，與 0.2.0 比較（同一台機器、模型已在快取，各 3 次取中位數）。
2. **早期錯誤提示**：暫時把 SenseVoice 模型資料夾改名，啟動後 Wayland 應跳出「找不到語音模型」那類通知（以前是在 setup 內訂閱、現在提早訂閱，確認沒漏）。測完改回。
3. **重複開啟**：v2c 已在跑時再開一次（或 `v2c --ibus` 重新啟動），第二個應立刻結束，日誌不應出現第二組「local ASR selected」。
4. 說幾句話，看 `Timings` 行的 `xruns=` 是否一直是 0；若 PipeWire 下常常非 0，記下來。

### 4.2 組態檔匯出／匯入（T5.2）
1. 設定頁底部 →「匯出設定」：檔案對話框能開（**.deb 與 AppImage 都要試**；AppImage 可能缺 GTK 主題或 GSettings schema，對話框當掉或沒樣式就記下來）。
2. 用文字編輯器開匯出檔：沒有金鑰（例如 `tn8227` 不應出現），有 `[v2c] format = 1`、`[[cloud]]`、`[route]`，有詞庫時才有 `[vocabulary]`。
3. 服務頁刪掉一個服務 → 「匯入設定」選剛才的檔 → 預覽顯示「新增」→ 套用 → 服務回來；需要金鑰的服務會出現金鑰欄位要你貼（匯出檔不含金鑰，也不含預設金鑰；TN／RD 要自動帶預設金鑰請改用「套用預設組態」）。
4. 改掉檔內某服務的 `base_url` 再匯入：預覽顯示「更新」並標示「舊金鑰會被刪除」。
5. 首頁 → 套用預設組態 →「從檔案載入」：走同一個預覽畫面。
6. 對話框如果完全打不開：GTK3 只有設 `GTK_USE_PORTAL=1` 或在 Flatpak 內才走 portal；可以試 `GTK_USE_PORTAL=1 v2c` 看看，並回報給 Windows 端（改用 rfd 的 `xdg-portal` feature 需要開卡、改 `Cargo.toml`）。

### 4.3 手寫組態檔
照 `docs/config-file.md` 手寫一份（例如只有 `[vocabulary]`），匯入確認詞庫增加；故意打錯欄位（`asr_modle`），預覽上方應出現「檔案中的這些部分不會使用，也不會寫入設定：…」並列出 `cloud[1].asr_modle`。

## 5. 0.2.0 留下、仍待實機確認的項目（摘要）
- **Wayland 通知（T4.9）**：正常說話不跳通知，只在出錯、文字沒輸入時通知；免持開始跳「按右 Ctrl 結束」。
- **WebKitGTK 顯示**：中文字型（Noto Sans CJK TC）、深淺色、圓角與 `<dialog>`、窄視窗底部導覽列；新的匯入預覽對話框也一起看。
- **圖示與系統列**：.deb 安裝後應用程式選單與頂列圖示是方案 B。
- **預設組態與金鑰**：套用 TN／RD 不用貼金鑰（GNOME 鑰匙圈要解鎖，`secret-tool search service v2c username tn-breeze-asr` 找得到）；`config.toml` 與日誌沒有 `tn8227`。Linux 的 `[local] asr` 仍要是 `sense_voice`。
- 結果記到 `tasks/human-checklist.md` 的 Phase 3、Phase 4、Phase 5 對應項目。

## 6. （選用）在這台 Linux 建 Android APK
Android 0.3.0 已在 Windows 建好（`releases/v2c_0.3.0_android.apk`，不進 git）。若要在 Linux 重建：`bash tools/build-android.sh --check`，簽署金鑰 `apps/android/keystore.properties`／`.jks` 不在 git，要從 Windows 那台複製（**同一把金鑰**才能覆蓋安裝）。

## 7. 給在 Linux 上接手的 agent（提示詞）

把下面整段貼給 Linux 上的 Claude Code：

```text
你在 v2c repo（Typeless 式語音輸入；Rust workspace＋Tauri v2 桌面＋IBus 引擎）的 Linux 機器上接手 0.3.0 的 Linux 驗證與交付。這台是 Ubuntu 22.04／GNOME 42 Wayland。

先讀：AGENTS.md、CLAUDE.md、linux_handover.md（全部）、tasks/plan.md 第 14 節、docs/linux-setup.md、docs/config-file.md。回覆我時用繁體中文（台灣）。

要做的事，依序：
1. git pull，確認 HEAD 包含 "release: version 0.3.0"。
2. 照 linux_handover.md 第 3 節跑完所有指令：cargo test --workspace、cargo clippy --workspace --all-targets -- -D warnings、cargo fmt --all --check、apps/desktop 的 npm ci／check／test／build、bash tools/build-linux.sh --check、bash tools/build-linux.sh。每條指令回報真實 exit code 與摘要。
3. clippy 或建置若在 Linux 的 cfg 分支失敗（main.rs 的 EarlyStatus／notes、linux_app/mod.rs 的 notes_rx），開分支 t4.10b-linux-fix 在自己的 worktree 只修 Linux 分支、不改 Windows 行為，補測試，跑完上面全部指令，交給獨立的審查 subagent（只給 diff 與 linux_handover.md），通過後回報我，等我核准再合併。
4. 第 4 節中你能自己做的部分：啟動時間量測（0.2.0 的 .deb 與 0.3.0 各 3 次取中位數；要先問我能不能關掉正在用的 v2c）、日誌檢查（重複開啟、Timings 的 xruns=、日誌與 config.toml 不含金鑰與辨識文字）。需要我在桌面上操作的（檔案對話框、通知、目視檢查）列成清單給我，不要假裝做過。
5. 產出的 .deb／AppImage 只留在 releases/linux/，更新 releases/linux/SHA256SUMS.txt；不要 commit 安裝檔。
6. 結果寫進 tasks/human-checklist.md 與 tasks/todo.md（Linux 對應項目），commit 到 main 前先問我；push 前也先問我。

規則：不要裝系統套件（缺什麼列出 apt 指令讓我自己跑）；測試產物放 tests/tmp/；不要在日誌、回報裡印出金鑰、詞庫或辨識文字；遇到卡片與 plan.md／v2cplan.md 衝突就停下來問我。
```
