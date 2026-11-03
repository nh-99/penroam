// Sync (developer preview) UI state. Nothing here runs unless the backend
// reports sync as available; nothing polls unless a device is waiting for
// pairing approval.
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "../platform/ipc";
import { onSyncChanged, onSyncStatus, syncApi, type SyncChanged, type SyncDevice, type SyncStatus } from "../platform/sync";

export type SetupStep = "none" | "recovery" | "pairing" | "compare";

export const useSync = defineStore("sync", () => {
  const available = ref(false);
  const configured = ref(false);
  const status = ref<SyncStatus | null>(null);
  const devices = ref<SyncDevice[]>([]);
  const step = ref<SetupStep>("none");
  const recoveryCode = ref<string | null>(null);
  const pairingCode = ref<string | null>(null);
  const comparison = ref<string | null>(null);
  const busy = ref(false);
  const error = ref<string | null>(null);
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
      status.value = info.status;
    } catch {
      available.value = false;
      return;
    }
    if (!available.value) return;
    unlisten.push(
      await onSyncStatus((s) => {
        status.value = s;
        configured.value = s !== null;
        if (s === null) devices.value = [];
        updatePolling();
      }),
    );
    unlisten.push(await onSyncChanged((c) => changeListeners.forEach((l) => l(c))));
    updatePolling();
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
    if (configured.value && step.value === "none") devices.value = (await syncApi.devices().catch(() => null)) ?? devices.value;
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
  const syncNow = () => run(() => syncApi.syncNow());
  const pause = (p: boolean) => run(() => syncApi.pause(p));
  const disconnect = async () => {
    const ok = await run(() => syncApi.disconnect());
    if (ok !== null) {
      configured.value = false;
      status.value = null;
      devices.value = [];
    }
  };
  const diagnostics = () => run(() => syncApi.diagnostics());

  function dispose() {
    unlisten.forEach((u) => u());
    unlisten = [];
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = null;
    started = false;
  }

  return {
    available,
    configured,
    status,
    devices,
    step,
    recoveryCode,
    pairingCode,
    comparison,
    busy,
    error,
    awaiting,
    init,
    refresh,
    onChanged,
    setupFirst,
    setupPair,
    setupRecovery,
    pollPairing,
    confirmComparison,
    finishRecovery,
    approve,
    remove,
    syncNow,
    pause,
    disconnect,
    diagnostics,
    dispose,
  };
});
