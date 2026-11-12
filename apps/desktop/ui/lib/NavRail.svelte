<script>
  // Main navigation: a left rail from 600px wide, a bottom bar below. Items are plain links
  // (`<a href="#/...">`), so the Settings page's "unsaved changes" guard sees the clicks.
  // `items`: [{ path, label, icon }]; `current`: the active item's path (or null).
  import Icon from "./Icon.svelte";
  import { hrefOf } from "./nav.js";

  let { items = [], current = null, label = "主選單" } = $props();
</script>

<nav class="rail" aria-label={label}>
  {#each items as item (item.path)}
    <a class="item" class:active={current === item.path} href={hrefOf(item.path)} aria-current={current === item.path ? "page" : undefined}>
      <span class="pill"><Icon name={item.icon} size={24} /></span>
      <span class="label">{item.label}</span>
    </a>
  {/each}
</nav>

<style>
  .rail {
    position: fixed;
    z-index: 10;
    top: 0;
    left: 0;
    bottom: 0;
    width: var(--md-nav-rail-width);
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: var(--md-space-3);
    padding: var(--md-space-6) var(--md-space-1);
    background: var(--md-surface);
    border-right: 1px solid var(--md-outline-variant);
  }
  .item {
    display: grid;
    justify-items: center;
    gap: var(--md-space-1);
    padding: var(--md-space-1) 0;
    border-radius: var(--md-radius-md);
    color: var(--md-on-surface-variant);
    text-decoration: none;
    font-size: var(--md-type-label-sm-size);
    line-height: var(--md-type-label-sm-line);
    font-weight: var(--md-type-label-sm-weight);
    text-align: center;
  }
  .pill {
    display: grid;
    place-items: center;
    width: 56px;
    height: 32px;
    border-radius: var(--md-radius-full);
    transition:
      background-color var(--md-duration-short) var(--md-ease-standard),
      width var(--md-duration-medium) var(--md-ease-spring);
  }
  .item:hover .pill {
    background: color-mix(in srgb, var(--md-on-surface) 8%, transparent);
  }
  .item.active {
    color: var(--md-on-surface);
  }
  .item.active .pill {
    background: var(--md-secondary-container);
    color: var(--md-on-secondary-container);
  }
  .item:focus-visible {
    outline: 2px solid var(--md-primary);
    outline-offset: 0;
  }

  @media (max-width: 599px) {
    .rail {
      top: auto;
      right: 0;
      width: auto;
      height: var(--md-nav-bar-height);
      flex-direction: row;
      justify-content: space-around;
      gap: 0;
      padding: var(--md-space-2) var(--md-space-2);
      border-right: none;
      border-top: 1px solid var(--md-outline-variant);
      background: var(--md-surface-container);
    }
    .item {
      flex: 1;
      min-width: 0;
    }
    .label {
      max-width: 100%;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
  }
</style>
