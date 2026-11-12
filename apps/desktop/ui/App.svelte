<script>
  import { invoke } from "@tauri-apps/api/core";
  import Home from "./routes/Home.svelte";
  import Settings from "./routes/Settings.svelte";
  import Library from "./routes/Library.svelte";
  import CloudSetup from "./routes/CloudSetup.svelte";
  import Overlay from "./routes/Overlay.svelte";
  import Onboarding from "./routes/Onboarding.svelte";
  import NavRail from "./lib/NavRail.svelte";
  import { NAV_ITEMS, activeNavPath, libraryTab, resolveRoute } from "./lib/nav.js";
  import { resumeStep, skipped as onboardingSkipped } from "./onboarding/api.js";

  // Hash routes (see lib/nav.js): #/ #/services #/library?tab=vocab|history #/settings, plus
  // #/onboarding (not in the menu) and #/overlay (the floating window, no menu).
  // Old URLs (#/cloud-setup, #/vocab, #/history) are replaced by their new place.
  const pages = {
    "": Home,
    services: CloudSetup,
    library: Library,
    settings: Settings,
    onboarding: Onboarding,
    overlay: Overlay,
  };

  let hash = $state(location.hash);
  const route = $derived(resolveRoute(hash));
  $effect(() => {
    const onHash = () => (hash = location.hash);
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  });
  $effect(() => {
    if (route.redirect) location.replace(route.redirect);
  });

  // First run: open onboarding when no local model exists (or to resume it after the
  // restart that loads newly installed models). Not after the user skipped it.
  $effect(() => {
    if (resolveRoute(location.hash).path === "overlay") return;
    invoke("onboarding_status")
      .then((s) => {
        const open = (s.needed && !onboardingSkipped()) || resumeStep();
        if (open && resolveRoute(location.hash).path === "") location.hash = "#/onboarding";
      })
      .catch(() => {});
  });

  const Page = $derived(pages[route.path] ?? Home);
  const overlay = $derived(route.path === "overlay");
</script>

{#if overlay}
  <Page />
{:else}
  <NavRail items={NAV_ITEMS} current={activeNavPath(route.path)} />
  <main>
    <div class="page">
      {#if route.path === "library"}
        <Library tab={libraryTab(route.query)} />
      {:else}
        <Page />
      {/if}
    </div>
  </main>
{/if}

<style>
  main {
    margin-left: var(--md-nav-rail-width);
    padding: var(--md-space-6);
  }
  .page {
    max-width: 960px;
    margin: 0 auto;
    min-width: 0;
  }
  @media (max-width: 599px) {
    main {
      margin-left: 0;
      padding: var(--md-space-4);
      padding-bottom: calc(var(--md-nav-bar-height) + var(--md-space-4));
    }
  }
</style>
