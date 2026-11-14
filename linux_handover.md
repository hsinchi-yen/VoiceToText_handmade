# Linux 交接：0.3.0 重新打包（介面精簡、金色浮動標籤）

> 2026-11-14 在 Windows 完成，已 push 到 `origin/main`。這份文件讓你切到 Linux（Ubuntu 22.04／GNOME 42 Wayland，IBus 引擎路線）後，把目前的 main **重新建置 0.3.0 的 .deb／AppImage** 並驗證。
> 版本號不變（仍是 0.3.0），`tools/build-linux.sh` 會覆蓋 `releases/linux/` 裡現有的 0.3.0 安裝檔。
> 上一輪（2026-11-13）Linux 0.3.0 的建置、啟動量測、T4.12b 金鑰庫時序修正都已完成，結果在 `tasks/todo.md`「0.3.0 交付」；那一版的交接內容看 `git show 29419f4:linux_handover.md`。

## 1. 結論先講

這次的改動**全部是共用的桌面前端（Svelte）＋一個 Rust 常數**，沒有 Linux 專屬程式要改。`git pull` 後重新建置即可。要注意的只有：

| 項目 | Linux 影響 |
|------|------------|
| 介面精簡（首頁、設定頁、服務頁） | 有：WebKitGTK 上要目視確認版面與兩層折疊（第 4.1 節） |
| 浮動標籤改金色、處理中改成填滿的圓球 | **Wayland 看不到**（Wayland 用通知取代浮動標籤，`main.rs` `overlay` 在 Wayland 關閉）；只有 X11 工作階段會顯示（第 4.2 節，選做） |
| `DONE_MS` 250 → 550 ms（`src-tauri/src/overlay.rs`） | 只影響浮動標籤；Wayland 通知不受影響 |
| 預設組態／匯入對話框修正（`7d08ed0`、`cf66fbf`） | 共用；上一輪 Linux 安裝檔已含 `f953eff`，但不含這兩個 |

## 2. 這次改了什麼（上一輪 Linux 建置之後，main 上的合併）

| 合併 | 內容 | 主要檔案 |
|------|------|----------|
| `7d08ed0`、`cf66fbf` | 對話框渲染失敗時仍可關閉；錯誤狀態用語、焦點移到「關閉」 | `ui/presets/**`、`ui/config-io/**` |
| `3f4f93b` ui-overlay-marquee | 錄音波形改為由左往右流動（取樣固定在時間格點上，波峰不閃爍） | `ui/overlay/{wave.js,Wave.svelte}`、`ui/lib/overlay-wave.test.js` |
| `acaa935` ui-slim | 介面再精簡（給一般使用者）：首頁只留狀態＋「套用預設組態」；「目前使用中」移到服務頁頂端；設定主畫面只留麥克風、一般（潤飾、開機自動啟動），其餘進「進階」，熱鍵時間／顯示卡記憶體／麥克風模式／雲端退回進「進階 → 專家」；版本與 CUDA 移到設定頁最下面 | `ui/routes/{Home,Settings,CloudSetup}.svelte`、`ui/settings/**`（新增 `Routes.svelte`）、`ui/config-io/ConfigIoSection.svelte`、`ui/home/logic.js`、`docs/**`、`README.md` |
| `40584b6` overlay-gold | 浮動標籤：波形金黃色漸層（淡金 → 金 → 琥珀）、最前端與免持圓點改 v2c 紫色 `#9b7bff`；不理會 `prefers-reduced-motion`（Windows 關閉「動畫效果」時以前會凍結）；辨識／潤飾／完成時標籤縮成圓形，金色液體圓球依估計進度填滿，完成時補滿、變白熱、向外發光；取代進度條；`DONE_MS` 550 | `ui/overlay/{wave.js,Wave.svelte,Orb.svelte}`（刪除 `Progress.svelte`）、`ui/routes/Overlay.svelte`、`src-tauri/src/overlay.rs` |

每項都經獨立審查 agent 通過。Windows 上 `npm run check`（204 檔 0 錯 0 警告）、`npm test`（57 項）、`npm run build`、`cargo test -p v2c-desktop overlay::`（19 項）、`cargo clippy -p v2c-desktop --all-targets -D warnings`、`cargo fmt --check` 全部通過；Windows 0.3.0 安裝檔已用這版重建。

## 3. 在 Linux 上的步驟

```bash
cd ~/<repo>
git pull                                   # HEAD 應包含 "Merge overlay-gold"
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cd apps/desktop && npm ci && npm run check && npm test && npm run build && cd ../..
bash tools/build-linux.sh --check           # 依賴檢查
bash tools/build-linux.sh                   # 覆蓋 releases/linux/v2c_0.3.0_amd64.deb、.AppImage、SHA256SUMS.txt
```

預期：`npm test` 57 項、`npm run check` 204 檔 0／0。`cargo test --workspace` 數量應和上一輪相同（858 通過、21 忽略；這次 Rust 只改了一個常數與它的測試範圍），不應有失敗。

