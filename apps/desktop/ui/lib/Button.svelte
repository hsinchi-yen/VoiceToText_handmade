<script>
  // Button. variant: filled | tonal | outlined | text. `danger` swaps the accent to error.
  // With `href` it renders a link styled as a button.
  import Icon from "./Icon.svelte";

  let {
    variant = "filled",
    type = "button",
    disabled = false,
    danger = false,
    icon = "",
    href = undefined,
    children,
    ...rest
  } = $props();
</script>

{#if href && !disabled}
  <a class="btn {variant}" class:danger {href} {...rest}>
    {#if icon}<Icon name={icon} size={18} />{/if}
    {@render children?.()}
  </a>
{:else}
  <button class="btn {variant}" class:danger {type} {disabled} {...rest}>
    {#if icon}<Icon name={icon} size={18} />{/if}
    {@render children?.()}
  </button>
{/if}

<style>
  .btn {
    --accent: var(--md-primary);
    --on-accent: var(--md-on-primary);
    --accent-container: var(--md-secondary-container);
    --on-accent-container: var(--md-on-secondary-container);
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--md-space-2);
    min-height: 40px;
    padding: 0 var(--md-space-6);
    border: 1px solid transparent;
    border-radius: var(--md-radius-full);
    font: inherit;
    font-size: var(--md-type-label-size);
    font-weight: var(--md-type-label-weight);
    line-height: 1;
    text-decoration: none;
    white-space: nowrap;
    cursor: pointer;
    user-select: none;
    transition:
      background-color var(--md-duration-short) var(--md-ease-standard),
      border-radius var(--md-duration-medium) var(--md-ease-spring),
      box-shadow var(--md-duration-short) var(--md-ease-standard);
  }
  .danger {
    --accent: var(--md-error);
    --on-accent: var(--md-on-error);
    --accent-container: var(--md-error-container);
    --on-accent-container: var(--md-on-error-container);
  }
  .filled {
    background: var(--accent);
    color: var(--on-accent);
  }
  .tonal {
    background: var(--accent-container);
    color: var(--on-accent-container);
  }
  .outlined {
    background: transparent;
    color: var(--accent);
    border-color: var(--md-outline);
  }
  .text {
    background: transparent;
    color: var(--accent);
    padding: 0 var(--md-space-3);
  }
  /* State layer. */
  .btn::after {
    content: "";
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: currentColor;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--md-duration-short) var(--md-ease-standard);
  }
  .btn:hover:not(:disabled)::after {
    opacity: 0.08;
  }
  .btn:active:not(:disabled)::after {
    opacity: 0.14;
  }
  .btn:active:not(:disabled) {
    border-radius: var(--md-radius-md);
  }
  .btn:focus-visible {
    outline: 2px solid var(--md-primary);
    outline-offset: 2px;
  }
  .btn:disabled {
    cursor: not-allowed;
    opacity: 1;
    background: color-mix(in srgb, var(--md-on-surface) 12%, transparent);
    color: color-mix(in srgb, var(--md-on-surface) 38%, transparent);
    border-color: transparent;
  }
  .outlined:disabled,
  .text:disabled {
    background: transparent;
  }
  .outlined:disabled {
    border-color: color-mix(in srgb, var(--md-on-surface) 12%, transparent);
  }
</style>
