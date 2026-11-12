<script>
  // Icon-only button. `label` is required: it is the accessible name and the tooltip.
  // variant: standard | tonal | filled.
  import Icon from "./Icon.svelte";

  let { icon, label, variant = "standard", disabled = false, type = "button", size = 24, ...rest } = $props();
</script>

<button class="ib {variant}" {type} {disabled} aria-label={label} title={label} {...rest}>
  <Icon name={icon} {size} />
</button>

<style>
  .ib {
    position: relative;
    display: inline-grid;
    place-items: center;
    width: 40px;
    height: 40px;
    min-height: 40px;
    padding: 0;
    border: none;
    border-radius: var(--md-radius-full);
    background: transparent;
    color: var(--md-on-surface-variant);
    cursor: pointer;
    transition: background-color var(--md-duration-short) var(--md-ease-standard);
  }
  .tonal {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
  }
  .filled {
    background: var(--md-primary);
    color: var(--md-on-primary);
  }
  .ib::after {
    content: "";
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: currentColor;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--md-duration-short) var(--md-ease-standard);
  }
  .ib:hover:not(:disabled)::after {
    opacity: 0.08;
  }
  .ib:active:not(:disabled)::after {
    opacity: 0.14;
  }
  .ib:focus-visible {
    outline: 2px solid var(--md-primary);
    outline-offset: 2px;
  }
  .ib:disabled {
    cursor: not-allowed;
    opacity: 0.38;
  }
</style>
