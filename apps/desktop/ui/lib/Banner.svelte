<script>
  // Inline message bar. variant: info | success | warning | error (error/warning are announced
  // as alerts, the others as status). `actions` is an optional button snippet; `onclose` adds
  // a dismiss button.
  import Icon from "./Icon.svelte";
  import IconButton from "./IconButton.svelte";

  let { variant = "info", title = "", onclose = undefined, children, actions, ...rest } = $props();
  const icon = $derived({ info: "info", success: "check", warning: "warning", error: "error" }[variant] ?? "info");
  const role = $derived(variant === "error" || variant === "warning" ? "alert" : "status");
</script>

<div class="banner {variant}" {role} {...rest}>
  <Icon name={icon} size={22} />
  <div class="content">
    {#if title}<strong>{title}</strong>{/if}
    <div>{@render children?.()}</div>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </div>
  {#if onclose}<IconButton icon="close" label="關閉" size={18} onclick={onclose} />{/if}
</div>

<style>
  .banner {
    --bg: var(--md-secondary-container);
    --fg: var(--md-on-secondary-container);
    display: flex;
    align-items: flex-start;
    gap: var(--md-space-3);
    padding: var(--md-space-3) var(--md-space-4);
    border-radius: var(--md-radius-md);
    background: var(--bg);
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .banner > :global(.icon) {
    margin-top: 1px;
  }
  .success {
    --bg: var(--md-success-container);
    --fg: var(--md-success);
  }
  .warning {
    --bg: var(--md-warning-container);
    --fg: var(--md-warning);
  }
  .error {
    --bg: var(--md-error-container);
    --fg: var(--md-on-error-container);
  }
  .content {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: var(--md-space-1);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--md-space-2);
    margin-top: var(--md-space-1);
  }
  .banner :global(.ib) {
    margin: -6px -8px -6px 0;
    color: inherit;
  }
</style>
