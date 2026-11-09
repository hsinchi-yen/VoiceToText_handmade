<script>
  // 「新增雲端服務」 wizard, 7 steps (tasks/plan.md §6.2). `editing` is a row from
  // cloud_list to change an existing service: a custom endpoint starts at step 2 (base_url
  // editable), an official service at step 3. The stored key is reused only if the kind and
  // endpoint stay the same (checked by the backend).
  import * as api from "./api.js";
  import { guides, billingWarning } from "./guides.js";

  let { kinds, editing = null, onDone, onCancel } = $props();

  const STEPS = ["選擇服務", "取得金鑰", "貼上金鑰", "選擇模型", "測試連線", "指定用途", "完成"];

  // svelte-ignore state_referenced_locally
  const start = editing;
  const firstStep = start ? (start.kind === "open_ai_compat" ? 2 : 3) : 1;
  let step = $state(firstStep);
  let draft = $state(
    start
      ? {
          id: start.id,
          kind: start.kind,
          base_url: start.base_url ?? "",
          api_key: "",
          asr_model: start.asr_model ?? "",
          llm_model: start.llm_model ?? "",
        }
      : { kind: null, base_url: "", api_key: "", asr_model: "", llm_model: "" },
  );
  let purposes = $state(
    start ? { ...start.purposes } : { asr: false, polish: false, translate: false },
  );
  let message = $state("");
  let busy = $state(false);
  let modelList = $state(null);
  let modelNote = $state("");
  let result = $state(null);
  let saved = $state(null);

  const info = $derived(kinds.find((k) => k.kind === draft.kind));
  const isCompat = $derived(draft.kind === "open_ai_compat");
  const keyOptional = $derived(isCompat || (editing && editing.has_key));
  const typedLast4 = $derived(api.last4(draft.api_key));

  function choose(kind) {
    const k = kinds.find((x) => x.kind === kind);
    draft.kind = kind;
    draft.asr_model = k.suggested_asr_model ?? "";
    draft.llm_model = k.suggested_llm_model ?? "";
    draft.base_url = "";
    result = null;
    modelList = null;
    message = "";
    step = 2;
  }

  async function run(fn) {
    message = "";
    busy = true;
    try {
      return await fn();
    } catch (code) {
      message = api.describe(code);
      return undefined;
    } finally {
      busy = false;
    }
  }

  function next() {
    message = "";
    if (step === 2 && isCompat && !draft.base_url.trim()) {
      message = api.describe("base_url_required");
      return;
    }
    if (step === 3 && !keyOptional && !draft.api_key.trim()) {
      message = api.describe("key_missing");
      return;
    }
    step += 1;
    if (step === 4) loadModels();
    if (step === 5) result = null;
  }

  function back() {
    message = "";
    if (step === firstStep) onCancel();
    else step -= 1;
  }

  async function loadModels() {
    modelList = null;
    modelNote = "";
    const r = await run(() => api.models(draft));
    if (r) {
      modelList = r.models;
      modelNote = r.models ? "" : (r.reason ?? "");
    }
  }

  async function runTest() {
    result = null;
    const r = await run(() => api.test(draft));
    if (r) {
      result = r;
      // Keep only purposes that passed.
      for (const p of ["asr", "polish", "translate"]) {
        if (!r.supports[p]) purposes[p] = false;
      }
      if (!editing && !purposes.asr && !purposes.polish && !purposes.translate) {
        purposes = { ...r.supports };
      }
    }
  }

  async function doSave() {
    const row = await run(() => api.save(draft, $state.snapshot(purposes)));
    if (row) {
      saved = row;
      draft.api_key = ""; // do not keep the key in the page
      step = 7;
    }
  }

  function cancel() {
    draft.api_key = "";
    onCancel();
  }

  const statusText = { passed: "通過", failed: "失敗", skipped: "略過" };
</script>

