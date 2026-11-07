// Sync UI state. Nothing here runs unless the backend
// reports sync as available.
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "../platform/ipc";
import { useSettings } from "./settings";
import { canBuy, cloudNotice, describeRestore, describeStorePurchase, onSyncChanged, onSyncStatus, onVaultsChanged as listenVaultsChanged, syncApi, type AccountView, type ErasedNotebook, type ServerInfo, type SyncChanged, type SyncDevice, type SyncStatus, type VaultSecret, type VaultsView } from "../platform/sync";

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
  /** Asked to download, and not in the library yet. */
  const downloading = ref<string[]>([]);
  /** In the account and not on this device: downloading one just puts it back here. */
  const cloudNotebooks = computed(() => erased.value.filter((n) => !n.inTrash));
  /** Deleted, with no copy on this device: the trash can still restore them. */
  const cloudTrash = computed(() => erased.value.filter((n) => n.inTrash));
  /** The account's keys aren't on the server: its recovery code is needed, once. */
  const needsRecoveryCode = ref(false);
  const vaults = ref<VaultsView>({ supported: false, vaults: [], notebooks: {} });
  /** A new vault's recovery code, until the user confirms saving it. Shown only this once. */
  const vaultRecoveryCode = ref<string | null>(null);
  /** Vaults that notebooks can go into on this device. */
  const openVaults = computed(() => vaults.value.vaults.filter((v) => v.unlocked));
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
  /** The outcome of the last store purchase or restore. */
  const storeMessage = ref<string | null>(null);
  /** The plan picker ("More cloud storage") is open. */
  const upgradeOpen = ref(false);
  /** Banner kinds the user closed in this run; a new kind shows again. */
  const dismissedNotices = ref<string[]>([]);
  const notice = computed(() => {
    const n = cloudNotice(account.value);
    return n && !dismissedNotices.value.includes(n.kind) ? n : null;
  });
  const changeListeners = new Set<(c: SyncChanged) => void>();
  const vaultListeners = new Set<() => void>();
  let unlisten: UnlistenFn[] = [];
  let started = false;

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
        if (s === null) {
          devices.value = [];
          erased.value = [];
          downloading.value = [];
          vaults.value = { supported: false, vaults: [], notebooks: {} };
        } else if (s.phase !== "syncing") void refreshCloudNotebooks();
      }),
    );
    unlisten.push(await onSyncChanged((c) => changeListeners.forEach((l) => l(c))));
    unlisten.push(
      await listenVaultsChanged(() => {
        void refreshVaults();
        vaultListeners.forEach((l) => l());
      }),
    );
    // coming back from the browser (checkout, billing portal): show the new plan
    window.addEventListener("focus", onFocus);
    // plan and storage for the banner (one request; servers without account.v1 answer null)
    if (configured.value) void reconcileStore();
    if (configured.value) void refreshAccount();
    if (configured.value) void refreshCloudNotebooks();
    if (configured.value) void refreshVaults();
  }

  function onFocus() {
    if (!configured.value) return;
    void reconcileStore();
    void refreshAccount();
  }

  /** Delivers anything the app store is still holding, such as a purchase cut off by a crash. */
  async function reconcileStore() {
    const r = await syncApi.storeReconcile().catch(() => null);
    if (r && r.applied > 0) await refreshAccount();
  }

  async function refreshCloudNotebooks() {
    const next = await syncApi.erasedNotebooks().catch(() => null);
    if (!next) return;
    // the engine stops listing a notebook once its install is queued; the card stays until it lands
    const pending = erased.value.filter((n) => downloading.value.includes(n.notebookId) && !next.some((m) => m.notebookId === n.notebookId));
    erased.value = [...next, ...pending];
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

  /** Buys a plan through the app store. Only the server's answer changes the plan. */
  async function buyInStore(offerId: string) {
    storeMessage.value = null;
    const r = await accountStep(() => syncApi.storePurchase(offerId));
    if (!r) return;
    storeMessage.value = describeStorePurchase(r);
    if (r.status === "active") upgradeOpen.value = false;
    await refreshAccount();
  }

  /** Brings back purchases made on this store account. */
  async function restorePurchases() {
    storeMessage.value = null;
    const r = await accountStep(() => syncApi.storeRestore());
    if (!r) return;
    storeMessage.value = describeRestore(r);
    await refreshAccount();
  }

  /** Opens the plan picker; only when this build can sell a plan. */
  function showUpgrade() {
    if (account.value && canBuy(account.value)) upgradeOpen.value = true;
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

  /** Calls `cb` whenever a vault is locked or unlocked here (notebooks left or came back). */
  function onVaultsChanged(cb: () => void): () => void {
    vaultListeners.add(cb);
    return () => vaultListeners.delete(cb);
  }

  function onChanged(cb: (c: SyncChanged) => void): () => void {
    changeListeners.add(cb);
    return () => changeListeners.delete(cb);
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
    if (configured.value) {
      devices.value = (await syncApi.devices().catch(() => null)) ?? devices.value;
      await refreshCloudNotebooks();
      await refreshAccount();
      await refreshVaults();
    }
  }

  async function refreshVaults() {
    const v = await syncApi.vaults().catch(() => null);
    if (v) vaults.value = v;
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
      // signing in is all a device needs
      else if (account) await connect(url, "");
      return account;
    } finally {
      signingIn.value = false;
    }
  }

  async function signOut(url: string) {
    const ok = await run(() => syncApi.signOut(url));
    if (ok !== null && server.value?.url === url) server.value = { ...server.value, signedInAccount: null };
  }

  /** Connects this device to the signed-in account. Pass a code when `needsRecoveryCode` is set. */
  async function connect(serverUrl: string, account: string, recoveryCode?: string) {
    const r = await run(() => syncApi.connect(serverUrl, account, recoveryCode));
    if (!r) return;
    needsRecoveryCode.value = r.needsRecoveryCode;
    if (r.needsRecoveryCode) return;
    configured.value = true;
    await refresh();
  }

  /** Runs a vault step, and shows what changed. True when it worked. */
  async function vaultStep(f: () => Promise<unknown>) {
    const ok = (await run(async () => {
      await f();
      return true;
    })) === true;
    if (ok) await refreshVaults();
    return ok;
  }

  const createVault = (name: string, passphrase: string) =>
    vaultStep(async () => {
      vaultRecoveryCode.value = await syncApi.vaultCreate(name, passphrase);
    });
  /** Shows a vault's new state at once: listing vaults needs the server, and locking and
   * unlocking work offline. */
  function markVault(id: string, unlocked: boolean) {
    vaults.value = {
      ...vaults.value,
      vaults: vaults.value.vaults.map((v) => (v.vaultId === id ? { ...v, unlocked, locked: !unlocked } : v)),
    };
  }
  const unlockVault = (id: string, secret: VaultSecret) =>
    vaultStep(async () => {
      await syncApi.vaultUnlock(id, secret);
      markVault(id, true);
    });
  const setVaultPassphrase = (id: string, secret: VaultSecret, next: string) => vaultStep(() => syncApi.vaultSetPassphrase(id, secret, next));
  const stopVaultSync = (id: string) => vaultStep(() => syncApi.vaultStopSync(id));
  const restoreVault = (id: string, secret: VaultSecret) => vaultStep(() => syncApi.vaultRestore(id, secret));
  const forgetVault = (id: string) => vaultStep(() => syncApi.vaultForget(id));
  /** Locks a vault here: "locked", "needsKey" (ask for the passphrase once) or "failed". */
  async function lockVault(id: string, secret?: VaultSecret): Promise<"locked" | "needsKey" | "failed"> {
    let answer: boolean | null = null;
    const ok = await vaultStep(async () => {
      answer = await syncApi.vaultLock(id, secret);
      if (answer) markVault(id, false);
    });
    if (!ok) return "failed";
    return answer ? "locked" : "needsKey";
  }

  /** The user saved the recovery code: it's gone from the app for good. */
  function finishVaultRecovery() {
    vaultRecoveryCode.value = null;
  }

  const remove = async (id: string) => {
    await run(() => syncApi.removeDevice(id));
    await refresh();
  };
  const rename = async (id: string, label: string) => {
    await run(() => syncApi.renameDevice(id, label));
    await refresh();
  };
  /** Brings back a notebook from `cloudNotebooks` or `cloudTrash`; rejects if that can't start. */
  async function download(id: string) {
    if (downloading.value.includes(id)) return;
    downloading.value.push(id);
    try {
      await syncApi.restoreErased(id);
    } catch (e) {
      downloading.value = downloading.value.filter((d) => d !== id);
      throw e;
    }
  }
  /** These notebooks are in the library, so their cloud cards are done with. */
  function arrived(ids: string[]) {
    downloading.value = downloading.value.filter((d) => !ids.includes(d));
    erased.value = erased.value.filter((n) => !ids.includes(n.notebookId));
  }
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
      needsRecoveryCode.value = false;
      vaults.value = { supported: false, vaults: [], notebooks: {} };
    }
  };
  const diagnostics = () => run(() => syncApi.diagnostics());

  function dispose() {
    unlisten.forEach((u) => u());
    unlisten = [];
    window.removeEventListener("focus", onFocus);
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
    downloading,
    cloudNotebooks,
    cloudTrash,
    needsRecoveryCode,
    vaults,
    openVaults,
    vaultRecoveryCode,
    busy,
    signingIn,
    server,
    error,
    account,
    accountError,
    accountBusy,
    storeMessage,
    buyInStore,
    restorePurchases,
    upgradeOpen,
    notice,
    showUpgrade,
    dismissNotice,
    refreshAccount,
    openBilling,
    requestDeletion,
    cancelDeletion,
    init,
    refresh,
    onChanged,
    onVaultsChanged,
    checkServer,
    changeServer,
    signIn,
    signOut,
    connect,
    refreshVaults,
    createVault,
    unlockVault,
    setVaultPassphrase,
    stopVaultSync,
    restoreVault,
    forgetVault,
    lockVault,
    finishVaultRecovery,
    remove,
    rename,
    download,
    arrived,
    syncNow,
    pause,
    disconnect,
    diagnostics,
    dispose,
  };
});
