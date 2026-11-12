<script>
  // List row: optional leading/trailing content, headline and supporting text.
  // With `href` it is a link, with `onclick` a button, otherwise a plain row.
  let { headline = "", supporting = "", href = undefined, onclick = undefined, selected = false, leading, trailing, children, ...rest } = $props();
</script>

{#snippet body()}
  {#if leading}<span class="leading">{@render leading()}</span>{/if}
  <span class="content">
    {#if headline}<span class="headline">{headline}</span>{/if}
    {#if supporting}<span class="supporting">{supporting}</span>{/if}
    {@render children?.()}
  </span>
  {#if trailing}<span class="trailing">{@render trailing()}</span>{/if}
{/snippet}

{#if href}
  <a class="item interactive" class:selected {href} aria-current={selected ? "page" : undefined} {...rest}>{@render body()}</a>
{:else if onclick}
  <button class="item interactive" class:selected type="button" {onclick} {...rest}>{@render body()}</button>
{:else}
  <div class="item" class:selected {...rest}>{@render body()}</div>
{/if}

<style>
  .item {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--md-space-4);
    width: 100%;
    min-height: 56px;
    padding: var(--md-space-2) var(--md-space-4);
    border: none;
    border-radius: var(--md-radius-md);
    background: transparent;
    color: var(--md-on-surface);
    font: inherit;
    text-align: left;
    text-decoration: none;
    overflow-wrap: anywhere;
  }
  .selected {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
  }
  .interactive {
    cursor: pointer;
  }
  .interactive::after {
    content: "";
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: currentColor;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--md-duration-short) var(--md-ease-standard);
  }
  .interactive:hover::after {
    opacity: 0.08;
  }
  .interactive:active::after {
    opacity: 0.14;
  }
  .interactive:focus-visible {
    outline: 2px solid var(--md-primary);
    outline-offset: -2px;
  }
  .leading,
  .trailing {
    display: inline-flex;
    flex: none;
    align-items: center;
    color: var(--md-on-surface-variant);
  }
  .content {
    display: grid;
    flex: 1;
    min-width: 0;
  }
  .headline {
    font-size: var(--md-type-body-size);
    line-height: var(--md-type-body-line);
  }
  .supporting {
    font-size: var(--md-type-body-sm-size);
    line-height: var(--md-type-body-sm-line);
    color: var(--md-on-surface-variant);
  }
</style>
