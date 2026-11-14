<script>
  // 一般: polish on/off and start with the system. Changes go to the page's draft (`save`);
  // routes/Settings.svelte writes them on 「儲存」.
  import { Card, Switch } from "../lib/index.js";
  import Row from "./Row.svelte";

  let { view, save } = $props();

  const cfg = $derived(view.config);
  const dirty = (field) => view.dirty?.has(field);
</script>

<Card tag="section" aria-labelledby="sec-general">
  <h2 id="sec-general">一般</h2>
  <div class="stack">
    <Row dirty={dirty("polish_enabled")}>
      <Switch checked={cfg.polish.enabled} label="潤飾"
        onchange={(e) => save([{ field: "polish_enabled", value: e.currentTarget.checked }])} />
      <p class="hint">去贅字、全形標點、確保繁體；不改語意。</p>
    </Row>
    <Row dirty={dirty("autostart")}>
      <Switch checked={!!view.autostart} disabled={view.autostart === null} label="開機自動啟動"
        onchange={(e) => save([{ field: "autostart", value: e.currentTarget.checked }])} />
      <p class="hint">在背景執行，只顯示系統列圖示。</p>
    </Row>
  </div>
</Card>

<style>
  h2 {
    margin: 0 0 var(--md-space-3);
  }
  .stack {
    display: grid;
    gap: var(--md-space-3);
  }
  p {
    margin: var(--md-space-2) 0 0;
  }
</style>
