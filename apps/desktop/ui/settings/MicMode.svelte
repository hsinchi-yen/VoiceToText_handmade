<script>
  // mic_mode (plan §7 recorder).
  import { SegmentedButton } from "../lib/index.js";
  import Row from "./Row.svelte";

  let { view, save } = $props();
  const options = [
    { value: "always_on", label: "常開（預設）" },
    { value: "on_press", label: "按下才開" },
  ];
</script>

<Row dirty={view.dirty?.has("mic_mode")}>
  <SegmentedButton label="麥克風模式" value={view.config.audio.mic_mode} {options}
    onchange={(e) => save([{ field: "mic_mode", value: e.currentTarget.value }])} />
</Row>
<p class="hint">
  常開：只在記憶體保留最近 0.5 秒，按下熱鍵時不吃第一個字；不存檔。按下才開：按住熱鍵時才開麥克風（第一個字可能被吃掉）。
  麥克風的變更會在重新啟動 v2c 後生效。
</p>

<style>
  p {
    margin: 0;
  }
</style>
