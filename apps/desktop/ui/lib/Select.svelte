<script>
  // Native <select> with M3 outlined styling. `options`: [{ value, label, disabled? }].
  // `value` is bindable. The native popup keeps keyboard and screen-reader behaviour.
  import Icon from "./Icon.svelte";

  let {
    value = $bindable(""),
    options = [],
    label = "",
    supporting = "",
    disabled = false,
    onchange = undefined,
    ...rest
  } = $props();
  const uid = $props.id();
</script>

<div class="field">
  {#if label}<label for={uid}>{label}</label>{/if}
  <div class="wrap">
    <select id={uid} bind:value {disabled} {onchange} {...rest}>
      {#each options as o (o.value)}
        <option value={o.value} disabled={o.disabled}>{o.label}</option>
      {/each}
    </select>
    <Icon name="chevron-down" size={20} />
  </div>
  {#if supporting}<div class="supporting">{supporting}</div>{/if}
</div>

<style>
  .field {
    display: grid;
    gap: var(--md-space-1);
    min-width: 0;
  }
  label {
    font-size: var(--md-type-label-sm-size);
    line-height: var(--md-type-label-sm-line);
    font-weight: var(--md-type-label-sm-weight);
    color: var(--md-on-surface-variant);
    padding-inline: var(--md-space-1);
  }
  .wrap {
    position: relative;
    display: grid;
    color: var(--md-on-surface-variant);
  }
  .wrap :global(.icon) {
    position: absolute;
    right: var(--md-space-3);
    top: 50%;
    transform: translateY(-50%);
    pointer-events: none;
  }
  select {
    appearance: none;
    width: 100%;
    min-height: 48px;
    padding: 0 var(--md-space-10) 0 var(--md-space-4);
    border: 1px solid var(--md-outline);
    border-radius: var(--md-radius-sm);
    background: var(--md-surface-container-lowest);
    color: var(--md-on-surface);
    font: inherit;
    cursor: pointer;
  }
  select:hover:not(:disabled) {
    border-color: var(--md-on-surface);
  }
  select:focus-visible {
    outline: none;
    border-color: var(--md-primary);
    box-shadow: 0 0 0 1px var(--md-primary);
  }
  select:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .supporting {
    padding-inline: var(--md-space-4);
    font-size: var(--md-type-label-sm-size);
    color: var(--md-on-surface-variant);
  }
</style>
