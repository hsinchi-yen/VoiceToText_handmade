<script>
  import { pick } from "../platform.js";
  // 「新增雲端服務」 wizard, 7 steps (tasks/plan.md §6.2). `editing` is a row from
  // cloud_list to change an existing service: a custom endpoint starts at step 2 (base_url
  // editable), an official service at step 3. The stored key is reused only if the kind and
  // endpoint stay the same (checked by the backend).
  // Custom endpoints also pick a `role` (what this one URL serves: text model, speech
  // recognition, or both). One endpoint = one service; `preset` starts a second endpoint
  // (e.g. speech on another port) from the 「完成」 step. `role` stays in the page: the
  // backend only sees which model fields are filled in.
  // Custom endpoints may also get an optional display name (step 2); `services` is the
  // current list, used for the default-id placeholder and the duplicate-name check.
  // Services with speech recognition may set their own ASR timeout (step 4, T1.15); blank
  // uses the global `[timeouts] asr_cloud_ms` (`info.default_asr_timeout_ms`).
  import * as api from "./api.js";
  import {
    guides,
    billingWarning,
    lanNote,
    roles,
    otherRole,
    asrTimeoutSuggestions,
  } from "./guides.js";

  let { kinds, services = [], editing = null, preset = null, onDone, onCancel } = $props();

  const STEPS = ["選擇服務", "端點網址／取得金鑰", "貼上金鑰", "選擇模型", "測試連線", "指定用途", "完成"];

  function inferRole(s) {
    if (s.asr_model && s.llm_model) return "both";
    return s.asr_model ? "asr" : "text";
  }

  // svelte-ignore state_referenced_locally
  const start = editing;
  // svelte-ignore state_referenced_locally
  const startPreset = editing ? null : preset;
  const firstStep = start ? (start.kind === "open_ai_compat" ? 2 : 3) : startPreset ? 2 : 1;
  let step = $state(firstStep);
  let draft = $state(
    start
      ? {
          id: start.id,
          kind: start.kind,
          name: start.name ?? "",
          base_url: start.base_url ?? "",
          api_key: "",
          asr_model: start.asr_model ?? "",
          llm_model: start.llm_model ?? "",
          asr_timeout_ms: start.asr_timeout_ms != null ? String(start.asr_timeout_ms) : "",
        }
      : {
          kind: startPreset?.kind ?? null,
          name: "",
          base_url: startPreset?.base_url ?? "",
          api_key: "",
          asr_model: "",
          llm_model: "",
          asr_timeout_ms: "",
        },
  );
  let role = $state(start ? inferRole(start) : (startPreset?.role ?? "text"));
  const wantAsr = $derived(role === "asr" || role === "both");
  const wantText = $derived(role === "text" || role === "both");
  const urlLooksOff = $derived(
    draft.base_url.trim() !== "" && !/\/v1\/?$/.test(draft.base_url.trim()),
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
  // The id this service has (editing) or will get (new): shown when no name is given.
  const defaultId = $derived(editing ? editing.id : api.nextCustomId(services));
  // Suggested timeout for the typed ASR model (shown as a hint only when the field is blank).
  const timeoutHint = $derived.by(() => {
    const model = draft.asr_model.trim().toLowerCase();
    if (!model || draft.asr_timeout_ms.trim()) return null;
    return asrTimeoutSuggestions.find((t) => model.includes(t.match)) ?? null;
  });

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
    if (step === 2 && isCompat) {
      const problem = api.nameProblem(draft.name, editing?.id ?? null, services);
      if (problem) {
        message = api.describe(problem);
        return;
      }
    }
    if (step === 3 && !keyOptional && !draft.api_key.trim()) {
      message = api.describe("key_missing");
      return;
    }
    // A text-only compatible endpoint hides the field: drop what was typed there before.
    if (step === 4 && isCompat && !wantAsr) draft.asr_timeout_ms = "";
    if (step === 4) {
      const problem = api.asrTimeoutProblem(draft.asr_timeout_ms);
      if (problem) {
        message = api.describe(problem);
        return;
      }
    }
    if (step === 4 && isCompat) {
      if ((wantAsr && !draft.asr_model.trim()) || (wantText && !draft.llm_model.trim())) {
        message = api.describe("model_required");
        return;
      }
      // Only test (and save) what this endpoint serves.
      if (!wantAsr) {
        draft.asr_model = "";
        draft.asr_timeout_ms = "";
      }
      if (!wantText) draft.llm_model = "";
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
        if (isCompat) {
          purposes.asr = r.supports.asr && wantAsr;
          purposes.polish = r.supports.polish && wantText;
          purposes.translate = r.supports.translate && wantText;
        }
      }
    }
  }

  // 「再新增一個端點」: same server, the other role (the port usually differs).
  function addAnother() {
    onDone(saved, { kind: "open_ai_compat", base_url: draft.base_url, role: otherRole[role] });
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
      <p class="note">
        <strong>一個端點＝一個服務。</strong>如果文字模型和語音辨識在不同的網址（例如
        <code>:8000/v1</code> 修字、<code>:8002/v1</code> 語音辨識），請分兩次新增：先完成一個，最後一步按「再新增一個端點」。
      </p>
      <label>
        端點網址（base_url）
        <input
          bind:value={draft.base_url}
          placeholder="http://10.0.0.5:8000/v1"
          spellcheck="false"
        />
      </label>
      {#if urlLooksOff}
        <p class="hint">網址通常以 <code>/v1</code> 結尾，請確認是否漏打。</p>
      {/if}
      <label>
        名稱（選填）
        <input
          bind:value={draft.name}
          placeholder={defaultId}
          maxlength={api.NAME_MAX_CHARS * 2}
          spellcheck="false"
        />
      </label>
      <p class="hint">
        例如「區網 Qwen」，最多 {api.NAME_MAX_CHARS} 個字。沒填就顯示代號 <code>{defaultId}</code>；改名不影響用途與金鑰。
      </p>
      <fieldset class="roles">
        <legend>這個端點提供什麼？</legend>
        {#each Object.entries(roles) as [key, r] (key)}
          <label class="check">
            <input type="radio" name="role" value={key} bind:group={role} />
            {r.label}
            <span class="hint">— {r.hint}</span>
          </label>
        {/each}
      </fieldset>
    {/if}
    <p class="warn">{isCompat ? lanNote : billingWarning}</p>
    <p class="hint">{pick("詳見安裝資料夾中的 docs\\cloud-setup.md", "詳見 /usr/lib/v2c/docs/cloud-setup.md")}</p>
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
    <p class="hint">金鑰只存在{pick(" Windows 認證管理員", " GNOME 登入鑰匙圈")}，不會寫進設定檔，畫面上也只顯示末 4 碼。</p>
  {:else if step === 4}
    <h2>選擇模型</h2>
    {#if busy}<p>正在讀取模型清單…</p>{/if}
    {#if modelNote}<p class="hint">{modelNote}，請自行輸入模型名稱。</p>{/if}
    <datalist id="cloud-models">
      {#each modelList ?? [] as m (m)}
        <option value={m}></option>
      {/each}
    </datalist>
    {#if isCompat}
      <p class="hint">
        端點：<code>{draft.base_url}</code>（{roles[role].label}）。模型名稱要和伺服器上的一樣；伺服器有提供清單時，點欄位就會出現。
      </p>
    {/if}
    {#if info.supports_asr && (!isCompat || wantAsr)}
      <label>
        語音辨識模型
        <input
          bind:value={draft.asr_model}
          list="cloud-models"
          spellcheck="false"
          placeholder={info.suggested_asr_model ?? "例如 breeze-asr-25 或 whisper-1"}
        />
      </label>
      <label>
        語音辨識逾時（毫秒，選填）
        <input
          bind:value={draft.asr_timeout_ms}
          inputmode="numeric"
          spellcheck="false"
          placeholder={`預設 ${info.default_asr_timeout_ms}`}
        />
      </label>
      <div class="picks">
        {#each asrTimeoutSuggestions as t (t.label)}
          <button type="button" onclick={() => (draft.asr_timeout_ms = String(t.ms))}>
            {t.label} 建議 {t.ms}
          </button>
        {/each}
        <button type="button" onclick={() => (draft.asr_timeout_ms = "")}>用預設</button>
      </div>
      {#if timeoutHint}
        <p class="hint">
          {timeoutHint.label} 建議逾時 {timeoutHint.ms} 毫秒，可按上面的按鈕填入。
        </p>
      {/if}
      <p class="hint">
        伺服器每句要花較久時（例如區網 Breeze-ASR-25 每句約 6 秒），請放寬這個服務的逾時，超過就會退回地端。留空使用預設值
        {info.default_asr_timeout_ms} 毫秒（設定檔 [timeouts] asr_cloud_ms），範圍 {api.ASR_TIMEOUT_MIN_MS}～{api.ASR_TIMEOUT_MAX_MS}。
      </p>
    {/if}
    {#if !isCompat || wantText}
      <label>
        文字模型（潤飾／中翻英）
        <input
          bind:value={draft.llm_model}
          list="cloud-models"
          spellcheck="false"
          placeholder={info.suggested_llm_model ?? "例如 qwen3-8b"}
        />
      </label>
    {/if}
    {#if modelList}<p class="hint">可以從下拉清單選擇，或直接輸入。</p>{/if}
  {:else if step === 5}
    <h2>測試連線</h2>
    <p class="hint">
      {#if isCompat}
        {wantAsr && wantText
          ? "會送出一段 3 秒的內建測試音檔與一句測試文字。"
          : wantAsr
            ? "會送出一段 3 秒的內建測試音檔（不是你的聲音）。"
            : "會送出一句測試文字。"}
        {#if wantAsr}
          伺服器較慢時，語音辨識可能因逾時失敗，可回上一步放寬「語音辨識逾時」（目前
          {draft.asr_timeout_ms.trim() || info.default_asr_timeout_ms} 毫秒）。
        {/if}
      {:else}
        {info.supports_asr ? "會送出一段 3 秒的內建測試音檔與一句測試文字。" : "會送出一句測試文字。"}
      {/if}
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
    <p>
      勾選要交給 {info.label}{isCompat ? `（${draft.base_url}）` : ""} 的用途，沒勾的維持原本的設定。
    </p>
    <p class="hint">同一個用途只能交給一個服務；勾選後，原本負責這個用途的服務會被取代。</p>
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
    <p>已儲存「{api.displayName(saved)}」（{saved?.label}）。</p>
    {#if isCompat && otherRole[role]}
      <p class="note">
        {role === "text"
          ? "語音辨識在另一個網址（例如同一台伺服器的 :8002/v1）嗎？"
          : "文字模型（潤飾、中翻英）在另一個網址（例如同一台伺服器的 :8000/v1）嗎？"}
        按下面的按鈕再新增一個端點，網址會先帶入這次的，只要改連接埠。
      </p>
    {/if}
    <p>
      想全部改回地端時，在系統列圖示按右鍵 →「全部改用地端」，立即生效；也可以在這裡移除服務。
    </p>
    <div class="nav">
      {#if isCompat && otherRole[role]}
        <button onclick={addAnother}>
          再新增一個端點（{roles[otherRole[role]].label}）
        </button>
      {/if}
      <button onclick={() => onDone(saved)}>回到雲端服務列表</button>
    </div>
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
  .note {
    padding: 0.5rem;
    border-left: 3px solid #0969da;
    background: color-mix(in srgb, #0969da 10%, transparent);
  }
  .picks {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }
  .roles {
    border: 1px solid color-mix(in srgb, currentColor 20%, transparent);
    padding: 0.25rem 0.75rem;
    margin: 0.5rem 0;
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
