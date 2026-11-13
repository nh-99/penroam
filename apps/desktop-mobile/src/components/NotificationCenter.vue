<script setup lang="ts">
// Mentions and replies, newest first. Each one opens the notebook and the thread when this device
// has that notebook; otherwise it says so and reveals nothing about it. A notification holds ids
// only, so the text is read from the thread, never from the notification. Muting is per notebook
// or per thread.
import { computed } from "vue";
import { AtSign, BellOff, Bell, MessageSquareReply } from "@lucide/vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { cleanName } from "../platform/display";
import { useComments } from "../stores/comments";
import { useLibrary } from "../stores/library";
import { useUi } from "../stores/ui";
import type { NotificationItem } from "../platform/comments";

const emit = defineEmits<{ close: [] }>();
const store = useComments();
const lib = useLibrary();
const ui = useUi();
const items = computed(() => [...store.notifications].sort((a, b) => b.at - a.at));
const titleOf = (id: string) => lib.notebooks.find((n) => n.id === id)?.title;
const when = (ms: number) => new Date(ms).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
const verb = (n: NotificationItem) => (n.kind === "mention" ? "mentioned you" : "replied in a thread you're in");

async function follow(n: NotificationItem) {
  void store.markRead(n);
  if (!titleOf(n.notebookId)) {
    // not on this device (yet) or no longer allowed: nothing about it is shown
    ui.toast("info", "That notebook isn't available on this device yet. Open Penroam when you're online and try again.");
    return;
  }
  emit("close");
  store.focusThread = n.threadId;
  store.drawerOpen = true;
  await lib.open(n.notebookId);
}
</script>

<template>
  <Modal title="Notifications" wide @close="emit('close')">
    <p v-if="!items.length" class="py-6 text-center text-sm text-muted" data-testid="notifications-empty">No notifications.</p>
    <ul v-else class="divide-y divide-line rounded-xl border border-line" aria-label="Notifications">
      <li v-for="n in items" :key="n.notificationId" class="flex flex-wrap items-center gap-2 px-3 py-2" :data-notification="n.notificationId">
        <component :is="n.kind === 'mention' ? AtSign : MessageSquareReply" :size="18" class="shrink-0 text-muted" aria-hidden="true" />
        <button type="button" class="touch-target min-w-0 flex-1 basis-48 text-left" @click="follow(n)">
          <span class="block text-sm" :class="n.read ? '' : 'font-semibold'">
            <span v-if="!n.read" class="sr-only">Unread. </span><bdi>{{ cleanName(n.actorDisplay) || "Someone" }}</bdi> {{ verb(n) }}
          </span>
          <span class="block text-xs text-muted">{{ titleOf(n.notebookId) ?? "A notebook that isn't on this device" }} · {{ when(n.at) }}</span>
        </button>
        <Btn variant="ghost" :aria-pressed="store.threadMuted(n.notebookId, n.threadId)" @click="store.muteThread(n.notebookId, n.threadId, !store.threadMuted(n.notebookId, n.threadId))">
          <component :is="store.threadMuted(n.notebookId, n.threadId) ? Bell : BellOff" :size="16" />{{ store.threadMuted(n.notebookId, n.threadId) ? "Unmute thread" : "Mute thread" }}
        </Btn>
        <Btn variant="ghost" :aria-pressed="store.prefs.mutedNotebooks.includes(n.notebookId)" @click="store.muteNotebook(n.notebookId, !store.prefs.mutedNotebooks.includes(n.notebookId))">
          {{ store.prefs.mutedNotebooks.includes(n.notebookId) ? "Unmute notebook" : "Mute notebook" }}
        </Btn>
      </li>
    </ul>
    <template #actions>
      <Btn @click="emit('close')">Close</Btn>
    </template>
  </Modal>
</template>
