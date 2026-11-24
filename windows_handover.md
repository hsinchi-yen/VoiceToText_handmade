# Windows 交接：遠端改用 vLLM、模型名稱自動偵測（2026-11-24）

> 在 Linux（Ubuntu 22.04／GNOME 42）上完成，已合併到 `main`（`93250b6` Merge auto-model-detect）。
> **結論：v2c 原本的程式在 vLLM 上就能正常運作。這次新增「模型名稱自動偵測」，內建 TN／RD 預設組態改用 `"auto"`，所以 Windows（和 Android）要重建安裝檔。** 版本號不變，仍是 0.3.0。

## 1. 現況總表

| 項目 | 結果 |
|------|------|
| 遠端文字模型 | 改用 vLLM 0.31.0（`10.88.95.2:8000`、`10.20.30.23:8000`）。`/v1/models` 列出 `Qwen3.8-27B-FP8`（正式名稱）和 `Qwen3.8-27B-UD-Q6_K_L`（舊名，伺服器保留的別名） |
| 語音辨識 TEA | 伺服器列出的名稱已經是 `tea-asr-1.1`；舊名 `tea-asr-1` 也還能用 |
| 改版前的 v2c | 在 vLLM 上全部正常：關閉思考、只讀正式回答欄位、閒置斷線後重送、404 處理（第 2 節） |
| 這次新增 | `[[cloud]]` 的 `open_ai_compat` 服務可以寫 `asr_model`／`llm_model = "auto"`；內建 TN／RD 預設組態和 `docs/examples/` 都改成 `"auto"`（第 3 節） |
| Windows 要做的事 | 拉 main、重建安裝檔、在桌面上驗證（第 4、5 節） |

## 2. 冒煙測試：改版前（v2c 自己的完整流程）

測試程式在 `tests/tmp/smoke/`（不納入版控）。流程和 app 一樣：雲端語音辨識 → 規則整理 → 潤飾或中翻英。潤飾逾時 1000 ms，上限是 `llm_cloud_ms` 3000 ms。每處理 4 段就停 7 秒，故意超過伺服器約 5 秒的閒置斷線時間。輸出只記毫秒數、字數、錯字率和警告，不記文字。

**連線測試**（等同 app 的「測試連線」）：RD TEA 441 ms、RD Breeze 5570 ms、RD Qwen 112 ms、TN TEA 455 ms、TN Qwen 121 ms，全部通過。

**模型名稱**（RD Qwen）：
- 舊名、正式名稱都成功，都是 350 ms。
- 錯的名稱：伺服器回 404，v2c 1 ms 內退回基本整理，不會卡住。

**連續運作**（RD 10 分 22 秒＋TN 5 分 9 秒，共 378 次）：

| 路線 | 次數 | 辨識 p50／p90 | 潤飾 p50／p90 | 翻譯 p50／p90 | 放開到出字 p50 |
|------|------|---------------|---------------|---------------|----------------|
| RD TEA＋RD Qwen | 108 | 1206／1932 ms | 379／409 ms | 334／339 ms | 1.55 秒 |
| RD Breeze＋RD Qwen | 36 | 5959／6146 ms | 386／446 ms | 344／348 ms | 6.35 秒 |
| RD TEA＋TN Qwen | 108 | 1249／1904 ms | 380／411 ms | 338／343 ms | 1.59 秒 |
| TN TEA＋TN Qwen | 126 | 1173／1821 ms | 380／410 ms | 335／342 ms | 1.55 秒 |

- **0 次錯誤、0 次退回基本整理、翻譯 0 次殘留中文。**
- 潤飾最慢 448 ms，1 秒的逾時設定不用改。
- 錯字率是整條流程算出來的（包含辨識錯誤），每次都一樣。`vocab-01` 約 0.22 是因為測試沒有載入詞庫，屬於預期內。

## 3. 這次的改動（`fd9c5a6`）

| 部分 | 內容 |
|------|------|
| `crates/v2c-core/src/auto_model.rs`（新增） | `"auto"`：第一次使用時 `GET {base_url}/models`，取第一個 id 並記住（每個服務各自記一份，設定重新載入後重查）。之後收到 404 時重新查一次，名稱變了才重送。查詢最多 2 秒，閒置斷線也會重試一次 |
| `llm/openai_compat.rs` | 新增 `with_model_detection()`。只在 `kind = open_ai_compat` 時開啟（`route::build_cloud_llm`、桌面的測試連線 `commands/cloud.rs`）；官方 OpenAI 和 `[local]` LM Studio 會把 `"auto"` 原樣送出 |
| `asr/compat.rs`、`asr/openai.rs` | 語音辨識同樣支援 `"auto"` |
| 內建預設組態、`docs/examples/` | TEA、Breeze、Qwen 三個服務都改成 `"auto"` |
| 介面 | 預設組態、匯入預覽、服務頁把 `auto` 顯示成「自動偵測」；精靈的說明加上「填 `auto` 會自動向伺服器查詢」 |
| 文件 | `docs/config-file.md`（`"auto"` 說明與限制）、`docs/linux-setup.md`（vLLM 註記）、`v2cplan.md` 第 2 節（決策紀錄） |

