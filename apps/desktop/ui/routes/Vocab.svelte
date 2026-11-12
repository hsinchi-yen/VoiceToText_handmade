<script>
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import { checkVocab } from "../vocab/check.js";
  import { Banner, Button, TextField } from "../lib/index.js";

  // Defaults until `vocab_get` answers; the backend values win.
  let limits = $state({ maxEntries: 500, maxEntryChars: 40 });
  let text = $state("");
  let saved = $state("");
  // True until the file was read; stays true when reading failed (`loadFailed`).
  let loading = $state(true);
  let loadFailed = $state(false);
  let saving = $state(false);
  let message = $state("");
  let error = $state("");

  const result = $derived(checkVocab(text, limits));
  const valid = $derived(result.tooLongLines.length === 0 && result.overLimit === 0);
  const dirty = $derived(text !== saved);

  function describeError(e) {
    switch (e?.code) {
      case "invalid":
        return "詞庫內容超過限制，請修正後再儲存";
      case "storage":
        return "無法讀寫詞庫檔案";
      case "forbidden":
        return "此視窗無法編輯詞庫";
      default:
        return "發生錯誤";
    }
  }

  // One-time load. If it fails, editing stays disabled so saving cannot overwrite the file
  // with an empty list.
  onMount(() => {
    invoke("vocab_get")
      .then((v) => {
        limits = { maxEntries: v.max_entries, maxEntryChars: v.max_entry_chars };
        text = v.text;
        saved = v.text;
        loading = false;
      })
      .catch((e) => {
        loadFailed = true;
        error = `${describeError(e)}，為避免覆蓋原有詞庫，目前無法編輯。`;
      });
  });

  async function save() {
    if (loading || loadFailed) return;
    saving = true;
    message = "";
    error = "";
    try {
      const r = await invoke("vocab_set", { text });
      // Show the normalised file content (comments and duplicates removed).
      const v = await invoke("vocab_get");
      text = v.text;
      saved = v.text;
      message = `已儲存 ${r.count} 個詞`;
    } catch (e) {
      error = describeError(e);
    } finally {
      saving = false;
    }
  }
</script>

<h1>詞庫</h1>
<p class="hint lead">人名、產品名、專有名詞，一行一個。辨識與潤飾時會優先採用這裡的寫法。</p>

<TextField
  bind:value={text}
  rows={16}
  spellcheck="false"
  disabled={loading}
  placeholder={"例如：\n台積電\nLM Studio\n吳宗翰"}
  label="詞庫（一行一個）"
/>

<div class="bar">
  <span class:bad={result.overLimit > 0} class="count">{result.count} / {limits.maxEntries} 個詞</span>
  <Button icon="check" onclick={save} disabled={loading || saving || !valid || !dirty}>
    {saving ? "儲存中…" : "儲存"}
  </Button>
</div>

<div class="feedback">
  {#if result.tooLongLines.length > 0}
    <Banner variant="error">
      第 {result.tooLongLines.join("、")} 行太長（每個詞最多 {limits.maxEntryChars} 個字）。
    </Banner>
  {/if}
  {#if result.overLimit > 0}
    <Banner variant="error">詞太多：最多 {limits.maxEntries} 個，請刪除 {result.overLimit} 個。</Banner>
  {/if}
  {#if error}
    <Banner variant="error">{error}</Banner>
  {/if}
  {#if message && !dirty}
    <Banner variant="success">{message}</Banner>
  {/if}
</div>
<p class="hint">以 # 開頭的行視為註解，重複的詞（不分大小寫）只保留第一個；儲存後會整理掉。</p>

<style>
  .lead {
    margin: 0 0 var(--md-space-4);
  }
  .bar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--md-space-3);
    margin-top: var(--md-space-3);
  }
  .count {
    color: var(--md-on-surface-variant);
    font-size: var(--md-type-body-sm-size);
  }
  .count.bad {
    color: var(--md-error);
  }
  .feedback {
    display: grid;
    gap: var(--md-space-2);
    margin-top: var(--md-space-3);
  }
</style>
