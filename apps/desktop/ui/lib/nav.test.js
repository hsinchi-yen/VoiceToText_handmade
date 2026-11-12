import { describe, expect, it } from "vitest";
import { activeNavPath, libraryHref, libraryTab, parseHash, resolveRoute } from "./nav.js";

describe("parseHash", () => {
  it("handles empty and root hashes", () => {
    expect(parseHash("")).toEqual({ path: "", query: {} });
    expect(parseHash("#/")).toEqual({ path: "", query: {} });
    expect(parseHash(undefined)).toEqual({ path: "", query: {} });
  });
  it("splits path and query", () => {
    expect(parseHash("#/library?tab=history&x=a%20b")).toEqual({
      path: "library",
      query: { tab: "history", x: "a b" },
    });
  });
  it("ignores malformed escapes and trailing slashes", () => {
    expect(parseHash("#/library/?tab=%E0%A4%A")).toEqual({ path: "library", query: {} });
  });
});

describe("resolveRoute", () => {
  it("redirects old URLs", () => {
    expect(resolveRoute("#/cloud-setup")).toMatchObject({ path: "services", redirect: "#/services" });
    expect(resolveRoute("#/vocab")).toMatchObject({
      path: "library",
      query: { tab: "vocab" },
      redirect: "#/library?tab=vocab",
    });
    expect(resolveRoute("#/history")).toMatchObject({
      path: "library",
      query: { tab: "history" },
      redirect: "#/library?tab=history",
    });
  });
  it("keeps current and hidden pages", () => {
    expect(resolveRoute("#/services").redirect).toBeNull();
    expect(resolveRoute("#/onboarding").path).toBe("onboarding");
    expect(resolveRoute("#/overlay").path).toBe("overlay");
    expect(resolveRoute("#/settings").path).toBe("settings");
  });
  it("falls back to home for unknown paths", () => {
    expect(resolveRoute("#/nope").path).toBe("");
    expect(resolveRoute("").path).toBe("");
  });
});

describe("libraryTab", () => {
  it("accepts known tabs and defaults otherwise", () => {
    expect(libraryTab({ tab: "history" })).toBe("history");
    expect(libraryTab({ tab: "vocab" })).toBe("vocab");
    expect(libraryTab({ tab: "zzz" })).toBe("vocab");
    expect(libraryTab({})).toBe("vocab");
    expect(libraryTab(undefined)).toBe("vocab");
  });
});

describe("activeNavPath", () => {
  it("marks menu pages only", () => {
    expect(activeNavPath("library")).toBe("library");
    expect(activeNavPath("")).toBe("");
    expect(activeNavPath("onboarding")).toBeNull();
  });
});

describe("library tab round trip", () => {
  it("libraryHref resolves back to the same tab, and the old URL re-selects it", () => {
    for (const tab of ["vocab", "history"]) {
      const r = resolveRoute(libraryHref(tab));
      expect(r.path).toBe("library");
      expect(libraryTab(r.query)).toBe(tab);
      expect(r.redirect).toBeNull();
    }
    // Tray "輸入紀錄" after the user switched to vocab: old URL redirects to the history tab.
    const r = resolveRoute("#/history");
    expect(libraryTab(resolveRoute(r.redirect).query)).toBe("history");
  });
  it("a bare unknown route keeps an empty raw path only for home", () => {
    expect(parseHash("#/foo").path).toBe("foo");
    expect(parseHash("#/").path).toBe("");
  });
});
