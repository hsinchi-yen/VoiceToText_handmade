import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri expects a fixed dev port (see src-tauri/tauri.conf.json `devUrl`).
export default defineConfig({
  root: "ui",
  plugins: [svelte({ configFile: false })],
  // Unit tests for the pure helpers (run with `npm test`).
  test: { include: ["lib/**/*.test.js"] },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    outDir: "../dist",
    emptyOutDir: true,
  },
});
