<script>
  // 套用預設組態 (T4.1): choose a preset, preview it, pick the speech recognition endpoint,
  // apply, then test each endpoint. Plain HTML + CSS; colours come from --md-* tokens.
  import * as api from "./api.js";

  let { onclose = () => {} } = $props();

  let step = $state(1);
  let builtins = $state([]);
  let source = $state(null);
  let preview = $state(null);
  let asrChoice = $state("");
  let keys = $state({});
  let busy = $state(false);
  let error = $state("");
  let applied = $state(null);
  // id -> { state: "testing" | "done", result?, error? }
  let tests = $state({});

  $effect(() => {
    api.list().then((l) => (builtins = l)).catch((c) => (error = api.describe(c)));
  });

  async function choose(src) {
    busy = true;
    error = "";
    try {
      preview = await api.preview(src);
      source = src;
      asrChoice = preview.asr_choices[0]?.id ?? "";
      keys = {};
      step = 2;
    } catch (code) {
      error = api.describe(code);
    } finally {
      busy = false;
    }
  }

  async function chooseFile() {
    error = "";
    try {
      const path = await api.pickFile();
      if (path) await choose({ file: path });
    } catch (code) {
      error = api.describe(code);
    }
  }

  function back() {
    step = 1;
    error = "";
  }

  const needsKey = $derived(preview?.needs_key.includes(asrChoice) ?? false);
  const chosenHasKey = $derived(
    preview?.services.find((s) => s.id === asrChoice)?.has_key ?? false,
  );

  async function apply() {
    busy = true;
    error = "";
    const pasted = {};
    for (const id of preview.needs_key) {
      const v = (keys[id] ?? "").trim();
      if (v) pasted[id] = v;
    }
    try {
      applied = await api.apply(source, asrChoice, pasted);
      keys = {};
      step = 3;
      runTests();
    } catch (code) {
      error = api.describe(code);
    } finally {
      busy = false;
    }
  }

  async function runTests() {
    tests = Object.fromEntries(applied.services.map((s) => [s.id, { state: "testing" }]));
    // The running config reloads on v2c:config-changed; give it a moment so stored keys apply.
    await new Promise((r) => setTimeout(r, 600));
    await Promise.all(
      applied.services.map(async (s) => {
        try {
          tests[s.id] = { state: "done", result: await api.testService(s) };
        } catch (code) {
          tests[s.id] = { state: "done", error: api.describe(code) };
        }
      }),
    );
  }

  const purposeText = (s) => s.purposes.map((p) => api.purposeLabel[p]).join("、");
</script>

