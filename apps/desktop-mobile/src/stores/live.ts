// Live collaboration state: other people's presence and the live channel's status per notebook.
// Rust is the source (`live://presence`, `live://status`); this mirrors it, drops stale entries,
// and holds the device's presence preference. Nothing here says anything was saved: that is sync's
// job, and a packet from another person never changes it.
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "../platform/ipc";
import { activePeers, liveApi, onLivePresence, onLiveStatus, type LiveState, type PresencePeer } from "../platform/live";
import { useUi } from "./ui";

export const useLive = defineStore("live", () => {
  const ui = useUi();
  const peersByNotebook = ref<Record<string, PresencePeer[]>>({});
  const stateByNotebook = ref<Record<string, LiveState>>({});
  /** Tells others where I am; off sends no awareness but live updates still arrive. */
  const sharePresence = ref(true);
  /** Bumped by a timer so expired peers disappear without a new event. */
  const clock = ref(Date.now());
  let unlisten: UnlistenFn[] = [];
  let timer: ReturnType<typeof setInterval> | null = null;

  /** Current, unexpired peers of a notebook. */
  const peersOf = (notebookId: string | undefined) => {
    void clock.value;
    return notebookId ? activePeers(peersByNotebook.value[notebookId] ?? [], clock.value) : [];
  };
  const stateOf = (notebookId: string | undefined): LiveState | undefined => (notebookId ? stateByNotebook.value[notebookId] : undefined);
  const anyPeers = computed(() => Object.keys(peersByNotebook.value).length > 0);

  async function init() {
    if (unlisten.length || !hasHost()) return;
    unlisten.push(
      await onLivePresence((e) => {
        peersByNotebook.value = { ...peersByNotebook.value, [e.notebookId]: e.peers };
      }),
      await onLiveStatus((e) => {
        stateByNotebook.value = { ...stateByNotebook.value, [e.notebookId]: e.state };
        // a channel that isn't live can't vouch for anyone being here
        if (e.state !== "live") peersByNotebook.value = { ...peersByNotebook.value, [e.notebookId]: [] };
      }),
    );
    timer = setInterval(() => (clock.value = Date.now()), 5000);
    try {
      sharePresence.value = (await liveApi.prefs()).sharePresence;
    } catch {
      // an older backend: presence stays on by default
    }
  }

  function dispose() {
    unlisten.forEach((u) => u());
    unlisten = [];
    if (timer) clearInterval(timer);
    timer = null;
  }

  async function setSharePresence(on: boolean) {
    const was = sharePresence.value;
    sharePresence.value = on;
    try {
      sharePresence.value = (await liveApi.prefs({ sharePresence: on })).sharePresence;
    } catch (e) {
      sharePresence.value = was;
      ui.error(`Could not change the setting: ${(e as Error).message}`);
    }
  }

  return { peersByNotebook, stateByNotebook, sharePresence, clock, peersOf, stateOf, anyPeers, init, dispose, setSharePresence };
});