<section class="wizard">
  <ol class="steps">
    {#each STEPS as s, i (s)}
      <li class:current={step === i + 1} class:done={step > i + 1}>{i + 1}. {s}</li>
    {/each}
  </ol>

  {#if step === 1}
    <h2>選擇服務</h2>
    <div class="cards">
      {#each kinds as k (k.kind)}
        <button class="card" onclick={() => choose(k.kind)}>
          <strong>{k.label}</strong>
          <span>{k.capabilities}</span>
        </button>
      {/each}
    </div>
  {:else if step === 2}
    <h2>{isCompat ? "填入端點網址" : `取得 ${info.label} 的 API 金鑰`}</h2>
    <ol>
      {#each guides[draft.kind] as line (line)}
        <li>{line}</li>
      {/each}
    </ol>
    {#if info.key_page_url}
      <p>
        <button onclick={() => run(() => api.openKeyPage(draft.kind))}>
          用瀏覽器開啟官方金鑰頁面
        </button>
        <code class="url">{info.key_page_url}</code>
      </p>
    {/if}
    {#if isCompat}
      <label>
        端點網址（base_url）
        <input
          bind:value={draft.base_url}
          placeholder="http://192.168.1.20:1234/v1"
          spellcheck="false"
        />
      </label>
    {/if}
    <p class="warn">{billingWarning}</p>
    <p class="hint">詳見安裝資料夾中的 docs\cloud-setup.md</p>
  {:else if step === 3}
    <h2>貼上金鑰{isCompat ? "（選填）" : ""}</h2>
    {#if editing}
      <p>
        {#if editing.has_key}
          已儲存的金鑰{editing.key_last4 ? `末 4 碼：${editing.key_last4}` : ""}。留空就沿用，貼上新的會取代。改了端點網址就必須重新貼上金鑰。
        {:else}
          目前沒有儲存金鑰。
        {/if}
      </p>
    {/if}
    <label>
      API 金鑰
      <input
        type="password"
        bind:value={draft.api_key}
        autocomplete="off"
        spellcheck="false"
        placeholder={isCompat ? "不需要金鑰就留空" : "貼上金鑰"}
      />
    </label>
    {#if typedLast4}
      <p class="hint">末 4 碼：{typedLast4}</p>
    {/if}
    <p class="hint">金鑰只存在 Windows 認證管理員，不會寫進設定檔，畫面上也只顯示末 4 碼。</p>
  {:else if step === 4}
    <h2>選擇模型</h2>
    {#if busy}<p>正在讀取模型清單…</p>{/if}
    {#if modelNote}<p class="hint">{modelNote}，請自行輸入模型名稱。</p>{/if}
    <datalist id="cloud-models">
      {#each modelList ?? [] as m (m)}
        <option value={m}></option>
      {/each}
    </datalist>
    {#if info.supports_asr}
      <label>
        語音辨識模型{isCompat ? "（不用語音辨識可留空）" : ""}
        <input
          bind:value={draft.asr_model}
          list="cloud-models"
          spellcheck="false"
          placeholder={info.suggested_asr_model ?? "例如 whisper-1"}
        />
      </label>
    {/if}
    <label>
      文字模型（潤飾／中翻英）{isCompat ? "（不用可留空）" : ""}
      <input
        bind:value={draft.llm_model}
        list="cloud-models"
        spellcheck="false"
        placeholder={info.suggested_llm_model ?? "例如 qwen3-8b"}
      />
    </label>
    {#if modelList}<p class="hint">可以從下拉清單選擇，或直接輸入。</p>{/if}
  {:else if step === 5}
    <h2>測試連線</h2>
    <p class="hint">
      {info.supports_asr ? "會送出一段 3 秒的內建測試音檔與一句測試文字。" : "會送出一句測試文字。"}
    </p>
    <button onclick={runTest} disabled={busy}>{busy ? "測試中…" : "開始測試"}</button>
    {#if result}
      <p class:ok={result.ok} class:bad={!result.ok}>
        <strong>{result.summary}</strong>（共 {result.latency_ms} ms）
      </p>
      <table>
        <tbody>
          {#each result.checks as c (c.name)}
            <tr>
              <td>{c.label}</td>
              <td class={c.status}>{statusText[c.status]}</td>
              <td>{c.latency_ms != null ? `${c.latency_ms} ms` : (c.reason ?? "")}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {:else if step === 6}
    <h2>指定用途</h2>
    <p>勾選要交給 {info.label} 的用途，沒勾的維持地端。</p>
    {#each Object.entries(api.purposeLabels) as [p, label] (p)}
      <label class="check">
        <input
          type="checkbox"
          bind:checked={purposes[p]}
          disabled={!result?.supports[p]}
        />
        {label}
        {#if p === "asr" && !info.supports_asr}
          <span class="hint">（此服務不提供）</span>
        {:else if !result?.supports[p]}
          <span class="hint">（測試未通過，無法選擇）</span>
        {/if}
      </label>
    {/each}
  {:else if step === 7}
    <h2>完成</h2>
    <p>已儲存「{saved?.label}」（{saved?.id}）。</p>
    <p>
      想全部改回地端時，在系統列圖示按右鍵 →「全部改用地端」，立即生效；也可以在這裡移除服務。
    </p>
    <button onclick={() => onDone(saved)}>回到雲端服務列表</button>
  {/if}

  {#if message}<p class="bad">{message}</p>{/if}

  {#if step > 1 && step < 7}
    <div class="nav">
      <button onclick={back} disabled={busy}>上一步</button>
      <button onclick={cancel} disabled={busy}>取消</button>
      {#if step < 5}
        <button onclick={next} disabled={busy}>下一步</button>
      {:else if step === 5}
        <button
          onclick={next}
          disabled={busy ||
            !result ||
            !(result.supports.asr || result.supports.polish || result.supports.translate)}
        >
          下一步
        </button>
      {:else}
        <button onclick={doSave} disabled={busy}>儲存</button>
      {/if}
    </div>
  {:else if step === 1}
    <div class="nav"><button onclick={cancel}>取消</button></div>
  {/if}
</section>

<style>
  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 0.75rem;
    list-style: none;
    padding: 0;
    font-size: 0.85em;
    opacity: 0.8;
  }
  .steps .current {
    font-weight: 600;
    opacity: 1;
  }
  .steps .done {
    opacity: 0.5;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 0.5rem;
  }
  .card {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.25rem;
    padding: 0.75rem;
    text-align: left;
  }
  label {
    display: block;
    margin: 0.5rem 0;
  }
  label input:not([type="checkbox"]) {
    display: block;
    width: 100%;
    box-sizing: border-box;
    margin-top: 0.25rem;
    padding: 0.35rem;
  }
  .check input {
    margin-right: 0.35rem;
  }
  .url {
    margin-left: 0.5rem;
    font-size: 0.85em;
    word-break: break-all;
  }
  .warn {
    padding: 0.5rem;
    border-left: 3px solid #d29922;
    background: color-mix(in srgb, #d29922 12%, transparent);
  }
  .hint {
    opacity: 0.75;
    font-size: 0.9em;
  }
  .ok,
  .passed {
    color: #1a7f37;
  }
  .bad,
  .failed {
    color: #cf222e;
  }
  table td {
    padding: 0.2rem 0.6rem 0.2rem 0;
  }
  .nav {
    display: flex;
    gap: 0.5rem;
    margin-top: 1rem;
  }
</style>
