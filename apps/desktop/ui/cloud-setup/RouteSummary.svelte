<script>
  // 「目前由誰處理」: one row per purpose, so users with several endpoints (e.g. a LAN text
  // model on :8000 and speech recognition on :8002) can see the whole wiring at a glance.
  import * as api from "./api.js";

  let { services } = $props();

  const rows = $derived(
    Object.entries(api.purposeLabels).map(([p, label]) => {
      const s = services.find((x) => x.purposes[p]);
      return { p, label, service: s ?? null };
    }),
  );
</script>

<table class="routes">
  <caption>目前由誰處理</caption>
  <tbody>
    {#each rows as r (r.p)}
      <tr>
        <th scope="row">{r.label}</th>
        {#if r.service}
          <td>
            <strong>{api.displayName(r.service)}</strong>
            <span class="id">{r.service.label}</span>
            <div class="url">{r.service.effective_base_url}</div>
          </td>
        {:else}
          <td>本機（地端模型）</td>
        {/if}
      </tr>
    {/each}
  </tbody>
</table>

<style>
  .routes {
    border-collapse: collapse;
    margin: 0.5rem 0 1rem;
    min-width: 60%;
  }
  caption {
    text-align: left;
    font-weight: 600;
    padding-bottom: 0.25rem;
  }
  th,
  td {
    text-align: left;
    vertical-align: top;
    padding: 0.35rem 0.75rem 0.35rem 0;
    border-bottom: 1px solid color-mix(in srgb, currentColor 15%, transparent);
  }
  th {
    white-space: nowrap;
    font-weight: normal;
    opacity: 0.8;
  }
  .id {
    opacity: 0.6;
    font-size: 0.85em;
    margin-left: 0.3rem;
  }
  .url {
    font-size: 0.85em;
    opacity: 0.8;
    word-break: break-all;
  }
</style>
