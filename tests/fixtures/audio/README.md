# 測試音檔錄製說明

這些音檔是你本人的聲音，**不會進 git**（`.gitignore` 已排除 `*.wav`）。repo 只保留每段的期望文字 `.txt`。

## 格式
- WAV、16 kHz、單聲道（mono）、16-bit PCM。
- 檔名與同名 `.txt` 對應，例如 `zh-01.wav` ↔ `zh-01.txt`。
- 用平常使用的麥克風、平常的環境錄製（不用刻意安靜），每段 3～10 秒。
- 錄製工具：Windows「錄音機」錄完後，用 Audacity 轉成 16 kHz mono WAV；或安裝 ffmpeg 後執行
  `ffmpeg -i input.m4a -ar 16000 -ac 1 -sample_fmt s16 zh-01.wav`

## 10 段內容（照稿唸，口語一點沒關係）

| 檔名 | 類型 | 稿子（= 期望文字的依據） |
|------|------|--------------------------|
| zh-01 | 純中 | 今天下午三點要開會，記得把報告準備好。 |
| zh-02 | 純中 | 這家餐廳的牛肉麵很好吃，下次我們再一起來。 |
| zh-03 | 純中 | 請幫我確認一下明天的高鐵票是幾點出發。 |
| en-01 | 純英 | Please send me the latest version of the slides before Friday. |
| en-02 | 純英 | I think we should test this on a real device first. |
| mix-01 | 夾雜 | 這個 meeting 我們 delay 一下，改到下禮拜。 |
| mix-02 | 夾雜 | 我已經把 code push 上去了，你再幫我 review 一下。 |
| mix-03 | 夾雜 | 這個 feature 的 deadline 是月底，但 QA 還沒排進去。 |
| filler-01 | 贅字＋自我更正 | 嗯…那個，我們明天，呃，不對，是後天早上十點見面。 |
| vocab-01 | 詞庫詞 | 請把 Breeze ASR 跟 LM Studio 的設定寫進 v2c 的文件。 |

## 期望文字的寫法
- 中文用台灣繁體、全形標點；英文保持原樣。
- `filler-01` 的期望文字是**潤飾後**的結果（去贅字、只保留更正後內容）；`filler-01.raw.txt` 是逐字稿，用來評估未潤飾的辨識。
- 你實際唸的和稿子不同時，請改 `.txt`，以你實際說的為準。
