<script>
  // Common choices: who handles each purpose, polish on/off, start with Windows. Changes go to
  // the page's draft (`save`); routes/Settings.svelte writes them on 「儲存」.
  import * as api from "./api.js";
  import { displayName } from "../cloud-setup/api.js";
  import { Card, Select, Switch } from "../lib/index.js";
  import Row from "./Row.svelte";

  let { view, save } = $props();

  const cfg = $derived(view.config);
  const dirty = (field) => view.dirty?.has(field);
  const purposes = [
    { field: "route_polish", key: "polish", label: "潤飾" },
    { field: "route_translate", key: "translate", label: "中翻英" },
  ];

  // T1.13: show the service's display name (custom compatible endpoints may have one).
  const serviceLabel = (s) =>
    `${api.kindLabels[s.kind] ?? s.kind}：${displayName(cfg.cloud.find((c) => c.id === s.id) ?? s)}`;

  const asrOptions = $derived([
    ...api.localAsrOptions(view).map((o) => ({ value: "local:" + o.value, label: "地端：" + o.label })),
    ...view.cloud.filter((s) => s.supports_asr).map((s) => ({ value: s.id, label: serviceLabel(s) })),
  ]);
  const llmOptions = $derived([
    { value: "local", label: "地端（本機 LM Studio）" },
    ...view.cloud.map((s) => ({ value: s.id, label: serviceLabel(s) })),
  ]);
</script>

<Card tag="section" aria-labelledby="sec-routes">
  <h2 id="sec-routes">各用途由誰處理</h2>
  <div class="stack">
    <Row dirty={dirty("route_asr") || dirty("local_asr")}>
      <Select label="語音辨識" value={view.asr_choice} options={asrOptions}
        onchange={(e) => save(api.asrChoiceChanges(e.currentTarget.value))} />
    </Row>
    {#each purposes as p (p.field)}
      <Row dirty={dirty(p.field)}>
        <Select label={p.label} value={cfg.route[p.key]} options={llmOptions}
          onchange={(e) => save([{ field: p.field, value: e.currentTarget.value }])} />
      </Row>
    {/each}
  </div>
  <p class="hint">
    潤飾與中翻英的「地端」是這台電腦的 LM Studio（<button class="link" onclick={() => api.openDoc("local-setup")}>設定說明</button>）；沒開時潤飾改用規則式去贅字。
    要用雲端服務或別台電腦，請到 <a href="#/services">雲端服務</a> 串接（選用，雲端會依用量收費）。
  </p>
</Card>

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
