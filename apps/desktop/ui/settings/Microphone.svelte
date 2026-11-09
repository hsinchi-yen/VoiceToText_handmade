<script>
  // Microphone device and mic_mode (plan §7 recorder).
  import * as api from "./api.js";
  import MicMeter from "./MicMeter.svelte";

  let { view, save } = $props();

  const cfg = $derived(view.config);
  let devices = $state([]);

  $effect(() => {
    api.inputDevices().then((d) => (devices = d)).catch(() => {});
  });

  // A saved device that is unplugged right now still shows in the list.
  const options = $derived(
    cfg.audio.device && !devices.includes(cfg.audio.device) ? [...devices, cfg.audio.device] : devices,
  );
</script>

<section class="settings-section">
  <h2>麥克風</h2>
  <label>
    裝置：
    <select value={cfg.audio.device ?? ""}
      onchange={(e) => save([{ field: "mic_device", value: e.currentTarget.value || null }])}>
      <option value="">系統預設</option>
      {#each options as d (d)}
        <option value={d}>{d}{devices.includes(d) ? "" : "（目前未連接）"}</option>
      {/each}
    </select>
  </label>
  <label>
    <input type="radio" name="mic_mode" value="always_on" checked={cfg.audio.mic_mode === "always_on"}
      onchange={() => save([{ field: "mic_mode", value: "always_on" }])} />
    常開（預設）：只在記憶體保留最近 0.5 秒，按下熱鍵時不吃第一個字；不存檔
  </label>
  <label>
    <input type="radio" name="mic_mode" value="on_press" checked={cfg.audio.mic_mode === "on_press"}
      onchange={() => save([{ field: "mic_mode", value: "on_press" }])} />
    按下才開：按住熱鍵時才開麥克風（第一個字可能被吃掉）
  </label>
  <p class="hint">麥克風的變更會在重新啟動 v2c 後生效。</p>
  <MicMeter device={cfg.audio.device} />
</section>
