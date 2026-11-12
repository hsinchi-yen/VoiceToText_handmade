<script>
  // Hotkey timing. The keys themselves are fixed (the keyboard hook supports Right Ctrl only).
  import { TextField } from "../lib/index.js";
  import Row from "./Row.svelte";

  let { view, save } = $props();

  const cfg = $derived(view.config);
  const dirty = (field) => view.dirty?.has(field);
  const keyLabel = { RightCtrl: "右 Ctrl", "RightCtrl+Shift": "右 Ctrl + Shift" };

  /** Saves a number input; puts the old value back if it was refused. */
  async function saveNumber(e, field, old) {
    const ok = await save([{ field, value: Number(e.currentTarget.value) }]);
    if (!ok) e.currentTarget.value = old;
  }
</script>

<p>
  輸入：<b>{keyLabel[cfg.hotkey.dictate] ?? cfg.hotkey.dictate}</b>（按住說話；快速按兩下進入免持，再按一下結束）<br />
  中翻英：<b>{keyLabel[cfg.hotkey.translate] ?? cfg.hotkey.translate}</b>　取消：Esc
</p>
<p class="hint">目前的鍵盤攔截只支援右 Ctrl，因此按鍵無法更改；之後的版本會開放錄製其他按鍵。</p>
<div class="grid">
  <Row dirty={dirty("hold_threshold_ms")}>
    <TextField type="number" min="150" max="1500" step="10" label="按住多久算「按住說話」（毫秒）"
      value={cfg.hotkey.hold_threshold_ms}
      onchange={(e) => saveNumber(e, "hold_threshold_ms", cfg.hotkey.hold_threshold_ms)} />
  </Row>
  <Row dirty={dirty("double_tap_window_ms")}>
    <TextField type="number" min="150" max="1000" step="10" label="雙擊間隔（毫秒）"
      value={cfg.hotkey.double_tap_window_ms}
      onchange={(e) => saveNumber(e, "double_tap_window_ms", cfg.hotkey.double_tap_window_ms)} />
  </Row>
  <Row dirty={dirty("hands_free_max_secs")}>
    <TextField type="number" min="10" max="3600" step="10" label="免持模式最長（秒）"
      value={cfg.hotkey.hands_free_max_secs}
      onchange={(e) => saveNumber(e, "hands_free_max_secs", cfg.hotkey.hands_free_max_secs)} />
  </Row>
</div>

<style>
  p {
    margin: 0;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
    gap: var(--md-space-3);
  }
</style>
