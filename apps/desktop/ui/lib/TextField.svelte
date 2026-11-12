<script>
  // Outlined text field (single line, or a textarea when `rows` > 1). `value` is bindable.
  // `error` (string) turns the field red and is announced; `supporting` is the helper text.
  let {
    value = $bindable(""),
    label = "",
    type = "text",
    rows = 1,
    supporting = "",
    error = "",
    disabled = false,
    ...rest
  } = $props();
  const uid = $props.id();
  const describedby = $derived(error || supporting ? uid + "-msg" : undefined);
</script>

<div class="field" class:invalid={!!error}>
  {#if label}<label for={uid}>{label}</label>{/if}
  {#if rows > 1}
    <textarea id={uid} {rows} bind:value {disabled} aria-invalid={error ? "true" : undefined}
      aria-describedby={describedby} {...rest}></textarea>
  {:else}
    <input id={uid} {type} bind:value {disabled} aria-invalid={error ? "true" : undefined}
      aria-describedby={describedby} {...rest} />
  {/if}
  {#if error}
    <div class="msg" id={uid + "-msg"} role="alert">{error}</div>
  {:else if supporting}
    <div class="msg" id={uid + "-msg"}>{supporting}</div>
  {/if}
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
  input,
  textarea {
    width: 100%;
    min-height: 48px;
    padding: 0 var(--md-space-4);
    border: 1px solid var(--md-outline);
    border-radius: var(--md-radius-sm);
    background: var(--md-surface-container-lowest);
    color: var(--md-on-surface);
    font: inherit;
    transition: border-color var(--md-duration-short) var(--md-ease-standard);
  }
  textarea {
    padding: var(--md-space-3) var(--md-space-4);
    resize: vertical;
  }
  input:hover:not(:disabled),
  textarea:hover:not(:disabled) {
    border-color: var(--md-on-surface);
  }
  input:focus-visible,
  textarea:focus-visible {
    outline: none;
    border-color: var(--md-primary);
    box-shadow: 0 0 0 1px var(--md-primary);
  }
  input:disabled,
  textarea:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .invalid input,
  .invalid textarea {
    border-color: var(--md-error);
  }
  .invalid input:focus-visible,
  .invalid textarea:focus-visible {
    box-shadow: 0 0 0 1px var(--md-error);
  }
  .invalid label {
    color: var(--md-error);
  }
  .msg {
    padding-inline: var(--md-space-4);
    font-size: var(--md-type-label-sm-size);
    line-height: var(--md-type-label-sm-line);
    color: var(--md-on-surface-variant);
  }
  .invalid .msg {
    color: var(--md-error);
  }
</style>
