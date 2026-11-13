// Fake Tauri backend for `npm run dev` in a plain browser. Loaded by main.js only when
// import.meta.env.DEV is true and window.__TAURI_INTERNALS__ is missing, so it is not part of
// the production bundle. All data is made up; nothing here is real user text.
//
// To mock a new command, add one entry to `commands` below (name -> (args) => result).
// Throw a short error code string to simulate a failure, like the real commands do.
// From the browser console: `__v2cMock.emit("v2c:status", { kind: "recording", rms: 0.05 })`.
import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";

const clone = (v) => JSON.parse(JSON.stringify(v));
const delay = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- State -------------------------------------------------------------------------------

const services = [
  {
    id: "tn-qwen",
    name: "TN Qwen3.8 27B",
    display_name: "TN Qwen3.8 27B",
    kind: "open_ai_compat",
    label: "自訂端點",
    base_url: "http://10.20.30.23:8000/v1",
    effective_base_url: "http://10.20.30.23:8000/v1",
    asr_model: null,
    llm_model: "Qwen3.8-27B-UD-Q6_K_L",
    asr_timeout_ms: null,
    purposes: { asr: false, polish: true, translate: true },
    supports_asr: false,
    has_key: false,
    key_last4: "",
  },
  {
    id: "tn-breeze-asr",
    name: "TN Breeze-ASR",
    display_name: "TN Breeze-ASR",
    kind: "open_ai_compat",
    label: "自訂端點",
    base_url: "http://10.20.30.23:8002/v1",
    effective_base_url: "http://10.20.30.23:8002/v1",
    asr_model: "breeze-asr-25",
    llm_model: null,
    asr_timeout_ms: 8000,
    purposes: { asr: true, polish: false, translate: false },
    supports_asr: true,
    has_key: true,
    key_last4: "a1b2",
  },
];

const config = {
  hotkey: { dictate: "RightCtrl", translate: "RightCtrl+Shift", hold_threshold_ms: 300, double_tap_window_ms: 400, hands_free_max_secs: 300 },
  polish: { enabled: true },
  route: { asr: "tn-breeze-asr", polish: "tn-qwen", translate: "tn-qwen", cloud_fallback_to_local: true },
  audio: { device: null, mic_mode: "always_on" },
  local: { asr: "auto", breeze_quant: "q5_0" },
  apps: [
    { exe: "WINWORD.EXE", newline: "enter", always_paste: false },
    { exe: "LINE.exe", newline: "shift_enter", always_paste: false },
  ],
  history: { enabled: true, retention_days: 7, exclude_apps: ["KeePass.exe"] },
  gpu: { profile: "balanced", vram_reserve_mb: 1024, keep_loaded: "auto", idle_unload_minutes: 10, yield_on_fullscreen: true, heavy_apps: ["Premiere Pro.exe"] },
};

let autostart = false;
let vocab = "v2c\nBreeze-ASR\n台灣華語\n";
let nextHistoryId = 4;
let history = [
  { id: 3, created_at: Math.floor(Date.now() / 1000) - 120, app: "WINWORD.EXE", chars: 28, mode: "dictate", text: "這是一段範例文字，用來預覽輸入紀錄的版面。" },
  { id: 2, created_at: Math.floor(Date.now() / 1000) - 3600, app: "LINE.exe", chars: 19, mode: "translate", text: "Let's meet at three tomorrow." },
  { id: 1, created_at: Math.floor(Date.now() / 1000) - 86400, app: "", chars: 12, mode: "dictate", text: "範例：明天下午三點開會。" },
];

const models = {
  models_dir: "C:\\Users\\demo\\AppData\\Roaming\\v2c\\models",
  entries: [
    { id: "breeze-asr-25-q5_0", kind: "breeze", installed: true, downloadable: true, partial_bytes: 0, size: 1100000000 },
    { id: "sensevoice-small", kind: "sensevoice_archive", installed: false, downloadable: true, partial_bytes: 0, size: 240000000 },
    { id: "tea-asr", kind: "tea_asr", installed: false, downloadable: true, partial_bytes: 600000000, size: 2340000000 },
  ],
};

