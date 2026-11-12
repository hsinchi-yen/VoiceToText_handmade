# v2c 應用程式圖示候選（T4.5）

三個原創方向，皆為 1024x1024 viewBox、大圓角方形底板（藍紫漸層）、不含文字與細線，並各附單色托盤版（透明背景，單色 #6D5EF5，亮色與暗色工作列皆可辨識）。以 `preview.html` 比較各尺寸。

| 候選 | 檔案 | 概念 |
|------|------|------|
| A | `candidate-a.svg` / `candidate-a-tray.svg` | 兩層向內收斂的聲波形成「V」，尖端接一條文字游標：語音變成游標處的文字。 |
| B | `candidate-b.svg` / `candidate-b-tray.svg` | 對話氣泡內的麥克風，其托架形成「V」：說話變成對話。托盤版僅保留麥克風與 V 托架。 |
| C | `candidate-c.svg` / `candidate-c-tray.svg` | 高低排列成 V 輪廓的聲波長條流向下方的 AI 星芒節點：語音交給 AI 代理。 |

## 選定後如何產生最終圖示

1. 在 `apps/desktop` 執行：`npm run tauri icon src-tauri/icons/src/candidate-<x>.svg`（會覆寫 `src-tauri/icons/` 內的 png/ico/icns）。
2. 托盤圖示使用對應的 `candidate-<x>-tray.svg`，另外轉為 16/24/32 px PNG 後放入托盤設定使用。
3. 只使用基本 SVG 元素（path、rect、circle、linearGradient），以確保小尺寸轉換正確。
