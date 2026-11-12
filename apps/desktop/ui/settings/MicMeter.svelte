<script>
  // Microphone check: level meter fed by `v2c:mic-level` (levels only; audio is never kept).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "./api.js";
  import { Button, LinearProgress } from "../lib/index.js";

  let { device = null, onHeard = () => {} } = $props();

  let running = $state(false);
  let level = $state(0);
  let peak = $state(0);
  let error = $state("");

  $effect(() => {
    const un = listen("v2c:mic-level", (e) => {
      if (e.payload.done) {
        running = false;
        level = 0;
        return;
      }
      level = Math.min(1, e.payload.rms * 8);
      peak = Math.max(peak, level);
      if (level > 0.15) onHeard();
    });
    return () => {
      un.then((f) => f());
      if (running) api.micTestStop().catch(() => {});
    };
  });

  async function start() {
    error = "";
    peak = 0;
    try {
      await api.micTestStart(device);
      running = true;
    } catch (code) {
      error = api.describe(code);
    }
  }

  async function stop() {
    await api.micTestStop().catch(() => {});
  }
</script>

<div class="mic">
  {#if running}
    <Button variant="tonal" onclick={stop}>停止測試</Button>
  {:else}
    <Button variant="tonal" icon="mic" onclick={start}>測試麥克風</Button>
  {/if}
  <div class="meter"><LinearProgress value={level} label="麥克風音量" /></div>
</div>
{#if running}
  <p class="hint">{peak > 0.15 ? "有聽到聲音" : "請說幾句話…"}</p>
{/if}
{#if error}<p class="bad">{error}</p>{/if}

<style>
  .mic {
    display: flex;
    align-items: center;
    gap: var(--md-space-3);
    margin: var(--md-space-3) 0;
  }
  .meter {
    flex: 1;
    min-width: 0;
    max-width: 260px;
  }
  p {
    margin: 0 0 var(--md-space-2);
  }
</style>
