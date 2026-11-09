<script>
  // [history]: on/off, retention, apps that are never recorded.
  import * as api from "./api.js";

  let { view, save } = $props();

  const h = $derived(view.config.history);
  let exclude = $state("");
  $effect(() => {
    exclude = h.exclude_apps.join("\n");
  });

  async function saveDays(e) {
    const ok = await save([{ field: "history_retention_days", value: Number(e.currentTarget.value) }]);
    if (!ok) e.currentTarget.value = h.retention_days;
  }
</script>

<section class="settings-section">
  <h2>輸入紀錄</h2>
  <label>
    <input type="checkbox" checked={h.enabled}
      onchange={(e) => save([{ field: "history_enabled", value: e.currentTarget.checked }])} />
    保存輸入紀錄（以 Windows DPAPI 加密，只有你的帳號能讀；不存錄音）
  </label>
  <label>
    保留天數
    <input type="number" min="1" max="365" value={h.retention_days} onchange={saveDays} />
  </label>
  <label for="exclude-apps">不記錄的 App（一行一個執行檔名稱）</label>
  <textarea id="exclude-apps" rows="4" bind:value={exclude} spellcheck="false"
    onchange={() => save([{ field: "history_exclude_apps", value: api.parseExeList(exclude) }])}></textarea>
  <p class="hint">輸入紀錄的變更會在重新啟動 v2c 後生效。</p>
</section>
