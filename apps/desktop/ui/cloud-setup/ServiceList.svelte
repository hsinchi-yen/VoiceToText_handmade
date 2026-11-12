<script>
  import { pick } from "../platform.js";
  // Connected services: 測試連線／修改／移除.
  import * as api from "./api.js";
  import { Banner, Button, Card } from "../lib/index.js";

  // `presets` is the `presets_list` answer: services whose id starts with a preset's
  // `id_prefix` are grouped under it; everything else goes to 「其他」.
  let { services, presets = [], onAdd, onEdit, onChanged } = $props();

  const groups = $derived.by(() => {
    const out = [];
    const rest = new Set(services);
    for (const p of presets) {
      const prefix = (p.id_prefix ?? "").toLowerCase();
      const items = prefix ? services.filter((s) => s.id.toLowerCase().startsWith(prefix)) : [];
      items.forEach((s) => rest.delete(s));
      if (items.length) out.push({ key: p.name, title: `${p.name} 區網服務`, items });
    }
    if (rest.size) out.push({ key: "other", title: "其他", items: services.filter((s) => rest.has(s)) });
    return out;
  });

  let message = $state("");
  let busyId = $state(null);
  let results = $state({});
  let confirmId = $state(null);

  const routed = (s) =>
    Object.entries(api.purposeLabels)
      .filter(([p]) => s.purposes[p])
      .map(([, label]) => label);

  async function testOne(s) {
    message = "";
    busyId = s.id;
    try {
      const draft = {
        id: s.id,
        kind: s.kind,
        base_url: s.base_url ?? "",
        asr_model: s.asr_model ?? "",
        llm_model: s.llm_model ?? "",
        asr_timeout_ms: s.asr_timeout_ms != null ? String(s.asr_timeout_ms) : "",
      };
      results[s.id] = await api.test(draft);
    } catch (code) {
      message = api.describe(code);
    } finally {
      busyId = null;
    }
  }

  async function removeOne(s) {
    message = "";
    busyId = s.id;
    try {
      const r = await api.remove(s.id);
      confirmId = null;
      message = `已移除「${api.displayName(s)}」，原本交給它的用途已改回地端。`;
      if (!r.key_deleted) {
        message += pick(
          "（金鑰沒能從 Windows 認證管理員刪除，v2c 已不會再使用它；可以自行到認證管理員刪除。）",
          "（金鑰沒能從 GNOME 鑰匙圈刪除，v2c 已不會再使用它；可以自行用「密碼和金鑰」刪除。）",
        );
      }
      onChanged();
    } catch (code) {
      message = api.describe(code);
    } finally {
      busyId = null;
    }
  }
</script>

<div class="top">
  <Button icon="plus" onclick={onAdd}>新增服務</Button>
</div>

{#if services.length === 0}
  <p class="hint">尚未串接任何服務，目前全部使用地端模型。</p>
{/if}

{#each groups as g (g.key)}
  <section class="group" aria-labelledby="grp-{g.key}">
    <h2 id="grp-{g.key}">{g.title}</h2>
    <ul class="services">
      {#each g.items as s (s.id)}
        {@const used = routed(s)}
        <Card tag="li" variant="outlined">
          <div class="head">
            <h3>{api.displayName(s)}</h3>
            <span class="id">{s.label}{s.name ? `（代號 ${s.id}）` : ""}</span>
          </div>
          <div class="chips">
            {#if s.asr_model}<span class="chip">語音辨識</span>{/if}
            {#if s.llm_model}<span class="chip">文字</span>{/if}
            {#each used as u (u)}<span class="chip on">用於{u}</span>{/each}
            {#if used.length === 0}<span class="chip off">未使用（維持地端）</span>{/if}
          </div>
          <dl>
            {#if s.kind === "open_ai_compat" || s.base_url}
              <dt>端點</dt>
              <dd>{s.effective_base_url}</dd>
            {/if}
            {#if s.asr_model}
              <dt>辨識模型</dt>
              <dd>{s.asr_model}</dd>
            {/if}
            {#if s.asr_timeout_ms != null}
              <dt>辨識逾時</dt>
              <dd>{s.asr_timeout_ms} ms</dd>
            {/if}
            {#if s.llm_model}
              <dt>文字模型</dt>
              <dd>{s.llm_model}</dd>
            {/if}
            <dt>金鑰</dt>
            <dd>{s.has_key ? (s.key_last4 ? `末 4 碼 ${s.key_last4}` : "已儲存") : "未設定"}</dd>
          </dl>
          <div class="actions">
            <Button variant="text" onclick={() => testOne(s)} disabled={busyId !== null}>
              {busyId === s.id ? "處理中…" : "測試連線"}
            </Button>
            <Button variant="text" onclick={() => onEdit(s)} disabled={busyId !== null}>修改</Button>
            {#if s.kind === "open_ai_compat"}
              <Button variant="text" onclick={() => onEdit(s)} disabled={busyId !== null}>重新命名</Button>
            {/if}
            {#if confirmId === s.id}
              <Button variant="tonal" danger onclick={() => removeOne(s)} disabled={busyId !== null}>
                確定移除（連同金鑰）
              </Button>
              <Button variant="text" onclick={() => (confirmId = null)}>取消</Button>
            {:else}
              <Button variant="text" danger onclick={() => (confirmId = s.id)} disabled={busyId !== null}>移除</Button>
            {/if}
          </div>
          {#if results[s.id]}
            <div class="result">
              <Banner variant={results[s.id].ok ? "success" : "error"}>
                {results[s.id].summary}（{results[s.id].latency_ms} ms）
              </Banner>
            </div>
          {/if}
        </Card>
      {/each}
    </ul>
  </section>
{/each}

{#if message}<div class="result"><Banner variant="info" onclose={() => (message = "")}>{message}</Banner></div>{/if}

<p class="hint">系統列圖示右鍵 →「全部改用地端」可隨時一鍵切回地端（服務與金鑰會保留）。</p>

<style>
  .top {
    margin: var(--md-space-4) 0 var(--md-space-2);
  }
  .group h2 {
    margin: var(--md-space-5) 0 var(--md-space-2);
  }
  .services {
    display: grid;
    gap: var(--md-space-3);
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: var(--md-space-1) var(--md-space-3);
  }
  .head h3 {
    margin: 0;
  }
  .id {
    color: var(--md-on-surface-variant);
    font-size: var(--md-type-body-sm-size);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--md-space-2);
    margin: var(--md-space-2) 0;
  }
  .chip {
    padding: 2px var(--md-space-3);
    border-radius: var(--md-radius-full);
    border: 1px solid var(--md-outline-variant);
    color: var(--md-on-surface-variant);
    font-size: var(--md-type-label-sm-size);
    line-height: var(--md-type-label-sm-line);
    font-weight: var(--md-type-label-sm-weight);
  }
  .chip.on {
    border-color: transparent;
    background: var(--md-primary-container);
    color: var(--md-on-primary-container);
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--md-space-1) var(--md-space-4);
    margin: var(--md-space-2) 0;
    font-size: var(--md-type-body-sm-size);
    line-height: var(--md-type-body-sm-line);
  }
  dt {
    color: var(--md-on-surface-variant);
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--md-space-1);
    margin: var(--md-space-2) calc(-1 * var(--md-space-3)) 0;
  }
  .result {
    margin-top: var(--md-space-3);
  }
</style>
