<script>
  import { emit, listen } from "@tauri-apps/api/event";
  import Wave from "../overlay/Wave.svelte";
  import Progress from "../overlay/Progress.svelte";

  // Floating status pill (T1.3). Payloads come from src-tauri/src/overlay.rs (`v2c:overlay`):
  // states, a volume level, and fixed UI strings. Never any recognised text.
  // Look (T4.7): a black pill; recording = scrolling green ECG trace, transcribing/polishing =
  // one estimated progress bar, errors/notices = short text.
  // Once the listener is registered we emit `v2c:overlay-ready`; Rust then re-sends whatever
  // was emitted before we were listening (e.g. a startup error).
  const FADE_MS = 300;
  // The "done" pill only lives ~250 ms: its fade is shorter so the bar is seen at 100% first.
  const DONE_FADE_MS = 150;

  let view = $state({ kind: "hidden" });
  let fading = $state(false);
  let fadeMs = $state(FADE_MS);
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
      fadeMs = view.kind === "done" ? DONE_FADE_MS : FADE_MS;
      const hideAfter = e.payload.hide_after_ms;
      if (hideAfter != null) {
        fadeTimer = setTimeout(() => (fading = true), Math.max(0, hideAfter - fadeMs));
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

  // Recording, transcribing, polishing and done show no text at all (flow over words); only
  // errors and notices carry a message the user needs to read.
  const isProgress = $derived(["transcribing", "polishing", "done"].includes(view.kind));
  const isNotice = $derived(view.kind === "error" || view.kind === "info");
</script>

{#if view.kind !== "hidden"}
  <div class="stage">
    <div
      class={`pill ${view.kind}`}
      class:fading
      class:compact={!isNotice}
      style:--fade={`${fadeMs}ms`}
      role="status"
      aria-live="polite"
    >
      {#if view.kind === "recording"}
        <!-- Hands-free: a solid green dot instead of a text badge. It is the one element that
             differs from push-to-talk, and it stays put while the trace moves. -->
        {#if view.hands_free}
          <span class="hf-dot" title="免持"></span>
        {/if}
        <Wave level={view.level ?? 0} />
      {:else if isProgress}
        <Progress kind={view.kind} />
      {:else}
        <span class={`accent ${view.kind}`}></span>
        <span class="label">{view.message ?? ""}</span>
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
  /* Always black, also in light mode (it floats over arbitrary apps), so fixed local colors. */
  .pill {
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 10px;
    max-width: calc(100vw - 8px);
    height: 40px;
    padding: 0 16px;
    border-radius: 20px;
    background: #0a0a0a;
    border: 1px solid #2a2d2b;
    color: #e8e8e8;
    font-family: var(--md-font);
    font-size: 14px;
    white-space: nowrap;
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.45);
    opacity: 1;
    transition: opacity var(--fade, 300ms) ease-out;
  }
  .pill.compact {
    width: 200px;
  }
  .pill.fading {
    opacity: 0;
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hf-dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #39ff6a;
    box-shadow: 0 0 6px rgba(57, 255, 106, 0.8);
  }
  .accent {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #9aa0a6;
  }
  .accent.error {
    background: #ff8a80;
  }
</style>
