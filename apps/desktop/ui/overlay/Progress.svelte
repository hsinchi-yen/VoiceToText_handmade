<script>
  import { ProgressEstimator } from "./wave.js";

  // One thin green bar for transcribing -> polishing -> done. There is no real progress signal,
  // so the value is an estimate (see ProgressEstimator) and a moving shimmer keeps it alive.
  let { kind } = $props();

  const est = new ProgressEstimator();
  const now = () => performance.now() / 1000;

  let p = $state(0);

  $effect(() => {
    est.setPhase(kind, now());
    let raf = 0;
    let stopped = false;
    const tick = () => {
      if (stopped) return;
      p = est.value(now());
      raf = requestAnimationFrame(tick);
    };
    tick();
    return () => {
      stopped = true;
      cancelAnimationFrame(raf);
    };
  });
</script>

<div class="track" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={Math.round(p * 100)}>
  <div class="fill" class:done={kind === "done"} style:width={`${p * 100}%`}></div>
</div>

<style>
  .track {
    flex: 1;
    height: 6px;
    border-radius: 3px;
    background: rgba(57, 255, 106, 0.14);
    overflow: hidden;
  }
  .fill {
    position: relative;
    height: 100%;
    border-radius: 3px;
    background: #39ff6a;
    box-shadow: 0 0 8px rgba(57, 255, 106, 0.55);
    overflow: hidden;
  }
  .fill::after {
    content: "";
    position: absolute;
    inset: 0;
    width: 40px;
    background: linear-gradient(90deg, transparent, rgba(255, 255, 255, 0.75), transparent);
    animation: shimmer 1s linear infinite;
  }
  .fill.done::after {
    display: none;
  }
  @keyframes shimmer {
    from {
      transform: translateX(-40px);
    }
    to {
      transform: translateX(260px);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .fill::after {
      animation: none;
      display: none;
    }
  }
</style>
