<script>
  // 設定 page (T1.11, plan §7). Every control saves right away through `settings_apply`
  // (only that key is rewritten in config.toml) and the engine reloads.
  import { listen } from "@tauri-apps/api/event";
  import * as api from "../settings/api.js";
  import General from "../settings/General.svelte";
  import LocalModels from "../settings/LocalModels.svelte";
  import Microphone from "../settings/Microphone.svelte";
  import AppRules from "../settings/AppRules.svelte";
  import HistorySettings from "../settings/HistorySettings.svelte";
  import Gpu from "../settings/Gpu.svelte";

  let view = $state(null);
  let message = $state("");
  let bad = $state(false);
  let restartNote = $state(false);
  let saving = $state(false);

  async function load() {
    try {
      view = await api.get();
      if (view.file_error) {
        bad = true;
        message = api.describe(view.file_error) + "；顯示的是目前執行中的設定。";
      }
    } catch (code) {
      bad = true;
      message = api.describe(code);
    }
  }

  /** Saves changes; returns true on success. */
  async function save(changes) {
    saving = true;
    try {
      const r = await api.apply(changes);
      view = r.settings;
      if (r.restart_required) restartNote = true;
      bad = false;
      message = "已儲存";
      return true;
    } catch (code) {
      bad = true;
      message = api.describe(code);
      return false;
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    load();
    // The tray and the 雲端服務 page also change config.toml.
    const un = listen("v2c:config-changed", () => {
      if (!saving) load();
    });
    return () => un.then((f) => f());
  });
</script>

<h1>設定</h1>

{#if message}
  <p class="msg" class:bad>{message}</p>
{/if}
{#if restartNote}
  <p class="msg">麥克風與輸入紀錄的變更會在重新啟動 v2c 後生效（系統列 → 結束，再開啟）。</p>
{/if}

{#if !view}
  <p>載入中…</p>
{:else}
  <General {view} {save} />
  <LocalModels {view} {save} />
  <Microphone {view} {save} />
  <AppRules {view} {save} />
  <HistorySettings {view} {save} />
  <Gpu {view} {save} />
{/if}

<style>
  .msg {
    position: sticky;
    top: 0;
    margin: 0 0 0.75rem;
    padding: 0.4rem 0.6rem;
    border-radius: 6px;
    background: color-mix(in srgb, #2da44e 15%, transparent);
    z-index: 1;
  }
  .msg.bad {
    background: color-mix(in srgb, #cf222e 18%, transparent);
  }
  :global(.settings-section) {
    margin: 0 0 1.25rem;
    padding: 0.75rem 1rem;
    border: 1px solid color-mix(in srgb, currentColor 18%, transparent);
    border-radius: 8px;
  }
  :global(.settings-section h2) {
    margin: 0 0 0.5rem;
    font-size: 1.05rem;
  }
  :global(.settings-section label) {
    display: block;
    margin: 0.35rem 0;
  }
  :global(.settings-section .hint) {
    opacity: 0.7;
    font-size: 0.88em;
    margin: 0.2rem 0 0.4rem;
  }
  :global(.settings-section input[type="number"]) {
    width: 6.5em;
  }
  :global(.settings-section textarea) {
    width: 100%;
    box-sizing: border-box;
    font-family: inherit;
  }
</style>
