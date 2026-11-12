<script>
  // Download / import the local speech models, with progress (`v2c:model-progress`).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "./api.js";
  import { Banner, Button, Card, LinearProgress } from "../lib/index.js";

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
    "tea-asr": "TEA-ASR-1.1（顯示卡，選用，約 2.2 GB 下載）",
  };
  const desc = {
    "tea-asr":
      "台灣華語與中英夾雜辨識較準，約需 2.6 GB 顯存；幾乎不加標點，靠潤飾補上。要在設定頁的「辨識引擎」選它才會使用。",
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

{#if note}<Banner variant="error">{note}</Banner>{/if}
{#if !st}
  <p>讀取中…</p>
{:else}
  <p class="hint">模型存放在 {st.models_dir}，只需要下載一次；下載中斷可以接著繼續。</p>
  <div class="models">
    {#each shown as e (e.id)}
      <Card variant="outlined" padding="var(--md-space-3) var(--md-space-4)">
        <div class="row">
          <b>{label[e.id] ?? e.id}</b>
          {#if e.installed}
            <span class="ok">已安裝</span>
          {:else if busy[e.id]}
            <Button variant="outlined" onclick={() => api.cancel(e.id)}>暫停</Button>
          {:else if e.downloadable}
            <Button variant="tonal" icon="download" onclick={() => run(e.id, () => api.download(e.id))}>
              {e.partial_bytes > 0 ? `繼續下載（已下載 ${api.mb(e.partial_bytes)}）` : `下載（${api.mb(e.size)}）`}
            </Button>
          {:else}
            <span class="hint">尚未提供下載</span>
          {/if}
        </div>
        {#if busy[e.id] && progress[e.id]}
          {@const p = progress[e.id]}
          <div class="bar"><LinearProgress value={(pct(p) ?? 0) / 100} label="下載進度" /></div>
          <span class="hint">
            {phaseText[p.phase] ?? ""} {pct(p) != null ? `${pct(p)}%（${api.mb(p.done)} / ${api.mb(p.total)}）` : ""}
          </span>
        {/if}
        {#if desc[e.id]}<p class="hint">{desc[e.id]}</p>{/if}
        {#if errors[e.id]}<p class="bad">{errors[e.id]}</p>{/if}
      </Card>
    {/each}

    {#if recommended === "breeze_q5_0"}
      <Card variant="outlined" padding="var(--md-space-3) var(--md-space-4)">
        <div class="row">
          <b>從本機檔案匯入 Breeze 模型</b>
          <Button variant="outlined" icon="upload" disabled={busy["breeze-import"]} onclick={() => run("breeze-import", api.importBreeze)}>
            選擇 .bin 檔…
          </Button>
        </div>
        <p class="hint">選擇 tools/convert-breeze 轉好的 ggml-breeze-asr-25-q5_0.bin 或 q8_0.bin，會複製到模型資料夾。</p>
        {#if busy["breeze-import"] && progress["breeze-import"]}
          <div class="bar"><LinearProgress value={(pct(progress["breeze-import"]) ?? 0) / 100} label="匯入進度" /></div>
        {/if}
        {#if errors["breeze-import"]}<p class="bad">{errors["breeze-import"]}</p>{/if}
      </Card>
    {/if}
  </div>
  {#if recommended === "breeze_q5_0"}
    <p class="hint">建議同時下載 SenseVoice：玩遊戲或顯存不足時，v2c 會自動改用它（CPU）。</p>
  {/if}
{/if}

<style>
  .models {
    display: grid;
    gap: var(--md-space-3);
    margin: var(--md-space-3) 0;
  }
  .row {
    display: flex;
    gap: var(--md-space-3);
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
  }
  .bar {
    margin: var(--md-space-2) 0 var(--md-space-1);
  }
  .ok {
    color: var(--md-success);
    font-weight: 600;
  }
  .hint {
    margin: var(--md-space-1) 0;
  }
</style>
