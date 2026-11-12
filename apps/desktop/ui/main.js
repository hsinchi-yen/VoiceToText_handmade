import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

async function boot() {
  // In a plain browser (`npm run dev` without Tauri) install fake backend commands before
  // the app mounts. `import.meta.env.DEV` is false in production builds, so the mock is
  // dropped from the bundle.
  if (import.meta.env.DEV && !window.__TAURI_INTERNALS__) {
    const { installMock } = await import("./dev-mock.js");
    installMock();
  }
  return mount(App, { target: document.getElementById("app") });
}

export default boot();
