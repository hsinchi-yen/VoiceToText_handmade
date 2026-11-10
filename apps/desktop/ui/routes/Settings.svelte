<script>
  // 設定 page (T1.11, T1.14; plan §7). Changes are a draft until 「儲存」: the sub-pages call
  // `save(changes)` (still returns true/false), which validates the whole draft with
  // `settings_preview` and shows the result, writing nothing. 「儲存」 sends the draft to
  // `settings_apply` in one call (config.toml is rewritten, the engine reloads); 「開機自動啟動」
  // is applied after it. Leaving the page with unsaved changes asks first.
  import { listen } from "@tauri-apps/api/event";
  import * as api from "../settings/api.js";
  import RuntimeStatus from "../settings/RuntimeStatus.svelte";
  import General from "../settings/General.svelte";
  import LocalModels from "../settings/LocalModels.svelte";
  import Microphone from "../settings/Microphone.svelte";
  import AppRules from "../settings/AppRules.svelte";
  import HistorySettings from "../settings/HistorySettings.svelte";
  import Gpu from "../settings/Gpu.svelte";

  const SELF = "#/settings";
  // Not a config.toml key: applied with `settings_set_autostart` after the rest.
  const AUTOSTART = "autostart";

  /** Settings as saved in config.toml. */
  let saved = $state(null);
  /** Settings with the draft applied (from `settings_preview`). */
  let draft = $state(null);
  /** Unsaved changes, field -> value. Replaced, never mutated. */
  let pending = $state({});
  let message = $state("");
  let bad = $state(false);
  let restartNote = $state(false);
  let saving = $state(false);
  /** Settings changed elsewhere while a draft exists. */
  let incoming = $state(null);
  /** Link the user tried to follow with unsaved changes. */
  let leaveTo = $state(null);
  /** The window was closed (hidden) with unsaved changes. */
  let hiddenWithDraft = $state(false);

  const count = $derived(Object.keys(pending).length);
  const view = $derived(
    draft && {
      ...draft,
      autostart: AUTOSTART in pending ? pending[AUTOSTART] : saved.autostart,
      dirty: new Set(Object.keys(pending)),
    },
  );

  const configChanges = (p) =>
    Object.entries(p)
      .filter(([field]) => field !== AUTOSTART)
      .map(([field, value]) => ({ field, value }));
  const sameConfig = (a, b) => JSON.stringify(a.config) === JSON.stringify(b.config);

  function show(text, isBad = false) {
    message = text;
    bad = isBad;
  }

  async function load() {
    try {
      saved = await api.get();
      draft = saved;
      pending = {};
      if (saved.file_error) show(api.describe(saved.file_error) + "；顯示的是目前執行中的設定。", true);
    } catch (code) {
      show(api.describe(code), true);
    }
  }

  /** Validates `next` and makes it the draft; false (draft unchanged) if refused. */
  async function setDraft(next) {
    try {
      const changes = configChanges(next);
      let settings = saved;
      if (changes.length) {
        const r = await api.preview(changes);
        settings = r.settings;
        next = { ...next };
        for (const field of r.unchanged) delete next[field];
      }
      if (AUTOSTART in next && next[AUTOSTART] === !!saved.autostart) {
        next = { ...next };
        delete next[AUTOSTART];
      }
      pending = next;
      draft = settings;
      if (bad || message) show("");
      return true;
    } catch (code) {
      show(api.describe(code), true);
      return false;
    }
  }

  // Keeps drafts in order when controls change quickly.
  let queue = Promise.resolve(true);

  /** The sub-pages' `save`: adds changes to the draft; returns true if they are valid. */
  function save(changes) {
    queue = queue.then(() => {
      const next = { ...pending };
      for (const c of changes) next[c.field] = c.value;
      return setDraft(next);
    });
    return queue;
  }

  /** Pending fields minus those `sent` with the same value (changes made meanwhile stay). */
  function without(sent) {
    const rest = { ...pending };
    for (const [field, value] of Object.entries(sent)) {
      if (field in rest && JSON.stringify(rest[field]) === JSON.stringify(value)) delete rest[field];
    }
    return rest;
  }

  /**
   * 「儲存」: writes the draft. Runs in the same queue as `save`, so a control changed while
   * saving is added to the draft after this finishes instead of being lost. Returns true when
   * nothing is left unsaved.
   */
  function commit() {
    queue = queue.then(doCommit);
    return queue;
  }

  async function doCommit() {
    saving = true;
    const sent = { ...pending };
    let configSaved = false;
    try {
      const changes = configChanges(sent);
      if (changes.length) {
        const r = await api.apply(changes);
        configSaved = true;
        saved = r.settings;
        if (r.restart_required) restartNote = true;
        const { [AUTOSTART]: _, ...configSent } = sent;
        pending = without(configSent);
      }
      if (AUTOSTART in sent) {
        saved = { ...saved, autostart: await api.setAutostart(sent[AUTOSTART]) };
        pending = without({ [AUTOSTART]: sent[AUTOSTART] });
      }
      let draftOk = true;
      if (Object.keys(pending).length) draftOk = await setDraft({ ...pending });
      else draft = saved;
      if (draftOk) show("已儲存並套用");
      return Object.keys(pending).length === 0;
    } catch (code) {
      if (configSaved) {
        draft = saved;
        show("設定已儲存，但開機自動啟動無法變更：" + api.describe(code), true);
      } else {
        show("沒有儲存：" + api.describe(code) + "（變更仍保留，可修正後再按「儲存」）", true);
      }
      return false;
    } finally {
      saving = false;
    }
  }

  function discard() {
    pending = {};
    draft = saved;
    show("已取消變更，回到已儲存的設定");
  }

  async function leave(saveFirst) {
    const to = leaveTo;
    if (saveFirst && !(await commit())) return;
    if (!saveFirst) discard();
    leaveTo = null;
    location.hash = to;
  }

  /** Settings changed elsewhere: keep the draft on top of them. */
  async function keepDraft() {
    saved = incoming;
    incoming = null;
    const kept = { ...pending };
    if (!(await setDraft(kept))) {
      // The draft no longer fits the new settings: drop it so the page and the bar agree.
      const reason = message;
      pending = {};
      draft = saved;
      show(reason + "；草稿與新的設定衝突，已改為重新載入", true);
    }
  }

  function reloadIncoming() {
    saved = incoming;
    draft = incoming;
    pending = {};
    incoming = null;
    show("已重新載入設定");
  }

  $effect(() => {
    load();
    // The tray and the 雲端服務 page also change config.toml (and so does our own 「儲存」).
    const un = listen("v2c:config-changed", async () => {
      if (saving) return;
      let fresh;
      try {
        fresh = await api.get();
      } catch {
        return;
      }
      if (!saved || sameConfig(fresh, saved) || count === 0) {
        saved = fresh;
        if (count === 0) draft = fresh;
      } else {
        incoming = fresh;
      }
    });
    return () => un.then((f) => f());
  });

  // Leaving with unsaved changes: links inside the app ask first.
  $effect(() => {
    const onClick = (e) => {
      if (count === 0) return;
      const a = e.target.closest?.('a[href^="#"]');
      const href = a?.getAttribute("href");
      if (!href || href === SELF) return;
      e.preventDefault();
      e.stopPropagation();
      leaveTo = href;
    };
    // Closing the window only hides it (the draft stays); ask when it is shown again.
    const onVisibility = () => {
      if (document.visibilityState === "hidden" && count > 0) hiddenWithDraft = true;
    };
    document.addEventListener("click", onClick, true);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("click", onClick, true);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  });

  $effect(() => {
    if (count === 0) {
      hiddenWithDraft = false;
      leaveTo = null;
    }
  });
