<script>
  // Tab bar (ARIA tabs pattern, roving tabindex: Left/Right/Home/End move and select).
  // `tabs`: [{ id, label, icon? }]; `value` (the selected id) is bindable. The caller renders
  // the panel: give it role="tabpanel" and aria-labelledby={`${idPrefix}-tab-${id}`}.
  import Icon from "./Icon.svelte";

  let { value = $bindable(""), tabs = [], idPrefix = "tabs", label = "", onchange = undefined } = $props();

  function select(id, focus = false) {
    if (id === value) return;
    value = id;
    onchange?.(id);
    if (focus) document.getElementById(`${idPrefix}-tab-${id}`)?.focus();
  }

  function onkeydown(e) {
    const i = tabs.findIndex((t) => t.id === value);
    let next;
    if (e.key === "ArrowRight") next = (i + 1) % tabs.length;
    else if (e.key === "ArrowLeft") next = (i - 1 + tabs.length) % tabs.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = tabs.length - 1;
    else return;
    e.preventDefault();
    select(tabs[next].id, true);
  }
</script>

<div class="tabs" role="tablist" aria-label={label || undefined} {onkeydown} tabindex="-1">
  {#each tabs as t (t.id)}
    <button
      class="tab"
      class:selected={value === t.id}
      id="{idPrefix}-tab-{t.id}"
      role="tab"
      type="button"
      aria-selected={value === t.id}
      aria-controls="{idPrefix}-panel-{t.id}"
      tabindex={value === t.id ? 0 : -1}
      onclick={() => select(t.id)}
    >
      {#if t.icon}<Icon name={t.icon} size={18} />{/if}
      {t.label}
    </button>
  {/each}
</div>

<style>
  .tabs {
    display: flex;
    gap: var(--md-space-1);
    border-bottom: 1px solid var(--md-outline-variant);
  }
  .tab {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--md-space-2);
    min-height: 48px;
    padding: 0 var(--md-space-5);
    border: none;
    border-radius: var(--md-radius-md) var(--md-radius-md) 0 0;
    background: transparent;
    color: var(--md-on-surface-variant);
    font: inherit;
    font-size: var(--md-type-label-size);
    font-weight: var(--md-type-label-weight);
    cursor: pointer;
    transition: color var(--md-duration-short) var(--md-ease-standard);
  }
  .tab:hover {
    background: color-mix(in srgb, var(--md-on-surface) 8%, transparent);
  }
  .tab.selected {
    color: var(--md-primary);
  }
  /* Active indicator. */
  .tab::after {
    content: "";
    position: absolute;
    left: var(--md-space-3);
    right: var(--md-space-3);
    bottom: -1px;
    height: 3px;
    border-radius: 3px 3px 0 0;
    background: var(--md-primary);
    transform: scaleX(0);
    transition: transform var(--md-duration-medium) var(--md-ease-spring);
  }
  .tab.selected::after {
    transform: scaleX(1);
  }
  .tab:focus-visible {
    outline: 2px solid var(--md-primary);
    outline-offset: -2px;
  }
</style>
