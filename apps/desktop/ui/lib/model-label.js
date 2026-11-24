// Display text for a configured model id. "auto" (any case, surrounding spaces ignored) means
// v2c asks the server's /models list on first use (v2c-core `auto_model`).
export const isAutoModel = (m) => typeof m === "string" && m.trim().toLowerCase() === "auto";

export const modelLabel = (m) => (isAutoModel(m) ? "自動偵測" : m);
