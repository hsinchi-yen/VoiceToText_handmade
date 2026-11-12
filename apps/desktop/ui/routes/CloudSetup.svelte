<script>
  // 服務 page (T1.7b, restyled T4.4): connected services grouped by preset + wizard (plan §6.2).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "../cloud-setup/api.js";
  import Wizard from "../cloud-setup/Wizard.svelte";
  import ServiceList from "../cloud-setup/ServiceList.svelte";
  import RouteSummary from "../cloud-setup/RouteSummary.svelte";
  import * as presetsApi from "../presets/api.js";
  import { Banner } from "../lib/index.js";

  let kinds = $state([]);
  let presets = $state([]);
  let services = $state([]);
  let loaded = $state(false);
  let message = $state("");
  // null = list; {} = new; {preset} = new, starting from a preset; a service row = edit.
  let wizard = $state(null);
  // Changes on every open so a second wizard (「再新增一個端點」) starts fresh.
  let wizardKey = $state(0);

  function openWizard(w) {
    wizardKey += 1;
    wizard = w;
  }

  async function refresh() {
    try {
      [kinds, services] = await Promise.all([api.kinds(), api.list()]);
      // Only used to group the list; a failure just means everything lands in 「其他」.
      presets = await presetsApi.list().catch(() => []);
      message = "";
    } catch (code) {
      message = api.describe(code);
    }
    loaded = true;
  }

  $effect(() => {
    refresh();
    // The tray's 「全部改用地端」 also changes the routes.
    const un = listen("v2c:config-changed", refresh);
    return () => un.then((f) => f());
  });

  function done(_saved, next) {
    refresh();
    wizard = null;
    if (next) openWizard({ preset: next });
  }
</script>

{#if !loaded}
  <h1>服務</h1>
  <p class="hint">載入中…</p>
{:else if wizard}
  <h1>{wizard.id ? "修改服務" : "新增服務"}</h1>
  {#key wizardKey}
    <Wizard
      {kinds}
      {services}
      editing={wizard.id ? wizard : null}
      preset={wizard.preset ?? null}
      onDone={done}
      onCancel={() => (wizard = null)}
    />
  {/key}
{:else}
  <h1>服務</h1>
  <p class="hint lead">公司區網請用<a href="#/">首頁的「套用預設組態」</a></p>
  <RouteSummary {services} />
  <ServiceList
    {services}
    {presets}
    onAdd={() => openWizard({})}
    onEdit={(s) => openWizard(s)}
    onChanged={refresh}
  />
  <details class="howto">
    <summary>區網或自架伺服器有兩個網址（文字模型、語音辨識）時怎麼設定？</summary>
    <ol>
      <li>按「新增服務」→「自訂相容端點」，填文字模型的網址（例如 <code>http://10.0.0.5:8000/v1</code>），選「文字模型」，填模型名稱，測試後勾「潤飾」「中翻英」。</li>
      <li>最後一步按「再新增一個端點」，把連接埠改成語音辨識的（例如 <code>:8002/v1</code>），選「語音辨識」，填模型名稱，測試後勾「語音辨識」。</li>
      <li>兩個服務各自保存金鑰；上面的「各用途由誰處理」會顯示每個用途交給哪個網址。</li>
    </ol>
  </details>
{/if}

{#if message}<div class="msg"><Banner variant="error">{message}</Banner></div>{/if}

<style>
  .lead {
    margin: 0 0 var(--md-space-4);
  }
  .howto {
    margin: var(--md-space-4) 0;
    padding: var(--md-space-3) var(--md-space-4);
    border-radius: var(--md-radius-md);
    background: var(--md-surface-container-low);
    font-size: var(--md-type-body-sm-size);
    line-height: var(--md-type-body-sm-line);
  }
  .howto summary {
    cursor: pointer;
    color: var(--md-on-surface);
    font-weight: 600;
  }
  .howto ol {
    margin: var(--md-space-2) 0 0;
    padding-left: var(--md-space-5);
  }
  .msg {
    margin-top: var(--md-space-4);
  }
</style>
