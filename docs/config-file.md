# v2c 組態檔說明（格式版本 1）

組態檔是一個純文字的 TOML 檔，用來在 Windows、Linux 與 Android 之間搬移設定，也可以直接用記事本手寫。三個平台的「匯出設定／匯入設定」讀寫的是同一種格式。

## 用途與範圍

組態檔只包含三類設定：

- **雲端／區網服務**（`[[cloud]]`）：Gemini、OpenAI、Claude，或自己架的相容端點。
- **各用途由誰處理**（`[route]`）：語音辨識、潤飾、中翻英交給本機或哪一個服務。
- **詞庫**（`[vocabulary]`）：你常用的專有名詞。

熱鍵、麥克風、顯示卡、App 規則、輸入紀錄等設定**不在**組態檔內，不會被匯出，匯入時也不會被改動。

## 金鑰怎麼處理

- **匯出永遠不含金鑰。** 檔案只在 `needs_key` 列出「哪些服務需要金鑰」，匯入時 v2c 會請你貼上；這樣檔案可以安全地傳給別人或放進雲端硬碟。
- 手寫的檔案可以用 `default_keys` 附上一把**共用金鑰**（例如公司內網伺服器大家共用的那把）。匯入時，你沒有另外輸入金鑰的服務就會存這把。
- **`default_keys` 的風險：** 金鑰會以明文寫在檔案裡，拿到檔案的人都看得到。只適合內網共用金鑰，不要放你個人的雲端金鑰；匯出功能也不會產生它。
- 匯入時，如果某個服務的網址（或種類）改變了，舊金鑰會被刪除（不會把金鑰送到新的主機），除非你貼上新的金鑰或檔案附有 `default_keys`。網址沒變就保留舊金鑰。

## 檔案結構

```text
[v2c]            格式版本（必填）
[preset]         需要金鑰的服務、共用金鑰（選填）
[route]          各用途由誰處理（選填）
[[cloud]]        服務，可以有很多個（選填）
[vocabulary]     詞庫（選填）
```

只有以上五種區段會被讀取。其他區段（例如從 `config.toml` 整份貼來的 `[hotkey]`）會被忽略，匯入預覽會列出「已忽略的區段」。

### `[v2c]`

| 欄位 | 必填 | 說明 |
|------|------|------|
| `format` | 必填 | 格式版本，目前是 `1`。大於 1 的檔案會被拒絕（「檔案由較新版本產生」）。 |
| `exported_from` | 選填 | 來源平台，只供顯示：`windows`／`linux`／`android`／`manual`。 |
| `exported_at` | 選填 | 匯出時間，只供顯示。 |

舊的「預設組態檔」（有 `[preset]` 但沒有 `[v2c]`）也能讀，視為格式 1。

### `[preset]`（選填）

| 欄位 | 說明 |
|------|------|
| `needs_key` | 需要金鑰的服務 id 清單；每個 id 都必須是檔內的服務。 |
| `default_keys` | 共用金鑰，例如 `{ "my-lan-asr" = "共用金鑰" }`。id 必須在 `needs_key` 內；金鑰去頭尾空白後不可空白、不可有控制字元、最長 256 字元。只限手寫。 |
| `title` | 預覽畫面的標題，只供顯示。 |
| `id_prefix` | 選填；寫了的話，檔內每個服務 id 都要以它開頭（公司預設檔用來和別組服務分開）。不可以 `openai`、`gemini`、`claude`、`custom`、`local` 開頭。 |
| `asr_choices` | 選填；匯入時讓你挑一個當語音辨識的服務 id 清單（`local` 或能辨識語音的服務）。 |
| `detect_llm_model` | 選填，預設 `false`；設為 `true` 時，匯入會向區網端點查詢模型清單，只有一個模型就用它。 |

### `[route]`（選填）

沒寫的欄位維持現狀，不會被改。

| 欄位 | 允許值 | 說明 |
|------|--------|------|
| `asr` | `"local"` 或檔內服務 id | 語音辨識由誰處理。Claude 不能辨識語音，不能填。 |
| `polish` | 同上 | 潤飾由誰處理。服務必須有文字模型。 |
| `translate` | 同上 | 中翻英由誰處理。服務必須有文字模型。 |
| `cloud_fallback_to_local` | `true`／`false` | 雲端失敗時改用本機。 |

`route` 只能指向 `"local"` 或**這個檔案裡**有定義的服務。

### `[[cloud]]`（選填，可重複）

