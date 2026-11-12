<script>
  // 「目前使用中」: what the running engine actually uses (`settings_runtime_status`), not what
  // config.toml says. Refreshes on status / config events and every 3 s while visible.
  import { listen } from "@tauri-apps/api/event";
  import * as api from "./api.js";
  import { Banner, Card, Icon, ListItem } from "../lib/index.js";

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
          { icon: "mic", label: "語音辨識", use: status.asr },
          { icon: "edit", label: status.polish_enabled ? "潤飾" : "潤飾（已關閉）", use: status.polish },
          { icon: "book", label: "中翻英", use: status.translate },
        ]
      : [],
  );
</script>

<Card tag="section" aria-labelledby="runtime-title" padding="var(--md-space-2)">
  <h2 id="runtime-title">目前使用中</h2>
  {#if error}
    <div class="pad"><Banner variant="error">{error}</Banner></div>
  {:else if !status}
    <p class="hint pad">讀取中…</p>
  {:else}
    <div class="list">
      {#each rows as r (r.label)}
        <ListItem headline={r.use.text} supporting={r.use.note ? `${r.label}・${r.use.note}` : r.label}>
          {#snippet leading()}<span class="badge" class:warn={r.use.warn}><Icon name={r.use.warn ? "warning" : r.icon} size={20} /></span>{/snippet}
        </ListItem>
      {/each}
      <ListItem
        headline={status.vram ? `可用 ${status.vram.free_mb} / ${status.vram.total_mb} MB` : "沒有偵測到 NVIDIA 顯示卡"}
        supporting={status.vram
          ? `顯示卡記憶體・保留 ${status.vram.reserve_mb} MB；${status.vram.gpu_engine} 約需 ${status.vram.breeze_need_mb} MB`
          : "顯示卡記憶體"}
      >
        {#snippet leading()}<span class="badge"><Icon name="gpu" size={20} /></span>{/snippet}
      </ListItem>
    </div>
  {/if}
</Card>

<style>
  h2 {
    margin: 0;
    padding: var(--md-space-3) var(--md-space-4) var(--md-space-1);
    font-size: var(--md-type-title-size);
    line-height: var(--md-type-title-line);
    font-weight: var(--md-type-title-weight);
  }
  .pad {
    margin: var(--md-space-2) var(--md-space-4) var(--md-space-3);
  }
  .list {
    display: grid;
  }
  .badge {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    border-radius: var(--md-radius-full);
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
  }
  .badge.warn {
    background: var(--md-warning-container);
    color: var(--md-warning);
  }
</style>
