<script>
  // 初次設定 (T1.11, plan §5.1 / §6.1): shown automatically when no local speech model is
  // installed. Local first: the flow itself only mentions cloud in the opt-in "略過，改用雲端
  // 服務" on the model step (the app menu, shown here too, always lists 雲端服務).
  import * as api from "../onboarding/api.js";
  import ModelStep from "../onboarding/ModelStep.svelte";
  import TryIt from "../onboarding/TryIt.svelte";
  import MicMeter from "../settings/MicMeter.svelte";
  import { Banner, Button, Card, LinearProgress } from "../lib/index.js";

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

  // Leaving through the menu without a model counts as skipping. (With a model, keep the
  // resume step: the restart that loads new models must come back here.)
  $effect(() => () => {
    // `st` is null until the status is known: then we cannot tell whether a model exists.
    if (st && !haveModel && step !== "done") {
      api.setResumeStep(null);
      api.setSkipped(true);
    }
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
    api.setSkipped(false);
    location.hash = "#/";
  }

  // Leave onboarding without (necessarily) a local model. Without one, the cloud page is
  // unlocked and onboarding no longer opens by itself at startup.
  function skip(to) {
    api.setResumeStep(null);
    if (!haveModel) api.setSkipped(true);
    location.hash = to;
  }
</script>

<div class="wrap">
  <div class="top">
    <p class="step-label">步驟 {index + 1} / {steps.length}　<strong>{titles[step]}</strong></p>
    {#if step !== "done"}
      <a
        href="#/"
        onclick={(e) => {
          e.preventDefault();
          skip("#/");
        }}>稍後再設定</a
      >
    {/if}
  </div>
  <LinearProgress value={(index + 1) / steps.length} label="初次設定進度" />

  {#if error}<div class="gap"><Banner variant="error">{error}</Banner></div>{/if}

  <Card tag="section" variant="elevated" padding="var(--md-space-6)" aria-label={titles[step]}>
    <h1>{titles[step]}</h1>
    {#if step === "welcome"}
      <p>v2c 讓你按住右 Ctrl 說中文或英文，放開後整理好的文字會輸入到游標處。</p>
      <ul>
        <li>語音辨識在你的電腦上執行：<b>免費、不需要帳號、不需要任何金鑰</b>。</li>
        <li>錄音只在記憶體中處理，不會存檔，也不會上傳。</li>
        <li>接下來會下載語音模型（只需要一次），再測試麥克風。</li>
      </ul>
      <div class="nav"><Button onclick={() => go("gpu")}>開始</Button></div>
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
        <div class="nav"><Button onclick={() => go("models")}>下一步</Button></div>
      {/if}
    {:else if step === "models"}
      <ModelStep
        recommended={st?.recommended ?? "sense_voice"}
        onInstalled={() => {
          installedNow = true;
          haveModel = true;
        }}
      />
      <div class="nav">
        <Button variant="outlined" onclick={() => go("gpu")}>上一步</Button>
        <Button disabled={!haveModel} onclick={afterModels}>
          {installedNow ? "載入模型並繼續（v2c 會重新啟動）" : "下一步"}
        </Button>
      </div>
      {#if !haveModel}
        <p class="hint">
          至少需要一個語音模型才能繼續。下載失敗也不影響 v2c 其他功能，可以稍後再回來（「設定 → 進階 → 重新執行初次設定」，或首頁的「前往初次設定」）。
        </p>
        <div class="alt">
          <p>不想下載，或下載一直失敗？也可以改用雲端語音辨識（需要你自己的金鑰，例如 OpenAI、Gemini，或區網的相容端點）。</p>
          <Button variant="tonal" onclick={() => skip("#/services")}>略過，改用雲端服務</Button>
        </div>
      {/if}
    {:else if step === "mic"}
      <p>按「測試麥克風」後說幾句話，音量條會跟著動。</p>
      <MicMeter onHeard={() => (heard = true)} />
      <p class="hint">要換麥克風，可以在完成後到「設定 → 麥克風」選擇。</p>
      <div class="nav">
        <Button variant="outlined" onclick={() => go("models")}>上一步</Button>
        <Button onclick={() => go("try")}>{heard ? "下一步" : "略過"}</Button>
      </div>
    {:else if step === "try"}
      <TryIt />
      <div class="nav">
        <Button variant="outlined" onclick={() => go("mic")}>上一步</Button>
        <Button onclick={() => go("done")}>下一步</Button>
      </div>
    {:else}
      <p>設定完成！v2c 會在背景執行，關閉這個視窗後仍可從系統列圖示開啟。</p>
      <ul>
        <li>按住右 Ctrl 說話；快速按兩下進入免持模式；Esc 取消。</li>
        <li>
          想要更通順的潤飾與「中文說、英文出」（右 Ctrl + Shift），可以自行安裝 LM Studio 並載入小模型（選用）。
          <button class="link" onclick={() => api.openDoc("local-setup")}>LM Studio 設定說明</button>
        </li>
      </ul>
      <div class="nav"><Button onclick={finish}>開始使用</Button></div>
    {/if}
  </Card>
</div>

<style>
  .wrap {
    display: grid;
    gap: var(--md-space-3);
    max-width: 640px;
    margin: 0 auto;
  }
  .top {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--md-space-3);
    font-size: var(--md-type-body-sm-size);
  }
  .step-label {
    margin: 0;
    color: var(--md-on-surface-variant);
  }
  .step-label strong {
    color: var(--md-on-surface);
  }
  .gap {
    margin: 0;
  }
  .nav {
    display: flex;
    flex-wrap: wrap;
    gap: var(--md-space-2);
    margin-top: var(--md-space-5);
  }
  .alt {
    margin-top: var(--md-space-4);
    padding: var(--md-space-3) var(--md-space-4);
    border: 1px dashed var(--md-outline);
    border-radius: var(--md-radius-md);
  }
  .alt p {
    margin: 0 0 var(--md-space-2);
  }
</style>