| 欄位 | 必填 | 說明 |
|------|------|------|
| `id` | 必填 | 服務代號，見下方「id 規則」。 |
| `kind` | 必填 | `gemini`、`open_ai`、`claude`、`open_ai_compat` 其中之一。 |
| `name` | 選填 | 顯示名稱，只有 `open_ai_compat` 有效（其他種類會忽略）。最長 32 字元、不可有控制字元、不可是 `local`，也不可和其他服務的名稱或 id 重複（不分大小寫）。 |
| `asr_model` | 選填 | 語音辨識模型。留白＝使用建議值（`open_ai_compat` 沒有建議值）。Claude 沒有語音辨識。 |
| `llm_model` | 選填 | 文字模型（潤飾、中翻英）。留白＝使用建議值。 |
| `base_url` | `open_ai_compat` 必填 | 服務網址。官方服務（gemini／open_ai／claude）通常不用寫；要寫就必須是 `https://`（本機 `localhost` 可用 `http://`）。`open_ai_compat` 可以用 `http://`（區網）。結尾的 `/` 會被去掉。 |
| `asr_timeout_ms` | 選填 | 這個服務的語音辨識逾時，毫秒，1000 到 60000。留白＝用全域設定。 |

### `[vocabulary]`（選填）

| 欄位 | 說明 |
|------|------|
| `words` | 詞彙清單，例如 `["Breeze-ASR", "台積電", "v2c"]`。 |

每一項的規則與詞庫頁相同：去頭尾空白、中間連續空白縮成一個、最長 40 字元（太長的會被略過）、不分大小寫的重複只留一個、以 `#` 開頭的項目會被略過。詞庫最多 500 項。

## id 規則

- 只能用英文字母、數字、`-`、`_`；不可以是空的。
- 前後不可以有空白（`" my-gemini"` 會被整份拒絕）。
- 不可以是 `local`（保留給本機）。
- 同一個檔案內不可重複。
- id 同時是金鑰庫裡的名稱，所以一旦用了就不要隨便改；改 id 等於新增另一個服務。

## 匯入怎麼合併

匯入是**合併**，不是取代：

- 依 id 處理檔內的服務：不存在就新增；存在但內容不同就更新；內容相同就不動。
- 檔內沒有的服務、以及它們的金鑰，完全不動。所以來回匯入不同的檔案不會掉金鑰。
- `[route]` 只改檔內有寫、而且和現在不同的欄位。
- 詞庫取聯集：現有的詞維持原順序，檔內的新詞加在後面，重複的略過；超過 500 項上限的部分不會加入，預覽會告訴你有幾個被擋下。
- 匯入前一定先顯示預覽（新增、更新、不變的服務，用途變更，詞庫新增數，已忽略的區段，缺少金鑰的服務），按「套用」才會寫入。
- 檔案有任何一個錯誤，整份拒絕，現有設定完全不變。

## 各平台會忽略什麼

| 內容 | Windows | Linux | Android |
|------|---------|-------|---------|
| 官方服務（gemini／open_ai／claude） | 套用 | 套用 | 套用 |
| `open_ai_compat` 的 HTTPS 端點 | 套用 | 套用 | 套用 |
| `open_ai_compat` 的 HTTP 區網端點 | 套用 | 套用 | 略過（手機只允許 HTTPS） |
| 與內建區網服務相同 id 的服務 | 更新 | 更新 | 略過（標示「已內建」） |
| `route.asr`、`route.translate` | 套用 | 套用 | 套用（成為該服務的用途） |
| `route.polish` | 套用 | 套用 | 忽略（手機沒有潤飾） |
| `cloud_fallback_to_local` | 套用 | 套用 | 忽略 |
| `[vocabulary]` | 套用 | 套用 | 忽略（手機沒有詞庫） |
| 其他區段 | 忽略 | 忽略 | 忽略 |

被略過的項目都會在預覽中列出原因。

## 完整範例

### 範例一：Gemini 辨識，Claude 中翻英

語音辨識交給 Gemini，中翻英交給 Claude，潤飾仍用本機。兩個服務都需要你自己的金鑰，匯入時貼上。

```toml
[v2c]
format = 1

[preset]
needs_key = ["my-gemini", "my-claude"]

[route]
asr = "my-gemini"
translate = "my-claude"

[[cloud]]
id = "my-gemini"
kind = "gemini"
asr_model = "gemini-3.6-flash"

[[cloud]]
id = "my-claude"
kind = "claude"
llm_model = "claude-haiku-5-5"
```

### 範例二：區網相容端點，附共用金鑰

公司內網有一台語音辨識伺服器與一台大型語言模型，大家共用同一把金鑰。