<div class="backdrop" role="presentation">
  <div class="dialog" role="dialog" aria-modal="true" aria-label="套用預設組態">
    <h2>套用預設組態</h2>

    {#if step === 1}
      <p class="hint">選擇一組預設組態，會一次設定語音辨識、潤飾與中翻英的區網服務。</p>
      <div class="choices">
        {#each builtins as b (b.name)}
          <button class="choice" disabled={busy} onclick={() => choose({ builtin: b.name })}>
            <strong>{b.title}</strong>
            <span>{b.services.length} 個端點</span>
          </button>
        {/each}
        <button class="choice" disabled={busy} onclick={chooseFile}>
          <strong>從檔案載入</strong>
          <span>選擇 .toml 預設組態檔</span>
        </button>
      </div>
    {:else if step === 2 && preview}
      <h3>{preview.title}</h3>
      <ul class="services">
        {#each preview.services as s (s.id)}
          <li class:chosen={s.id === asrChoice}>
            <div class="name">{s.name ?? s.id}</div>
            <div class="url">{s.base_url}</div>
            <div class="meta">
              {#if s.asr_model}語音辨識模型：{s.asr_model}　{/if}
              {#if s.llm_model}
                文字模型：{s.llm_model}（{api.sourceLabel[s.model_source] ?? "預設值"}）
              {/if}
              {#if s.asr_timeout_ms}逾時 {s.asr_timeout_ms} ms{/if}
            </div>
            <div class="meta">用途：{purposeText(s) || "（語音辨識備選）"}</div>
          </li>
        {/each}
      </ul>

      <fieldset>
        <legend>語音辨識使用</legend>
        {#each preview.asr_choices as c (c.id)}
          <label>
            <input type="radio" name="asr" value={c.id} bind:group={asrChoice} />
            {c.name}
          </label>
        {/each}
      </fieldset>

      {#if needsKey}
        <label class="key">
          API 金鑰{chosenHasKey ? "（已儲存；留空則沿用）" : "（可稍後再補）"}
          <input
            type="password"
            autocomplete="off"
            spellcheck="false"
            bind:value={keys[asrChoice]}
          />
        </label>
      {/if}

      {#if preview.ignored.length}
        <p class="hint">
          檔案中的這些區段不會使用，也不會寫入設定：{preview.ignored.join("、")}
        </p>
      {/if}
      <p class="hint">潤飾與中翻英會使用 {preview.route.polish}；其他既有服務不會被更動。</p>
    {:else if step === 3 && applied}
      <p class="ok">已套用「{applied.title}」，設定立即生效。</p>
      {#if applied.key_missing.length}
        <p class="warn">
          尚未設定金鑰：{applied.key_missing.join("、")}。到「雲端服務」頁補上後才能連線。
        </p>
      {/if}
      <ul class="services">
        {#each applied.services as s (s.id)}
          {@const t = tests[s.id]}
          <li>
            <div class="name">{s.name ?? s.id}</div>
            <div class="url">{s.base_url}</div>
            <div class="meta">
              {#if !t || t.state === "testing"}
                測試連線中…
              {:else if t.error}
                <span class="bad">測試失敗：{t.error}</span>
              {:else if t.result.ok}
                <span class="good">連線成功（{t.result.latency_ms} ms）</span>
              {:else}
                <span class="bad">{t.result.summary}</span>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}

    {#if error}<p class="bad" role="alert">{error}</p>{/if}

    <div class="actions">
      {#if step === 1}
        <button onclick={onclose}>取消</button>
      {:else if step === 2}
        <button disabled={busy} onclick={back}>返回</button>
        <button class="primary" disabled={busy || !asrChoice} onclick={apply}>套用</button>
      {:else}
        <button class="primary" onclick={onclose}>完成</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.45);
    display: grid;
    place-items: center;
    z-index: 50;
  }
  .dialog {
    width: min(560px, calc(100vw - 32px));
    max-height: calc(100vh - 48px);
    overflow-y: auto;
    padding: 20px 24px;
    border-radius: var(--md-shape-large, 16px);
    background: var(--md-surface-container-high, Canvas);
    color: var(--md-on-surface, CanvasText);
    box-shadow: 0 8px 32px rgb(0 0 0 / 0.35);
  }
  h2,
  h3 {
    margin: 0 0 12px;
  }
  .hint {
    color: var(--md-on-surface-variant, GrayText);
    font-size: 0.9rem;
  }
  .choices {
    display: grid;
    gap: 8px;
  }
  .choice {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 12px 16px;
    text-align: left;
    cursor: pointer;
    border: 1px solid var(--md-outline-variant, color-mix(in srgb, currentColor 25%, transparent));
    border-radius: var(--md-shape-medium, 12px);
    background: var(--md-surface-container, transparent);
    color: inherit;
    font: inherit;
  }
  .choice span {
    font-size: 0.85rem;
    color: var(--md-on-surface-variant, GrayText);
  }
  .services {
    list-style: none;
    padding: 0;
    margin: 0 0 12px;
    display: grid;
    gap: 8px;
  }
  .services li {
    padding: 10px 12px;
    border: 1px solid var(--md-outline-variant, color-mix(in srgb, currentColor 25%, transparent));
    border-radius: var(--md-shape-medium, 12px);
  }
  .services li.chosen {
    border-color: var(--md-primary, #2f81f7);
  }
  .name {
    font-weight: 600;
  }
  .url {
    font-family: monospace;
    font-size: 0.85rem;
    word-break: break-all;
  }
  .meta {
    font-size: 0.85rem;
    color: var(--md-on-surface-variant, GrayText);
  }
  fieldset {
    margin: 0 0 12px;
    border: 1px solid var(--md-outline-variant, color-mix(in srgb, currentColor 25%, transparent));
    border-radius: var(--md-shape-medium, 12px);
    display: grid;
    gap: 6px;
  }
  .key {
    display: grid;
    gap: 4px;
    margin-bottom: 12px;
  }
  .key input {
    padding: 8px 10px;
    font: inherit;
    border-radius: var(--md-shape-small, 8px);
    border: 1px solid var(--md-outline, color-mix(in srgb, currentColor 40%, transparent));
    background: var(--md-surface, transparent);
    color: inherit;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  .actions button {
    padding: 8px 20px;
    font: inherit;
    cursor: pointer;
    border-radius: var(--md-shape-full, 999px);
    border: 1px solid var(--md-outline, color-mix(in srgb, currentColor 40%, transparent));
    background: transparent;
    color: inherit;
  }
  .actions .primary {
    background: var(--md-primary, #2f81f7);
    color: var(--md-on-primary, #fff);
    border-color: transparent;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .good,
  .ok {
    color: var(--md-success, #1a7f37);
  }
  .bad {
    color: var(--md-error, #d1242f);
  }
  .warn {
    color: var(--md-warning, #9a6700);
  }
</style>
