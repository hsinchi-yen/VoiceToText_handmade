// Pure routing helpers for the hash router in App.svelte (no DOM access, unit-tested).

/** Menu entries, in order. `path` is the hash path without "#/". */
export const NAV_ITEMS = [
  { path: "", label: "首頁", icon: "home" },
  { path: "services", label: "服務", icon: "services" },
  { path: "library", label: "詞庫與紀錄", icon: "library" },
  { path: "settings", label: "設定", icon: "settings" },
];

/** Pages that exist but are not in the menu. */
export const HIDDEN_PATHS = ["onboarding", "overlay"];

export const LIBRARY_TABS = ["vocab", "history"];
export const DEFAULT_LIBRARY_TAB = "vocab";

/** Old URLs and where they go now. Values are full hashes. */
export const REDIRECTS = {
  "cloud-setup": "#/services",
  vocab: "#/library?tab=vocab",
  history: "#/library?tab=history",
};

export const hrefOf = (path) => "#/" + path;

/** Splits "#/library?tab=vocab" into `{ path: "library", query: { tab: "vocab" } }`. */
export function parseHash(hash) {
  const raw = String(hash ?? "").replace(/^#\/?/, "");
  const [path, search = ""] = raw.split("?", 2);
  const query = {};
  for (const part of search.split("&")) {
    if (!part) continue;
    const [k, v = ""] = part.split("=", 2);
    try {
      query[decodeURIComponent(k)] = decodeURIComponent(v);
    } catch {
      // Malformed escape: ignore this pair.
    }
  }
  return { path: path.replace(/\/+$/, ""), query };
}

const KNOWN = new Set([...NAV_ITEMS.map((i) => i.path), ...HIDDEN_PATHS]);

/**
 * Resolves a hash to what to show. `redirect` (a full hash) is set when the URL is an old one
 * and the caller should replace it; `path` is then already the target's path.
 * Unknown paths show the home page.
 */
export function resolveRoute(hash) {
  const { path, query } = parseHash(hash);
  const redirect = REDIRECTS[path] ?? null;
  if (redirect) {
    const target = parseHash(redirect);
    return { path: target.path, query: target.query, redirect };
  }
  return { path: KNOWN.has(path) ? path : "", query, redirect: null };
}

/** The library tab named by a query, falling back to the default for missing/unknown values. */
export function libraryTab(query) {
  return LIBRARY_TABS.includes(query?.tab) ? query.tab : DEFAULT_LIBRARY_TAB;
}

export const libraryHref = (tab) => `#/library?tab=${tab}`;

/** Which menu entry is active for a resolved path (hidden pages: none). */
export function activeNavPath(path) {
  return NAV_ITEMS.some((i) => i.path === path) ? path : null;
}
