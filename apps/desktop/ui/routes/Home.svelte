<script>
  // 首頁 (T4.2): status hero, what is in use, active preset, and two one-click controls that
  // save immediately through `settings_apply` (not the Settings draft). Never shows user text.
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import * as api from "../settings/api.js";
  import { displayName } from "../cloud-setup/api.js";
  import * as presets from "../presets/api.js";
  import RuntimeStatus from "../settings/RuntimeStatus.svelte";
  import PresetDialog from "../presets/PresetDialog.svelte";
  import ConfigImportDialog from "../config-io/ConfigImportDialog.svelte";
  import { Banner, Button, Card, Icon, LinearProgress, Select, Switch } from "../lib/index.js";
  import { activePreset, asrSelectOptions, heroState } from "../home/logic.js";

  // Status codes from src-tauri/src/controller.rs (StatusEvent). Never contains user text.
  const errorText = {
    model_missing: "還沒有語音模型：請到「初次設定」下載",
    model_load_failed: "語音模型載入失敗",
    asr_loading: "語音模型載入中，請稍候",
    asr_switching: "切換語音模型中，這段語音會用新模型辨識",
    sensevoice_not_available: "SenseVoice 模型無法載入（請到「初次設定」重新下載）",
    not_configured: "未設定翻譯模型",
    inject_elevated: "目前視窗以系統管理員身分執行，無法輸入",
    inject_paste_unsupported: "此 App 需要貼上模式（尚未支援）",
    inject_partial: "只輸入了部分文字",
    inject_modifiers_held: "修飾鍵未放開，已取消輸入",
    hotkey_failed: "熱鍵安裝失敗",
    inject_no_input: "沒有使用 v2c 輸入法的輸入框，文字未輸入（已保留在「輸入紀錄」）",
    ime_disconnected: "v2c 輸入法連線中斷，文字未輸入（已保留在「輸入紀錄」）",
    mic_not_found: "找不到麥克風",
    mic_lost: "麥克風中斷，正在重新連線",
    config_invalid: "設定檔格式錯誤，已使用預設值",
    busy: "處理中，這段語音已略過",
  };

  let info = $state(null);
  let status = $state(null);
  let latency = $state(null);
  let view = $state(null);
  let presetList = $state([]);
  let presetOpen = $state(false);
  let importPath = $state(null);
  let polish = $state(true);
  let asr = $state("");
  let saving = $state(false);
  let error = $state("");
  let presetsFailed = $state(false);

  function onStatus(s) {
    if (!s) return;
    status = s;
    if (s.kind === "done" && s.chars > 0) latency = s.latency_ms;
  }

  async function loadSettings() {
    try {
      view = await api.get();
      polish = view.config.polish.enabled;
      asr = view.asr_choice;
    } catch (code) {
      error = api.describe(code);
    }
  }

  $effect(() => {
    let alive = true;
    const uns = [];
    invoke("app_info").then((i) => (info = i)).catch(() => {});
    invoke("last_status").then(onStatus).catch(() => {});
    presets.list().then((l) => (presetList = l)).catch(() => (presetsFailed = true));
    loadSettings();
    listen("v2c:status", (e) => onStatus(e.payload)).then((u) => (alive ? uns.push(u) : u()));
    listen("v2c:config-changed", loadSettings).then((u) => (alive ? uns.push(u) : u()));
    return () => {
      alive = false;
      uns.forEach((u) => u());
    };
  });

  /** Saves one control right away; puts it back and shows the reason if refused. */
  async function applyNow(changes, revert) {
    saving = true;
    error = "";
    try {
      await api.apply(changes);
    } catch (code) {
      error = api.describe(code);
      revert();
    } finally {
      saving = false;
    }
  }

  function onPolish(e) {
    const next = e.currentTarget.checked;
    applyNow([{ field: "polish_enabled", value: next }], () => (polish = !next));
  }

  function onAsr(e) {
    const next = e.currentTarget.value;
    const prev = view.asr_choice;
    applyNow(api.asrChoiceChanges(next), () => (asr = prev));
  }

  const hero = $derived(heroState(status, info));
  const heroDetail = $derived(status?.kind === "error" ? (errorText[status.code] ?? "") : "");
  const level = $derived(status?.kind === "recording" ? Math.min(1, status.rms * 8) : 0);
  const needsModel = $derived(
    status?.kind === "error" && (status.code === "model_missing" || status.code === "sensevoice_not_available"),
  );
  const presetTitle = $derived(view ? activePreset(view.config.route, presetList) : "");
  // settings_get gives cloud rows as {id, kind, supports_asr}; names live in config.cloud.
  const serviceLabel = (s) =>
    `${api.kindLabels[s.kind] ?? s.kind}：${displayName(view.config.cloud.find((c) => c.id === s.id) ?? s)}`;
  const asrOptions = $derived(view ? asrSelectOptions(api.localAsrOptions(view), view.cloud, serviceLabel) : []);
  const keys = [
    { combo: ["右 Ctrl"], text: "說話" },
    { combo: ["Shift", "右 Ctrl"], text: "中翻英" },
    { combo: ["Esc"], text: "取消" },
  ];
