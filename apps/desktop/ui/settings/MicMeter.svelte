<script>
  // Microphone check: level meter fed by `v2c:mic-level` (levels only; audio is never kept).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "./api.js";

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
    <button onclick={stop}>停止測試</button>
  {:else}
    <button onclick={start}>測試麥克風</button>
  {/if}
  <div class="meter" aria-label="麥克風音量"><div style:width={`${level * 100}%`}></div></div>
  {#if running}
    <span class="hint">{peak > 0.15 ? "有聽到聲音" : "請說幾句話…"}</span>
  {/if}
</div>
{#if error}<p class="bad">{error}</p>{/if}

<style>
  .mic {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin: 0.4rem 0;
  }
  .meter {
    flex: 1;
    max-width: 260px;
    height: 8px;
    background: color-mix(in srgb, currentColor 15%, transparent);
    border-radius: 4px;
    overflow: hidden;
  }
  .meter div {
    height: 100%;
    background: #2da44e;
    transition: width 50ms linear;
  }
  .hint {
    opacity: 0.75;
    font-size: 0.9em;
  }
  .bad {
    color: #cf222e;
  }
</style>