const kinds = [
  { kind: "open_ai", label: "OpenAI", capabilities: "語音辨識、潤飾、中翻英", supports_asr: true, key_required: true, default_base_url: "https://api.openai.com/v1", suggested_asr_model: "whisper-1", suggested_llm_model: "gpt-4o-mini", key_page_url: "https://platform.openai.com/api-keys", default_asr_timeout_ms: 8000 },
  { kind: "gemini", label: "Gemini", capabilities: "語音辨識、潤飾、中翻英", supports_asr: true, key_required: true, default_base_url: null, suggested_asr_model: "gemini-2.5-flash", suggested_llm_model: "gemini-2.5-flash", key_page_url: "https://aistudio.google.com/apikey", default_asr_timeout_ms: 8000 },
  { kind: "claude", label: "Claude", capabilities: "潤飾、中翻英（不支援語音辨識）", supports_asr: false, key_required: true, default_base_url: null, suggested_asr_model: null, suggested_llm_model: "claude-haiku-4-5", key_page_url: "https://console.anthropic.com/settings/keys", default_asr_timeout_ms: 8000 },
  { kind: "open_ai_compat", label: "自訂端點", capabilities: "LM Studio、Ollama 或公司內部服務", supports_asr: true, key_required: false, default_base_url: "http://localhost:1234/v1", suggested_asr_model: null, suggested_llm_model: null, key_page_url: null, default_asr_timeout_ms: 8000 },
];

// Config field name (as used by settings_apply) -> path inside `config`.
const FIELD_PATHS = {
  hold_threshold_ms: ["hotkey", "hold_threshold_ms"],
  double_tap_window_ms: ["hotkey", "double_tap_window_ms"],
  hands_free_max_secs: ["hotkey", "hands_free_max_secs"],
  polish_enabled: ["polish", "enabled"],
  route_asr: ["route", "asr"],
  route_polish: ["route", "polish"],
  route_translate: ["route", "translate"],
  cloud_fallback_to_local: ["route", "cloud_fallback_to_local"],
  mic_device: ["audio", "device"],
  mic_mode: ["audio", "mic_mode"],
  local_asr: ["local", "asr"],
  breeze_quant: ["local", "breeze_quant"],
  apps: ["apps"],
  history_enabled: ["history", "enabled"],
  history_retention_days: ["history", "retention_days"],
  history_exclude_apps: ["history", "exclude_apps"],
  gpu_profile: ["gpu", "profile"],
  gpu_reserve_mb: ["gpu", "vram_reserve_mb"],
  gpu_keep_loaded: ["gpu", "keep_loaded"],
  gpu_idle_unload_minutes: ["gpu", "idle_unload_minutes"],
  gpu_yield_on_fullscreen: ["gpu", "yield_on_fullscreen"],
  gpu_heavy_apps: ["gpu", "heavy_apps"],
};

function applyTo(cfg, changes) {
  const next = clone(cfg);
  for (const { field, value } of changes) {
    const path = FIELD_PATHS[field];
    if (!path) throw "out_of_range";
    let o = next;
    for (const k of path.slice(0, -1)) o = o[k];
    o[path.at(-1)] = value;
  }
  return next;
}

function settingsView(cfg) {
  const asr_choice = cfg.route.asr === "local" ? `local:${cfg.local.asr}` : cfg.route.asr;
  return {
    config: { ...clone(cfg), cloud: clone(services) },
    asr_choice,
    // Real settings_get returns ServiceRef rows only; names live in config.cloud.
    cloud: services.map(({ id, kind, supports_asr }) => ({ id, kind, supports_asr })),
    breeze_q5_0: true,
    breeze_q8_0: false,
    sensevoice: true,
    tea_asr: false,
    tea_build: true,
    cuda_build: true,
    autostart,
    file_error: null,
  };
}

// ---- Commands ----------------------------------------------------------------------------

