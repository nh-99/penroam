<script setup lang="ts">
// One discussion: where it is attached, its comments, and what this person may do with them.
// Delivery is always visible: a queued or failed comment says so and is never shown as sent.
// Authors edit and delete their own comments (an edited one says so); the owner can remove
// someone else's, which leaves "Removed by the owner" rather than silently erasing it.
import { computed, ref } from "vue";
import { BellOff, Bell, CheckCircle2, CircleDot, LocateFixed, Paperclip, RotateCcw, Trash2, SquarePen } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import { cleanName } from "../platform/display";
import CommentBody from "./CommentBody.vue";
import CommentComposer from "./CommentComposer.vue";
import { anchorLabel, authorName, deliveryLabel, orphanReason, plainBody, type CommentView, type Mentionable, type ThreadView } from "../platform/comments";

const props = defineProps<{ thread: ThreadView; members: Mentionable[]; canComment: boolean; isModerator: boolean; selfId: string; muted: boolean; busy?: boolean; expanded?: boolean }>();
const emit = defineEmits<{
  reply: [body: string];
  edit: [commentId: string, body: string];
  remove: [commentId: string];
  resolve: [resolved: boolean];
  locate: [];
  reattach: [];
  retry: [body: string];
  mute: [muted: boolean];
  toggle: [];
}>();

const replying = ref(false);
const editing = ref<string | null>(null);
const orphan = computed(() => (props.thread.placement.kind === "orphaned" ? props.thread.placement : null));
const first = computed(() => props.thread.comments[0]);
// the thread's own authors give names to ids the member list no longer has
const authors = computed<Mentionable[]>(() => props.thread.comments.map((c) => ({ accountId: c.authorAccountId, display: c.authorDisplay })));
const when = (ms: number) => new Date(ms).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
const isMine = (c: CommentView) => !!props.selfId && c.authorAccountId === props.selfId;
const canResolve = computed(() => props.canComment || props.isModerator);
const summary = computed(() => `${first.value ? authorName(first.value) : "Thread"}: ${anchorLabel(props.thread.anchor)}${props.thread.status === "resolved" ? " (resolved)" : ""}${props.thread.unread ? " (new)" : ""}`);

function sendReply(body: string) {
  replying.value = false;
  emit("reply", body);
}
function saveEdit(id: string, body: string) {
  editing.value = null;
  emit("edit", id, body);
}
</script>

