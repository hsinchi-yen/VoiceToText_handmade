<script>
  // 雲端服務 page (T1.7b): connected services + 「新增雲端服務」 wizard (plan §6.2).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "../cloud-setup/api.js";
  import Wizard from "../cloud-setup/Wizard.svelte";
  import ServiceList from "../cloud-setup/ServiceList.svelte";

  let kinds = $state([]);
  let services = $state([]);
  let loaded = $state(false);
  let message = $state("");
  // null = list; {} = new; a service row = edit.
  let wizard = $state(null);

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

  function done() {
    wizard = null;
    refresh();
  }
</script>

<h1>雲端服務</h1>

{#if !loaded}
  <p>載入中…</p>
{:else if wizard}
  <Wizard
    {kinds}
    editing={wizard.id ? wizard : null}
    onDone={done}
    onCancel={() => (wizard = null)}
  />
{:else}
  <p class="hint">預設全部使用地端模型，不需要任何金鑰。串接雲端服務是選用的。</p>
  <ServiceList
    {services}
    onAdd={() => (wizard = {})}
    onEdit={(s) => (wizard = s)}
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
</style>
