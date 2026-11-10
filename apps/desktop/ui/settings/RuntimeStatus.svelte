<script>
  // 「目前使用中」: what the running engine actually uses (`settings_runtime_status`), not what
  // config.toml says. Refreshes on status / config events and every 3 s while visible.
  import { listen } from "@tauri-apps/api/event";
  import * as api from "./api.js";

  let status = $state(null);
  let error = $state("");

  async function refresh() {
    if (document.visibilityState === "hidden") return;
    try {
      status = await api.runtimeStatus();
      error = "";
    } catch (code) {
      error = api.describe(code);
    }
  }

  $effect(() => {
    refresh();
    const poll = setInterval(refresh, 3000);
    // Status events arrive every 50 ms while recording: refresh once things settle.
    let timer;
    const soon = () => {
      clearTimeout(timer);
      timer = setTimeout(refresh, 400);
    };
    const uns = [listen("v2c:status", soon), listen("v2c:config-changed", soon)];
    return () => {
      clearInterval(poll);
      clearTimeout(timer);
      uns.forEach((u) => u.then((f) => f()));
    };
  });

  const rows = $derived(
    status
      ? [
          { label: "語音辨識", use: status.asr },
          { label: `潤飾（${status.polish_enabled ? "開啟" : "關閉"}）`, use: status.polish },
          { label: "中翻英", use: status.translate },
        ]
      : [],
  );
</script>

<section class="settings-section runtime">
  <h2>目前使用中</h2>
  {#if error}
    <p class="bad">{error}</p>
  {:else if !status}
    <p class="hint">讀取中…</p>
  {:else}
    <dl>
      {#each rows as r (r.label)}
        <dt>{r.label}</dt>
        <dd>
          {r.use.text}
          {#if r.use.note}<span class="note" class:bad={r.use.warn}>{r.use.note}</span>{/if}
        </dd>
      {/each}
      <dt>顯示卡記憶體</dt>
      <dd>
        {#if status.vram}
          可用 {status.vram.free_mb} / {status.vram.total_mb} MB（保留 {status.vram.reserve_mb} MB；Breeze 約需
          {status.vram.breeze_need_mb} MB）
        {:else}
          沒有偵測到 NVIDIA 顯示卡
        {/if}
      </dd>
    </dl>
    <p class="hint">這裡是 v2c 實際在用的引擎與服務；設定按「儲存」之後才會改變。</p>
  {/if}
</section>

<style>
  dl {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 0.25rem 0.75rem;
    margin: 0 0 0.4rem;
  }
  dt {
    font-weight: 600;
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .note {
    display: block;
    font-size: 0.88em;
    opacity: 0.75;
  }
  .note.bad {
    opacity: 1;
  }
  @media (max-width: 420px) {
    dl {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
