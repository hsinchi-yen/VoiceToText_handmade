<script>
  // 詞庫與紀錄: the vocabulary and the input history as two tabs (#/library?tab=vocab|history).
  // The URL is the single source of truth: `tab` comes from it (App.svelte), and selecting a
  // tab replaces the hash (no history entry, but `hashchange` fires so App stays in step).
  import Tabs from "../lib/Tabs.svelte";
  import Vocab from "./Vocab.svelte";
  import History from "./History.svelte";
  import { DEFAULT_LIBRARY_TAB, libraryHref } from "../lib/nav.js";

  let { tab = DEFAULT_LIBRARY_TAB } = $props();

  const tabs = [
    { id: "vocab", label: "詞庫", icon: "book" },
    { id: "history", label: "輸入紀錄", icon: "history" },
  ];

  const onchange = (id) => location.replace(libraryHref(id));
</script>

<Tabs value={tab} {tabs} idPrefix="library" label="詞庫與紀錄" {onchange} />
<div class="panel" role="tabpanel" id="library-panel-{tab}" aria-labelledby="library-tab-{tab}">
  {#if tab === "history"}
    <History />
  {:else}
    <Vocab />
  {/if}
</div>

<style>
  .panel {
    padding-top: var(--md-space-4);
  }
</style>
