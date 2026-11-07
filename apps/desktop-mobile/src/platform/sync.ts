// Typed facade over the sync commands (apps/desktop-mobile/src-tauri/src/sync.rs).
// Servers either sign users in through the system
// browser (auth.v1) or are the local development test server (typed account name).
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
  /** The server new setups use (Penroam Cloud) unless developer mode picks another. */
  defaultServerUrl: string;
  /** Debug build: developer mode (custom servers) can be turned on. */
  devBuild: boolean;
}

/** Taps on the version number that turn developer mode on or off. */
export const DEV_MODE_TAPS = 5;
/** All of them within this many milliseconds. */
export const DEV_MODE_WINDOW_MS = 3000;

/**
 * Counts taps; returns `true` on the tap that completes `taps` taps within
 * `windowMs`, then starts counting again.
 */
export function tapCounter(taps = DEV_MODE_TAPS, windowMs = DEV_MODE_WINDOW_MS): (now: number) => boolean {
  let times: number[] = [];
  return (now) => {
    times = [...times.filter((t) => now - t < windowMs), now];
    if (times.length < taps) return false;
    times = [];
    return true;
  };
}

export interface SyncDevice {
  deviceId: string;
  label: string;
  fingerprint: string;
  active: boolean;
  thisDevice: boolean;
  addedBy: string;
}

