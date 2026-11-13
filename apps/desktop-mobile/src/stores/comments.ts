// Comments state: the open notebook's threads and the account's notifications, as Rust reports
// them (`comments_list`, `notifications_list`), refreshed on `comments://changed` and
// `notifications://changed`. Delivery states come from Rust; this never marks anything sent.
import { defineStore } from "pinia";
import { computed, ref } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "../platform/ipc";
import { describeSharingError } from "../platform/sharing";
import {
  commentsApi,
  notificationsApi,
  onCommentsChanged,
  onNotificationsChanged,
  splitThreads,
  unreadCount,
  type AnchorView,
  type CommentFilter,
  type CommentsView,
  type Mentionable,
  type NotificationItem,
  type NotificationPrefs,
  type ThreadView,
} from "../platform/comments";
import { useUi } from "./ui";

export const useComments = defineStore("comments", () => {
  const ui = useUi();
  const notebookId = ref<string | null>(null);
  const view = ref<CommentsView>({ canComment: false, isModerator: false, threads: [] });
  const members = ref<Mentionable[]>([]);
  const filter = ref<CommentFilter>("open");
  const drawerOpen = ref(false);
  /** A thread to show once it is loaded (a notification was followed). */
  const focusThread = ref<string | null>(null);
  const notifications = ref<NotificationItem[]>([]);
  const unreadNotifications = ref(0);
  const prefs = ref<NotificationPrefs>({ mutedNotebooks: [], mutedThreads: [] });
  const centerOpen = ref(false);
  let unlisten: UnlistenFn[] = [];

  const threads = computed(() => view.value.threads);
  const split = computed(() => splitThreads(view.value.threads, filter.value));
  const unreadThreads = computed(() => unreadCount(view.value.threads));
  const openCount = computed(() => view.value.threads.filter((t) => t.status === "open").length);
  const find = (id: string): ThreadView | undefined => view.value.threads.find((t) => t.threadId === id);
  const notebookMuted = computed(() => !!notebookId.value && prefs.value.mutedNotebooks.includes(notebookId.value));
  const threadMuted = (nb: string, thread: string) => prefs.value.mutedThreads.some((t) => t.notebookId === nb && t.threadId === thread);

  const fail = (what: string, e: unknown) => ui.error(`${what}: ${describeSharingError(e)}`);

  async function load(id: string | null) {
    notebookId.value = id;
    if (!id || !hasHost()) {
      view.value = { canComment: false, isModerator: false, threads: [] };
      members.value = [];
      return;
    }
    try {
      const v = await commentsApi.list(id);
      if (notebookId.value === id) view.value = v;
    } catch {
      // not a shared notebook, or offline with nothing cached: nothing to show
      if (notebookId.value === id) view.value = { canComment: false, isModerator: false, threads: [] };
    }
    try {
      const m = await commentsApi.mentionable(id);
      if (notebookId.value === id) members.value = m;
    } catch {
      members.value = [];
    }
  }

  async function refreshNotifications() {
    if (!hasHost()) return;
    try {
      const n = await notificationsApi.list();
      notifications.value = n.items;
      unreadNotifications.value = n.unread;
      prefs.value = await notificationsApi.prefs();
    } catch {
      // keep what was shown
    }
  }

  async function init() {
    if (unlisten.length || !hasHost()) return;
    unlisten.push(
      await onCommentsChanged((id) => {
        if (id === notebookId.value) void load(id);
      }),
      await onNotificationsChanged(() => void refreshNotifications()),
    );
    await refreshNotifications();
  }

  function dispose() {
    unlisten.forEach((u) => u());
    unlisten = [];
  }

  /** Runs a comment command, then reloads; failures are reported and return false. */
  async function run(what: string, fn: () => Promise<unknown>): Promise<boolean> {
    const id = notebookId.value;
    if (!id) return false;
    try {
      await fn();
      await load(id);
      return true;
    } catch (e) {
      fail(what, e);
      await load(id);
      return false;
    }
  }

  const createThread = (anchor: AnchorView, body: string) => run("Could not add the comment", () => commentsApi.createThread(notebookId.value!, anchor, body));
  const reply = (threadId: string, body: string) => run("Could not send the reply", () => commentsApi.reply(notebookId.value!, threadId, body));
  const edit = (commentId: string, body: string) => run("Could not save the change", () => commentsApi.edit(notebookId.value!, commentId, body));
  const remove = (commentId: string) => run("Could not delete the comment", () => commentsApi.remove(notebookId.value!, commentId));
  const setResolved = (threadId: string, resolved: boolean) => run(resolved ? "Could not resolve the thread" : "Could not reopen the thread", () => commentsApi.setResolved(notebookId.value!, threadId, resolved));
  const reattach = (threadId: string, anchor: AnchorView) => run("Could not re-attach the comment", () => commentsApi.reattach(notebookId.value!, threadId, anchor));
  /** Retrying a failed comment sends its text again as a new reply or thread; Rust drops the failed one. */
  const retry = (threadId: string, body: string) => reply(threadId, body);

  async function markRead(n: NotificationItem) {
    if (n.read) return;
    try {
      await notificationsApi.markRead(n.notificationId);
      n.read = true;
      unreadNotifications.value = Math.max(0, unreadNotifications.value - 1);
    } catch (e) {
      fail("Could not mark it as read", e);
    }
  }

  async function savePrefs(next: NotificationPrefs) {
    try {
      prefs.value = await notificationsApi.setPrefs(next);
    } catch (e) {
      fail("Could not change the setting", e);
    }
  }
  const muteNotebook = (id: string, muted: boolean) =>
    savePrefs({ ...prefs.value, mutedNotebooks: muted ? [...new Set([...prefs.value.mutedNotebooks, id])] : prefs.value.mutedNotebooks.filter((x) => x !== id) });
  const muteThread = (nb: string, thread: string, muted: boolean) =>
    savePrefs({
      ...prefs.value,
      mutedThreads: muted ? [...prefs.value.mutedThreads.filter((t) => !(t.notebookId === nb && t.threadId === thread)), { notebookId: nb, threadId: thread }] : prefs.value.mutedThreads.filter((t) => !(t.notebookId === nb && t.threadId === thread)),
    });

  return {
    notebookId, view, members, filter, drawerOpen, focusThread, notifications, unreadNotifications, prefs, centerOpen,
    threads, split, unreadThreads, openCount, notebookMuted, find, threadMuted,
    load, init, dispose, refreshNotifications, createThread, reply, edit, remove, setResolved, reattach, retry, markRead, muteNotebook, muteThread,
  };
});