/** One entry per backend command used by the UI. Args are the invoke() payload object. */
export const commands = {
  // status / app
  app_info: () => ({ version: "0.2.0-dev", cuda: true, data_dir: "C:\\Users\\demo\\AppData\\Roaming\\v2c" }),
  last_status: () => ({ kind: "done", chars: 28, latency_ms: 640 }),
  open_doc: () => null,

  // onboarding / models (onboarding_status.needed=false: dev starts on the home page)
  onboarding_status: () => ({ needed: false, nvidia: true, gpu_name: "NVIDIA GeForce RTX 4060 (mock)", total_vram_mb: 8188, cuda_build: true, recommended: "breeze_q5_0" }),
  models_status: () => clone(models),
  models_download: async ({ id }) => {
    for (let i = 1; i <= 10; i++) {
      await delay(250);
      await emit("v2c:model-progress", { id, done: i * 100000000, total: 1000000000 });
    }
    const m = models.entries.find((e) => e.id === id);
    if (m) m.installed = true;
    return null;
  },
  models_cancel: () => null,
  models_import_breeze: () => null,
  onboarding_restart: () => null,

  // settings
  settings_get: () => settingsView(config),
  settings_preview: ({ changes }) => ({ settings: settingsView(applyTo(config, changes)), unchanged: [] }),
  settings_apply: async ({ changes }) => {
    Object.assign(config, applyTo(config, changes));
    await emit("v2c:config-changed", null);
    return { settings: settingsView(config), restart_required: changes.some((c) => c.field.startsWith("mic_") || c.field.startsWith("history_")) };
  },
  settings_set_autostart: ({ enabled }) => (autostart = !!enabled),
  settings_input_devices: () => ["Microphone (USB Audio) [mock]", "Headset Mic (Realtek) [mock]"],
  settings_runtime_status: () => ({
    asr: { text: "雲端：TN Breeze-ASR（自訂端點）" },
    polish_enabled: config.polish.enabled,
    polish: { text: "雲端：TN Qwen3.8 27B（自訂端點）" },
    translate: { text: "雲端：TN Qwen3.8 27B（自訂端點）" },
    vram: { free_mb: 5200, total_mb: 8188, reserve_mb: 1024, gpu_engine: "Breeze-ASR-25", breeze_need_mb: 2600 },
  }),
  settings_test_local_asr: async () => {
    await delay(600);
    return { ok: true, summary: "本機語音辨識測試通過（模擬）", steps: [{ label: "載入模型", status: "passed", detail: "Breeze-ASR-25", latency_ms: 420 }, { label: "辨識測試音檔", status: "passed", detail: null, latency_ms: 310 }] };
  },
  gpu_usage: () => ({ available: true, name: "NVIDIA GeForce RTX 4060 (mock)", total_mb: 8188, free_mb: 5200, used_mb: 2988, v2c_mb: 2100, breeze_need_mb: 2600, reserve_mb: 1024, profile: config.gpu.profile, cuda_build: true }),
  mic_test_start: async () => {
    let n = 0;
    const timer = setInterval(() => {
      n += 1;
      if (n > 30) {
        clearInterval(timer);
        emit("v2c:mic-level", { done: true });
      } else {
        emit("v2c:mic-level", { done: false, rms: 0.02 + Math.abs(Math.sin(n / 3)) * 0.08 });
      }
    }, 100);
    return null;
  },
  mic_test_stop: () => {
    emit("v2c:mic-level", { done: true });
    return null;
  },

  // cloud services
  cloud_kinds: () => clone(kinds),
  cloud_list: () => clone(services),
  cloud_models: async () => {
    await delay(300);
    return { models: ["Qwen3.8-27B-UD-Q6_K_L", "breeze-asr-25"], reason: null };
  },
  cloud_test_connection: async () => {
    await delay(400);
    return { ok: true, latency_ms: 38, summary: "連線成功（模擬）", checks: [], supports: { asr: true, polish: true, translate: true }, models: null };
  },
  cloud_save: async ({ draft, purposes }) => {
    const row = {
      id: draft.id ?? `custom-${services.length + 1}`,
      name: draft.name ?? null,
      display_name: draft.name ?? draft.id ?? "custom",
      kind: draft.kind,
      label: kinds.find((k) => k.kind === draft.kind)?.label ?? draft.kind,
      base_url: draft.base_url ?? null,
      effective_base_url: draft.base_url ?? "https://example.invalid/v1",
      asr_model: draft.asr_model ?? null,
      llm_model: draft.llm_model ?? null,
      asr_timeout_ms: draft.asr_timeout_ms ? Number(draft.asr_timeout_ms) : null,
      purposes: purposes ?? { asr: false, polish: true, translate: true },
      supports_asr: !!draft.asr_model,
      has_key: !!draft.api_key,
      key_last4: "",
    };
    const i = services.findIndex((s) => s.id === row.id);
    if (i >= 0) services[i] = row;
    else services.push(row);
    await emit("v2c:config-changed", null);
    return clone(row);
  },
  cloud_remove: async ({ id }) => {
    const i = services.findIndex((s) => s.id === id);
    if (i >= 0) services.splice(i, 1);
    await emit("v2c:config-changed", null);
    return { removed: true };
  },
  cloud_open_key_page: () => null,

  // presets (T4.1)
  presets_list: () => [
    { name: "TN", title: "TN 區網服務", id_prefix: "tn-", services: [{ id: "tn-tea-asr" }, { id: "tn-breeze-asr" }, { id: "tn-qwen" }] },
    { name: "RD", title: "RD 區網服務", id_prefix: "rd-", services: [{ id: "rd-tea-asr" }, { id: "rd-breeze-asr" }, { id: "rd-qwen" }] },
  ],
  presets_preview: async ({ source }) => {
    await delay(200);
    const p = source.builtin === "RD" ? "rd" : "tn";
    const host = p === "rd" ? "10.88.95.2" : "10.20.30.23";
    const up = p.toUpperCase();
    return {
      title: `${up} 區網服務`,
      id_prefix: `${p}-`,
      services: [
        { id: `${p}-tea-asr`, name: `${up} TEA-ASR`, base_url: `http://${host}:8001/v1`, asr_model: "tea-asr-1", llm_model: null, model_source: null, asr_timeout_ms: null, purposes: ["asr"], needs_key: true, has_key: false, has_default_key: true },
        { id: `${p}-breeze-asr`, name: `${up} Breeze-ASR`, base_url: `http://${host}:8002/v1`, asr_model: "breeze-asr-25", llm_model: null, model_source: null, asr_timeout_ms: 8000, purposes: ["asr"], needs_key: true, has_key: false, has_default_key: true },
        { id: `${p}-qwen`, name: `${up} Qwen3.8 27B`, base_url: `http://${host}:8000/v1`, asr_model: null, llm_model: "Qwen3.8-27B-UD-Q6_K_L", model_source: "detected", asr_timeout_ms: null, purposes: ["polish", "translate"], needs_key: false, has_key: false, has_default_key: false },
      ],
      route: { polish: `${p}-qwen`, translate: `${p}-qwen`, cloud_fallback_to_local: true },
      asr_choices: [{ id: `${p}-tea-asr`, name: `${up} TEA-ASR` }, { id: `${p}-breeze-asr`, name: `${up} Breeze-ASR` }],
      needs_key: [`${p}-tea-asr`, `${p}-breeze-asr`],
      ignored: [],
    };
  },
  presets_apply: async ({ source, asrChoice }) => {
    await delay(300);
    const pv = await commands.presets_preview({ source });
    config.route.asr = asrChoice;
    config.route.polish = pv.route.polish;
    config.route.translate = pv.route.translate;
    await emit("v2c:config-changed", null);
    return { title: pv.title, asr_choice: asrChoice, services: pv.services, key_missing: [] };
  },

  // configuration file export / import (T5.2)
  config_pick_open: () => null,
  config_pick_save: () => null,
  config_export: () => ({ services: services.length, vocab: 0, skipped: [], route_reset: [] }),
  config_import_preview: () => {
    throw { code: "file_unreadable", line: null };
  },
  config_import_apply: () => {
    throw { code: "file_unreadable", line: null };
  },

  // vocabulary
  vocab_get: () => ({ text: vocab, max_entries: 500, max_entry_chars: 40 }),
  vocab_set: ({ text }) => {
    vocab = text;
    return { count: text.split("\n").filter((l) => l.trim() && !l.trim().startsWith("#")).length };
  },

  // history
  history_list: () => clone(history),
  history_reoutput: () => null,
  history_copy: () => null,
  history_delete: ({ id }) => {
    history = history.filter((h) => h.id !== id);
    return null;
  },
  history_clear: () => {
    history = [];
    return null;
  },
};

// A fresh entry on demand, for trying the history page: __v2cMock.addHistory().
function addHistory() {
  history.unshift({ id: nextHistoryId++, created_at: Math.floor(Date.now() / 1000), app: "notepad.exe", chars: 10, mode: "dictate", text: "新增的範例紀錄。" });
  return emit("v2c:status", { kind: "done", chars: 10, latency_ms: 500 });
}

export function installMock() {
  mockWindows("main");
  mockIPC(
    (cmd, args) => {
      const fn = commands[cmd];
      if (!fn) {
        console.warn(`[dev-mock] no mock for command "${cmd}"`);
        throw "mock_missing";
      }
      return fn(args ?? {});
    },
    { shouldMockEvents: true },
  );
  window.__v2cMock = { emit, commands, addHistory };
  console.info("[dev-mock] fake Tauri backend installed (browser dev mode)");
}
