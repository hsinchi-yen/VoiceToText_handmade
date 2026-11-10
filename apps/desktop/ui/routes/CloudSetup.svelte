<script>
  // 雲端服務 page (T1.7b): connected services + 「新增雲端服務」 wizard (plan §6.2).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "../cloud-setup/api.js";
  import Wizard from "../cloud-setup/Wizard.svelte";
  import ServiceList from "../cloud-setup/ServiceList.svelte";
  import RouteSummary from "../cloud-setup/RouteSummary.svelte";

  let kinds = $state([]);
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

<h1>雲端服務</h1>

{#if !loaded}
  <p>載入中…</p>
{:else if wizard}
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
  <p class="hint">預設全部使用地端模型，不需要任何金鑰。串接雲端服務是選用的。</p>
  <RouteSummary {services} />
  <details class="howto">
    <summary>區網或自架伺服器有兩個網址（文字模型、語音辨識）時怎麼設定？</summary>
    <ol>
      <li>按「新增雲端服務」→「自訂相容端點」，填文字模型的網址（例如 <code>http://10.0.0.5:8000/v1</code>），選「文字模型」，填模型名稱，測試後勾「潤飾」「中翻英」。</li>
      <li>最後一步按「再新增一個端點」，把連接埠改成語音辨識的（例如 <code>:8002/v1</code>），選「語音辨識」，填模型名稱，測試後勾「語音辨識」。</li>
      <li>兩個服務各自保存金鑰；上面的「目前由誰處理」會顯示每個用途交給哪個網址。</li>
    </ol>
  </details>
  <ServiceList
    {services}
    onAdd={() => openWizard({})}
    onEdit={(s) => openWizard(s)}
    onChanged={refresh}
  />
{/if}

{#if message}<p class="bad">{message}</p>{/if}

<style>
  .hint {
    opacity: 0.75;
    font-size: 0.9em;
  }
  .bad {
    color: #cf222e;
  }
  .howto {
    margin: 0 0 1rem;
    font-size: 0.95em;
  }
  .howto summary {
    cursor: pointer;
  }
</style>