</script>

<div class="settings-page">
  <h1>設定</h1>

  <div class="bar" class:pending={count > 0}>
    {#if leaveTo}
      <span>有 {count} 項變更尚未儲存，要離開設定頁嗎？</span>
      <span class="actions">
        <button onclick={() => leave(true)} disabled={saving}>{saving ? "處理中…" : "儲存後離開"}</button>
        <button onclick={() => leave(false)} disabled={saving}>捨棄變更並離開</button>
        <button onclick={() => (leaveTo = null)} disabled={saving}>留在設定頁</button>
      </span>
    {:else if count > 0}
      <span>
        {hiddenWithDraft ? `上次關閉視窗時，還有 ${count} 項變更尚未儲存` : `有 ${count} 項變更尚未儲存`}
      </span>
      <span class="actions">
        <button class="primary" onclick={commit} disabled={saving}>{saving ? "處理中…" : "儲存"}</button>
        <button onclick={discard} disabled={saving}>取消變更</button>
        {#if hiddenWithDraft}<button onclick={() => (hiddenWithDraft = false)}>繼續編輯</button>{/if}
      </span>
    {:else}
      <span class="ok">所有設定已儲存</span>
    {/if}
    {#if message}<span class="msg" class:bad class:ok={!bad}>{message}</span>{/if}
    {#if incoming}
      <span class="msg">
        設定已在別處變更（系統列或雲端服務頁）。
        <button onclick={reloadIncoming}>重新載入（捨棄草稿）</button>
        <button onclick={keepDraft}>保留草稿</button>
      </span>
    {/if}
  </div>
  {#if restartNote}
    <p class="hint">麥克風與輸入紀錄的變更會在重新啟動 v2c 後生效（系統列 → 結束，再開啟）。</p>
  {/if}

  <RuntimeStatus />

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
</div>

<style>
  .bar {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.35rem 0.75rem;
    margin: 0 0 0.75rem;
    padding: 0.45rem 0.7rem;
    border-radius: 6px;
    background: color-mix(in srgb, #2da44e 12%, #f6f8fa);
  }
  .bar.pending {
    background: color-mix(in srgb, #d29922 20%, #f6f8fa);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  .msg {
    flex-basis: 100%;
  }
  .primary {
    font-weight: 600;
  }
  .settings-page :global(.ok) {
    color: #1a7f37;
  }
  .settings-page :global(.bad) {
    color: #cf222e;
  }
  .settings-page :global(.hint) {
    opacity: 0.7;
    font-size: 0.88em;
    margin: 0.2rem 0 0.4rem;
  }
  .settings-page :global(.link) {
    background: none;
    border: none;
    padding: 0;
    color: #2f81f7;
    cursor: pointer;
    text-decoration: underline;
    font: inherit;
  }
  @media (prefers-color-scheme: dark) {
    .bar {
      background: color-mix(in srgb, #2da44e 15%, #0d1117);
    }
    .bar.pending {
      background: color-mix(in srgb, #d29922 22%, #0d1117);
    }
    .settings-page :global(.ok) {
      color: #3fb950;
    }
    .settings-page :global(.bad) {
      color: #f85149;
    }
  }
  :global(.settings-section) {
    margin: 0 0 1.25rem;
    padding: 0.75rem 1rem;
    border: 1px solid color-mix(in srgb, currentColor 18%, transparent);
    border-radius: 8px;
    overflow-wrap: anywhere;
  }
  :global(.settings-section h2) {
    margin: 0 0 0.5rem;
    font-size: 1.05rem;
  }
  :global(.settings-section label) {
    display: block;
    margin: 0.35rem 0;
  }
  :global(.settings-section select) {
    max-width: 100%;
  }
  /* A changed but unsaved field. */
  :global(.settings-section .dirty) {
    border-left: 3px solid #d29922;
    padding-left: 0.45rem;
  }
  :global(.settings-section .dirty::after) {
    content: "未儲存";
    margin-left: 0.4em;
    padding: 0 0.3em;
    border: 1px solid #d29922;
    border-radius: 4px;
    color: #9a6700;
    font-size: 0.75em;
    white-space: nowrap;
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
