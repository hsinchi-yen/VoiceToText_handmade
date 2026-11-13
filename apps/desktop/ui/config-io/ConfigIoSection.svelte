<script>
  // 匯出／匯入設定 (T5.2): bottom of the 設定 page. The exported file has no keys but does
  // carry the vocabulary.
  import * as api from "./api.js";
  import * as logic from "./logic.js";
  import { Banner, Button } from "../lib/index.js";
  import ConfigImportDialog from "./ConfigImportDialog.svelte";

  let busy = $state(false);
  let message = $state("");
  let notes = $state([]);
  let bad = $state(false);
  let importPath = $state(null);

  function show(text, isBad = false, extra = []) {
    message = text;
    bad = isBad;
    notes = extra;
  }

  async function exportConfig() {
    busy = true;
    show("");
    try {
      const path = await api.pickSave(logic.exportFileName());
      if (!path) return;
      const r = await api.exportTo(path);
      show(`已匯出 ${r.services} 個服務與 ${r.vocab} 個詞庫項目。檔案不含金鑰，但含詞庫，分享前請確認。`, false, logic.exportNotes(r));
    } catch (err) {
      show("匯出失敗：" + api.describe(err), true);
    } finally {
      busy = false;
    }
  }

  async function importConfig() {
    busy = true;
    show("");
    try {
      const path = await api.pickOpen();
      if (path) importPath = path;
    } catch (err) {
      show("無法開啟檔案：" + api.describe(err), true);
    } finally {
      busy = false;
    }
  }
</script>

<section class="io">
  <h2>匯出與匯入設定</h2>
  <p class="hint">把雲端／區網服務、各用途由誰處理與詞庫存成一個檔案，換電腦或分享給同事時匯入。檔案不含金鑰，但含詞庫，分享前請確認。</p>
  <p class="buttons">
    <Button variant="outlined" icon="upload" disabled={busy} onclick={exportConfig}>匯出設定</Button>
    <Button variant="outlined" icon="download" disabled={busy} onclick={importConfig}>匯入設定</Button>
  </p>
  {#if message}
    <Banner variant={bad ? "error" : "success"} onclose={() => show("")}>
      {message}
      {#each notes as n}<br />{n}{/each}
    </Banner>
  {/if}
</section>

{#if importPath}
  <ConfigImportDialog path={importPath} onclose={() => (importPath = null)} />
{/if}

<style>
  .io {
    display: grid;
    gap: var(--md-space-2);
  }
  h2 {
    margin: 0;
    font-size: var(--md-type-title-size);
    font-weight: var(--md-type-title-weight);
  }
  .hint {
    margin: 0;
    color: var(--md-on-surface-variant);
    font-size: var(--md-type-body-sm-size);
  }
  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: var(--md-space-2);
    margin: 0;
  }
</style>
