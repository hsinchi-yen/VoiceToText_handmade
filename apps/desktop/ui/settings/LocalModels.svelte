<script>
  // Local speech models and LM Studio (polish / translate).
  import { listen } from "@tauri-apps/api/event";
  import * as api from "./api.js";
  import * as models from "../onboarding/api.js";

  let { view, save } = $props();

  const cfg = $derived(view.config);
  let baseUrl = $state("");
  let model = $state("");
  let testing = $state(false);
  let result = $state(null);
  let testError = $state("");

  // Re-sync the text fields when the saved config changes.
  $effect(() => {
    baseUrl = cfg.local.llm_base_url;
    model = cfg.local.llm_model ?? "";
  });

  const have = (ok) => (ok ? "已安裝" : "未安裝");

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
      result = await api.testLm(baseUrl);
    } catch (code) {
      testError = api.describe(code);
    } finally {
      testing = false;
    }
  }

  async function saveLm() {
    await save([
      { field: "llm_base_url", value: baseUrl },
      { field: "llm_model", value: model.trim() || null },
    ]);
  }
</script>

<section class="settings-section">
  <h2>地端語音模型</h2>
  <label>
    辨識引擎：
    <select value={cfg.local.asr} onchange={(e) => save([{ field: "local_asr", value: e.currentTarget.value }])}>
      <option value="auto">自動（有 NVIDIA 顯示卡用 Breeze，否則 SenseVoice）</option>
      <option value="breeze">Breeze-ASR-25（顯示卡）</option>
      <option value="tea_asr">TEA-ASR-1.1（顯示卡，{have(view.tea_asr)}）</option>
      <option value="sense_voice">SenseVoice-Small（CPU）</option>
    </select>
  </label>
  {#if cfg.local.asr === "tea_asr" && !view.tea_build}
    <p class="bad">這個版本沒有內建 TEA-ASR（需要 CUDA 版），目前改用 SenseVoice。</p>
  {:else if cfg.local.asr === "tea_asr" && !view.tea_asr}
    <p class="bad">尚未安裝 TEA-ASR 模型，目前改用 SenseVoice。請在下方下載。</p>
  {/if}
  <label>
    Breeze 版本：
    <select value={cfg.local.breeze_quant}
      onchange={(e) => save([{ field: "breeze_quant", value: e.currentTarget.value }])}>
      <option value="q5_0">q5_0（約 1.5 GB 顯存，{have(view.breeze_q5_0)}）</option>
      <option value="q8_0">q8_0（較準、多約 0.6 GB，{have(view.breeze_q8_0)}）</option>
    </select>
  </label>
  <p class="hint">
    SenseVoice：{have(view.sensevoice)}。{view.cuda_build ? "" : "這個版本未啟用 CUDA，Breeze 無法使用顯示卡。"}
    下載或匯入模型請到 <a href="#/onboarding">初次設定</a>。
  </p>
  <div class="tea">
    <b>TEA-ASR-1.1 模型</b>（顯示卡，約需 2.6 GB 顯存；台灣華語與中英夾雜較準，幾乎不加標點，靠潤飾補上）：
    {#if tea?.installed || view.tea_asr}
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
    <p class="hint">下載完成後不用重新啟動；選了 TEA-ASR-1.1 就會在幾秒內載入。也可以手動把 TEA-ASR-1.1-Q8_0.gguf 放進模型資料夾，見
      <button class="link" onclick={() => api.openDoc("local-setup")}>設定說明</button>。</p>
  </div>
</section>

<section class="settings-section">
  <h2>LM Studio（潤飾與中翻英，選用）</h2>
  <p class="hint">
    在 LM Studio 載入 3B～4B 的模型並開啟 Local Server。
    <button class="link" onclick={() => api.openDoc("local-setup")}>設定說明</button>
  </p>
  <label>
    位址
    <input type="text" size="34" bind:value={baseUrl} onchange={saveLm} spellcheck="false" />
  </label>
  <label>
    模型
    <input type="text" size="28" bind:value={model} onchange={saveLm} list="lm-models"
      placeholder="留空＝使用 LM Studio 目前載入的模型" spellcheck="false" />
    <datalist id="lm-models">
      {#each result?.models ?? [] as m (m)}<option value={m}></option>{/each}
    </datalist>
  </label>
  <button onclick={test} disabled={testing}>{testing ? "測試中…" : "測試連線"}</button>
  {#if result}
    <span>連線成功（{result.latency_ms} ms），{result.models.length} 個模型可用</span>
  {/if}
  {#if testError}<span class="bad">{testError}</span>{/if}
</section>

<style>
  .link {
    background: none;
    border: none;
    padding: 0;
    color: #2f81f7;
    cursor: pointer;
    text-decoration: underline;
    font: inherit;
  }
  .bad {
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
