<script>
  // 初次設定 (T1.11, plan §5.1 / §6.1): shown automatically when no local speech model is
  // installed. Local first: no cloud options anywhere in this flow.
  import * as api from "../onboarding/api.js";
  import ModelStep from "../onboarding/ModelStep.svelte";
  import TryIt from "../onboarding/TryIt.svelte";
  import MicMeter from "../settings/MicMeter.svelte";

  const steps = ["welcome", "gpu", "models", "mic", "try", "done"];
  const titles = {
    welcome: "歡迎使用 v2c",
    gpu: "偵測顯示卡",
    models: "下載語音模型",
    mic: "測試麥克風",
    try: "試試看",
    done: "完成",
  };

  let step = $state(steps.includes(api.resumeStep()) ? api.resumeStep() : "welcome");
  let st = $state(null);
  let installedNow = $state(false);
  let haveModel = $state(false);
  let heard = $state(false);
  let error = $state("");

  async function refresh() {
    try {
      st = await api.status();
      haveModel = !st.needed;
    } catch (code) {
      error = api.describe(code);
    }
  }

  $effect(() => {
    refresh();
  });

  const index = $derived(steps.indexOf(step));
  const go = (s) => {
    step = s;
    api.setResumeStep(s === "done" ? null : s);
  };

  async function afterModels() {
    if (installedNow) {
      // The engine picks up new models at startup: restart and continue at the mic step.
      api.setResumeStep("mic");
      try {
        await api.restart();
      } catch (code) {
        error = api.describe(code);
      }
    } else {
      go("mic");
    }
  }

  function finish() {
    api.setResumeStep(null);
    location.hash = "#/";
  }
</script>

<h1>{titles[step]}</h1>
<ol class="steps">
  {#each steps as s, i (s)}
    <li class:current={i === index} class:past={i < index}>{titles[s]}</li>
  {/each}
</ol>

{#if error}<p class="bad">{error}</p>{/if}

{#if step !== "done"}
  <p class="skip"><a href="#/" onclick={() => api.setResumeStep(null)}>稍後再設定</a></p>
{/if}

{#if step === "welcome"}
  <p>v2c 讓你按住右 Ctrl 說中文或英文，放開後整理好的文字會輸入到游標處。</p>
  <ul>
    <li>語音辨識在你的電腦上執行：<b>免費、不需要帳號、不需要任何金鑰</b>。</li>
    <li>錄音只在記憶體中處理，不會存檔，也不會上傳。</li>
    <li>接下來會下載語音模型（只需要一次），再測試麥克風。</li>
  </ul>
  <button onclick={() => go("gpu")}>開始</button>
{:else if step === "gpu"}
  {#if !st}
    <p>偵測中…</p>
  {:else}
    {#if st.nvidia}
      <p>偵測到 <b>{st.gpu_name ?? "NVIDIA 顯示卡"}</b>（{st.total_vram_mb} MB 顯示記憶體）。</p>
    {:else}
      <p>沒有偵測到 NVIDIA 顯示卡。</p>
    {/if}
    {#if st.recommended === "breeze_q5_0"}
      <p>建議使用 <b>Breeze-ASR-25 q5_0</b>（在顯示卡上執行，台灣中文辨識最準），並下載 SenseVoice 作為備援。</p>
    {:else}
      <p>
        將使用 <b>SenseVoice-Small</b>（在 CPU 上執行，不需要顯示卡）。
        {#if st.nvidia && !st.cuda_build}這個版本未啟用 CUDA，因此不使用顯示卡。{/if}
      </p>
    {/if}
    <button onclick={() => go("models")}>下一步</button>
  {/if}
{:else if step === "models"}
  <ModelStep
    recommended={st?.recommended ?? "sense_voice"}
    onInstalled={() => {
      installedNow = true;
      haveModel = true;
    }}
  />
  <p>
    <button onclick={() => go("gpu")}>上一步</button>
    <button disabled={!haveModel} onclick={afterModels}>
      {installedNow ? "載入模型並繼續（v2c 會重新啟動）" : "下一步"}
    </button>
  </p>
  {#if !haveModel}<p class="hint">至少需要一個語音模型才能繼續。下載失敗也不影響 v2c 其他功能，可以稍後再回來。</p>{/if}
{:else if step === "mic"}
  <p>按「測試麥克風」後說幾句話，音量條會跟著動。</p>
  <MicMeter onHeard={() => (heard = true)} />
  <p class="hint">要換麥克風，可以在完成後到「設定 → 麥克風」選擇。</p>
  <p>
    <button onclick={() => go("models")}>上一步</button>
    <button onclick={() => go("try")}>{heard ? "下一步" : "略過"}</button>
  </p>
{:else if step === "try"}
  <TryIt />
  <p>
    <button onclick={() => go("mic")}>上一步</button>
    <button onclick={() => go("done")}>下一步</button>
  </p>
{:else}
  <p>設定完成！v2c 會在背景執行，關閉這個視窗後仍可從系統列圖示開啟。</p>
  <ul>
    <li>按住右 Ctrl 說話；快速按兩下進入免持模式；Esc 取消。</li>
    <li>
      想要更通順的潤飾與「中文說、英文出」（右 Ctrl + Shift），可以自行安裝 LM Studio 並載入小模型（選用）。
      <button class="link" onclick={() => api.openDoc("local-setup")}>LM Studio 設定說明</button>
    </li>
  </ul>
  <button onclick={finish}>開始使用</button>
{/if}

<style>
  .steps {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem 1rem;
    padding: 0;
    margin: 0 0 1rem;
    list-style: none;
    font-size: 0.88em;
  }
  .steps li {
    opacity: 0.45;
  }
  .steps li.past {
    opacity: 0.7;
  }
  .steps li.current {
    opacity: 1;
    font-weight: 600;
  }
  .skip a {
    color: #2f81f7;
  }
  .skip {
    text-align: right;
    margin: 0;
    font-size: 0.88em;
  }
  .hint {
    opacity: 0.75;
    font-size: 0.9em;
  }
  .bad {
    color: #cf222e;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    color: #2f81f7;
    cursor: pointer;
    text-decoration: underline;
    font: inherit;
  }
</style>
