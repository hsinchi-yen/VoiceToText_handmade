<script>
  import { GOLD_STOPS, LevelHistory, LevelSampler, RECORD_PURPLE, buildTrace, traceOffset } from "./wave.js";

  // Golden oscilloscope/ECG trace driven by the ~20 Hz `level` of the recording view. Levels go
  // into a time-stamped history; every animation frame the trace is rebuilt from it (with
  // interpolation) so it enters on the left and travels to the right like a marquee, smoothly
  // at display rate. The rAF loop only exists while this component is mounted, i.e. while the
  // overlay shows "recording".
  // It always scrolls, also with prefers-reduced-motion: Windows reports that whenever its
  // "Animation effects" are off, and a frozen trace no longer reads as "listening" (user
  // decision 2026-11-14). The motion is a slow, small-area drift, not a flash or a zoom.
  // `view` is a new object per overlay event, so the effect below also runs for repeated equal
  // levels (needed to tell "steady" from "no events").
  let { view } = $props();

  const history = new LevelHistory();
  const sampler = new LevelSampler(history);
  const now = () => performance.now() / 1000;
  /** Radius (CSS px) of the bright dot at the newest (left) end of the trace. */
  const HEAD_R = 2.2;

  let canvas;

  $effect(() => {
    sampler.setLevel(view.level ?? 0, now());
  });

  $effect(() => {
    const ctx = canvas.getContext("2d");
    let raf = 0;
    let stopped = false;
    let cw = 0;
    let ch = 0;
    let dpr = 1;

    function fit() {
      const w = canvas.clientWidth;
      const h = canvas.clientHeight;
      const r = window.devicePixelRatio || 1;
      if (w !== cw || h !== ch || r !== dpr) {
        cw = w;
        ch = h;
        dpr = r;
        canvas.width = Math.max(1, Math.round(w * r));
        canvas.height = Math.max(1, Math.round(h * r));
      }
    }

    function stroke(pts, width, color) {
      ctx.beginPath();
      pts.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
      ctx.lineWidth = width;
      ctx.strokeStyle = color;
      ctx.stroke();
    }

    function frame() {
      if (stopped) return;
      sampler.tick(now());
      fit();
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, cw, ch);
      const count = Math.max(8, Math.floor(cw / 1.5));
      const t = now();
      const trace = buildTrace(history, t, count);
      const off = traceOffset(t, count);
      const mid = ch / 2;
      const half = ch / 2 - 2;
      // Leave room on the left for the head dot (the newest point).
      const x0 = HEAD_R + 1;
      const dx = (cw - x0) / (count - 1);
      const pts = trace.map((y, i) => [x0 + (i + off) * dx, mid - y * half]);
      // Keep the line (and the head dot) attached to the left edge between grid points.
      pts.unshift([x0, pts[0][1]]);
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      // Gold along the trace: light gold where the voice enters, deep amber as it leaves.
      const gold = ctx.createLinearGradient(x0, 0, cw, 0);
      for (const [k, c] of GOLD_STOPS) gold.addColorStop(k, c);
      stroke(pts, 7, "rgba(247, 197, 43, 0.12)"); // soft glow
      stroke(pts, 4, "rgba(247, 197, 43, 0.26)");
      stroke(pts, 1.7, gold); // core line
      // The head dot uses v2c's purple.
      const [hx, hy] = pts[0];
      ctx.beginPath();
      ctx.arc(hx, hy, HEAD_R, 0, Math.PI * 2);
      ctx.fillStyle = RECORD_PURPLE;
      ctx.shadowColor = "rgba(155, 123, 255, 0.85)";
      ctx.shadowBlur = 5;
      ctx.fill();
      ctx.shadowBlur = 0;
      raf = requestAnimationFrame(frame);
    }
    frame();
    return () => {
      stopped = true;
      cancelAnimationFrame(raf);
    };
  });
</script>

<canvas bind:this={canvas} aria-hidden="true"></canvas>

<style>
  canvas {
    flex: 1;
    min-width: 0;
    height: 32px;
    /* The newest signal (and its head dot) enters sharp on the left; the trail fades out as it
       travels off to the right. */
    mask-image: linear-gradient(to right, #000, #000 65%, transparent);
  }
</style>
