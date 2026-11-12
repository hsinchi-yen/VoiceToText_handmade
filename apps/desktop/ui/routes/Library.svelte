<script>
  // 詞庫與紀錄: the vocabulary and the input history as two tabs (#/library?tab=vocab|history).
  import Tabs from "../lib/Tabs.svelte";
  import Vocab from "./Vocab.svelte";
  import History from "./History.svelte";
  import { DEFAULT_LIBRARY_TAB, libraryHref } from "../lib/nav.js";

  let { tab = DEFAULT_LIBRARY_TAB } = $props();

  let active = $state(DEFAULT_LIBRARY_TAB);
  // Follows the URL (a redirect or a link to the other tab).
  $effect(() => {
    active = tab;
  });

  const tabs = [
    { id: "vocab", label: "詞庫", icon: "book" },
    { id: "history", label: "輸入紀錄", icon: "history" },
  ];

  // Keep the URL in step with the selected tab without adding history entries.
  function onchange(id) {
    history.replaceState(null, "", libraryHref(id));
  }
</script>

<Tabs bind:value={active} {tabs} idPrefix="library" label="詞庫與紀錄" {onchange} />
<div class="panel" role="tabpanel" id="library-panel-{active}" aria-labelledby="library-tab-{active}">
  {#if active === "history"}
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
