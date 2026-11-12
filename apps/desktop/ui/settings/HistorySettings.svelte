<script>
  import { isLinux, pick } from "../platform.js";
  // [history]: on/off, retention, apps that are never recorded.
  import * as api from "./api.js";
  import { Switch, TextField } from "../lib/index.js";
  import Row from "./Row.svelte";

  let { view, save } = $props();

  const h = $derived(view.config.history);
  const dirty = (field) => view.dirty?.has(field);
  let exclude = $state("");
  $effect(() => {
    exclude = h.exclude_apps.join("\n");
  });

  async function saveDays(e) {
    const ok = await save([{ field: "history_retention_days", value: Number(e.currentTarget.value) }]);
    if (!ok) e.currentTarget.value = h.retention_days;
  }
</script>

<Row dirty={dirty("history_enabled")}>
  <Switch checked={h.enabled} label="保存輸入紀錄"
    onchange={(e) => save([{ field: "history_enabled", value: e.currentTarget.checked }])} />
  <p class="hint">{pick("以 Windows DPAPI 加密", "以 AES-256-GCM 加密，金鑰存在 GNOME 登入鑰匙圈")}，只有你的帳號能讀；不存錄音。</p>
</Row>
<Row dirty={dirty("history_retention_days")}>
  <TextField type="number" min="1" max="365" label="保留天數" value={h.retention_days} onchange={saveDays} />
</Row>
<Row dirty={dirty("history_exclude_apps")}>
  <TextField rows={4} label="不記錄的 App（一行一個執行檔名稱）" bind:value={exclude} spellcheck="false"
    onchange={() => save([{ field: "history_exclude_apps", value: api.parseExeList(exclude) }])} />
</Row>
{#if isLinux}
  <p class="hint">Linux（GNOME Wayland）目前無法得知前景程式，這份清單暫時不會生效；不想留紀錄時請關閉上方「保存輸入紀錄」。</p>
{/if}
<p class="hint">輸入紀錄的變更會在重新啟動 v2c 後生效。</p>

<style>
  p {
    margin: var(--md-space-1) 0 0;
  }
</style>