```toml
[v2c]
format = 1

[preset]
title = "公司區網服務"
needs_key = ["lan-asr"]
default_keys = { "lan-asr" = "貼上共用金鑰" }

[route]
asr = "lan-asr"
polish = "lan-llm"
translate = "lan-llm"
cloud_fallback_to_local = true

[[cloud]]
id = "lan-asr"
name = "區網 Breeze-ASR"
kind = "open_ai_compat"
base_url = "http://192.168.1.10:8002/v1"
asr_model = "breeze-asr-25"
asr_timeout_ms = 8000

[[cloud]]
id = "lan-llm"
name = "區網 Qwen"
kind = "open_ai_compat"
base_url = "http://192.168.1.10:8000/v1"
llm_model = "Qwen3.8-27B-UD-Q6_K_L"
```

### 範例三：只有詞庫

不動任何服務，只把詞彙加進詞庫。

```toml
[v2c]
format = 1

[vocabulary]
words = ["Breeze-ASR", "台積電", "v2c"]
```

可以直接複製範例檔 `docs/examples/v2c-config.example.toml` 來改。

## 常見錯誤訊息與修正方法

錯誤訊息的格式是「第幾行：錯誤代碼」，為了保護金鑰與詞庫，訊息裡不會出現檔案內容。

| 錯誤代碼 | 意思 | 怎麼修 |
|----------|------|--------|
| `syntax` | 不是合法的 TOML | 檢查該行附近的引號、括號、`=`。字串要用雙引號。 |
| `invalid_value` | 欄位的型別不對 | 例如 `format = "1"` 要改成 `format = 1`。 |
| `missing_field` | 少了必填欄位 | `[[cloud]]` 一定要有 `id` 與 `kind`。 |
| `unknown_kind` | `kind` 拼錯了 | 只能是 `gemini`、`open_ai`、`claude`、`open_ai_compat`。 |
| `format_missing` | 沒有格式版本 | 在檔案開頭加上 `[v2c]` 與 `format = 1`。 |
| `format_too_new` | 檔案由較新版本產生 | 更新 v2c，或請對方用舊版匯出。 |
| `id_invalid` | id 含空白或不允許的字元 | 只用英文字母、數字、`-`、`_`，前後不留空白。 |
| `id_reserved` | id 是 `local` | 換一個 id。 |
| `id_duplicate` | id 重複 | 每個 `[[cloud]]` 的 id 不可相同。 |
| `id_prefix` | id 不是以 `id_prefix` 開頭 | 讓 id 以 `id_prefix` 開頭，或拿掉 `id_prefix`。 |
| `prefix_invalid` | `id_prefix` 是空的，或和內建服務 id 開頭相同 | 換一個前綴，例如 `team-`。 |
| `base_url_required` | `open_ai_compat` 沒有 `base_url` | 補上服務網址，例如 `http://192.168.1.10:8002/v1`。 |
| `base_url_invalid` | 網址不被允許 | 官方服務的網址必須是 `https://`；網址格式要正確。 |
| `name_too_long`／`name_invalid`／`name_reserved` | 名稱太長、含控制字元，或是 `local` | 名稱最長 32 字元，換一個。 |
| `name_taken` | 名稱和別的服務名稱或 id 重複 | 換一個名稱。匯入時如果是和你現有的服務重複，也是同樣處理。 |
| `asr_timeout_range` | 逾時不在 1000 到 60000 之間 | 改成範圍內的毫秒數。 |
| `route_unknown_service` | `[route]` 指向檔案裡沒有的服務 | 把服務也寫進這個檔案，或改成 `"local"`。 |
| `route_no_asr` | 把 Claude 當成語音辨識 | `route.asr` 改成其他服務或 `"local"`。 |
| `route_no_model` | 指定的服務沒有對應的模型 | 補上 `asr_model` 或 `llm_model`。 |
| `needs_key_unknown` | `needs_key` 列了檔案裡沒有的服務 | 補上該服務，或從 `needs_key` 移除。 |
| `default_key_not_listed` | `default_keys` 的 id 不在 `needs_key` 內 | 把這個 id 加進 `needs_key`。 |
| `default_key_invalid` | 共用金鑰是空的、太長或含控制字元 | 重新貼上正確的金鑰（最長 256 字元）。 |
| `asr_choice_invalid` | `asr_choices` 列了不能辨識語音的服務 | 只列 `local` 或能辨識語音的服務。 |
| `too_large` | 檔案超過 64 KB | 拆成較小的檔案。 |
