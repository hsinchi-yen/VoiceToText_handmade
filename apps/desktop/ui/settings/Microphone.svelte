<script>
  // Microphone device and level test.
  import * as api from "./api.js";
  import { Card, Select } from "../lib/index.js";
  import Row from "./Row.svelte";
  import MicMeter from "./MicMeter.svelte";

  let { view, save } = $props();

  const cfg = $derived(view.config);
  let devices = $state([]);

  $effect(() => {
    api.inputDevices().then((d) => (devices = d)).catch(() => {});
  });

  // A saved device that is unplugged right now still shows in the list.
  const options = $derived([
    { value: "", label: "系統預設" },
    ...(cfg.audio.device && !devices.includes(cfg.audio.device) ? [...devices, cfg.audio.device] : devices).map(
      (d) => ({ value: d, label: d + (devices.includes(d) ? "" : "（目前未連接）") }),
    ),
  ]);
</script>

<Card tag="section" aria-labelledby="sec-mic">
  <h2 id="sec-mic">麥克風</h2>
  <Row dirty={view.dirty?.has("mic_device")}>
    <Select label="裝置" value={cfg.audio.device ?? ""} {options}
      onchange={(e) => save([{ field: "mic_device", value: e.currentTarget.value || null }])} />
  </Row>
  <MicMeter device={cfg.audio.device} />
  <p class="hint">麥克風的變更會在重新啟動 v2c 後生效。</p>
</Card>

<style>
  h2 {
    margin: 0 0 var(--md-space-3);
  }
  p {
    margin: 0;
  }
</style>
