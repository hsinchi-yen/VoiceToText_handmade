<script>
  import { pick } from "../platform.js";
  // Connected services: 測試連線／修改／移除.
  import * as api from "./api.js";

  let { services, onAdd, onEdit, onChanged } = $props();

  let message = $state("");
  let busyId = $state(null);
  let results = $state({});
  let confirmId = $state(null);

  function routed(s) {
    const used = Object.entries(api.purposeLabels)
      .filter(([p]) => s.purposes[p])
      .map(([, label]) => label);
    return used.length ? used.join("、") : "未使用（全部維持地端）";
  }

  function provides(s) {
    const p = [];
    if (s.llm_model) p.push("文字模型");
    if (s.asr_model) p.push("語音辨識");
    return p.length ? p.join("、") : "尚未設定模型";
  }

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

<p>
  <button onclick={onAdd}>新增雲端服務</button>
</p>

{#if services.length === 0}
  <p>尚未串接任何雲端服務，目前全部使用地端模型。</p>
{:else}
  <ul class="services">
    {#each services as s (s.id)}
      <li>
        <div class="title">
          <strong>{api.displayName(s)}</strong> <span class="id">{s.label}{s.name ? `（代號 ${s.id}）` : ""}</span>
        </div>
        <div class="meta">
          {#if s.kind === "open_ai_compat"}提供：{provides(s)}<br />{/if}
          用途：{routed(s)}<br />
          {#if s.kind === "open_ai_compat" || s.base_url}端點：{s.effective_base_url}<br />{/if}
          {#if s.asr_model}語音辨識模型：{s.asr_model}<br />{/if}
          {#if s.asr_timeout_ms != null}辨識逾時 {s.asr_timeout_ms} ms<br />{/if}
          {#if s.llm_model}文字模型：{s.llm_model}<br />{/if}
          金鑰：{s.has_key ? (s.key_last4 ? `末 4 碼 ${s.key_last4}` : "已儲存") : "未設定"}
        </div>
        <div class="actions">
          <button onclick={() => testOne(s)} disabled={busyId !== null}>
            {busyId === s.id ? "處理中…" : "測試連線"}
          </button>
          <button onclick={() => onEdit(s)} disabled={busyId !== null}>修改</button>
          {#if s.kind === "open_ai_compat"}
            <button onclick={() => onEdit(s)} disabled={busyId !== null}>重新命名</button>
          {/if}
          {#if confirmId === s.id}
            <button class="danger" onclick={() => removeOne(s)} disabled={busyId !== null}>
              確定移除（連同金鑰）
            </button>
            <button onclick={() => (confirmId = null)}>取消</button>
          {:else}
            <button onclick={() => (confirmId = s.id)} disabled={busyId !== null}>移除</button>
          {/if}
        </div>
        {#if results[s.id]}
          <div class="result" class:ok={results[s.id].ok} class:bad={!results[s.id].ok}>
            {results[s.id].summary}（{results[s.id].latency_ms} ms）
          </div>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

{#if message}<p>{message}</p>{/if}

<p class="hint">系統列圖示右鍵 →「全部改用地端」可隨時一鍵切回地端（服務與金鑰會保留）。</p>

<style>
  .services {
    list-style: none;
    padding: 0;
  }
  .services li {
    padding: 0.6rem 0;
    border-bottom: 1px solid color-mix(in srgb, currentColor 15%, transparent);
  }
  .id {
    opacity: 0.6;
    font-size: 0.85em;
  }
  .meta {
    margin: 0.25rem 0;
    font-size: 0.9em;
    word-break: break-all;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  .danger {
    color: #cf222e;
  }
  .ok {
    color: #1a7f37;
  }
  .bad {
    color: #cf222e;
  }
  .hint {
    opacity: 0.75;
    font-size: 0.9em;
  }
</style>
