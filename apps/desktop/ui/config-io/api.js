// Calls to src-tauri/src/commands/config_io.rs. Errors are `{ code, line }` (never file content).
import { invoke } from "@tauri-apps/api/core";
import * as presets from "../presets/api.js";
import { errorMessage } from "./logic.js";

export const describe = (err) => errorMessage(err, presets.describe);

/** Native dialogs; resolve to a path or null when closed. */
export const pickOpen = () => invoke("config_pick_open");
export const pickSave = (defaultName) => invoke("config_pick_save", { defaultName });

export const exportTo = (path) => invoke("config_export", { path });
export const preview = (path) => invoke("config_import_preview", { path });
export const apply = (path, fileHash, asrChoice, keys) =>
  invoke("config_import_apply", { path, fileHash, asrChoice: asrChoice || null, keys });
