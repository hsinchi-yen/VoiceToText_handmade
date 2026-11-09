<script>
  import { invoke } from "@tauri-apps/api/core";
  import Home from "./routes/Home.svelte";
  import Settings from "./routes/Settings.svelte";
  import Vocab from "./routes/Vocab.svelte";
  import History from "./routes/History.svelte";
  import CloudSetup from "./routes/CloudSetup.svelte";
  import Overlay from "./routes/Overlay.svelte";
  import Onboarding from "./routes/Onboarding.svelte";
  import { resumeStep } from "./onboarding/api.js";

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
  // Until a local speech model is installed, cloud options are not shown (plan §6.1).
  let needsModels = $state(false);

  $effect(() => {
    const onHash = () => (route = current());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  });

  // First run: open onboarding when no local model exists (or to resume it after the
  // restart that loads newly installed models).
  $effect(() => {
    if (current() === "overlay") return;
    invoke("onboarding_status")
      .then((s) => {
        needsModels = s.needed;
        if ((s.needed || resumeStep()) && current() === "") location.hash = "#/onboarding";
      })
      .catch(() => {});
  });

  const Page = $derived((routes[route] ?? routes[""]).page);
  const hidden = (path) => path === "overlay" || (needsModels && path === "cloud-setup");
</script>

{#if route !== "overlay" && route !== "onboarding"}
  <nav>
    {#each Object.entries(routes) as [path, r] (path)}
      {#if !hidden(path)}
        <a href={"#/" + path} class:active={route === path}>{r.label}</a>
      {/if}
    {/each}
  </nav>
{/if}

<main>
  <Page />
</main>
