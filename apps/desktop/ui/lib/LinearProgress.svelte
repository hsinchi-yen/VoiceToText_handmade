<script>
  // Progress bar. `value` is 0..1; null/undefined = indeterminate. `label` is the accessible name.
  let { value = null, label = "", ...rest } = $props();
  const pct = $derived(value == null ? null : Math.round(Math.min(1, Math.max(0, value)) * 100));
</script>

<div
  class="progress"
  class:indeterminate={pct === null}
  role="progressbar"
  aria-label={label || undefined}
  aria-valuemin="0"
  aria-valuemax="100"
  aria-valuenow={pct ?? undefined}
  {...rest}
>
  <div class="bar" style:width={pct === null ? undefined : `${pct}%`}></div>
</div>

<style>
  .progress {
    position: relative;
    height: 6px;
    border-radius: var(--md-radius-full);
    background: var(--md-secondary-container);
    overflow: hidden;
  }
  .bar {
    height: 100%;
    border-radius: inherit;
    background: var(--md-primary);
    transition: width var(--md-duration-medium) var(--md-ease-standard);
  }
  .indeterminate .bar {
    position: absolute;
    width: 40%;
    animation: slide 1.4s var(--md-ease-standard) infinite;
  }
  @keyframes slide {
    from {
      left: -40%;
    }
    to {
      left: 100%;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .indeterminate .bar {
      animation: none;
      left: 30%;
    }
  }
</style>
