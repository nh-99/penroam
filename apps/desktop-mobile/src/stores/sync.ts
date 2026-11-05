// Sync (developer preview) UI state. Nothing here runs unless the backend
// reports sync as available; nothing polls unless a device is waiting for
// pairing approval.
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "../platform/ipc";
import { useSettings } from "./settings";
import { cloudNotice, onSyncChanged, onSyncStatus, syncApi, type AccountView, type ErasedNotebook, type ServerInfo, type SyncChanged, type SyncDevice, type SyncStatus } from "../platform/sync";

export type SetupStep = "none" | "recovery" | "pairing" | "compare";

/** Penroam Cloud; the backend's value replaces this once known. */
export const PENROAM_CLOUD_URL = "https://sync.penroam.ink";

export const useSync = defineStore("sync", () => {
  const settings = useSettings();
  const available = ref(false);
  const defaultServerUrl = ref(PENROAM_CLOUD_URL);
  const devBuild = ref(false);
  /** The setup form may ask for a server address: debug build with developer mode on. */
  const customServers = computed(() => devBuild.value && settings.s.developerMode);
  const configured = ref(false);
  const status = ref<SyncStatus | null>(null);
  const devices = ref<SyncDevice[]>([]);
  const erased = ref<ErasedNotebook[]>([]);
  const step = ref<SetupStep>("none");
  const recoveryCode = ref<string | null>(null);
  const pairingCode = ref<string | null>(null);
  const comparison = ref<string | null>(null);
  const busy = ref(false);
  /** A browser sign-in is in progress. */
  const signingIn = ref(false);
  /** The server checked in the setup form, and what it needs. */
  const server = ref<(ServerInfo & { url: string }) | null>(null);
  const error = ref<string | null>(null);
  /** Plan and storage of the synced account; null when unknown or the server has no plans. */
  const account = ref<AccountView | null>(null);
  const accountError = ref<string | null>(null);
  /** A browser step for the account is in progress (billing page, deletion sign-in). */
  const accountBusy = ref(false);
  /** The plan picker ("More cloud storage") is open. */
  const upgradeOpen = ref(false);
  /** Banner kinds the user closed in this run; a new kind shows again. */
  const dismissedNotices = ref<string[]>([]);
  const notice = computed(() => {
    const n = cloudNotice(account.value);
    return n && !dismissedNotices.value.includes(n.kind) ? n : null;
  });
  const changeListeners = new Set<(c: SyncChanged) => void>();
  let unlisten: UnlistenFn[] = [];
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let started = false;

  const awaiting = computed(() => status.value?.phase === "awaitingApproval");

  async function init() {
    if (started || !hasHost()) return;
    started = true;
    try {
      const info = await syncApi.info();
      available.value = info.available;
      configured.value = info.configured;
      defaultServerUrl.value = info.defaultServerUrl;
      devBuild.value = info.devBuild;
      status.value = info.status;
    } catch {
      available.value = false;
      return;
    }
    if (!available.value) return;
    unlisten.push(
      await onSyncStatus((s) => {
        // uploads just hit the storage limit: fetch the plan so the banner can explain it
        if (s?.phase === "quotaExceeded" && status.value?.phase !== "quotaExceeded") void refreshAccount();
        status.value = s;
        configured.value = s !== null;
        if (s === null) devices.value = [];
        updatePolling();
      }),
    );
    unlisten.push(await onSyncChanged((c) => changeListeners.forEach((l) => l(c))));
    // coming back from the browser (checkout, billing portal): show the new plan
    window.addEventListener("focus", onFocus);
    updatePolling();
    // plan and storage for the banner (one request; servers without account.v1 answer null)
    if (configured.value) void refreshAccount();
  }

  function onFocus() {
    if (configured.value) void refreshAccount();
  }

  async function refreshAccount() {
    if (!available.value || !configured.value) {
      account.value = null;
      return;
    }
    try {
      const was = account.value;
      account.value = await syncApi.account();
      accountError.value = null;
      // more storage (a new plan, deleted notebooks): upload the queued changes now rather than
      // at the next scheduled attempt
      if (was && !was.writable && account.value?.writable && status.value?.phase === "quotaExceeded") void syncApi.syncNow().catch(() => {});
      // subscribed (no more offers): the picker has done its job
      if (!account.value?.offers.length) upgradeOpen.value = false;
    } catch (e) {
      accountError.value = (e as Error).message ?? String(e);
    }
  }

  async function accountStep<T>(f: () => Promise<T>): Promise<T | null> {
    accountBusy.value = true;
    accountError.value = null;
    try {
      return await f();
    } catch (e) {
      accountError.value = (e as Error).message ?? String(e);
      return null;
    } finally {
      accountBusy.value = false;
    }
  }

  /** Opens the checkout or the billing portal in the system browser (desktop builds). */
  const openBilling = (action: "subscribe" | "manage_billing", offerId?: string) => accountStep(() => syncApi.openBilling(action, offerId));

  /** Opens the plan picker; only when this build may sell plans and the server offers some. */
  function showUpgrade() {
    if (account.value?.purchasesAllowed && account.value.offers.length > 0) upgradeOpen.value = true;
  }

  function dismissNotice(kind: string) {
    if (!dismissedNotices.value.includes(kind)) dismissedNotices.value.push(kind);
  }

  /** Signs in again in the browser, then requests deletion of the cloud copy. */
  async function requestDeletion() {
    signingIn.value = true;
    try {
      const r = await accountStep(() => syncApi.requestDeletion());
      await refreshAccount();
      return r;
    } finally {
      signingIn.value = false;
    }
  }

  async function cancelDeletion() {
    await accountStep(() => syncApi.cancelDeletion());
    await refreshAccount();
  }

  function onChanged(cb: (c: SyncChanged) => void): () => void {
    changeListeners.add(cb);
    return () => changeListeners.delete(cb);
  }

  function updatePolling() {
    const want = configured.value && step.value === "pairing";
    if (want && !pollTimer) pollTimer = setInterval(() => void pollPairing(), 3000);
    if (!want && pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  async function run<T>(f: () => Promise<T>): Promise<T | null> {
    busy.value = true;
    error.value = null;
    try {
      return await f();
    } catch (e) {
      error.value = (e as Error).message ?? String(e);
      return null;
    } finally {
      busy.value = false;
    }
  }

  async function refresh() {
    if (!available.value) return;
    const info = await syncApi.info().catch(() => null);
    if (info) {
      configured.value = info.configured;
      status.value = info.status;
    }
    if (configured.value && step.value === "none") {
      devices.value = (await syncApi.devices().catch(() => null)) ?? devices.value;
      erased.value = (await syncApi.erasedNotebooks().catch(() => null)) ?? erased.value;
      await refreshAccount();
    }
  }

  /** Checks a server address (setup form step 1). */
  async function checkServer(url: string) {
    const info = await run(() => syncApi.serverInfo(url));
    server.value = info ? { ...info, url } : null;
  }

  /** Forgets the checked server (to type another address). */
  function changeServer() {
    server.value = null;
    error.value = null;
  }

  /** Browser sign-in: before setup, or again after the sign-in ended. */
  async function signIn(url: string) {
    signingIn.value = true;
    try {
      const account = await run(() => syncApi.signIn(url));
      if (account && server.value?.url === url) server.value = { ...server.value, signedInAccount: account };
      if (account && configured.value) await refresh();
      return account;
    } finally {
      signingIn.value = false;
    }
  }

  async function signOut(url: string) {
    const ok = await run(() => syncApi.signOut(url));
    if (ok !== null && server.value?.url === url) server.value = { ...server.value, signedInAccount: null };
  }

  async function setupFirst(serverUrl: string, account: string, label: string) {
    const r = await run(() => syncApi.setupFirst(serverUrl, account, label));
    if (r?.recoveryCode) {
      configured.value = true;
      recoveryCode.value = r.recoveryCode;
      step.value = "recovery";
    }
  }

  async function setupPair(serverUrl: string, account: string, label: string) {
    const r = await run(() => syncApi.setupPair(serverUrl, account, label));
    if (r?.pairingCode) {
      configured.value = true;
      pairingCode.value = r.pairingCode;
      step.value = "pairing";
      updatePolling();
    }
  }

  async function setupRecovery(serverUrl: string, account: string, label: string, code: string) {
    const ok = await run(() => syncApi.setupRecovery(serverUrl, account, label, code));
    if (ok !== null) {
      configured.value = true;
      step.value = "none";
      await refresh();
    }
  }

  async function pollPairing() {
    if (step.value !== "pairing") return;
    try {
      const sas = await syncApi.pollPairing();
      if (sas) {
        comparison.value = sas;
        step.value = "compare";
        updatePolling();
      }
    } catch (e) {
      error.value = (e as Error).message;
    }
  }

  /** The user compared the codes on both screens. */
  async function confirmComparison(match: boolean) {
    if (match) {
      step.value = "none";
      comparison.value = null;
      pairingCode.value = null;
      await refresh();
      return;
    }
    await run(() => syncApi.disconnect());
    configured.value = false;
    step.value = "none";
    comparison.value = null;
    pairingCode.value = null;
    error.value =
      "The codes did not match, so this device was disconnected without receiving any keys. Someone may be interfering with the connection; try again on a network you trust.";
  }

  function finishRecovery() {
    recoveryCode.value = null;
    step.value = "none";
    void refresh();
  }

  const approve = (code: string, label: string) => run(() => syncApi.approveDevice(code, label));
  const remove = async (id: string) => {
    await run(() => syncApi.removeDevice(id));
    await refresh();
  };
  const rename = async (id: string, label: string) => {
    await run(() => syncApi.renameDevice(id, label));
    await refresh();
  };
  const restoreErased = async (id: string) => {
    await run(() => syncApi.restoreErased(id));
    await refresh();
  };
  const syncNow = () => run(() => syncApi.syncNow());
  const pause = (p: boolean) => run(() => syncApi.pause(p));
  const disconnect = async () => {
    const ok = await run(() => syncApi.disconnect());
    if (ok !== null) {
      configured.value = false;
      status.value = null;
      devices.value = [];
      server.value = null;
      account.value = null;
      upgradeOpen.value = false;
    }
  };
  const diagnostics = () => run(() => syncApi.diagnostics());

  function dispose() {
    unlisten.forEach((u) => u());
    unlisten = [];
    window.removeEventListener("focus", onFocus);
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = null;
    started = false;
  }

  return {
    available,
    defaultServerUrl,
    devBuild,
    customServers,
    configured,
    status,
    devices,
    erased,
    step,
    recoveryCode,
    pairingCode,
    comparison,
    busy,
    signingIn,
    server,
    error,
    account,
    accountError,
    accountBusy,
    upgradeOpen,
    notice,
    showUpgrade,
    dismissNotice,
    refreshAccount,
    openBilling,
    requestDeletion,
    cancelDeletion,
    awaiting,
    init,
    refresh,
    onChanged,
    checkServer,
    changeServer,
    signIn,
    signOut,
    setupFirst,
    setupPair,
    setupRecovery,
    pollPairing,
    confirmComparison,
    finishRecovery,
    approve,
    remove,
    rename,
    restoreErased,
    syncNow,
    pause,
    disconnect,
    diagnostics,
    dispose,
  };
});
