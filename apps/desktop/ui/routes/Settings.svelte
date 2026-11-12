<script>
  // 設定 page (T1.11, T1.14; plan §7). Changes are a draft until 「儲存」: the sub-pages call
  // `save(changes)` (still returns true/false), which validates the whole draft with
  // `settings_preview` and shows the result, writing nothing. 「儲存」 sends the draft to
  // `settings_apply` in one call (config.toml is rewritten, the engine reloads); 「開機自動啟動」
  // is applied after it. Leaving the page with unsaved changes asks first.
  import { listen } from "@tauri-apps/api/event";
  import * as api from "../settings/api.js";
  import { Banner, Button, Card } from "../lib/index.js";
  import General from "../settings/General.svelte";
  import Microphone from "../settings/Microphone.svelte";
  import Advanced from "../settings/Advanced.svelte";

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
  <p class="top">
    <a href="#/">查看目前使用中 → 首頁</a>
    {#if count === 0 && !leaveTo}<span class="ok">所有設定已儲存</span>{/if}
  </p>
  {#if restartNote}
    <Banner variant="info">麥克風與輸入紀錄的變更會在重新啟動 v2c 後生效（系統列 → 結束，再開啟）。</Banner>
  {/if}

  {#if !view}
    <p>載入中…</p>
  {:else}
    <Microphone {view} {save} />
    <General {view} {save} />
    <Advanced {view} {save} />
    <p class="foot">
      <Button variant="outlined" icon="refresh" href="#/onboarding">重新執行初次設定</Button>
    </p>
  {/if}

  {#if count > 0 || leaveTo || message || incoming}
    <div class="dock">
      {#if incoming}
        <Banner variant="warning">
          設定已在別處變更（系統列或雲端服務頁）。
          {#snippet actions()}
            <Button variant="tonal" onclick={reloadIncoming}>重新載入（捨棄草稿）</Button>
            <Button variant="text" onclick={keepDraft}>保留草稿</Button>
          {/snippet}
        </Banner>
      {/if}
      {#if message}
        <Banner variant={bad ? "error" : "success"} onclose={() => show("")}>{message}</Banner>
      {/if}
      {#if leaveTo}
        <Card variant="elevated" padding="0"><div class="bar">
          <span>有 {count} 項變更尚未儲存，要離開設定頁嗎？</span>
          <span class="actions">
            <Button onclick={() => leave(true)} disabled={saving}>{saving ? "處理中…" : "儲存後離開"}</Button>
            <Button variant="outlined" onclick={() => leave(false)} disabled={saving}>捨棄變更並離開</Button>
            <Button variant="text" onclick={() => (leaveTo = null)} disabled={saving}>留在設定頁</Button>
          </span>
        </div></Card>
      {:else if count > 0}
        <Card variant="elevated" padding="0"><div class="bar">
          <span>
            {hiddenWithDraft ? `上次關閉視窗時，還有 ${count} 項變更尚未儲存` : `有 ${count} 項變更尚未儲存`}
          </span>
          <span class="actions">
            <Button onclick={commit} disabled={saving}>{saving ? "處理中…" : "儲存"}</Button>
            <Button variant="outlined" onclick={discard} disabled={saving}>取消變更</Button>
            {#if hiddenWithDraft}<Button variant="text" onclick={() => (hiddenWithDraft = false)}>繼續編輯</Button>{/if}
          </span>
        </div></Card>
      {/if}
    </div>
  {/if}
</div>

<style>
  .settings-page {
    display: grid;
    gap: var(--md-space-4);
    min-width: 0;
  }
  .settings-page > :global(*) {
    min-width: 0;
  }
  h1 {
    margin: 0;
  }
  .top {
    display: flex;
    flex-wrap: wrap;
    gap: var(--md-space-2) var(--md-space-4);
    margin: 0;
  }
  .foot {
    margin: 0;
  }
  /* Floating save bar: stays at the bottom of the window (above the bottom menu on phones). */
  .dock {
    position: sticky;
    bottom: var(--md-space-4);
    z-index: 5;
    display: grid;
    gap: var(--md-space-2);
  }
  .dock :global(.bar) {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--md-space-2) var(--md-space-4);
    padding: var(--md-space-3) var(--md-space-4);
    border: 1px solid var(--md-warning);
    border-radius: var(--md-radius-lg);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--md-space-2);
  }
  @media (max-width: 599px) {
    .dock {
      bottom: calc(var(--md-nav-bar-height) + var(--md-space-2));
    }
  }
</style>
