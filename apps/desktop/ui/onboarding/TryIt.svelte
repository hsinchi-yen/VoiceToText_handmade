<script>
  // 「試試看」: hold Right Ctrl with the cursor in this text box; the text is typed here.
  import { listen } from "@tauri-apps/api/event";
  import { TextField } from "../lib/index.js";

  let text = $state("");
  let status = $state(null);

  $effect(() => {
    document.querySelector(".try-field textarea")?.focus();
    const un = listen("v2c:status", (e) => (status = e.payload));
    return () => un.then((f) => f());
  });

  const errorText = {
    model_missing: "還沒有語音模型：請回上一步下載，或重新啟動 v2c 讓它載入模型",
    asr_loading: "語音模型載入中，請稍候幾秒再試",
    mic_not_found: "找不到麥克風",
    hotkey_failed: "熱鍵安裝失敗",
  };

  function describe(s) {
    switch (s?.kind) {
      case undefined:
        return "按住右 Ctrl 說一句話，放開後文字會出現在下面。";
      case "recording":
        return "錄音中…放開右 Ctrl 結束";
      case "transcribing":
        return "辨識中…";
      case "polishing":
        return "潤飾中…";
      case "done":
        return s.chars > 0 ? `完成！放開到出字 ${s.latency_ms} ms` : "沒有聽到聲音，再試一次";
      case "error":
        return errorText[s.code] ?? `錯誤：${s.code}`;
      default:
        return "";
    }
  }
</script>

<p>{describe(status)}</p>
<div class="try-field"><TextField bind:value={text} rows={4} placeholder="把游標放在這裡，按住右 Ctrl 說話" label="試試看" /></div>
<p class="hint">快速按兩下右 Ctrl 進入免持模式，再按一下結束；Esc 取消。</p>
