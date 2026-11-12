<script>
  // Surface container. variant: filled (default) | elevated | outlined. `tag` changes the
  // element (e.g. "section", "li"); give it `aria-labelledby` or a heading for landmarks.
  let { variant = "filled", tag = "div", padding = "var(--md-space-4)", children, ...rest } = $props();
</script>

<svelte:element this={tag} class="card {variant}" style:padding {...rest}>
  {@render children?.()}
</svelte:element>

<style>
  .card {
    min-width: 0;
    border-radius: var(--md-radius-lg);
    background: var(--md-surface-container);
    color: var(--md-on-surface);
    overflow-wrap: anywhere;
  }
  .elevated {
    background: var(--md-surface-container-low);
    box-shadow:
      0 1px 2px rgb(0 0 0 / 0.14),
      0 2px 6px rgb(0 0 0 / 0.1);
  }
  .outlined {
    background: var(--md-surface);
    border: 1px solid var(--md-outline-variant);
  }
</style>
