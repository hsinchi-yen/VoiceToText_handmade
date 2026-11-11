<script>
  import { invoke } from "@tauri-apps/api/core";
  import Home from "./routes/Home.svelte";
  import Settings from "./routes/Settings.svelte";
  import Vocab from "./routes/Vocab.svelte";
  import History from "./routes/History.svelte";
  import CloudSetup from "./routes/CloudSetup.svelte";
  import Overlay from "./routes/Overlay.svelte";
  import Onboarding from "./routes/Onboarding.svelte";
  import { resumeStep, skipped as onboardingSkipped } from "./onboarding/api.js";

  // Hash routes: #/settings, #/vocab, ... The overlay window (T1.3) loads #/overlay.
  const routes = {
    "": { page: Home, label: "狀態" },
    settings: { page: Settings, label: "設定" },
    vocab: { page: Vocab, label: "詞庫" },
    history: { page: History, label: "紀錄" },
    "cloud-setup": { page: CloudSetup, label: "雲端服務" },
    onboarding: { page: Onboarding, label: "初次設定" },
    overlay: { page: Overlay, label: "浮動標籤" },
  };

  const current = () => location.hash.replace(/^#\/?/, "");
  let route = $state(current());
  $effect(() => {
    const onHash = () => (route = current());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  });

  // First run: open onboarding when no local model exists (or to resume it after the
  // restart that loads newly installed models). Not after the user skipped it.
  $effect(() => {
    if (current() === "overlay") return;
    invoke("onboarding_status")
      .then((s) => {
        const open = (s.needed && !onboardingSkipped()) || resumeStep();
        if (open && current() === "") location.hash = "#/onboarding";
      })
      .catch(() => {});
  });

  const Page = $derived((routes[route] ?? routes[""]).page);
  // Every page except the overlay is always in the menu, onboarding included (2026-11-11
  // decision: the cloud page is reachable from the first run on).
  const menu = Object.entries(routes).filter(([path]) => path !== "overlay");
</script>

{#if route !== "overlay"}
  <nav>
    {#each menu as [path, r] (path)}
      <a href={"#/" + path} class:active={route === path}>{r.label}</a>
    {/each}
  </nav>
{/if}

<main>
  <Page />
</main>
