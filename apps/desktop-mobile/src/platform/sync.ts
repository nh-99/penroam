// Typed facade over the sync commands (apps/desktop-mobile/src-tauri/src/sync.rs).
// Sync is a developer preview: only the local development test server works.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { CommandError, hasHost } from "./ipc";

export type SyncPhase =
  | "awaitingApproval"
  | "idle"
  | "syncing"
  | "offline"
  | "backoff"
  | "paused"
  | "needsReauth"
  | "revoked"
  | "needsUpgrade"
  | "quotaExceeded"
  | "rebaseRequired"
  | "integrityAlert"
  | "error";

export interface SyncStatus {
  phase: SyncPhase;
  queued: number;
  pendingRemote: number;
  quarantined: number;
  conflicts: number;
  upToDate: boolean;
  lastSuccessMs: number | null;
  lastError: string | null;
  deviceId: string;
  accountId: string;
  serverUrl: string;
  secretStore: string;
  securityReviewPending: boolean;
}

export interface SyncInfo {
  available: boolean;
  configured: boolean;
  status: SyncStatus | null;
}

export interface SyncDevice {
  deviceId: string;
  label: string;
  fingerprint: string;
  active: boolean;
  thisDevice: boolean;
  addedBy: string;
}

export type ConflictKind =
  | "notebookTitle"
  | "notebookCover"
  | "notebookTrash"
  | "pageOrder"
  | "pageTemplate"
  | "pageTrash"
  | "element"
  | "elementTrash"
  | "editedWhileDeleted";

export interface ConflictTarget {
  kind: ConflictKind;
  pageId?: string;
  elementId?: string;
  group?: number;
}

export interface SyncConflict {
  target: ConflictTarget;
  winner: string;
  /** The value currently shown (same shapes as the alternates' values). */
  winnerValue: unknown;
  alternates: { opId: string; value: unknown }[];
}

export interface SetupResult {
  recoveryCode: string | null;
  pairingCode: string | null;
}

export interface SyncChanged {
  notebookIds: string[];
  pages: [string, string][];
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasHost()) throw new CommandError("noHost", "Sync needs the Penroam app.");
  try {
    return await invoke<T>(cmd, args);
  } catch (e: any) {
    if (e && typeof e === "object" && "code" in e) throw new CommandError(e.code, e.message ?? String(e));
    throw new CommandError("io", String(e));
  }
}

export const syncApi = {
  info: () => call<SyncInfo>("sync_info"),
  setupFirst: (serverUrl: string, account: string, label: string) => call<SetupResult>("sync_setup_first", { serverUrl, account, label }),
  setupPair: (serverUrl: string, account: string, label: string) => call<SetupResult>("sync_setup_pair", { serverUrl, account, label }),
  setupRecovery: (serverUrl: string, account: string, label: string, code: string) =>
    call<void>("sync_setup_recovery", { serverUrl, account, label, code }),
  pollPairing: () => call<string | null>("sync_poll_pairing"),
  approveDevice: (code: string, label: string) => call<string>("sync_approve_device", { code, label }),
  removeDevice: (deviceId: string) => call<void>("sync_remove_device", { deviceId }),
  devices: () => call<SyncDevice[]>("sync_devices"),
  syncNow: () => call<void>("sync_now"),
  pause: (paused: boolean) => call<void>("sync_pause", { paused }),
  conflicts: (notebookId: string) => call<SyncConflict[]>("sync_conflicts", { notebookId }),
  resolveConflict: (notebookId: string, target: ConflictTarget, choose: string) =>
    call<void>("sync_resolve_conflict", { notebookId, target, choose }),
  diagnostics: () => call<string>("sync_diagnostics"),
  disconnect: () => call<void>("sync_disconnect"),
};

export async function onSyncStatus(cb: (s: SyncStatus | null) => void): Promise<UnlistenFn> {
  return listen<SyncStatus | null>("sync://status", (e) => cb(e.payload));
}

export async function onSyncChanged(cb: (c: SyncChanged) => void): Promise<UnlistenFn> {
  return listen<SyncChanged>("sync://changed", (e) => cb(e.payload));
}

/** What the status line says, per phase (actionable, never claims more than is true). */
export function describeStatus(s: SyncStatus | null): { tone: "ok" | "busy" | "muted" | "warn" | "danger"; text: string } {
  if (!s) return { tone: "muted", text: "Sync is not set up" };
  switch (s.phase) {
    case "awaitingApproval":
      return { tone: "busy", text: "Waiting for approval from a trusted device" };
    case "paused":
      return { tone: "muted", text: s.queued ? `Paused · ${s.queued} change(s) queued on this device` : "Paused" };
    case "offline":
      return { tone: "muted", text: s.queued ? `Offline · ${s.queued} change(s) queued on this device` : "Offline · will sync when the server is reachable" };
    case "backoff":
      return { tone: "warn", text: `Server busy or unreachable · retrying automatically${s.queued ? ` · ${s.queued} queued` : ""}` };
    case "needsReauth":
      return { tone: "warn", text: "Sign-in expired · changes stay queued on this device" };
    case "revoked":
      return { tone: "danger", text: "This device was removed from the account; local notes are kept" };
    case "needsUpgrade":
      return { tone: "warn", text: "Another device uses a newer Penroam · update to receive those changes (nothing is lost)" };
    case "quotaExceeded":
      return { tone: "warn", text: "Server storage is full · changes stay queued on this device" };
    case "rebaseRequired":
      return { tone: "warn", text: "This device was offline too long for the server's history · local changes are kept and still uploaded" };
    case "integrityAlert":
      return { tone: "danger", text: "Security warning: data that failed verification was set aside and not applied" };
    case "error":
      return { tone: "danger", text: `Sync error${s.lastError ? `: ${s.lastError}` : ""}` };
    case "syncing":
      return { tone: "busy", text: "Syncing…" };
    case "idle":
      if (s.queued > 0) return { tone: "busy", text: `${s.queued} change(s) queued for upload` };
      if (s.upToDate) return { tone: "ok", text: "Up to date" };
      if (s.pendingRemote > 0) return { tone: "warn", text: `${s.pendingRemote} received change(s) waiting to be applied` };
      return { tone: "busy", text: "Uploaded · checking for changes" };
  }
}

/** Header icon choice. Green only when everything is really up to date. */
export function statusIcon(s: SyncStatus | null): "check" | "upload" | "off" | "alert" | "busy" {
  if (!s) return "off";
  if (s.conflicts > 0 || ["revoked", "needsUpgrade", "quotaExceeded", "rebaseRequired", "integrityAlert", "error", "needsReauth"].includes(s.phase)) return "alert";
  if (s.phase === "offline" || s.phase === "paused") return "off";
  if (s.queued > 0) return "upload";
  if (s.upToDate) return "check";
  return "busy";
}

export function formatAgo(ms: number | null, now = Date.now()): string {
  if (ms == null) return "never";
  const d = Math.max(0, Math.round((now - ms) / 1000));
  if (d < 10) return "just now";
  if (d < 60) return `${d} s ago`;
  if (d < 3600) return `${Math.round(d / 60)} min ago`;
  if (d < 86400) return `${Math.round(d / 3600)} h ago`;
  return new Date(ms).toLocaleString();
}

/** Recovery code groups for display (`PRK1-XXXXX-…` → groups). */
export function codeGroups(code: string): string[] {
  return code.split("-").filter(Boolean);
}
