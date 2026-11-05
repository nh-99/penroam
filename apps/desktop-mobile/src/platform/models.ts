// Typed facade over the handwriting model downloader (src-tauri/src/models.rs). Models come from
// a signed catalog; the backend does all checking, and this file only shows what it reports.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { CommandError, hasHost } from "./ipc";
import type { ModelSummary } from "./search";

/** The model the catalog recommends for this device. */
export interface OfferedModel {
  id: string;
  version: string;
  name: string;
  license: string;
  languages: string[];
  bytes: number;
}

export interface ModelsStatus {
  /** This build can download models. Without it, models can only be installed from a file. */
  enabled: boolean;
  phase: "idle" | "checking" | "downloading" | "installing";
  /** The work in progress is the first-run download, which nobody asked for. */
  automatic: boolean;
  received: number;
  total: number;
  /** The server was asked at least once since the app started. */
  checked: boolean;
  available: OfferedModel | null;
  error: string | null;
}

async function call<T>(cmd: string): Promise<T> {
  if (!hasHost()) throw new CommandError("noHost", "Models need the Penroam app.");
  return await invoke<T>(cmd);
}

export const modelsApi = {
  status: () => call<ModelsStatus>("models_status"),
  check: () => call<void>("models_check"),
  download: () => call<void>("models_download"),
  cancel: () => call<void>("models_cancel"),
  onStatus: (fn: (s: ModelsStatus) => void): Promise<UnlistenFn> => listen<ModelsStatus>("models://status", (e) => fn(e.payload)),
};

/** The catalog's model, when it is not the one installed. A different id counts: models get replaced. */
export function modelToOffer(available: OfferedModel | null, installed: ModelSummary | null): OfferedModel | null {
  if (!available) return null;
  return installed && installed.id === available.id && installed.version === available.version ? null : available;
}

/** What to tell the user about a status change, if anything. A download they started shows its own progress. */
export function modelNotice(prev: ModelsStatus | null, next: ModelsStatus): string | null {
  if (next.automatic && next.phase === "downloading" && prev?.phase !== "downloading") return "Downloading the handwriting model for search…";
  if (prev?.phase === "installing" && next.phase === "idle" && !next.error) return "Handwriting model installed. Your handwriting is being read in the background.";
  return null;
}
