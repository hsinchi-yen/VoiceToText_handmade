<script>
  // Local speech models: engine, Breeze quantisation, and 「測試」 (runs the built-in 3 s test
  // clip through the engine the saved settings use; the transcript is never shown or kept).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "./api.js";
  import * as models from "../onboarding/api.js";

  let { view, save } = $props();

  const cfg = $derived(view.config);
  const dirty = (field) => view.dirty?.has(field);
  // The test runs the saved settings, not the draft.
  const testOutdated = $derived(["local_asr", "breeze_quant", "gpu_profile", "gpu_reserve_mb"].some(dirty));
  let testing = $state(false);
  let result = $state(null);
  let testError = $state("");

  const have = (ok) => (ok ? "已安裝" : "未安裝");
  const mark = { passed: "✓", failed: "✗", skipped: "–" };

  // TEA-ASR model: status and download (2.18 GB) from models.json entry `tea-asr`.
  const TEA_ID = "tea-asr";
  let tea = $state(null);
  let teaBusy = $state(false);
  let teaProgress = $state(null);
  let teaError = $state("");

  async function refreshTea() {
    try {
      tea = (await models.models()).entries.find((e) => e.id === TEA_ID) ?? null;
    } catch (code) {
      teaError = models.describe(code);
    }
  }

  $effect(() => {
    refreshTea();
    const un = listen("v2c:model-progress", (e) => {
      if (e.payload.id === TEA_ID) teaProgress = e.payload;
    });
    return () => un.then((f) => f());
  });

  async function downloadTea() {
    teaError = "";
    teaBusy = true;
    try {
      await models.download(TEA_ID);
    } catch (code) {
      if (code === "not_gguf") teaError = "下載的檔案不是 GGUF 模型檔，已刪除，請重新下載";
      else if (code !== "cancelled") teaError = models.describe(code);
    } finally {
      teaBusy = false;
      teaProgress = null;
      await refreshTea();
    }
  }

  const pct = (p) => (p?.total ? Math.floor((p.done / p.total) * 100) : null);

  async function test() {
    testing = true;
    result = null;
    testError = "";
    try {
      result = await api.testLocalAsr();
    } catch (code) {
      testError = code === "already_running" ? "測試進行中，請稍候" : api.describe(code);
    } finally {
      testing = false;
    }
  }
</script>

<section class="settings-section">
  <h2>地端語音模型</h2>
  <div class="row">
    <label class:dirty={dirty("local_asr")}>
      辨識引擎：
      <select value={cfg.local.asr} onchange={(e) => save([{ field: "local_asr", value: e.currentTarget.value }])}>
        {#each api.localAsrOptions(view) as o (o.value)}
          <option value={o.value}>{o.label}</option>
        {/each}
      </select>
    </label>
    <button onclick={test} disabled={testing}>{testing ? "處理中…" : "測試"}</button>
  </div>
  {#if testOutdated}
    <p class="hint">「測試」使用已儲存的設定；要測試剛改的引擎，請先按上方「儲存」。</p>
  {/if}
  {#if testError}<p class="bad">{testError}</p>{/if}
  {#if result}
    <p class={result.ok ? "ok" : "bad"}><strong>{result.summary}</strong></p>
    <table class="steps">
      <tbody>
        {#each result.steps as s (s.label)}
          <tr>
            <td class={s.status}>{mark[s.status]}</td>
            <td>{s.label}</td>
            <td>{s.detail ?? ""}</td>
            <td class="ms">{s.latency_ms != null ? `${s.latency_ms} ms` : ""}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  <label class:dirty={dirty("breeze_quant")}>
    Breeze 版本：
    <select value={cfg.local.breeze_quant}
      onchange={(e) => save([{ field: "breeze_quant", value: e.currentTarget.value }])}>
      <option value="q5_0">q5_0（約 1.5 GB 顯存，{have(view.breeze_q5_0)}）</option>
      <option value="q8_0">q8_0（較準、多約 0.6 GB，{have(view.breeze_q8_0)}）</option>
    </select>
  </label>
  <p class="hint">
    SenseVoice：{have(view.sensevoice)}。{view.cuda_build ? "" : "這個版本沒有 CUDA，Breeze 無法使用顯示卡。"}
    下載或匯入模型請到 <a href="#/onboarding">初次設定</a>。
  </p>
  <div class="tea">
    <b>TEA-ASR-1.1 模型</b>（顯示卡，約需 2.6 GB 顯存；台灣華語與中英夾雜較準，幾乎不加標點，靠潤飾補上）：
    {#if !view.tea_build}
      <!-- No CUDA build (the Linux package, dev builds): the model could not run here, so do
           not offer the 2.2 GB download. Windows release builds have CUDA and are unchanged. -->
      <span class="hint">這個版本無法在本機執行（需要 NVIDIA 顯示卡與 CUDA 版）。要用 TEA-ASR，請在「雲端服務」接上提供它的伺服器。</span>
    {:else if tea?.installed || view.tea_asr}
      <span>已安裝</span>
    {:else if teaBusy}
      <button onclick={() => models.cancel(TEA_ID)}>暫停</button>
    {:else if tea?.downloadable}
      <button onclick={downloadTea}>
        {tea.partial_bytes > 0 ? `繼續下載（已下載 ${models.mb(tea.partial_bytes)}）` : `下載（${models.mb(tea.size)}）`}
      </button>
    {:else if tea}
      <span class="hint">尚未提供下載</span>
    {/if}
    {#if teaBusy && teaProgress}
      <div class="bar"><div style:width={`${pct(teaProgress) ?? 0}%`}></div></div>
      <span class="hint">
        {pct(teaProgress) != null ? `${pct(teaProgress)}%（${models.mb(teaProgress.done)} / ${models.mb(teaProgress.total)}）` : ""}
      </span>
    {/if}
    {#if teaError}<p class="bad">{teaError}</p>{/if}
    {#if view.tea_build}<p class="hint">下載完成後不用重新啟動；選了 TEA-ASR-1.1 就會在幾秒內載入。也可以手動把 TEA-ASR-1.1-Q8_0.gguf 放進模型資料夾，見
      <button class="link" onclick={() => api.openDoc("local-setup")}>設定說明</button>。</p>{/if}
  </div>
</section>

<style>
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.25rem 0.75rem;
  }
  .steps {
    border-collapse: collapse;
    margin: 0.25rem 0 0.5rem;
  }
  .steps td {
    padding: 0.15rem 0.4rem;
    vertical-align: top;
  }
  .steps .ms {
    white-space: nowrap;
    opacity: 0.75;
  }
  .passed {
    color: #1a7f37;
  }
  .failed {
    color: #cf222e;
  }
  .tea {
    margin-top: 0.6em;
  }
  .bar {
    height: 6px;
    background: #d0d7de;
    border-radius: 3px;
    margin: 0.3em 0;
    max-width: 24em;
  }
  .bar > div {
    height: 100%;
    background: #2f81f7;
    border-radius: 3px;
  }
</style>
