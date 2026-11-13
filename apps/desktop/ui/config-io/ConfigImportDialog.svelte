<script>
  // 匯入設定 (T5.2): preview what the file would change, ask for keys, apply. The file is read
  // again when 「套用」 is pressed. Used by Settings and by 套用預設組態 → 從檔案載入.
  import { onMount } from "svelte";
  import * as api from "./api.js";
  import * as logic from "./logic.js";
  import { Banner, Button, Dialog, Icon, ListItem, SegmentedButton, Select, TextField } from "../lib/index.js";

  let { path, onclose = () => {} } = $props();

  let open = $state(true);
  let step = $state(1);
  let preview = $state(null);
  let asrChoice = $state("");
  let keys = $state({});
  let busy = $state(true);
  let error = $state("");
  let applied = $state(null);

  onMount(async () => {
    try {
      preview = await api.preview(path);
      asrChoice = preview.asr_choices[0]?.id ?? "";
      step = 2;
    } catch (err) {
      error = api.describe(err);
    } finally {
      busy = false;
    }
  });

  function finish() {
    open = false;
    onclose();
  }

  async function apply() {
    busy = true;
    error = "";
    try {
      applied = await api.apply(path, preview.file_hash, asrChoice, logic.typedKeys(preview, keys));
      keys = {};
      step = 3;
    } catch (err) {
      error = api.describe(err);
    } finally {
      busy = false;
    }
  }

  const nameOf = (s) => s.name ?? s.id;
  const asrOptions = $derived((preview?.asr_choices ?? []).map((c) => ({ value: c.id, label: c.name })));
  const counts = $derived(logic.statusCounts(preview));
  const rows = $derived(logic.routeRows(preview));
  const vocab = $derived(logic.vocabText(preview));
  const hasTypedKeys = $derived(Object.keys(logic.typedKeys(preview, keys)).length > 0);
  const keyServices = $derived((preview?.services ?? []).filter((s) => s.needs_key));
  const detail = (s) =>
    [
      s.base_url,
      s.asr_model ? `語音辨識模型 ${s.asr_model}` : "",
      s.llm_model ? `文字模型 ${s.llm_model}` : "",
      s.asr_timeout_ms ? `逾時 ${s.asr_timeout_ms} ms` : "",
    ]
      .filter(Boolean)
      .join("・");
  const purposeText = (s) => s.purposes.map((p) => logic.purposeLabel[p]).join("、");
  const keyHint = (s) =>
    s.drops_old_key
      ? "端點改變，舊金鑰會刪除；請貼上新金鑰"
      : s.has_key
        ? "已儲存金鑰；留空則沿用"
        : s.has_default_key
          ? "檔案附有預設金鑰，可不填"
          : "可稍後再補";
</script>

<Dialog bind:open title="匯入設定" dismissible={!busy} {onclose}>
  {#if step === 2 && preview}
    {#if preview.title}<p class="title">{preview.title}</p>{/if}
    {#if preview.exported_from || preview.exported_at}
      <p class="hint">
        來源：{preview.exported_from ?? "未標示"}{preview.exported_at ? `・${preview.exported_at}` : ""}
      </p>
    {/if}
    {#if preview.nothing_to_do}
      <Banner variant="info">這個檔案的內容和目前的設定相同，不需要匯入（仍可在下方補上金鑰後套用）。</Banner>
    {/if}

    {#if preview.services.length}
      <p class="hint">服務：新增 {counts.added}、更新 {counts.updated}、不變 {counts.unchanged}。其他既有服務與它們的金鑰不會被更動。</p>
      <div class="list">
        {#each preview.services as s (s.id)}
          <ListItem
            headline="{nameOf(s)}（{logic.statusLabel[s.status]}）"
            supporting="{detail(s)}{purposeText(s) ? `　用途：${purposeText(s)}` : ''}{s.drops_old_key ? '　舊金鑰會被刪除' : ''}"
          >
            {#snippet leading()}<Icon name={s.status === "unchanged" ? "check" : "cloud"} />{/snippet}
          </ListItem>
        {/each}
      </div>
    {/if}

    {#if asrOptions.length}
      {#if asrOptions.length > 4}
        <Select label="語音辨識使用" bind:value={asrChoice} options={asrOptions} />
      {:else}
        <div class="group">
          <span class="label">語音辨識使用</span>
          <SegmentedButton bind:value={asrChoice} options={asrOptions} label="語音辨識使用" />
        </div>
      {/if}
    {/if}

    {#if rows.length}
      <div class="group">
        <span class="label">用途設定的變更</span>
        {#each rows as r}
          <p class="row">{r.label}：{r.from} → {r.to}</p>
        {/each}
      </div>
    {/if}

    {#if vocab}<p class="hint">詞庫：{vocab}。</p>{/if}

    {#each keyServices as s (s.id)}
      <TextField
        label="{nameOf(s)} 的 API 金鑰"
        type="password"
        autocomplete="off"
        spellcheck="false"
        supporting={keyHint(s)}
        bind:value={keys[s.id]}
      />
    {/each}

    {#if preview.ignored.length}
      <Banner variant="info">檔案中的這些部分不會使用，也不會寫入設定：{preview.ignored.join("、")}</Banner>
    {/if}
  {:else if step === 3 && applied}
    <Banner variant="success">已匯入：{logic.appliedText(applied)}。設定立即生效。</Banner>
    {#if applied.key_missing.length}
      <Banner variant="warning">
        尚未設定金鑰：{applied.key_missing.map((id) => preview?.names?.[id] ?? id).join("、")}。到「服務」頁補上後才能連線。
      </Banner>
    {/if}
  {:else if busy}
    <p class="hint">讀取檔案中…</p>
  {/if}

  {#if error}<Banner variant="error">{error}</Banner>{/if}

  {#snippet actions()}
    {#if step === 2}
      <Button variant="text" disabled={busy} onclick={finish}>取消</Button>
      <Button disabled={busy || (asrOptions.length > 0 && !asrChoice) || preview?.nothing_to_do && !hasTypedKeys} onclick={apply}>套用</Button>
    {:else}
      <Button onclick={finish}>{step === 3 ? "完成" : "關閉"}</Button>
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
  .row {
    margin: 0;
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
</style>