<template>
  <li class="rounded-xl border border-line p-3 epaper:border-2" :data-thread="thread.threadId" :aria-label="summary">
    <div class="flex flex-wrap items-center gap-2">
      <button type="button" class="touch-target flex min-w-0 flex-1 items-center gap-1.5 text-left text-sm font-medium" :aria-expanded="expanded" @click="emit('toggle')">
        <span v-if="thread.unread" class="inline-flex h-2.5 w-2.5 shrink-0 rounded-full bg-brand epaper:bg-black" aria-hidden="true" />
        <span v-if="thread.unread" class="sr-only">New activity.</span>
        <span class="truncate">{{ anchorLabel(thread.anchor) }}</span>
      </button>
      <span class="inline-flex items-center gap-1 rounded-full border border-line px-2 py-0.5 text-xs text-muted epaper:border-black epaper:text-black" data-testid="thread-status">
        <CheckCircle2 v-if="thread.status === 'resolved'" :size="12" aria-hidden="true" /><CircleDot v-else :size="12" aria-hidden="true" />
        {{ thread.status === "resolved" ? (thread.resolvedByDisplay ? `Resolved by ${cleanName(thread.resolvedByDisplay)}` : "Resolved") : "Open" }}
      </span>
    </div>

    <div v-if="orphan" class="mt-2 rounded-lg bg-surface-2 p-2 text-xs epaper:border" data-testid="orphan-note">
      <p class="font-medium">Not on the page</p>
      <p class="text-muted">{{ orphanReason(orphan) }}</p>
      <p v-if="orphan.quote" class="mt-1 border-l-2 border-line pl-2 italic">“{{ orphan.quote }}”</p>
      <div class="mt-2 flex flex-wrap gap-2">
        <Btn v-if="orphan.lastPageId" variant="outline" @click="emit('locate')"><LocateFixed :size="16" />Locate</Btn>
        <Btn v-if="canComment" variant="outline" @click="emit('reattach')"><Paperclip :size="16" />Re-attach…</Btn>
      </div>
    </div>

    <ol v-if="expanded" class="mt-2 space-y-3">
      <li v-for="c in thread.comments" :key="c.commentId" class="space-y-1" :data-comment="c.commentId">
        <p class="flex flex-wrap items-baseline gap-x-2 text-xs text-muted">
          <bdi class="font-medium text-ink">{{ authorName(c) }}</bdi>
          <time>{{ when(c.at) }}</time>
          <span v-if="c.edited && !c.deleted" data-testid="edited">(edited)</span>
          <span v-if="deliveryLabel(c.delivery)" class="rounded bg-surface-2 px-1.5 py-0.5 font-medium text-ink epaper:border" :class="c.delivery === 'failed' ? 'text-danger' : ''" data-testid="delivery">{{ deliveryLabel(c.delivery) }}</span>
        </p>
        <p v-if="c.deleted" class="text-sm italic text-muted" data-testid="tombstone">{{ c.removedByOwner ? "Removed by the owner" : "Deleted" }}</p>
        <CommentComposer v-else-if="editing === c.commentId" label="Edit comment" submit="Save" :members="members" :authors="authors" :initial="c.body" autofocus @submit="(b) => saveEdit(c.commentId, b)" @cancel="editing = null" />
        <CommentBody v-else :body="c.body" :members="members" :authors="authors" />
        <div v-if="!c.deleted && editing !== c.commentId" class="flex flex-wrap gap-1">
          <Btn v-if="c.delivery === 'failed'" variant="outline" @click="emit('retry', c.body)"><RotateCcw :size="16" />Retry</Btn>
          <Btn v-if="isMine(c) && c.delivery === 'sent' && canComment" variant="ghost" :aria-label="`Edit comment by ${authorName(c)}`" @click="editing = c.commentId"><SquarePen :size="16" />Edit</Btn>
          <Btn v-if="(isMine(c) && c.delivery === 'sent') || (isModerator && c.delivery === 'sent')" variant="ghost" class="text-danger" :aria-label="isMine(c) ? 'Delete your comment' : `Remove comment by ${authorName(c)}`" @click="emit('remove', c.commentId)">
            <Trash2 :size="16" />{{ isMine(c) ? "Delete" : "Remove" }}
          </Btn>
        </div>
      </li>
    </ol>
    <p v-else-if="first" class="mt-1 line-clamp-2 text-sm text-muted">{{ first.deleted ? (first.removedByOwner ? "Removed by the owner" : "Deleted") : plainBody(first.body, members, authors) }}</p>

    <div v-if="expanded" class="mt-3 space-y-2">
      <CommentComposer v-if="replying" label="Reply" submit="Send reply" :members="members" :busy="busy" autofocus @submit="sendReply" @cancel="replying = false" />
      <div v-else class="flex flex-wrap gap-2">
        <Btn v-if="canComment" variant="outline" @click="replying = true">Reply</Btn>
        <Btn v-if="canResolve && !orphan" variant="outline" @click="emit('resolve', thread.status === 'open')">{{ thread.status === "open" ? "Resolve" : "Reopen" }}</Btn>
        <Btn v-if="!orphan" variant="ghost" @click="emit('locate')"><LocateFixed :size="16" />Locate</Btn>
        <Btn variant="ghost" :aria-pressed="muted" @click="emit('mute', !muted)"><component :is="muted ? Bell : BellOff" :size="16" />{{ muted ? "Unmute thread" : "Mute thread" }}</Btn>
      </div>
      <p v-if="!canComment" class="text-xs text-muted">You can read this discussion but not write in it.</p>
    </div>
  </li>
</template>