## 4. Linux 需要實機確認的事

### 4.1 介面精簡（WebKitGTK，必看）
1. 首頁：只有狀態區（含快速鍵提示）和「預設組態」卡，卡片整寬；沒有「常用設定」、「目前使用中」、「上次放開到出字」。
2. 服務頁：最上面是「目前使用中」，每秒更新不卡頓。
3. 設定頁：主畫面只有「麥克風」、「一般」，最下面一行「版本 0.3.0・CUDA 未啟用」（Linux 不用 CUDA）。
4. 展開「進階」：各用途由誰處理、地端語音模型、App 規則、輸入紀錄、匯出／匯入設定、最下面「重新執行初次設定」。再展開「專家」：熱鍵時間、顯示卡記憶體、麥克風模式、雲端（有雲端服務才出現）。
5. 在「專家 → 熱鍵時間」改一個值、收合「進階」：標題要顯示「1 項未儲存」，底部儲存列出現；按儲存後消失。切頁時有未儲存會詢問。
6. 「進階 → 各用途由誰處理」改語音辨識 → 儲存 → 服務頁「目前使用中」跟著變。
7. 窄視窗（底部導覽列）時以上版面沒有水平捲動。

### 4.2 浮動標籤（選做，只在 X11）
Wayland（預設）**沒有**浮動標籤，改用通知，行為和上一輪一樣（正常說話不跳通知）。若要看新動畫，在登入畫面選「Ubuntu on Xorg」：
1. 按住右 Ctrl 說話：金色波形從左邊紫色圓點進來、往右流動，大聲時波峰大。
2. 放開：標籤縮成圓形，金色液體上升；完成時補滿、發白光、淡出（約 0.55 秒）。
3. 雙擊右 Ctrl：左邊紫點、右邊金色「按右 Ctrl 結束」。
4. X11 下浮動標籤的透明背景、置中位置若有問題，記下來回報（這次沒改 Linux 視窗程式）。

### 4.3 回歸（簡短）
- 套用預設組態 TN → 預覽 → 套用：能正常完成，Breeze 不用貼金鑰；對話框出錯時「關閉」能回到首頁。
- 按住右 Ctrl 說話（IBus 輸入來源）文字照常輸入；日誌與 `config.toml` 不含金鑰、詞庫、辨識文字。

結果記到 `tasks/human-checklist.md`（Phase 4／5 的 Linux 項目）與 `tasks/todo.md`「0.3.0 交付」。

## 5. 給在 Linux 上接手的 agent（提示詞）

把下面整段貼給 Linux 上的 Claude Code：

```text
你在 v2c repo（Typeless 式語音輸入；Rust workspace＋Tauri v2 桌面＋IBus 引擎）的 Linux 機器上，把目前的 main 重新打包成 Linux 0.3.0 安裝檔並驗證。這台是 Ubuntu 22.04／GNOME 42 Wayland。

先讀：AGENTS.md、CLAUDE.md、linux_handover.md（全部）、docs/linux-setup.md。回覆我時用繁體中文（台灣）。

要做的事，依序：
1. git pull，確認 HEAD 包含 "Merge overlay-gold"。
2. 照 linux_handover.md 第 3 節跑完所有指令：cargo test --workspace、cargo clippy --workspace --all-targets -- -D warnings、cargo fmt --all --check、apps/desktop 的 npm ci／check／test／build、bash tools/build-linux.sh --check、bash tools/build-linux.sh。每條指令回報真實 exit code 與摘要（測試數量）。
3. 若有失敗：先判斷是不是 Linux 專屬（cfg 分支、WebKitGTK、套件）；開分支 t0.3.0-linux-fix 在自己的 worktree 只修必要的地方、不改 Windows 行為，補測試，跑完上面全部指令，交給獨立的審查 subagent（只給 diff 與 linux_handover.md），通過後回報我，等我核准再合併。
4. 產出的 .deb／AppImage 只留在 releases/linux/，更新 releases/linux/SHA256SUMS.txt；不要 commit 安裝檔。回報兩個檔案的大小與 SHA-256。
5. 安裝前先問我能不能關掉正在用的 v2c；同意後安裝新的 .deb（sudo 指令列給我自己跑），做第 4.3 節裡你能從日誌確認的部分（啟動、日誌不含金鑰與辨識文字）。
6. 第 4.1、4.2 節需要我在桌面上目視操作的，列成清單給我，不要假裝做過。
7. 結果寫進 tasks/todo.md（「0.3.0 交付」下新增一項 2026-11-14 Linux 重新打包）與 tasks/human-checklist.md；commit 到 main 前先問我；push 前也先問我。

規則：不要裝系統套件（缺什麼列出 apt 指令讓我自己跑）；測試產物放 tests/tmp/；不要在日誌、回報裡印出金鑰、詞庫或辨識文字；遇到與 tasks/plan.md／v2cplan.md 衝突就停下來問我。
```
