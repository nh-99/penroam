<script setup lang="ts">
// The notebook's activity: what changed, who did it and when, newest first, with filters for page,
// person and kind. Summaries come from the kind and count of each change only, and authorship from
// the server's record: changes from before sharing say "Before sharing", unknown stays "Unknown",
// and edits sent late say "Edited offline". Entries are a keyboard list (arrows, Home, End; Enter
// goes to the page).
import { computed, nextTick, onMounted, ref } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { cleanName } from "../platform/display";
import AuthorBadge from "./AuthorBadge.vue";
import { ACTIVITY_KINDS, activityPeople, activityText, filterActivity, historyApi, personKey, type ActivityEntry, type ActivityKind } from "../platform/history";
import { describeSharingError } from "../platform/sharing";

const props = defineProps<{ notebookId: string; pageNumbers: Record<string, number> }>();
const emit = defineEmits<{ close: []; goto: [pageId: string] }>();
const entries = ref<ActivityEntry[]>([]);
const cursor = ref<string | undefined>();
const loading = ref(true);
const error = ref<string | null>(null);
const page = ref("");
const person = ref("");
const kind = ref<ActivityKind | "">("");
const list = ref<HTMLElement | null>(null);

const shown = computed(() => filterActivity(entries.value, { pageId: page.value, person: person.value, kind: kind.value }));
const people = computed(() => activityPeople(entries.value));
const pages = computed(() => Object.entries(props.pageNumbers).sort((a, b) => a[1] - b[1]));
const when = (ms: number) => new Date(ms).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });

async function load(more = false) {
  loading.value = true;
  try {
    const r = await historyApi.activity(props.notebookId, more ? cursor.value : undefined);
    entries.value = more ? [...entries.value, ...r.entries] : r.entries;
    cursor.value = r.nextCursor;
    error.value = null;
  } catch (e) {
    error.value = describeSharingError(e);
  } finally {
    loading.value = false;
  }
}
onMounted(() => load());

function key(e: KeyboardEvent) {
  const items = Array.from(list.value?.querySelectorAll<HTMLElement>("[data-entry]") ?? []);
  const i = items.indexOf(document.activeElement as HTMLElement);
  const to = e.key === "ArrowDown" ? i + 1 : e.key === "ArrowUp" ? i - 1 : e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : null;
  if (to === null || !items.length) return;
  e.preventDefault();
  void nextTick(() => items[Math.max(0, Math.min(items.length - 1, to))].focus());
}
</script>

<template>
  <Modal title="Activity" wide @close="emit('close')">
    <div class="mb-3 grid gap-2 sm:grid-cols-3">
      <label class="text-xs">Page
        <select v-model="page" class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-2 text-sm epaper:border-2">
          <option value="">All pages</option>
          <option v-for="[id, n] in pages" :key="id" :value="id">Page {{ n }}</option>
        </select>
      </label>
      <label class="text-xs">Person
        <select v-model="person" class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-2 text-sm epaper:border-2">
          <option value="">Everyone</option>
          <option v-for="a in people" :key="personKey(a)" :value="personKey(a)">{{ a.kind === "member" ? cleanName(a.display) || "Collaborator" : a.kind === "former" ? "Former member" : a.kind === "erased" ? "Former collaborator" : a.kind === "beforeSharing" ? "Before sharing" : a.kind === "unverified" ? "Unverified author" : "Unknown" }}</option>
        </select>
      </label>
      <label class="text-xs">Kind
        <select v-model="kind" class="mt-1 h-11 w-full rounded-xl border border-line bg-surface px-2 text-sm epaper:border-2">
          <option value="">All changes</option>
          <option v-for="k in ACTIVITY_KINDS" :key="k.id" :value="k.id">{{ k.label }}</option>
        </select>
      </label>
    </div>
    <p v-if="error" class="text-sm text-danger" role="alert">Could not load the activity: {{ error }}</p>
    <p v-else-if="loading && !entries.length" class="text-sm text-muted" role="status">Loading…</p>
    <p v-else-if="!shown.length" class="py-6 text-center text-sm text-muted" data-testid="activity-empty">Nothing matches.</p>
    <ol v-else ref="list" class="max-h-[55dvh] divide-y divide-line overflow-y-auto rounded-xl border border-line" aria-label="Changes, newest first" @keydown="key">
      <li v-for="e in shown" :key="e.entryId">
        <button type="button" data-entry class="flex w-full min-h-11 flex-col items-start gap-0.5 px-3 py-2 text-left hover:bg-surface-2 focus-visible:bg-surface-2" :disabled="!e.pageId" @click="e.pageId && emit('goto', e.pageId)">
          <span class="text-sm">{{ activityText(e) }}<span v-if="e.pageId && pageNumbers[e.pageId]"> · page {{ pageNumbers[e.pageId] }}</span></span>
          <span class="flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted">
            <AuthorBadge :author="e.author" />
            <time>{{ when(e.acceptedAt ?? e.at) }}</time>
            <span v-if="e.editedOffline" class="rounded bg-surface-2 px-1.5 py-0.5 font-medium text-ink epaper:border" data-testid="offline">Edited offline<template v-if="e.acceptedAt"> · written {{ when(e.at) }}</template></span>
          </span>
        </button>
      </li>
    </ol>
    <template #actions>
      <Btn v-if="cursor" variant="outline" :disabled="loading" @click="load(true)">Show older changes</Btn>
      <Btn @click="emit('close')">Close</Btn>
    </template>
  </Modal>
</template>
