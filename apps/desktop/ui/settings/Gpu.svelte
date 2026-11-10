<script>
  // [gpu] settings + live VRAM bar (plan §5.1). Polls `gpu_usage` every 2 s only while this
  // page is open and the window is visible.
  import * as api from "./api.js";

  let { view, save } = $props();

  const g = $derived(view.config.gpu);
  const dirty = (field) => view.dirty?.has(field);
  let usage = $state(null);
  let heavy = $state("");
  $effect(() => {
    heavy = g.heavy_apps.join("\n");
  });

  $effect(() => {
    let stopped = false;
    async function poll() {
      if (stopped || document.visibilityState === "hidden") return;
      try {
        usage = await api.gpuUsage();
      } catch {
        usage = null;
      }
    }
    poll();
    const t = setInterval(poll, 2000);
    return () => {
      stopped = true;
      clearInterval(t);
    };
  });

  const profiles = [
    { value: "balanced", label: "均衡（預設）", note: "Breeze q5_0 上顯示卡＋LM Studio 3B，約 3.8 GB" },
    { value: "low_vram", label: "省顯存", note: "Breeze 上顯示卡，LM Studio 用小模型或 CPU，約 1.5～2.7 GB" },
    { value: "cpu_only", label: "不用顯卡", note: "SenseVoice（CPU），0 GB；適合玩遊戲、剪片" },
  ];

  const pct = (mb) => (usage && usage.total_mb ? `${(mb / usage.total_mb) * 100}%` : "0%");
  const others = $derived(usage ? Math.max(0, usage.used_mb - (usage.v2c_mb ?? 0)) : 0);
  const roomForBreeze = $derived(usage ? usage.free_mb - usage.reserve_mb >= usage.breeze_need_mb : false);

  async function saveNumber(e, field, old) {
    const ok = await save([{ field, value: Number(e.currentTarget.value) }]);
    if (!ok) e.currentTarget.value = old;
  }
</script>

<section class="settings-section">
  <h2>顯示卡記憶體（VRAM）</h2>
  {#if !usage}
    <p class="hint">讀取中…</p>
  {:else if !usage.available}
    <p class="hint">沒有偵測到 NVIDIA 顯示卡：語音辨識使用 SenseVoice（CPU）。</p>
  {:else}
    <p>{usage.name ?? "NVIDIA GPU"}：共 {usage.total_mb} MB</p>
    <div class="bar" aria-label="顯示卡記憶體使用量">
      {#if usage.v2c_mb}<div class="v2c" style:width={pct(usage.v2c_mb)} title="v2c"></div>{/if}
      <div class="others" style:width={pct(others)} title="其他程式"></div>
      <div class="reserve" style:left={`calc(100% - ${pct(usage.reserve_mb)})`} title="保留額度"></div>
    </div>
    <p class="legend">
      {#if usage.v2c_mb}<span class="sw v2c"></span>v2c {usage.v2c_mb} MB　{/if}
      <span class="sw others"></span>{usage.v2c_mb ? "其他程式" : "所有程式"} {others} MB
      <span class="sw free"></span>可用 {usage.free_mb} MB
      <span class="sw reserve-sw"></span>保留 {usage.reserve_mb} MB
    </p>
    <p class="hint">
      Breeze（{view.config.local.breeze_quant}）約需 {usage.breeze_need_mb} MB：
      {roomForBreeze ? "目前放得下。" : "目前可用量扣掉保留額度後放不下，會改用 CPU。"}
      {usage.cuda_build ? "" : "這個版本未啟用 CUDA。"}
    </p>
  {/if}

  {#each profiles as p (p.value)}
    <label class:dirty={dirty("gpu_profile") && g.profile === p.value}>
      <input type="radio" name="gpu_profile" value={p.value} checked={g.profile === p.value}
        onchange={() => save([{ field: "gpu_profile", value: p.value }])} />
      <b>{p.label}</b>：{p.note}
    </label>
  {/each}
  <p class="hint">上面的用量是估計值；LM Studio 的部分要在 LM Studio 自己調整。</p>
  <label class:dirty={dirty("gpu_reserve_mb")}>
    保留給畫面與其他程式（MB）
    <input type="number" min="0" max="16384" step="100" value={g.vram_reserve_mb}
      onchange={(e) => saveNumber(e, "gpu_reserve_mb", g.vram_reserve_mb)} />
  </label>
  <label class:dirty={dirty("gpu_keep_loaded")}>
    模型常駐：
    <select value={g.keep_loaded} onchange={(e) => save([{ field: "gpu_keep_loaded", value: e.currentTarget.value }])}>
      <option value="auto">自動（閒置卸載、顯存不足讓路）</option>
      <option value="always">一直載入</option>
      <option value="on_demand">用到才載入</option>
    </select>
  </label>
  <label class:dirty={dirty("gpu_idle_unload_minutes")}>
    閒置幾分鐘後卸載
    <input type="number" min="1" max="240" value={g.idle_unload_minutes}
      onchange={(e) => saveNumber(e, "gpu_idle_unload_minutes", g.idle_unload_minutes)} />
  </label>
  <label class:dirty={dirty("gpu_yield_on_fullscreen")}>
    <input type="checkbox" checked={g.yield_on_fullscreen}
      onchange={(e) => save([{ field: "gpu_yield_on_fullscreen", value: e.currentTarget.checked }])} />
    全螢幕程式（遊戲）在前景時改用 CPU
  </label>
  <label for="heavy-apps" class:dirty={dirty("gpu_heavy_apps")}>吃顯卡的程式（在前景時改用 CPU，一行一個執行檔名稱）</label>
  <textarea id="heavy-apps" rows="3" bind:value={heavy} spellcheck="false"
    onchange={() => save([{ field: "gpu_heavy_apps", value: api.parseExeList(heavy) }])}></textarea>
</section>

<style>
  .bar {
    position: relative;
    display: flex;
    height: 14px;
    border-radius: 4px;
    overflow: hidden;
    background: color-mix(in srgb, #2da44e 35%, transparent);
  }
  .v2c {
    background: #2f81f7;
  }
  .others {
    background: #8c959f;
  }
  .reserve {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    background: #cf222e;
  }
  .legend {
    font-size: 0.88em;
  }
  .sw {
    display: inline-block;
    width: 0.8em;
    height: 0.8em;
    margin-right: 0.25em;
    border-radius: 2px;
    vertical-align: -0.05em;
  }
  .sw.v2c {
    background: #2f81f7;
  }
  .sw.others {
    background: #8c959f;
  }
  .sw.free {
    background: color-mix(in srgb, #2da44e 35%, transparent);
  }
  .sw.reserve-sw {
    background: #cf222e;
  }
</style>
