// Typed facade over the search commands (apps/desktop-mobile/src-tauri/src/search.rs).
// Search runs entirely on this device against a derived index in the app's cache directory;
// queries and recognized text never leave the device.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { CommandError, hasHost } from "./ipc";

export type SearchKind = "title" | "text" | "ink" | "pdf";

/** Rectangle in page points (the page model's coordinates). */
export interface PageRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface SnippetPart {
  text: string;
  hit: boolean;
}

export interface SearchHit {
  notebookId: string;
  notebookTitle: string;
  pageId: string | null;
  pageIndex: number | null;
  kind: SearchKind;
  snippet: SnippetPart[];
  bounds: PageRect | null;
  matchBounds: PageRect[];
  regionKey: string | null;
  confidence: number | null;
  fuzzy: boolean;
  score: number;
}

export interface SearchSettings {
  enabled: boolean;
  handwriting: boolean;
  language: string | null;
  paused: boolean;
  pauseOnBattery: boolean;
  recognizer: RecognizerChoice;
}

/** Which recognizer to prefer: both, the device's own, or the Penroam model. */
export type RecognizerChoice = "auto" | "platform" | "model";

export interface RecognizerInfo {
  id: string;
  modelVersion: string;
  name: string;
  languages: string[];
  license: string;
}

/** A Penroam handwriting model (installed, or inspected before installing). */
export interface ModelSummary {
  id: string;
  version: string;
  name: string;
  license: string;
  languages: string[];
  bytes: number;
  provenance: unknown;
}

export type SearchState = "off" | "starting" | "indexing" | "idle" | "paused" | "waiting-for-power" | "error";

export interface SearchStatus {
  settings: SearchSettings;
  state: SearchState;
  queued: number;
  recognizer: RecognizerInfo | null;
  /** The platform's own recognizer, if this device has one. */
  platform: RecognizerInfo | null;
  /** The installed Penroam handwriting model, if any. */
  model: ModelSummary | null;
  modelError: string | null;
  language: string | null;
  stats: { pages: number; incompletePages: number; textEntries: number; inkEntries: number; pdfEntries: number; corrections: number };
  indexBytes: number;
  onBattery: boolean | null;
  lastError: string | null;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasHost()) throw new CommandError("noHost", "Search needs the Penroam app.");
  try {
    return await invoke<T>(cmd, args);
  } catch (e: any) {
    if (e && typeof e === "object" && "code" in e) throw new CommandError(e.code, e.message ?? String(e));
    throw new CommandError("io", String(e));
  }
}

export const searchApi = {
  query: (query: string) => call<SearchHit[]>("search_query", { query }),
  status: () => call<SearchStatus>("search_status"),
  setSettings: (settings: SearchSettings) => call<void>("search_set_settings", { settings }),
  rebuild: () => call<void>("search_rebuild"),
  correct: (notebookId: string, pageId: string, regionKey: string, text: string | null) =>
    call<void>("search_correct", { notebookId, pageId, regionKey, text }),
  inspectModel: (path: string) => call<ModelSummary>("search_inspect_model", { path }),
  installModel: (path: string) => call<ModelSummary>("search_install_model", { path }),
  removeModel: (id: string) => call<void>("search_remove_model", { id }),
  onStatus: (fn: (s: SearchStatus) => void): Promise<UnlistenFn> => listen<SearchStatus>("search://status", (e) => fn(e.payload)),
};

/** What reads handwriting right now. Mirrors `RecognizerChoice::resolve` in the backend. */
export function recognizerSummary(s: Pick<SearchStatus, "platform" | "model" | "settings">): string | null {
  const choice = s.settings.recognizer;
  const platform = s.platform && (choice !== "model" || !s.model) ? s.platform : null;
  const model = s.model && (choice !== "platform" || !s.platform) ? s.model : null;
  if (platform && model) return `Uses ${platform.name} on this device, with the Penroam handwriting model as a fallback.`;
  if (platform) return `Uses ${platform.name} on this device.`;
  if (model) return "Uses the Penroam handwriting model on this device.";
  return null;
}

/** Rectangles to outline for a hit: the matching handwritten words, else the whole source. */
export function highlightRects(h: SearchHit): PageRect[] {
  if (h.matchBounds.length) return h.matchBounds;
  if (h.bounds && h.kind !== "pdf" && h.kind !== "title") return [h.bounds];
  return [];
}

/** One-line state for the settings screen. */
export function describeState(s: SearchStatus): string {
  switch (s.state) {
    case "off":
      return "Search is off. No index is kept.";
    case "starting":
      return "Starting…";
    case "indexing":
      return `Indexing: ${s.queued} page${s.queued === 1 ? "" : "s"} to check`;
    case "paused":
      return s.queued ? `Paused, ${s.queued} page${s.queued === 1 ? "" : "s"} waiting` : "Paused";
    case "waiting-for-power":
      return `Waiting for a charger: ${s.queued} page${s.queued === 1 ? "" : "s"} to check`;
    case "error":
      return `The search index could not be opened${s.lastError ? `: ${s.lastError}` : ""}`;
    default:
      return "Up to date";
  }
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
