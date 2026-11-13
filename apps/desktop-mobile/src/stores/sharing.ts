// Shared-notebook UI state: the list Rust reports (`shared_list`), refreshed whenever the
// `sync://shared` event fires. Rust is authoritative; this only mirrors its latest answer and
// turns failures into messages. Nothing runs unless a host and sync are available.
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "../platform/ipc";
import { acceptProblem, describeSharingError, onSharedChanged, sharingApi, type IncomingInvitationView, type SharedNotebookView } from "../platform/sharing";
import { useLibrary } from "./library";
import { useUi } from "./ui";

export const useSharing = defineStore("sharing", () => {
  const ui = useUi();
  const notebooks = ref<SharedNotebookView[]>([]);
  const invitations = ref<IncomingInvitationView[]>([]);
  /** The server supports sharing and this device is signed in. */
  const available = ref(false);
  /** Invitations can't be matched to this account until it signs in again (stale verified email). */
  const invitationsNeedSignIn = ref(false);
  /** Invitation or space ids being accepted, declined or removed. */
  const working = ref<string[]>([]);
  /** Invitations whose accepting needs something first, by invitation id. */
  const problems = ref<Record<string, "reauth" | "oldCopy">>({});
  let unlisten: UnlistenFn | null = null;

  /** Notebooks shared with this account by somebody else (including ones still joining). */
  const withMe = computed(() => notebooks.value.filter((n) => !n.owner));
  /** Notebooks this account owns and has shared. */
  const mine = computed(() => notebooks.value.filter((n) => n.owner));
  /** The shared entry of a local notebook, if it is one. */
  const byNotebook = computed(() => {
    const m = new Map<string, SharedNotebookView>();
    for (const n of notebooks.value) if (n.notebookId) m.set(n.notebookId, n);
    return m;
  });
  /** Ids of local notebooks that belong in "Shared with me" instead of the main shelf. */
  const withMeIds = computed(() => new Set(withMe.value.flatMap((n) => (n.notebookId ? [n.notebookId] : []))));

  async function refresh() {
    if (!hasHost()) return;
    try {
      const v = await sharingApi.list();
      notebooks.value = v.notebooks;
      invitations.value = v.invitations;
      available.value = v.available;
      invitationsNeedSignIn.value = v.invitationsNeedSignIn;
    } catch {
      // offline or no sharing: keep the last list so cards don't vanish while the network is down
    }
  }

  async function init() {
    if (unlisten || !hasHost()) return;
    unlisten = await onSharedChanged(() => void refresh());
    await refresh();
  }

  function dispose() {
    unlisten?.();
    unlisten = null;
  }

  async function run<T>(id: string, what: string, fn: () => Promise<T>): Promise<T | null> {
    working.value.push(id);
    try {
      const r = await fn();
      await refresh();
      return r;
    } catch (e) {
      ui.error(`${what}: ${describeSharingError(e)}`);
      return null;
    } finally {
      working.value = working.value.filter((w) => w !== id);
    }
  }

  async function accept(inv: IncomingInvitationView) {
    problems.value = Object.fromEntries(Object.entries(problems.value).filter(([k]) => k !== inv.invitationId));
    working.value.push(inv.invitationId);
    try {
      const v = await sharingApi.accept(inv.invitationId);
      ui.success("Joining the shared notebook…");
      await refresh();
      return v;
    } catch (e) {
      const p = acceptProblem(e);
      // these two are answered on the card itself, with the action that fixes them
      if (p) problems.value = { ...problems.value, [inv.invitationId]: p };
      else ui.error(`Could not accept the invitation: ${describeSharingError(e)}`);
      return null;
    } finally {
      working.value = working.value.filter((w) => w !== inv.invitationId);
    }
  }
  async function decline(inv: IncomingInvitationView) {
    working.value.push(inv.invitationId);
    try {
      await sharingApi.decline(inv.invitationId);
      await refresh();
    } catch (e) {
      // like accepting: a stale email needs a fresh sign-in, which the card offers
      if (acceptProblem(e) === "reauth") problems.value = { ...problems.value, [inv.invitationId]: "reauth" };
      else ui.error(`Could not decline the invitation: ${describeSharingError(e)}`);
    } finally {
      working.value = working.value.filter((w) => w !== inv.invitationId);
    }
  }

  /** Copies the notebook (with edits not yet sent) into the personal library. */
  async function keepPrivateCopy(n: SharedNotebookView) {
    const r = await run(n.spaceId, "Could not make a private copy", () => sharingApi.keepPrivateCopy(n.spaceId));
    if (r) {
      await useLibrary().refresh();
      ui.success("Kept a private copy in your notebooks.");
    }
    return r;
  }

  /** Removes only this device's copy; nothing is deleted for the others. */
  async function removeLocal(n: SharedNotebookView) {
    const r = await run(n.spaceId, "Could not remove the notebook", () => sharingApi.removeLocal(n.spaceId));
    await useLibrary().refresh();
    return r;
  }

  return { notebooks, invitations, available, invitationsNeedSignIn, working, problems, withMe, mine, byNotebook, withMeIds, refresh, init, dispose, accept, decline, keepPrivateCopy, removeLocal };
});
