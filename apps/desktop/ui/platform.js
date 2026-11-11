// Which desktop OS the shared UI runs on. The same Svelte UI ships in the Windows (WebView2)
// and Linux (WebKitGTK) builds; only texts that name OS-specific things (key store, history
// encryption, file paths) differ. Windows keeps its original texts: `isLinux` is false there.
export const isLinux =
  typeof navigator !== "undefined" && /Linux/i.test(navigator.userAgent) && !/Android/i.test(navigator.userAgent);

/** `windows` on Windows (and anything else), `linux` on Linux. */
export const pick = (windows, linux) => (isLinux ? linux : windows);
