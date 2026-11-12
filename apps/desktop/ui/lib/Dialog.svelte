<script>
  // Modal dialog on the native <dialog> element: showModal() traps focus (the rest of the page
  // is inert), Esc closes it, and focus returns to the opener. A click on the backdrop also
  // closes it unless `dismissible` is false. `open` is bindable; `actions` is the button row.
  let {
    open = $bindable(false),
    title = "",
    dismissible = true,
    onclose = undefined,
    children,
    actions,
    ...rest
  } = $props();

  let el = $state(null);
  const uid = $props.id();

  $effect(() => {
    if (!el) return;
    if (open && !el.open) el.showModal();
    else if (!open && el.open) el.close();
  });

  function handleClose() {
    if (open) {
      open = false;
      onclose?.();
    }
  }

  function handleCancel(e) {
    if (!dismissible) e.preventDefault();
  }

  function handleClick(e) {
    if (dismissible && e.target === el) el.close();
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
<dialog
  bind:this={el}
  class="dialog"
  aria-labelledby={title ? uid + "-title" : undefined}
  onclose={handleClose}
  oncancel={handleCancel}
  onclick={handleClick}
  {...rest}
>
  <div class="surface">
    {#if title}<h2 id={uid + "-title"}>{title}</h2>{/if}
    <div class="body">{@render children?.()}</div>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </div>
</dialog>

<style>
  .dialog {
    width: min(560px, calc(100vw - 2 * var(--md-space-4)));
    max-height: calc(100vh - 2 * var(--md-space-4));
    padding: 0;
    border: none;
    border-radius: var(--md-radius-xl);
    background: var(--md-surface-container-high);
    color: var(--md-on-surface);
    box-shadow: 0 8px 32px rgb(0 0 0 / 0.3);
  }
  .dialog[open] {
    animation: pop var(--md-duration-medium) var(--md-ease-spring);
  }
  .dialog::backdrop {
    background: var(--md-scrim);
  }
  .surface {
    display: grid;
    gap: var(--md-space-4);
    padding: var(--md-space-6);
  }
  h2 {
    margin: 0;
    font-size: var(--md-type-headline-size);
    line-height: var(--md-type-headline-line);
  }
  .body {
    color: var(--md-on-surface-variant);
    overflow-y: auto;
    max-height: calc(100vh - 220px);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--md-space-2);
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.92);
    }
    to {
      opacity: 1;
      transform: scale(1);
    }
  }
</style>
