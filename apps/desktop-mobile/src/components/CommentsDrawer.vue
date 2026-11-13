<script setup lang="ts">
// The notebook's comments: filters (Open / Resolved / Mine / Mentions), threads on the page, an
// orphaned section for those whose anchor is gone, and the ways to start a new comment. On narrow
// screens it is a bottom sheet. Every control is a real button of touch size, none hover-only.
import { computed, watch } from "vue";
import { MessageSquarePlus, MousePointer2, Hand, Crosshair, Square, FileText, Type, X } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import CommentThread from "./CommentThread.vue";
import CommentComposer from "./CommentComposer.vue";
import { FILTERS, anchorLabel, type AnchorView } from "../platform/comments";
import { useComments } from "../stores/comments";

export type StartKind = "selection" | "element" | "text" | "point" | "rect" | "pdf";
defineProps<{
  /** Which ways of attaching a comment are available right now. */
  can: Record<StartKind, boolean>;
  /** The anchor chosen for a comment being written, if any. */
  pending: AnchorView | null;
  /** Placement is waiting for a tap or drag on the page. */
  placing: StartKind | null;
  selfId: string;
  narrow?: boolean;
}>();
const emit = defineEmits<{ close: []; start: [kind: StartKind]; cancelStart: []; submitNew: [body: string]; locate: [threadId: string]; reattach: [threadId: string]; }>();
const store = useComments();
// the open thread is the store's, so a marker, a notification and this list all agree on it
const openThread = computed({ get: () => store.focusThread, set: (v: string | null) => (store.focusThread = v) });
// a thread that doesn't pass the current filter would be invisible: switch to the one it is in
watch([() => store.focusThread, () => store.view], () => {
  const t = store.focusThread ? store.find(store.focusThread) : undefined;
  if (t && !store.split.placed.concat(store.split.orphaned).some((x) => x.threadId === t.threadId)) store.filter = t.status === "resolved" ? "resolved" : "open";
}, { immediate: true });
const sections = computed(() => store.split);
const empty = computed(() => !sections.value.placed.length && !sections.value.orphaned.length);
const starts: { kind: StartKind; label: string; icon: any }[] = [
  { kind: "selection", label: "Selected object", icon: MousePointer2 },
  { kind: "element", label: "Pick an object", icon: Hand },
  { kind: "text", label: "Selected text", icon: Type },
  { kind: "point", label: "A point", icon: Crosshair },
  { kind: "rect", label: "An area", icon: Square },
  { kind: "pdf", label: "PDF area", icon: FileText },
];
const toggle = (id: string) => (openThread.value = openThread.value === id ? null : id);
</script>

<template>
  <section
    role="complementary"
    aria-label="Comments"
    class="flex min-h-0 flex-col border-line bg-surface"
    :class="narrow ? 'safe-bottom fixed inset-x-0 bottom-0 z-40 max-h-[70vh] rounded-t-2xl border-t-2 shadow-xl' : 'w-80 shrink-0 border-l epaper:border-l-2'"
    data-testid="comments-drawer"
    @keydown.esc.stop="emit('close')"
  >
    <header class="flex items-center gap-2 px-3 py-2">
      <h2 class="mr-auto text-sm font-semibold">Comments</h2>
      <button type="button" class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" aria-label="Close comments" @click="emit('close')"><X :size="20" /></button>
    </header>

    <div class="px-3 pb-2">
      <div v-if="store.view.canComment" class="space-y-2">
        <template v-if="pending">
          <p class="text-xs text-muted">Commenting on: {{ anchorLabel(pending) }}</p>
          <CommentComposer label="New comment" submit="Add comment" :members="store.members" autofocus @submit="(b) => emit('submitNew', b)" @cancel="emit('cancelStart')" />
        </template>
        <template v-else-if="placing">
          <p class="text-sm" role="status">{{ placing === "point" ? "Tap the page where the comment goes." : placing === "element" ? "Tap the drawing, text or image to comment on." : placing === "rect" ? "Drag over the area to comment on." : "Drag over the part of the PDF." }}</p>
          <Btn variant="outline" @click="emit('cancelStart')">Cancel</Btn>
        </template>
        <template v-else>
          <p class="flex items-center gap-1.5 text-xs font-medium text-muted"><MessageSquarePlus :size="14" aria-hidden="true" />Add a comment on</p>
          <div class="grid grid-cols-2 gap-2" role="group" aria-label="Add a comment on">
            <Btn v-for="s in starts" :key="s.kind" variant="outline" class="!justify-start" :disabled="!can[s.kind]" @click="emit('start', s.kind)">
              <component :is="s.icon" :size="16" aria-hidden="true" />{{ s.label }}
            </Btn>
          </div>
        </template>
      </div>
      <p v-else class="text-xs text-muted" data-testid="no-comment-note">You can read comments but not write them.</p>
    </div>

    <div class="flex gap-1 px-3 pb-2" role="radiogroup" aria-label="Show">
      <button v-for="f in FILTERS" :key="f.id" type="button" role="radio" class="touch-target flex-1 rounded-xl border border-line px-1 text-xs font-medium epaper:border-2"
        :class="store.filter === f.id ? 'bg-brand-soft ring-1 ring-brand/40 epaper:bg-black epaper:text-white' : 'hover:bg-surface-2'" :aria-checked="store.filter === f.id" @click="store.filter = f.id">
        {{ f.label }}
      </button>
    </div>

    <div class="min-h-0 flex-1 space-y-4 overflow-y-auto px-3 pb-4">
      <p v-if="empty" class="py-6 text-center text-sm text-muted" data-testid="comments-empty">No comments here.</p>
      <ul v-if="sections.placed.length" class="space-y-2" aria-label="Comments on the page">
        <CommentThread v-for="t in sections.placed" :key="t.threadId" :thread="t" :members="store.members" :can-comment="store.view.canComment" :is-moderator="store.view.isModerator" :self-id="selfId"
          :muted="store.threadMuted(store.notebookId ?? '', t.threadId)" :expanded="openThread === t.threadId"
          @toggle="toggle(t.threadId)" @reply="(b) => store.reply(t.threadId, b)" @edit="(id, b) => store.edit(id, b)" @remove="(id) => store.remove(id)"
          @resolve="(r) => store.setResolved(t.threadId, r)" @locate="emit('locate', t.threadId)" @retry="(b) => store.retry(t.threadId, b)" @mute="(m) => store.muteThread(store.notebookId!, t.threadId, m)" />
      </ul>
      <section v-if="sections.orphaned.length" aria-labelledby="orphans-h" data-testid="orphaned-section">
        <h3 id="orphans-h" class="mb-1 text-xs font-semibold uppercase tracking-wide text-muted">Not on the page</h3>
        <ul class="space-y-2">
          <CommentThread v-for="t in sections.orphaned" :key="t.threadId" :thread="t" :members="store.members" :can-comment="store.view.canComment" :is-moderator="store.view.isModerator" :self-id="selfId"
            :muted="store.threadMuted(store.notebookId ?? '', t.threadId)" :expanded="openThread === t.threadId"
            @toggle="toggle(t.threadId)" @reply="(b) => store.reply(t.threadId, b)" @edit="(id, b) => store.edit(id, b)" @remove="(id) => store.remove(id)"
            @resolve="(r) => store.setResolved(t.threadId, r)" @locate="emit('locate', t.threadId)" @reattach="emit('reattach', t.threadId)" @retry="(b) => store.retry(t.threadId, b)" @mute="(m) => store.muteThread(store.notebookId!, t.threadId, m)" />
        </ul>
      </section>
    </div>
  </section>
</template>
