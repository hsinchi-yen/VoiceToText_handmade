<script>
  import { emit, listen } from "@tauri-apps/api/event";

  // Floating status pill (T1.3). Payloads come from src-tauri/src/overlay.rs (`v2c:overlay`):
  // states, a volume level, and fixed UI strings. Never any recognised text.
  // Once the listener is registered we emit `v2c:overlay-ready`; Rust then re-sends whatever
  // was emitted before we were listening (e.g. a startup error).
  const FADE_MS = 300;

  let view = $state({ kind: "hidden" });
  let fading = $state(false);
  let fadeTimer;

  $effect(() => {
    // The overlay window is transparent: drop the app background for this route only.
    const root = document.documentElement;
    const prev = [root.style.background, document.body.style.background];
    root.style.background = "transparent";
    document.body.style.background = "transparent";

    let unlisten;
    let disposed = false;
    listen("v2c:overlay", (e) => {
      clearTimeout(fadeTimer);
      fading = false;
      view = e.payload.view;
      const hideAfter = e.payload.hide_after_ms;
      if (hideAfter != null) {
        fadeTimer = setTimeout(() => (fading = true), Math.max(0, hideAfter - FADE_MS));
      }
    }).then((u) => {
      if (disposed) {
        u();
        return;
      }
      unlisten = u;
      emit("v2c:overlay-ready").catch(() => {});
    });

    return () => {
      disposed = true;
      unlisten?.();
      clearTimeout(fadeTimer);
      [root.style.background, document.body.style.background] = prev;
    };
  });

  const label = $derived(
    {
      recording: "錄音中",
      transcribing: "辨識中…",
      polishing: "潤飾中…",
    }[view.kind] ??
      view.message ??
      "",
  );
</script>

{#if view.kind !== "hidden"}
  <div class="stage">
    <div class={`pill ${view.kind}`} class:fading role="status" aria-live="polite">
      {#if view.kind === "recording"}
        <span class="dot"></span>
        <span class="label">{label}</span>
        {#if view.hands_free}
          <span class="badge">免持</span>
        {/if}
        <span class="meter"><span style:width={`${(view.level ?? 0) * 100}%`}></span></span>
        {#if view.hands_free}
          <span class="hint">按右 Ctrl 結束</span>
        {/if}
      {:else}
        {#if view.kind === "transcribing" || view.kind === "polishing"}
          <span class="spinner"></span>
        {:else if view.kind === "done"}
          <span class="icon">✓</span>
        {:else if view.kind === "error"}
          <span class="icon">!</span>
        {/if}
        <span class="label">{label}</span>
      {/if}
    </div>
  </div>
{/if}

<style>
  .stage {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
    user-select: none;
  }
  .pill {
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: calc(100vw - 8px);
    height: 40px;
    padding: 0 16px;
    border-radius: 20px;
    background: rgba(28, 31, 36, 0.92);
    color: #f0f3f6;
    font-size: 14px;
    white-space: nowrap;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.35);
    opacity: 1;
    transition: opacity 300ms ease-out;
  }
  .pill.fading {
    opacity: 0;
  }
  .pill.error {
    background: rgba(140, 32, 32, 0.94);
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .dot {
    flex: none;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: #f85149;
    animation: pulse 1s ease-in-out infinite;
  }
  .meter {
    flex: none;
    width: 96px;
    height: 6px;
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.18);
    overflow: hidden;
  }
  .meter span {
    display: block;
    height: 100%;
    background: #3fb950;
    transition: width 50ms linear;
  }
  .badge {
    flex: none;
    padding: 1px 6px;
    border-radius: 6px;
    background: #1f6feb;
    color: #fff;
    font-size: 12px;
    font-weight: 600;
  }
  .hint {
    flex: none;
    color: rgba(240, 243, 246, 0.7);
    font-size: 12px;
  }
  .spinner {
    flex: none;
    width: 12px;
    height: 12px;
    border: 2px solid rgba(255, 255, 255, 0.3);
    border-top-color: #f0f3f6;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  .icon {
    flex: none;
    font-weight: 700;
  }
  .done .icon {
    color: #3fb950;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
