<script>
  // Single-choice segmented control (native radios, so arrow keys work).
  // `options`: [{ value, label, icon? }]; `value` is bindable; `label` names the group.
  import Icon from "./Icon.svelte";

  let { value = $bindable(""), options = [], label = "", disabled = false, onchange = undefined } = $props();
  const name = $props.id();
</script>

<div class="group" role="radiogroup" aria-label={label || undefined}>
  {#each options as o (o.value)}
    <label class="seg" class:checked={value === o.value} class:disabled={disabled || o.disabled}>
      <input type="radio" {name} value={o.value} bind:group={value} disabled={disabled || o.disabled} {onchange} />
      {#if value === o.value}<Icon name="check" size={16} />{:else if o.icon}<Icon name={o.icon} size={16} />{/if}
      <span>{o.label}</span>
    </label>
  {/each}
</div>

<style>
  .group {
    display: inline-flex;
    width: fit-content;
    max-width: 100%;
    border: 1px solid var(--md-outline);
    border-radius: var(--md-radius-full);
    overflow: hidden;
  }
  .seg {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--md-space-2);
    min-height: 40px;
    padding: 0 var(--md-space-4);
    font-size: var(--md-type-label-size);
    font-weight: var(--md-type-label-weight);
    color: var(--md-on-surface);
    cursor: pointer;
    transition: background-color var(--md-duration-short) var(--md-ease-standard);
  }
  .seg + .seg {
    border-left: 1px solid var(--md-outline);
  }
  .seg:hover:not(.disabled) {
    background: color-mix(in srgb, var(--md-on-surface) 8%, transparent);
  }
  .checked {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
  }
  .checked:hover:not(.disabled) {
    background: color-mix(in srgb, var(--md-on-secondary-container) 8%, var(--md-secondary-container));
  }
  .disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  input {
    position: absolute;
    inset: 0;
    opacity: 0;
    margin: 0;
    cursor: inherit;
  }
  .seg:has(input:focus-visible) {
    outline: 2px solid var(--md-primary);
    outline-offset: -2px;
  }
</style>
