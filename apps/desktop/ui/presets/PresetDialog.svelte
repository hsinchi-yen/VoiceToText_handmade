<script>
  // 套用預設組態 (T4.1/T4.2): choose a preset, preview it, pick the speech recognition endpoint,
  // apply, then test each endpoint. Built from ui/lib components.
  import { onMount } from "svelte";
  import * as api from "./api.js";
  import { Banner, Button, Dialog, Icon, LinearProgress, ListItem, SegmentedButton, Select, TextField } from "../lib/index.js";

  let { onclose = () => {} } = $props();

  let open = $state(true);
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

  onMount(() => {
    api.list().then((l) => (builtins = l)).catch((c) => (error = api.describe(c)));
  });

  function finish() {
    open = false;
    onclose();
  }

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
  const asrOptions = $derived((preview?.asr_choices ?? []).map((c) => ({ value: c.id, label: c.name })));
  const testing = $derived(Object.values(tests).some((t) => t.state === "testing"));

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
    // The backend emits v2c:config-changed before presets_apply returns; tauri 2.12 runs its
    // listen_any handler (the engine reload) synchronously inside emit, so no wait is needed.
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

  const nameOf = (id) => {
    const s = preview?.services.find((x) => x.id === id);
    return s ? (s.name ?? s.id) : id === "local" ? "本機" : id;
  };
  const purposeText = (s) => s.purposes.map((p) => api.purposeLabel[p]).join("、");
  const detail = (s) =>
    [
      s.base_url,
      s.asr_model ? `語音辨識模型 ${s.asr_model}` : "",
      s.llm_model ? `文字模型 ${s.llm_model}（${api.sourceLabel[s.model_source] ?? "預設值"}）` : "",
      s.asr_timeout_ms ? `逾時 ${s.asr_timeout_ms} ms` : "",
    ]
      .filter(Boolean)
      .join("・");
  const testText = (t) =>
    !t || t.state === "testing"
      ? "測試連線中…"
      : t.error
        ? `測試失敗：${t.error}`
        : t.result.ok
          ? `連線成功（${t.result.latency_ms} ms）`
          : t.result.summary;
  const testOk = (t) => t?.state === "done" && !t.error && t.result.ok;
</script>

<Dialog bind:open title="套用預設組態" dismissible={!busy} {onclose}>
  {#if step === 1}
    <p class="hint">選擇一組預設組態，會一次設定語音辨識、潤飾與中翻英的區網服務。</p>
    <div class="list">
      {#each builtins as b (b.name)}
        <ListItem headline={b.title} supporting="{b.services.length} 個端點" onclick={() => !busy && choose({ builtin: b.name })}>
          {#snippet leading()}<Icon name="services" />{/snippet}
          {#snippet trailing()}<Icon name="chevron" size={20} />{/snippet}
        </ListItem>
      {/each}
      <ListItem headline="從檔案載入" supporting="選擇 .toml 預設組態檔" onclick={() => !busy && chooseFile()}>
        {#snippet leading()}<Icon name="upload" />{/snippet}
        {#snippet trailing()}<Icon name="chevron" size={20} />{/snippet}
      </ListItem>
    </div>
  {:else if step === 2 && preview}
    <p class="title">{preview.title}</p>
    <div class="list">
      {#each preview.services as s (s.id)}
        <ListItem
          headline={s.name ?? s.id}
          supporting="{detail(s)}　用途：{purposeText(s) || '語音辨識備選'}"
          selected={s.id === asrChoice}
        >
          {#snippet leading()}<Icon name={s.id === asrChoice ? "check" : "cloud"} />{/snippet}
        </ListItem>
      {/each}
    </div>

    {#if asrOptions.length > 4}
      <Select label="語音辨識使用" bind:value={asrChoice} options={asrOptions} />
    {:else}
      <div class="group">
        <span class="label" id="asr-label">語音辨識使用</span>
        <SegmentedButton bind:value={asrChoice} options={asrOptions} label="語音辨識使用" />
      </div>
    {/if}

    {#if needsKey}
      <TextField
        label="API 金鑰"
        type="password"
        autocomplete="off"
        spellcheck="false"
        supporting={chosenHasKey ? "已儲存；留空則沿用" : "可稍後再補"}
        bind:value={keys[asrChoice]}
      />
    {/if}

    {#if preview.ignored.length}
      <Banner variant="info">檔案中的這些區段不會使用，也不會寫入設定：{preview.ignored.join("、")}</Banner>
    {/if}
    <p class="hint">潤飾使用 {nameOf(preview.route.polish)}，中翻英使用 {nameOf(preview.route.translate)}；其他既有服務不會被更動。</p>
  {:else if step === 3 && applied}
    <Banner variant="success">已套用「{applied.title}」，設定立即生效。</Banner>
    {#if applied.key_missing.length}
      <Banner variant="warning">尚未設定金鑰：{applied.key_missing.join("、")}。到「服務」頁補上後才能連線。</Banner>
    {/if}
    {#if testing}<LinearProgress label="測試連線中" />{/if}
    <div class="list">
      {#each applied.services as s (s.id)}
        {@const t = tests[s.id]}
        <ListItem headline={s.name ?? s.id} supporting="{s.base_url}　{testText(t)}">
          {#snippet leading()}
            <span class="result" class:ok={testOk(t)} class:fail={t?.state === "done" && !testOk(t)}>
              <Icon name={testOk(t) ? "check" : t?.state === "done" ? "error" : "refresh"} />
            </span>
          {/snippet}
        </ListItem>
      {/each}
    </div>
  {/if}

  {#if error}<Banner variant="error">{error}</Banner>{/if}

  {#snippet actions()}
    {#if step === 1}
      <Button variant="text" onclick={finish}>取消</Button>
    {:else if step === 2}
      <Button variant="text" disabled={busy} onclick={back}>返回</Button>
      <Button disabled={busy || !asrChoice} onclick={apply}>套用</Button>
    {:else}
      <Button onclick={finish}>完成</Button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .hint {
    margin: 0;
    color: var(--md-on-surface-variant);
    font-size: var(--md-type-body-sm-size);
  }
  .title {
    margin: 0;
    font-size: var(--md-type-title-size);
    font-weight: var(--md-type-title-weight);
  }
  .list {
    display: grid;
    gap: var(--md-space-1);
  }
  .group {
    display: grid;
    gap: var(--md-space-2);
  }
  .label {
    font-size: var(--md-type-label-size);
    color: var(--md-on-surface-variant);
  }
  .result {
    display: inline-flex;
    color: var(--md-on-surface-variant);
  }
  .result.ok {
    color: var(--md-success);
  }
  .result.fail {
    color: var(--md-error);
  }
</style>
