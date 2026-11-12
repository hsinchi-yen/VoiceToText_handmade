<script>
  // Inline SVG icon from icons.js. Decorative unless `label` is set.
  import { ICONS } from "./icons.js";

  let { name, size = 24, label = "", ...rest } = $props();
  const paths = $derived(ICONS[name] ?? []);
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  role={label ? "img" : undefined}
  aria-label={label || undefined}
  aria-hidden={label ? undefined : "true"}
  focusable="false"
  {...rest}
>
  {#each paths as d}
    <path {d} />
  {/each}
</svg>

<style>
  .icon {
    flex: none;
    display: inline-block;
    vertical-align: middle;
  }
</style>
