<script>
  // Local speech models: engine, Breeze quantisation, and 「測試」 (runs the built-in 3 s test
  // clip through the engine the saved settings use; the transcript is never shown or kept).
  import * as api from "./api.js";

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

  async function test() {
    testing = true;
    result = null;
    testError = "";
    try {
      result = await api.testLocalAsr();
    } catch (code) {
      testError = api.describe(code);
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
</style>
