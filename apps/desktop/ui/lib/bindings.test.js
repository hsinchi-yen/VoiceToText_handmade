// Svelte 5 throws `props_invalid_value` (also in production) when `bind:value` hands
// `undefined` to a component prop that has a fallback, e.g. TextField's `value = $bindable("")`.
// A map lookup like `bind:value={keys[id]}` is undefined until the user types, so the whole
// dialog failed to render (the preset and import previews looked dead). Use a function binding
// with a default instead: `bind:value={() => keys[id] ?? "", (v) => (keys[id] = v)}`.
import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const uiRoot = fileURLToPath(new URL("..", import.meta.url));

function svelteFiles(dir) {
  return readdirSync(dir).flatMap((name) => {
    if (name === "node_modules") return [];
    const p = join(dir, name);
    if (statSync(p).isDirectory()) return svelteFiles(p);
    return name.endsWith(".svelte") ? [p] : [];
  });
}

describe("component bindings", () => {
  it("never bind a map lookup that can be undefined", () => {
    const offenders = svelteFiles(uiRoot).flatMap((file) =>
      readFileSync(file, "utf8")
        .split("\n")
        .map((line, i) => ({ line, n: i + 1 }))
        .filter(({ line }) => /bind:value=\{\s*[\w.]+\[[^\]]+\]\s*\}/.test(line))
        .map(({ n }) => `${relative(uiRoot, file)}:${n}`),
    );
    expect(offenders).toEqual([]);
  });
});
