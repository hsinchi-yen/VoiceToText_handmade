<script>
  // One settings field. A changed but unsaved field gets an accent bar and a 「未儲存」 chip.
  let { dirty = false, children, ...rest } = $props();
</script>

<div class="row" class:dirty {...rest}>
  {@render children?.()}
  {#if dirty}<span class="chip">未儲存</span>{/if}
</div>

<style>
  .row {
    position: relative;
    min-width: 0;
    padding-block: var(--md-space-1);
    padding-inline-start: var(--md-space-3);
    border-inline-start: 3px solid transparent;
  }
  .row.dirty {
    border-inline-start-color: var(--md-warning);
  }
  .chip {
    position: absolute;
    top: 0;
    right: 0;
    padding: 0 var(--md-space-2);
    border-radius: var(--md-radius-full);
    background: var(--md-warning-container);
    color: var(--md-warning);
    font-size: var(--md-type-label-sm-size);
    line-height: 18px;
    white-space: nowrap;
  }
</style>
