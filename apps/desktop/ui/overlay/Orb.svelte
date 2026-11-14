<script>
  import { GOLD_STOPS, ProgressEstimator, easeToFull, orbLook } from "./wave.js";

  // Progress orb for transcribing -> polishing -> done: a gold liquid rises in a ball, a soft
  // core light breathes behind it, and at "done" the ball tops up to 100%, turns white-hot and
  // blooms outward (user's pick 2026-11-14: option A with option C's finish). There is no real
  // progress signal, so the fill is an estimate (see ProgressEstimator); it never goes back.
  let { kind } = $props();

  const est = new ProgressEstimator();
  const now = () => performance.now() / 1000;

  let canvas;
  let p = $state(0);
  // Fill reached before "done", so the top-up eases from there instead of jumping.
  let lastBeforeDone = 0;
  let doneAt = null;

  $effect(() => {
    const t = now();
    if (kind === "done") {
      if (doneAt == null) doneAt = t;
    } else {
      doneAt = null;
      est.setPhase(kind, t);
    }
  });

  $effect(() => {
    const ctx = canvas.getContext("2d");
    let raf = 0;
    let stopped = false;
    let size = 0;
    let dpr = 1;

    function fit() {
      const s = canvas.clientWidth;
      const r = window.devicePixelRatio || 1;
      if (s !== size || r !== dpr) {
        size = s;
        dpr = r;
        canvas.width = canvas.height = Math.max(1, Math.round(s * r));
      }
    }

    function gold(top, bottom) {
      const g = ctx.createLinearGradient(0, top, 0, bottom);
      for (const [k, c] of GOLD_STOPS) g.addColorStop(k, c);
      return g;
    }

    function frame() {
      if (stopped) return;
      const t = now();
      const doneSecs = doneAt == null ? null : t - doneAt;
      if (doneSecs == null) {
        lastBeforeDone = est.value(t);
        p = lastBeforeDone;
      } else {
        p = easeToFull(lastBeforeDone, doneSecs);
      }
      const look = orbLook(p, t, doneSecs);

      fit();
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, size, size);
      const cx = size / 2;
      const r = size / 2 - 1.5;

      ctx.save();
      ctx.beginPath();
      ctx.arc(cx, cx, r, 0, Math.PI * 2);
      ctx.clip();
      ctx.fillStyle = "rgba(247, 197, 43, 0.08)";
      ctx.fillRect(0, 0, size, size);

      // Core light behind the liquid (breathes until done).
      const cr = Math.max(0.5, r * look.core);
      const core = ctx.createRadialGradient(cx, cx, 0, cx, cx, cr);
      core.addColorStop(0, "rgba(255, 251, 232, 0.85)");
      core.addColorStop(0.45, "rgba(255, 230, 128, 0.5)");
      core.addColorStop(1, "rgba(224, 122, 31, 0)");
      ctx.fillStyle = core;
      ctx.fillRect(0, 0, size, size);

      // Liquid: two layers, the front one with a small wave sliding right like the trace.
      const level = cx + r - look.fill * 2 * r;
      const a = look.wave * r;
      const liquid = gold(cx - r, cx + r);
      for (const [shift, alpha] of [
        [2.2, 0.4],
        [0, 1],
      ]) {
        ctx.beginPath();
        ctx.moveTo(0, size);
        for (let x = 0; x <= size; x += 1) {
          ctx.lineTo(x, level + a * Math.sin((x / size) * 2 * Math.PI * 1.1 - t * 7 + shift));
        }
        ctx.lineTo(size, size);
        ctx.closePath();
        ctx.globalAlpha = alpha;
        ctx.fillStyle = liquid;
        ctx.fill();
      }
      ctx.globalAlpha = 1;

      // Finish: the whole ball turns white-hot.
      if (look.hot > 0) {
        ctx.fillStyle = `rgba(255, 248, 220, ${(0.7 * look.hot).toFixed(3)})`;
        ctx.fillRect(0, 0, size, size);
      }
      ctx.restore();

      ctx.beginPath();
      ctx.arc(cx, cx, r, 0, Math.PI * 2);
      ctx.strokeStyle = "rgba(247, 197, 43, 0.65)";
      ctx.lineWidth = 1.5;
      ctx.stroke();

      canvas.style.filter =
        look.bloom > 0
          ? `drop-shadow(0 0 ${(12 * look.bloom).toFixed(1)}px rgba(255, 214, 90, ${(0.85 * look.bloom).toFixed(2)}))`
          : "";
      raf = requestAnimationFrame(frame);
    }
    frame();
    return () => {
      stopped = true;
      cancelAnimationFrame(raf);
    };
  });
</script>

<div class="orb" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(p * 100)}>
  <canvas bind:this={canvas} aria-hidden="true"></canvas>
</div>

<style>
  .orb {
    flex: none;
    width: 30px;
    height: 30px;
  }
  canvas {
    display: block;
    width: 30px;
    height: 30px;
  }
</style>
