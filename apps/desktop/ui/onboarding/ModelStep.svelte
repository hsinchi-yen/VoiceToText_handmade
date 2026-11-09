<script>
  // Download / import the local speech models, with progress (`v2c:model-progress`).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "./api.js";

  let { recommended, onInstalled } = $props();

  let st = $state(null);
  let progress = $state({});
  let errors = $state({});
  let busy = $state({});
  let note = $state("");

  async function refresh() {
    try {
      st = await api.models();
    } catch (code) {
      note = api.describe(code);
    }
  }

  $effect(() => {
    refresh();
    const un = listen("v2c:model-progress", (e) => {
      const p = e.payload;
      progress[p.id] = p;
    });
    return () => un.then((f) => f());
  });

  const label = {
    sensevoice: "SenseVoice-Small（CPU，約 160 MB 下載）",
    "breeze-q5_0": "Breeze-ASR-25 q5_0（顯示卡，建議）",
    "breeze-q8_0": "Breeze-ASR-25 q8_0（顯示卡，較準、較大）",
  };
  const phaseText = {
    download: "下載中",
    verify: "驗證中",
    extract: "解壓縮中",
    copy: "複製中",
  };

  // No NVIDIA GPU: only SenseVoice is offered.
  const shown = $derived(
    (st?.entries ?? []).filter((e) => recommended === "breeze_q5_0" || e.kind === "sensevoice_archive"),
  );

  async function run(id, fn) {
    errors[id] = "";
    busy[id] = true;
    try {
      const r = await fn();
      await refresh();
      // The import returns null when the file dialog was closed.
      if (!(id === "breeze-import" && r === null)) onInstalled();
    } catch (code) {
      errors[id] = api.describe(code);
      await refresh();
    } finally {
      busy[id] = false;
      delete progress[id];
    }
  }

  const pct = (p) => (p?.total ? Math.floor((p.done / p.total) * 100) : null);
</script>

{#if note}<p class="bad">{note}</p>{/if}
{#if !st}
  <p>讀取中…</p>
{:else}
  <p class="hint">模型存放在 {st.models_dir}，只需要下載一次；下載中斷可以接著繼續。</p>
  {#each shown as e (e.id)}
    <div class="model">
      <div class="row">
        <b>{label[e.id] ?? e.id}</b>
        {#if e.installed}
          <span class="ok">已安裝</span>
        {:else if busy[e.id]}
          <button onclick={() => api.cancel(e.id)}>暫停</button>
        {:else if e.downloadable}
          <button onclick={() => run(e.id, () => api.download(e.id))}>
            {e.partial_bytes > 0 ? `繼續下載（已下載 ${api.mb(e.partial_bytes)}）` : `下載（${api.mb(e.size)}）`}
          </button>
        {:else}
          <span class="hint">尚未提供下載</span>
        {/if}
      </div>
      {#if busy[e.id] && progress[e.id]}
        {@const p = progress[e.id]}
        <div class="bar"><div style:width={`${pct(p) ?? 0}%`}></div></div>
        <span class="hint">
          {phaseText[p.phase] ?? ""} {pct(p) != null ? `${pct(p)}%（${api.mb(p.done)} / ${api.mb(p.total)}）` : ""}
        </span>
      {/if}
      {#if errors[e.id]}<p class="bad">{errors[e.id]}</p>{/if}
    </div>
  {/each}

  {#if recommended === "breeze_q5_0"}
    <div class="model">
      <div class="row">
        <b>從本機檔案匯入 Breeze 模型</b>
        <button disabled={busy["breeze-import"]} onclick={() => run("breeze-import", api.importBreeze)}>
          選擇 .bin 檔…
        </button>
      </div>
      <p class="hint">選擇 tools/convert-breeze 轉好的 ggml-breeze-asr-25-q5_0.bin 或 q8_0.bin，會複製到模型資料夾。</p>
      {#if busy["breeze-import"] && progress["breeze-import"]}
        <div class="bar"><div style:width={`${pct(progress["breeze-import"]) ?? 0}%`}></div></div>
      {/if}
      {#if errors["breeze-import"]}<p class="bad">{errors["breeze-import"]}</p>{/if}
    </div>
    <p class="hint">建議同時下載 SenseVoice：玩遊戲或顯存不足時，v2c 會自動改用它（CPU）。</p>
  {/if}
{/if}

<style>
  .model {
    margin: 0.6rem 0;
    padding: 0.5rem 0.75rem;
    border: 1px solid color-mix(in srgb, currentColor 15%, transparent);
    border-radius: 6px;
  }
  .row {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
  }
  .bar {
    height: 8px;
    margin: 0.4rem 0 0.2rem;
    background: color-mix(in srgb, currentColor 15%, transparent);
    border-radius: 4px;
    overflow: hidden;
  }
  .bar div {
    height: 100%;
    background: #2f81f7;
  }
  .ok {
    color: #2da44e;
    font-weight: 600;
  }
  .hint {
    opacity: 0.75;
    font-size: 0.9em;
  }
  .bad {
    color: #cf222e;
  }
</style>