export interface ErasedNotebook {
  notebookId: string;
  title: string;
  inTrash: boolean;
  cover: string;
  pageCount: number;
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

/** What a server needs before setup (see `sync_server_info`). */
export interface ServerInfo {
  /** Sign-in through the browser; false = development test server (typed account name). */
  signIn: boolean;
  /** Account this device is signed in to on that server. */
  signedInAccount: string | null;
}

export interface SetupResult {
  recoveryCode: string | null;
  pairingCode: string | null;
}

/** A paid plan offered by the server (desktop builds only; mobile builds never get offers). */
export interface PlanOffer {
  id: string;
  name: string;
  priceLabel: string;
  quotaBytes: number;
}

/** A plan sold through the app store. The price is the store's own. */
export interface StoreOffer {
  id: string;
  name: string;
  quotaBytes: number;
  price: string;
  /** month, year or similar. */
  period: string | null;
}

/** The end of a store purchase. */
export interface StorePurchaseResult {
  status: "active" | "pending" | "cancelled" | "conflict" | "waiting";
  message: string | null;
}

/** How a batch of store transactions ended. */
export interface StoreSettled {
  applied: number;
  conflicts: number;
  waiting: number;
  message: string | null;
}

/** Plan and storage of the synced account (`sync_account`, servers with `account.v1`). */
export interface AccountView {
  planName: string;
  /** free, trialing, active, canceling, past_due, granted, or a newer value. */
  planStatus: string;
  renewsAtMs: number | null;
  endsAtMs: number | null;
  usedBytes: number;
  quotaBytes: number | null;
  freeQuotaBytes: number | null;
  writable: boolean;
  /** over_quota, suspended, deletion_pending, or a newer value. */
  readOnlyReason: string | null;
  offers: PlanOffer[];
  canManageBilling: boolean;
  canRequestDeletion: boolean;
  canCancelDeletion: boolean;
  deletionEraseAfterMs: number | null;
  /** This build may show Stripe prices and open billing pages (desktop). */
  purchasesAllowed: boolean;
  /** This build sells through its app store. */
  storePurchases: boolean;
  /** Plans this account can buy in the app store. */
  storeOffers: StoreOffer[];
  /** Where a paid plan is managed: web, app_store, google_play or a newer value. */
  managedIn: string | null;
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

/** The plan is one the app store bills, so only the store can change or cancel it. */
export function storeManaged(a: AccountView): boolean {
  return a.managedIn === "app_store" || a.managedIn === "google_play";
}

/** What to say after a store purchase, or null when the result speaks for itself. */
export function describeStorePurchase(r: StorePurchaseResult): string | null {
  switch (r.status) {
    case "pending":
      return "Waiting for approval. Your plan starts once the purchase is approved.";
    case "waiting":
      return "Your purchase went through. It will reach your account shortly, and you won't be charged again.";
    case "conflict":
      return r.message;
    default:
      return null;
  }
}

/** What to say after restoring purchases. */
export function describeRestore(r: StoreSettled): string {
  if (r.conflicts > 0) return r.message ?? "This purchase belongs to another Penroam account.";
  if (r.waiting > 0) return "Your purchases couldn't be checked just now. Try again in a moment.";
  return r.applied > 0 ? "Your purchases are restored." : "There are no purchases to restore.";
}

export const syncApi = {
  info: () => call<SyncInfo>("sync_info"),
  serverInfo: (serverUrl: string) => call<ServerInfo>("sync_server_info", { serverUrl }),
  storePurchase: (offerId: string) => call<StorePurchaseResult>("sync_store_purchase", { offerId }),
  storeRestore: () => call<StoreSettled>("sync_store_restore"),
  storeReconcile: () => call<StoreSettled>("sync_store_reconcile"),
  /** Opens the system browser; resolves with the account id after the user signed in. */
  signIn: (serverUrl: string) => call<string>("sync_sign_in", { serverUrl }),
  signOut: (serverUrl: string) => call<void>("sync_sign_out", { serverUrl }),
  setupFirst: (serverUrl: string, account: string, label: string) => call<SetupResult>("sync_setup_first", { serverUrl, account, label }),
  setupPair: (serverUrl: string, account: string, label: string) => call<SetupResult>("sync_setup_pair", { serverUrl, account, label }),
  setupRecovery: (serverUrl: string, account: string, label: string, code: string) =>
    call<void>("sync_setup_recovery", { serverUrl, account, label, code }),
  pollPairing: () => call<string | null>("sync_poll_pairing"),
  approveDevice: (code: string, label: string) => call<string>("sync_approve_device", { code, label }),
  removeDevice: (deviceId: string) => call<void>("sync_remove_device", { deviceId }),
  renameDevice: (deviceId: string, label: string) => call<void>("sync_rename_device", { deviceId, label }),
  erasedNotebooks: () => call<ErasedNotebook[]>("sync_erased_notebooks"),
  restoreErased: (notebookId: string) => call<void>("sync_restore_erased", { notebookId }),
  devices: () => call<SyncDevice[]>("sync_devices"),
  syncNow: () => call<void>("sync_now"),
  pause: (paused: boolean) => call<void>("sync_pause", { paused }),
  conflicts: (notebookId: string) => call<SyncConflict[]>("sync_conflicts", { notebookId }),
  resolveConflict: (notebookId: string, target: ConflictTarget, choose: string) =>
    call<void>("sync_resolve_conflict", { notebookId, target, choose }),
  diagnostics: () => call<string>("sync_diagnostics"),
  disconnect: () => call<void>("sync_disconnect"),
  account: () => call<AccountView | null>("sync_account"),
  /** Opens checkout (`subscribe` + offer id) or the billing portal in the system browser. */
  openBilling: (action: "subscribe" | "manage_billing", offerId?: string) => call<void>("sync_open_billing", { action, offerId: offerId ?? null }),
  /** Signs in again in the browser, then asks the server to erase the cloud copy. */
  requestDeletion: () => call<{ eraseAfterMs: number }>("sync_request_account_deletion"),
  cancelDeletion: () => call<void>("sync_cancel_account_deletion"),
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
      return { tone: "warn", text: "Sign-in expired · sign in again to resume; changes stay queued on this device" };
    case "revoked":
      return { tone: "danger", text: "This device was removed from the account; local notes are kept" };
    case "needsUpgrade":
      return { tone: "warn", text: "Another device uses a newer Penroam · update to receive those changes (nothing is lost)" };
    case "quotaExceeded":
      return { tone: "warn", text: "Uploads paused: cloud storage is full or the account is read-only · changes stay queued on this device" };
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

/** Byte counts for people: 812 KB, 4.2 MB, 5 GB (decimal-looking labels, binary units). */
export function formatBytes(n: number): string {
  const units = ["bytes", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  if (i === 0) return `${n} ${n === 1 ? "byte" : "bytes"}`;
  return `${v >= 10 || Number.isInteger(v) ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** Share of the allowance in use, 0..1 (null when unlimited). */
export function usageFraction(a: AccountView): number | null {
  if (a.quotaBytes == null) return null;
  if (a.quotaBytes === 0) return 1;
  return Math.min(1, a.usedBytes / a.quotaBytes);
}

function day(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
}

/**
 * One line about the plan. Never promises anything the server did not say; unknown statuses
 * show the plan name only.
 */
export function describePlan(a: AccountView): string {
  switch (a.planStatus) {
    case "free":
      return "Free storage";
    case "active":
      return a.renewsAtMs ? `${a.planName} · renews ${day(a.renewsAtMs)}` : a.planName;
    case "trialing":
      return a.renewsAtMs ? `${a.planName} · trial until ${day(a.renewsAtMs)}` : `${a.planName} · trial`;
    case "canceling":
      return a.endsAtMs ? `${a.planName} · ends ${day(a.endsAtMs)}` : `${a.planName} · cancelled`;
    case "past_due":
      return a.endsAtMs
        ? `${a.planName} · payment failed; update your payment method by ${day(a.endsAtMs)}`
        : `${a.planName} · payment failed; update your payment method`;
    case "granted":
      return a.endsAtMs ? `${a.planName} · until ${day(a.endsAtMs)}` : a.planName;
    default:
      return a.planName;
  }
}

/** This build can sell this account a plan, one way or the other. */
export function canBuy(a: AccountView): boolean {
  return (a.purchasesAllowed && a.offers.length > 0) || a.storeOffers.length > 0;
}

/**
 * Why uploads are paused, and what still works. The text points at a plan only when
 * this build can sell one.
 */
export function describeReadOnly(a: AccountView): string | null {
  if (a.writable) return null;
  const keep = "Your notes stay on this device and you can still download and export them; new changes wait on this device.";
  switch (a.readOnlyReason) {
    case "over_quota":
      return canBuy(a) || (a.purchasesAllowed && a.canManageBilling)
        ? `Cloud storage is full, so uploads are paused. ${keep} Delete notebooks you no longer need, or choose a plan with more storage.`
        : `Cloud storage is full, so uploads are paused. ${keep} Delete notebooks you no longer need to make room.`;
    case "deletion_pending":
      return `This account is scheduled for deletion, so uploads are paused. ${keep}`;
    case "suspended":
      return `Uploads for this account are paused by Penroam support. ${keep}`;
    default:
      return `Uploads are paused. ${keep}`;
  }
}

/** Storage share from which the banner warns before uploads pause. */
export const NEARLY_FULL = 0.9;

/** A prompt about cloud storage or payment, shown as a banner over the library and editor. */
export interface CloudNotice {
  /** full: uploads paused for lack of storage; nearlyFull: at NEARLY_FULL or more; pastDue: payment failed. */
  kind: "full" | "nearlyFull" | "pastDue";
  text: string;
  /** Show "See plans" (desktop builds with offers from the server). */
  canUpgrade: boolean;
  /** Show "Update payment method" (desktop builds with a billing account). */
  canManage: boolean;
}

/**
 * What the banner says, or null for nothing. Builds that can't sell get the same facts without
 * any button that leads to a purchase. Deletion and suspension are explained in settings only.
 */
export function cloudNotice(a: AccountView | null): CloudNotice | null {
  if (!a) return null;
  const canUpgrade = canBuy(a);
  const canManage = a.purchasesAllowed && a.canManageBilling;
  if (!a.writable && a.readOnlyReason === "over_quota") {
    return {
      kind: "full",
      text: canUpgrade
        ? "Cloud storage is full, so uploads are paused. Your notes are safe on this device. Get more storage to sync again."
        : "Cloud storage is full, so uploads are paused. Your notes are safe on this device. Delete notebooks you no longer need to make room.",
      canUpgrade,
      canManage: false,
    };
  }
  if (a.planStatus === "past_due") {
    return { kind: "pastDue", text: describePlan(a), canUpgrade: false, canManage };
  }
  const f = usageFraction(a);
  if (a.writable && f !== null && f >= NEARLY_FULL) {
    return {
      kind: "nearlyFull",
      text: `Cloud storage is ${Math.floor(f * 100)}% full (${formatBytes(a.usedBytes)} of ${formatBytes(a.quotaBytes!)}). Uploads pause when it is full.`,
      canUpgrade,
      canManage: false,
    };
  }
  return null;
}
