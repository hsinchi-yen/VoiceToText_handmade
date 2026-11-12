<script>
  // Collapsible group built from a ListItem button. Content is mounted the first time it is
  // opened and then only hidden, so local state (test results, text drafts) survives.
  // `fields` are the draft fields inside; the header shows how many are unsaved.
  import { Icon, ListItem } from "../lib/index.js";

  let { title, supporting = "", fields = [], dirty = null, open = $bindable(false), nested = false, children } = $props();
  const uid = $props.id();

  let opened = $state(false);
  const count = $derived(fields.filter((f) => dirty?.has(f)).length);
  $effect(() => {
    if (open) opened = true;
  });
</script>

<div class="expander" class:nested>
  <ListItem headline={title} {supporting} onclick={() => (open = !open)}
    aria-expanded={open} aria-controls={uid}>
    {#snippet trailing()}
      {#if count > 0}<span class="chip">{count} 項未儲存</span>{/if}
      <span class="chev" class:open><Icon name="chevron-down" /></span>
    {/snippet}
  </ListItem>
  <div id={uid} class="body" hidden={!open}>
    {#if opened}{@render children?.()}{/if}
  </div>
</div>

<style>
  .expander {
    min-width: 0;
  }
  .body {
    display: grid;
    gap: var(--md-space-3);
    padding: var(--md-space-2) var(--md-space-4) var(--md-space-4);
  }
  .body[hidden] {
    display: none;
  }
  .nested .body {
    padding-inline: var(--md-space-3);
  }
  .chip {
    margin-right: var(--md-space-2);
    padding: 0 var(--md-space-2);
    border-radius: var(--md-radius-full);
    background: var(--md-warning-container);
    color: var(--md-warning);
    font-size: var(--md-type-label-sm-size);
    line-height: 18px;
    white-space: nowrap;
  }
  .chev {
    display: inline-flex;
    transition: transform var(--md-duration-medium) var(--md-ease-standard);
  }
  .chev.open {
    transform: rotate(180deg);
  }
</style>
