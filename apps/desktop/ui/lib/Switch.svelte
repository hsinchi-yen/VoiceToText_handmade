<script>
  // On/off switch (native checkbox with role="switch"). `label` is the visible text, or pass
  // children; `checked` is bindable.
  let { checked = $bindable(false), disabled = false, label = "", onchange = undefined, children, ...rest } = $props();
</script>

<label class="switch" class:disabled>
  <input type="checkbox" role="switch" bind:checked {disabled} {onchange} {...rest} />
  <span class="track" aria-hidden="true"><span class="thumb"></span></span>
  {#if label || children}
    <span class="text">{#if label}{label}{/if}{@render children?.()}</span>
  {/if}
</label>

<style>
  .switch {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: var(--md-space-3);
    cursor: pointer;
  }
  .switch.disabled {
    cursor: not-allowed;
    opacity: 0.45;
  }
  input {
    position: absolute;
    opacity: 0;
    width: 52px;
    height: 32px;
    margin: 0;
    cursor: inherit;
  }
  .track {
    position: relative;
    flex: none;
    width: 52px;
    height: 32px;
    border: 2px solid var(--md-outline);
    border-radius: var(--md-radius-full);
    background: var(--md-surface-container-highest);
    transition:
      background-color var(--md-duration-medium) var(--md-ease-standard),
      border-color var(--md-duration-medium) var(--md-ease-standard);
  }
  .thumb {
    position: absolute;
    top: 50%;
    left: 6px;
    width: 16px;
    height: 16px;
    border-radius: var(--md-radius-full);
    background: var(--md-outline);
    transform: translateY(-50%);
    transition:
      left var(--md-duration-medium) var(--md-ease-spring),
      width var(--md-duration-medium) var(--md-ease-spring),
      height var(--md-duration-medium) var(--md-ease-spring),
      background-color var(--md-duration-medium) var(--md-ease-standard);
  }
  .switch:active:not(.disabled) .thumb {
    width: 28px;
    height: 28px;
  }
  input:checked + .track {
    background: var(--md-primary);
    border-color: var(--md-primary);
  }
  input:checked + .track .thumb {
    left: 24px;
    width: 24px;
    height: 24px;
    background: var(--md-on-primary);
  }
  .switch:active:not(.disabled) input:checked + .track .thumb {
    left: 20px;
    width: 28px;
    height: 28px;
  }
  input:focus-visible + .track {
    outline: 2px solid var(--md-primary);
    outline-offset: 3px;
  }
  .text {
    line-height: var(--md-type-body-line);
  }
</style>
