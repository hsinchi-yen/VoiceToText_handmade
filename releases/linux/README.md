# Linux 安裝檔

安裝檔（.deb、AppImage）不放進 git，請用這個資料夾的 `build.sh` 從原始碼自己建置：

```sh
git clone git@github.com:hsinchi-yen/VoiceToText_handmade.git
cd VoiceToText_handmade
bash releases/linux/build.sh --check   # 列出缺少的套件（不會自動安裝）
bash releases/linux/build.sh           # 建置，需要幾分鐘
```

完成後這個資料夾會有 `v2c_<版本>_amd64.deb`、`v2c_<版本>_amd64.AppImage` 和 `SHA256SUMS.txt`（舊版會被刪除）。安裝：

```sh
cd releases/linux
sha256sum -c --ignore-missing SHA256SUMS.txt
sudo apt install ./v2c_*_amd64.deb
ibus restart        # 或登出再登入
```

接著加入「v2c 語音輸入」輸入來源，步驟見 [docs/linux-setup.md](../../docs/linux-setup.md) 第 7 節（用指令加入見 7.2.1）。

需要的工具：Rust（[rustup.rs](https://rustup.rs)）、Node.js／npm，以及 `--check` 列出的 apt 套件。
