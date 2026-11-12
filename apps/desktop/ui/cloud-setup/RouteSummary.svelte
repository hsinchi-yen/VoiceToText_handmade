<script>
  // 「各用途由誰處理」: one row per purpose, so users with several endpoints (e.g. a LAN text
  // model on :8000 and speech recognition on :8002) can see the whole wiring at a glance.
  import * as api from "./api.js";
  import { Card, ListItem } from "../lib/index.js";

  let { services } = $props();

  const rows = $derived(
    Object.entries(api.purposeLabels).map(([p, label]) => {
      const s = services.find((x) => x.purposes[p]);
      return { p, label, service: s ?? null };
    }),
  );
</script>

<section aria-labelledby="route-title">
  <h2 id="route-title">各用途由誰處理</h2>
  <Card variant="outlined" padding="var(--md-space-1)">
    {#each rows as r (r.p)}
      <ListItem
        headline={r.label}
        supporting={r.service
          ? `${api.displayName(r.service)}　${r.service.effective_base_url}`
          : "地端（本機模型）"}
      />
    {/each}
  </Card>
</section>

<style>
  h2 {
    margin: var(--md-space-4) 0 var(--md-space-2);
  }
</style>
