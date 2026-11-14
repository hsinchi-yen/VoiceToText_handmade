<script>
  // 進階: everything that is not a common choice, one collapsed group per topic, with the
  // rarely needed tuning one level deeper under 「專家」. The outer group remembers whether it
  // was open (localStorage; may be unavailable). A dirty field shows on every enclosing header.
  import { Button, Card } from "../lib/index.js";
  import Expander from "./Expander.svelte";
  import Routes from "./Routes.svelte";
  import ConfigIoSection from "../config-io/ConfigIoSection.svelte";
  import Hotkeys from "./Hotkeys.svelte";
  import LocalModels from "./LocalModels.svelte";
  import Gpu from "./Gpu.svelte";
  import AppRules from "./AppRules.svelte";
  import HistorySettings from "./HistorySettings.svelte";
  import MicMode from "./MicMode.svelte";
  import Fallback from "./Fallback.svelte";

  let { view, save } = $props();

  const KEY = "v2c.settings.advancedOpen";
  const GPU_FIELDS = [
    "gpu_profile",
    "gpu_reserve_mb",
    "gpu_keep_loaded",
    "gpu_idle_unload_minutes",
    "gpu_yield_on_fullscreen",
    "gpu_heavy_apps",
  ];
  const GROUPS = {
    routes: ["route_asr", "local_asr", "route_polish", "route_translate"],
    hotkeys: ["hold_threshold_ms", "double_tap_window_ms", "hands_free_max_secs"],
    models: ["local_asr", "breeze_quant"],
    gpu: GPU_FIELDS,
    apps: ["apps"],
    history: ["history_enabled", "history_retention_days", "history_exclude_apps"],
    micMode: ["mic_mode"],
    cloud: ["cloud_fallback_to_local"],
  };

  function readOpen() {
    try {
      return localStorage.getItem(KEY) === "1";
    } catch {
      return false;
    }
  }
  let open = $state(readOpen());
  $effect(() => {
    try {
      localStorage.setItem(KEY, open ? "1" : "0");
    } catch {
      // Storage is unavailable: the state is just not remembered.
    }
  });

  const expert = [...GROUPS.hotkeys, ...GROUPS.gpu, ...GROUPS.micMode, ...GROUPS.cloud];
  const all = [...new Set(Object.values(GROUPS).flat())];
</script>

<Card tag="section" aria-label="進階" padding="var(--md-space-1)">
  <Expander title="進階" supporting="各用途由誰處理、地端模型、App 規則、輸入紀錄、匯出／匯入設定等" fields={all}
    dirty={view.dirty} bind:open>
    <Expander nested title="各用途由誰處理" supporting="語音辨識、潤飾、中翻英" fields={GROUPS.routes} dirty={view.dirty}>
      <Routes {view} {save} />
    </Expander>
    <Expander nested title="地端語音模型" supporting="辨識引擎、下載、測試" fields={GROUPS.models} dirty={view.dirty}>
      <LocalModels {view} {save} />
    </Expander>
    <Expander nested title="App 規則" supporting="各程式的換行鍵與貼上方式" fields={GROUPS.apps} dirty={view.dirty}>
      <AppRules {view} {save} />
    </Expander>
    <Expander nested title="輸入紀錄" fields={GROUPS.history} dirty={view.dirty}>
      <HistorySettings {view} {save} />
    </Expander>
    <Expander nested title="匯出／匯入設定" supporting="換電腦或分享給同事">
      <ConfigIoSection heading={false} />
    </Expander>
    <Expander nested title="專家" supporting="熱鍵時間、顯示卡記憶體、麥克風模式等" fields={expert} dirty={view.dirty}>
      <Expander nested title="熱鍵時間" fields={GROUPS.hotkeys} dirty={view.dirty}><Hotkeys {view} {save} /></Expander>
      <Expander nested title="顯示卡記憶體（VRAM）" fields={GROUPS.gpu} dirty={view.dirty}>
        <Gpu {view} {save} />
      </Expander>
      <Expander nested title="麥克風模式" fields={GROUPS.micMode} dirty={view.dirty}>
        <MicMode {view} {save} />
      </Expander>
      {#if view.cloud.length}
        <Expander nested title="雲端" fields={GROUPS.cloud} dirty={view.dirty}>
          <Fallback {view} {save} />
        </Expander>
      {/if}
    </Expander>
    <p class="foot">
      <Button variant="outlined" icon="refresh" href="#/onboarding">重新執行初次設定</Button>
    </p>
  </Expander>
</Card>

<style>
  .foot {
    margin: var(--md-space-2) 0 0;
    padding-inline: var(--md-space-3);
  }
</style>