**驗證（Linux）**：
- `cargo test --workspace` 874 項通過、0 失敗、21 項忽略。
- `cargo clippy -D warnings` 和 `cargo fmt --check` 都乾淨。
- `npm test` 59 項通過，`svelte-check` 206 個檔案 0 錯誤、0 警告。
- 把所有服務的模型都改成 `"auto"`，對實際的 TN／RD 伺服器跑了 217 次完整流程：0 錯誤、0 次退回基本整理。vLLM 遇到不存在的名稱會回 404，所以 `"auto"` 能成功，代表確實是先查到 `Qwen3.8-27B-FP8` 再送出。
- 獨立審查第一輪提出修正，修完後第二輪通過。

**已經套用過預設組態的使用者**：`config.toml` 裡還是舊名稱，在伺服器保留舊名期間照常可用。重新「套用預設組態」一次就會換成 `"auto"`。

**沒做的事**：
- 精靈裡依模型名稱提示 Breeze 逾時的功能，填 `auto` 時不會出現。內建預設組態已經明確設定 8000 ms，所以不影響。
- Android APK 沒有重建，因為這台 Linux 的 Android SDK 已經移除。新的預設組態和核心程式要等重建 APK 才會進到手機；舊 APK 用舊名稱，照常可用。

## 4. Windows 上的步驟

```powershell
git pull                                   # HEAD 應包含 "Merge auto-model-detect"
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cd apps/desktop; npm ci; npm run check; npm test; npm run build; cd ../..
powershell -ExecutionPolicy Bypass -File tools/build-installer.ps1   # 更新 releases/ 與 SHA256SUMS.txt
```

預期結果：
- `cargo test --workspace` 約 874 項通過。Windows 的 cfg 分支數量可能不同，但不應有失敗。
- `npm test` 59 項。
- `npm run check` 206 個檔案，0 錯誤、0 警告。

## 5. Windows 實機確認（🧑，約 5 分鐘）

1. 安裝新安裝檔後，首頁 →「套用預設組態」→ TN（或 RD）→ 預覽：三個服務的模型都顯示「自動偵測」→ 套用。
2. 服務頁：三個服務都顯示「自動偵測」。各按一次「測試連線」，都要通過，Qwen 的模型清單出現兩個名稱。
3. 按住熱鍵說一句中文：文字正常出現。日誌（`%APPDATA%\v2c\logs`）裡 `llm_ms` 約 0.3～0.5 秒，沒有 `PolishFellBack`。
4. 用翻譯熱鍵說一句中文：輸出英文。
5. 日誌和 `config.toml` 裡沒有金鑰，也沒有辨識出來的文字。

結果記到 `tasks/todo.md`（「遠端改用 vLLM」一節）和 `tasks/human-checklist.md`。

## 6. 給 Windows 上接手的 agent（提示詞）

```text
你在 v2c repo 的 Windows 機器上。先讀 AGENTS.md、CLAUDE.md、windows_handover.md（全部）。回覆我時用繁體中文（台灣）。
1. git pull，確認 HEAD 包含 "Merge auto-model-detect"。
2. 照 windows_handover.md 第 4 節跑完所有指令，每條回報真實 exit code 與摘要（測試數量）。有失敗時：先判斷是不是 Windows 專屬；開分支在自己的 worktree 只修必要的地方，補測試、跑完全部指令，交給獨立審查 subagent（只給 diff 與 windows_handover.md），通過後回報我，等我核准再合併。
3. 用 tools/build-installer.ps1 重建安裝檔，放在 releases/，更新 releases/SHA256SUMS.txt；回報大小與 SHA-256。不要 commit 安裝檔。
4. 安裝前先問我能不能關掉正在用的 v2c。第 5 節需要我在桌面上操作的項目列成清單給我，不要假裝做過；你能從日誌確認的部分（沒有 PolishFellBack、沒有金鑰和辨識文字）可以先做。
5. 結果寫進 tasks/todo.md 與 tasks/human-checklist.md；commit 到 main 前先問我，push 前也先問我。
規則：不要在日誌或回報裡印出金鑰、詞庫或辨識文字；測試產物放 tests/tmp/；不要安裝系統軟體；遇到與 tasks/plan.md／v2cplan.md 衝突就停下來問我。
```