</script>

<div class="home">
  <h1 class="sr-only">首頁</h1>

  <section class="hero {hero.tone}" aria-live="polite">
    <div class="orb"><Icon name={hero.tone === "problem" ? "warning" : "mic"} size={36} /></div>
    <div class="hero-text">
      <p class="state">{hero.title}</p>
      {#if hero.sub}<p class="sub">{hero.sub}</p>{/if}
      {#if hero.note}<p class="sub">{hero.note}</p>{/if}
      {#if heroDetail && !hero.note}<p class="sub">{heroDetail}</p>{/if}
      {#if hero.tone === "ready" && status?.kind !== "recording"}
        <p class="sub">{latency != null ? `上次放開到出字 ${latency} ms` : "放開後文字會輸入到游標位置"}</p>
      {/if}
      {#if needsModel}
        <p class="sub"><a href="#/onboarding">前往初次設定</a>　或　<a href="#/services">改用雲端服務</a></p>
      {/if}
    </div>
    <div class="meter">
      {#if status?.kind === "recording"}<LinearProgress value={level} label="音量" />{/if}
    </div>
    <ul class="chips" aria-label="快速鍵">
      {#each keys as k (k.text)}
        <li>
          <span class="combo">{#each k.combo as c, i (c)}{#if i}<span class="plus">+</span>{/if}<kbd>{c}</kbd>{/each}</span>
          <span>{k.text}</span>
        </li>
      {/each}
    </ul>
  </section>

  {#if error}
    <Banner variant="error" onclose={() => (error = "")}>{error}</Banner>
  {/if}

  <div class="grid">
    <RuntimeStatus />

    <div class="col">
      <Card tag="section" aria-labelledby="preset-title">
        <h2 id="preset-title">預設組態</h2>
        {#if view}
          <p class="current"><Icon name="services" size={20} /> <strong>{presetTitle}</strong></p>
          {#if presetsFailed}<p class="hint">讀不到預設組態清單，目前組態可能顯示為「自訂」。</p>{/if}
        {:else}
          <p class="hint">讀取中…</p>
        {/if}
        <p class="hint">一次設定語音辨識、潤飾與中翻英要用哪一組區網服務。</p>
        <div class="row"><Button variant="tonal" icon="download" onclick={() => (presetOpen = true)}>套用預設組態</Button></div>
      </Card>

      <Card tag="section" aria-labelledby="quick-title">
        <h2 id="quick-title">常用設定</h2>
        {#if view}
          <div class="controls">
            <Switch bind:checked={polish} disabled={saving} onchange={onPolish} label="潤飾（去贅字、確保繁體）" />
            <Select
              label="語音辨識由誰處理"
              bind:value={asr}
              options={asrOptions}
              disabled={saving}
              onchange={onAsr}
            />
          </div>
        {:else}
          <p class="hint">讀取中…</p>
        {/if}
      </Card>
    </div>
  </div>

  {#if info}
    <p class="info hint">版本 {info.version}・CUDA {info.cuda ? "已啟用" : "未啟用"}</p>
  {/if}
</div>

{#if presetOpen}
  <PresetDialog onclose={() => (presetOpen = false)} onfile={(p) => (importPath = p)} />
{/if}
{#if importPath}
  <ConfigImportDialog path={importPath} onclose={() => (importPath = null)} />
{/if}

<style>
  .home {
    display: grid;
    gap: var(--md-space-4);
    max-width: 960px;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
  .hero {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: center;
    gap: var(--md-space-4) var(--md-space-5);
    padding: var(--md-space-6);
    border-radius: var(--md-radius-xl);
    background: var(--md-primary-container);
    color: var(--md-on-primary-container);
  }
  .hero.busy {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
  }
  .hero.loading {
    background: var(--md-surface-container-high);
    color: var(--md-on-surface);
  }
  .hero.problem {
    background: var(--md-error-container);
    color: var(--md-on-error-container);
  }
  .orb {
    display: grid;
    place-items: center;
    width: 72px;
    height: 72px;
    border-radius: var(--md-radius-full);
    background: var(--md-primary);
    color: var(--md-on-primary);
  }
  .hero.problem .orb {
    background: var(--md-error);
    color: var(--md-on-error);
  }
  .hero.loading .orb,
  .hero.busy .orb {
    background: var(--md-surface-container-highest);
    color: var(--md-on-surface);
  }
  .state {
    margin: 0;
    font-size: var(--md-type-headline-size);
    line-height: var(--md-type-headline-line);
    font-weight: 600;
  }
  .sub {
    margin: var(--md-space-1) 0 0;
    font-size: var(--md-type-body-size);
    opacity: 0.85;
  }
  .sub a {
    color: inherit;
  }
  .meter {
    min-height: 4px;
  }
  .meter,
  .chips {
    grid-column: 1 / -1;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--md-space-2);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .chips li {
    display: inline-flex;
    align-items: center;
    gap: var(--md-space-2);
    padding: var(--md-space-1) var(--md-space-3);
    border-radius: var(--md-radius-full);
    background: color-mix(in srgb, currentColor 10%, transparent);
    font-size: var(--md-type-label-size);
  }
  .combo {
    display: inline-flex;
    align-items: center;
    gap: var(--md-space-1);
  }
  .plus {
    opacity: 0.6;
  }
  kbd {
    padding: 1px var(--md-space-2);
    border-radius: var(--md-radius-sm);
    background: var(--md-surface);
    color: var(--md-on-surface);
    font: inherit;
    font-weight: 600;
    white-space: nowrap;
  }
  .grid {
    display: grid;
    gap: var(--md-space-4);
    align-items: start;
  }
  .col {
    display: grid;
    gap: var(--md-space-4);
  }
  @media (min-width: 720px) {
    .grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
  @media (max-width: 480px) {
    .hero {
      grid-template-columns: minmax(0, 1fr);
      padding: var(--md-space-5);
    }
  }
  h2 {
    margin: 0 0 var(--md-space-2);
    font-size: var(--md-type-title-size);
    line-height: var(--md-type-title-line);
    font-weight: var(--md-type-title-weight);
  }
  .current {
    display: flex;
    align-items: center;
    gap: var(--md-space-2);
    margin: 0 0 var(--md-space-1);
    font-size: var(--md-type-title-size);
  }
  .hint {
    margin: 0 0 var(--md-space-3);
  }
  .row {
    display: flex;
  }
  .controls {
    display: grid;
    gap: var(--md-space-5);
  }
  .info {
    margin: 0;
  }
</style>
