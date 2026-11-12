<script>
  // Main navigation: a left rail from 600px wide, a bottom bar below. Items are plain links
  // (`<a href="#/...">`), so the Settings page's "unsaved changes" guard sees the clicks.
  // The v2c logo (inline SVG, no external URL) sits on top of the rail and is hidden in the bottom bar.
  // `items`: [{ path, label, icon }]; `current`: the active item's path (or null).
  import Icon from "./Icon.svelte";
  import { hrefOf } from "./nav.js";

  let { items = [], current = null, label = "主選單" } = $props();
</script>

<nav class="rail" aria-label={label}>
  <svg class="brand" viewBox="0 0 1024 1024" width="32" height="32" role="img" aria-label="v2c">
    <defs>
      <linearGradient id="v2c-brand-bg" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="#5A5CF2" />
        <stop offset="1" stop-color="#9B4DE8" />
      </linearGradient>
    </defs>
    <rect x="48" y="48" width="928" height="928" rx="232" fill="url(#v2c-brand-bg)" />
    <path d="M300 190 H724 A112 112 0 0 1 836 302 V560 A112 112 0 0 1 724 672 H640 L512 840 L384 672 H300 A112 112 0 0 1 188 560 V302 A112 112 0 0 1 300 190 Z" fill="#FFFFFF" />
    <rect x="447" y="245" width="130" height="165" rx="65" fill="#6D4DF0" />
    <path d="M290 340 L512 580 L734 340" fill="none" stroke="#FFC23D" stroke-width="120" stroke-linecap="round" stroke-linejoin="round" />
  </svg>
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
  .brand {
    align-self: center;
    flex: none;
    margin-bottom: var(--md-space-2);
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
    .brand {
      display: none;
    }
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
