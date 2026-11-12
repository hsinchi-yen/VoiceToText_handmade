# v2c 應用程式圖示候選（T4.5）

三個原創方向，皆為 1024x1024 viewBox、大圓角方形底板（藍紫漸層）、不含文字與細線。以 `preview.html` 比較各尺寸。**使用者已選定候選 C。**

| 候選 | 檔案 | 概念 |
|------|------|------|
| A | `candidate-a.svg` / `candidate-a-tray.svg` | 兩層向內收斂的聲波形成「V」，尖端接一條文字游標：語音變成游標處的文字。 |
| B | `candidate-b.svg` / `candidate-b-tray.svg` | 對話氣泡內的麥克風，其托架形成「V」：說話變成對話。 |
| C（已選定） | `candidate-c.svg`、`candidate-c-tray-light.svg`、`candidate-c-tray-dark.svg` | 五條聲波長條階梯式下降，輪廓形成「V」，語音向下匯入 V 尖端的 AI 代理節點（實心圓，左右各一顆衛星點）。 |

## 候選 C 的托盤版本

托盤使用兩個單色版本，依 Windows 工作列主題切換：
- `candidate-c-tray-light.svg`：深色字形 `#3B2FB8`，用於淺色工作列。對 `#f3f3f3` 對比約 8.3:1。
- `candidate-c-tray-dark.svg`：淺色字形 `#C9C2FF`，用於深色工作列。對 `#202020` 對比約 9.8:1。
- `candidate-c-tray.svg`：等同深色工作列版本（別名）。

托盤版是一個粗 V（筆畫 210 單位、圓角）加其尖端下方的一個實心圓節點，不含衛星點；16 px 時筆畫約 3 px。

## 選定後如何產生最終圖示

1. 在 `apps/desktop` 執行：`npm run tauri icon src-tauri/icons/src/candidate-c.svg`（會覆寫 `src-tauri/icons/` 內的 png/ico/icns）。
2. 托盤圖示將兩個托盤 SVG 轉為 16/24/32 px PNG，程式依工作列主題載入對應版本。
3. 只使用基本 SVG 元素（path、rect、circle、linearGradient），以確保小尺寸轉換正確。
