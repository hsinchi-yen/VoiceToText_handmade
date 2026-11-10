<script>
  // Hotkeys, polish, per-purpose routing, start with Windows.
  import * as api from "./api.js";
  import { displayName } from "../cloud-setup/api.js";

  let { view, save } = $props();

  const cfg = $derived(view.config);
  const keyLabel = { RightCtrl: "右 Ctrl", "RightCtrl+Shift": "右 Ctrl + Shift" };
  const purposes = [
    { field: "route_asr", key: "asr", label: "語音辨識", asr: true },
    { field: "route_polish", key: "polish", label: "潤飾", asr: false },
    { field: "route_translate", key: "translate", label: "中翻英", asr: false },
  ];
  let autostartBusy = $state(false);
  let autostartError = $state("");

  /** Saves a number input; puts the old value back if it was refused. */
  async function saveNumber(e, field, old) {
    const ok = await save([{ field, value: Number(e.currentTarget.value) }]);
    if (!ok) e.currentTarget.value = old;
  }

  async function toggleAutostart(e) {
    autostartBusy = true;
    autostartError = "";
    try {
      view.autostart = await api.setAutostart(e.currentTarget.checked);
    } catch (code) {
      autostartError = api.describe(code);
      e.currentTarget.checked = !!view.autostart;
    } finally {
      autostartBusy = false;
    }
  }
</script>

<section class="settings-section">
  <h2>熱鍵</h2>
  <p>
    輸入：<b>{keyLabel[cfg.hotkey.dictate] ?? cfg.hotkey.dictate}</b>（按住說話；快速按兩下進入免持，再按一下結束）<br />
    中翻英：<b>{keyLabel[cfg.hotkey.translate] ?? cfg.hotkey.translate}</b>　取消：Esc
  </p>
  <p class="hint">目前的鍵盤攔截只支援右 Ctrl，因此按鍵無法更改；之後的版本會開放錄製其他按鍵。</p>
  <label>
    按住多久算「按住說話」（毫秒）
    <input type="number" min="150" max="1500" step="10" value={cfg.hotkey.hold_threshold_ms}
      onchange={(e) => saveNumber(e, "hold_threshold_ms", cfg.hotkey.hold_threshold_ms)} />
  </label>
  <label>
    雙擊間隔（毫秒）
    <input type="number" min="150" max="1000" step="10" value={cfg.hotkey.double_tap_window_ms}
      onchange={(e) => saveNumber(e, "double_tap_window_ms", cfg.hotkey.double_tap_window_ms)} />
  </label>
  <label>
    免持模式最長（秒）
    <input type="number" min="10" max="3600" step="10" value={cfg.hotkey.hands_free_max_secs}
      onchange={(e) => saveNumber(e, "hands_free_max_secs", cfg.hotkey.hands_free_max_secs)} />
  </label>
</section>

<section class="settings-section">
  <h2>潤飾與用途</h2>
  <label>
    <input type="checkbox" checked={cfg.polish.enabled}
      onchange={(e) => save([{ field: "polish_enabled", value: e.currentTarget.checked }])} />
    潤飾（去贅字、全形標點、確保繁體；不改語意）
  </label>
  <p class="hint">潤飾與中翻英預設使用你自己的 LM Studio；沒開時潤飾改用規則式去贅字。</p>
  {#each purposes as p (p.field)}
    <label>
      {p.label}：
      <select value={cfg.route[p.key]}
        onchange={(e) => save([{ field: p.field, value: e.currentTarget.value }])}>
        <option value="local">地端（{p.asr ? "Breeze／SenseVoice" : "LM Studio"}）</option>
        {#each view.cloud.filter((s) => !p.asr || s.supports_asr) as s (s.id)}
          <option value={s.id}>{api.kindLabels[s.kind] ?? s.kind}：{displayName(cfg.cloud.find((c) => c.id === s.id) ?? s)}</option>
        {/each}
      </select>
    </label>
  {/each}
  {#if view.cloud.length}
    <label>
      <input type="checkbox" checked={cfg.route.cloud_fallback_to_local}
        onchange={(e) => save([{ field: "cloud_fallback_to_local", value: e.currentTarget.checked }])} />
      雲端失敗時自動改用地端
    </label>
  {/if}
  {#if view.sensevoice || view.breeze_q5_0 || view.breeze_q8_0}
    <p class="hint">要使用雲端服務，請先到 <a href="#/cloud-setup">雲端服務</a> 頁串接（選用，會依用量收費）。</p>
  {/if}
</section>

<section class="settings-section">
  <h2>啟動</h2>
  <label>
    <input type="checkbox" checked={!!view.autostart} disabled={autostartBusy || view.autostart === null}
      onchange={toggleAutostart} />
    開機時自動啟動 v2c（在背景執行，只顯示系統列圖示）
  </label>
  {#if autostartError}<p class="hint">{autostartError}</p>{/if}
</section>
