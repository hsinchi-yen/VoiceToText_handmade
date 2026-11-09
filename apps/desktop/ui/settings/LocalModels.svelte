<script>
  // Local speech models and LM Studio (polish / translate).
  import * as api from "./api.js";

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
      <option value="sense_voice">SenseVoice-Small（CPU）</option>
    </select>
  </label>
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
</style>
