<script>
  import { LevelHistory, LevelSampler, buildTrace } from "./wave.js";

  // Green oscilloscope/ECG trace driven by the ~20 Hz `level` of the recording view. Levels go
  // into a time-stamped history; every animation frame the trace is rebuilt from it (with
  // interpolation) so it enters on the left and travels to the right like a marquee, smoothly
  // at display rate. The rAF loop only
  // exists while this component is mounted, i.e. while the overlay shows "recording".
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
    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
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
      const trace = buildTrace(history, now(), count, { scroll: !motion.matches });
      const mid = ch / 2;
      const half = ch / 2 - 2;
      // Leave room on the left for the head dot (the newest point).
      const x0 = HEAD_R + 1;
      const pts = trace.map((y, i) => [x0 + (i / (count - 1)) * (cw - x0), mid - y * half]);
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      stroke(pts, 7, "rgba(57, 255, 106, 0.10)"); // soft glow
      stroke(pts, 4, "rgba(57, 255, 106, 0.22)");
      stroke(pts, 1.6, "#39ff6a"); // core line
      const [hx, hy] = pts[0];
      ctx.beginPath();
      ctx.arc(hx, hy, HEAD_R, 0, Math.PI * 2);
      ctx.fillStyle = "#d8ffe2";
      ctx.fill();
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
