<script setup lang="ts">
// Library-wide search: typed text, handwriting (where this device can recognize it), PDF text
// and notebook titles, from the local index. Choosing a result opens its page and outlines
// the matching ink; the note itself is never changed.
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { Search, Type, PenLine, FileText, BookOpen, LoaderCircle, Pencil } from "@lucide/vue";
import Modal from "./ui/Modal.vue";
import PromptDialog from "./PromptDialog.vue";
import { useSearch } from "../stores/search";
import { useUi } from "../stores/ui";
import { describeState, searchApi, type SearchHit, type SearchKind } from "../platform/search";

const emit = defineEmits<{ close: []; open: [hit: SearchHit] }>();
const search = useSearch();
const ui = useUi();
const hits = ref<SearchHit[]>([]);
const busy = ref(false);
const error = ref<string | null>(null);
const searched = ref("");
const correcting = ref<SearchHit | null>(null);
let timer: ReturnType<typeof setTimeout> | null = null;
let seq = 0;

const icons: Record<SearchKind, any> = { title: BookOpen, text: Type, ink: PenLine, pdf: FileText };
const kindLabel: Record<SearchKind, string> = { title: "Notebook title", text: "Typed text", ink: "Handwriting", pdf: "PDF text" };

async function run() {
  const q = search.query.trim();
  const my = ++seq;
  if (!q) {
    hits.value = [];
    searched.value = "";
    error.value = null;
    // an in-flight request is now stale and will skip its own reset, so clear the spinner here
    busy.value = false;
    return;
  }
  busy.value = true;
  try {
    const r = await searchApi.query(q);
    if (my !== seq) return;
    hits.value = r;
    searched.value = q;
    error.value = null;
  } catch (e: any) {
    if (my === seq) error.value = e?.message ?? String(e);
  } finally {
    if (my === seq) busy.value = false;
  }
}

watch(
  () => search.query,
  () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => void run(), 180);
  },
  { immediate: true },
);
onBeforeUnmount(() => timer && clearTimeout(timer));

/** Results grouped by notebook, in rank order of each notebook's best hit. */
const groups = computed(() => {
  const m = new Map<string, { id: string; title: string; hits: SearchHit[] }>();
  for (const h of hits.value) {
    let g = m.get(h.notebookId);
    if (!g) m.set(h.notebookId, (g = { id: h.notebookId, title: h.notebookTitle || "Untitled", hits: [] }));
    g.hits.push(h);
  }
  return [...m.values()];
});

const st = computed(() => search.status);
const indexingNote = computed(() => {
  const s = st.value;
  if (!s || s.state === "idle" || s.state === "off") return null;
  return describeState(s);
});
const handwritingNote = computed(() => {
  const s = st.value;
  if (!s || s.state === "off") return null;
  if (!s.recognizer) return "Handwriting isn't searchable on this device yet: it has no handwriting recognizer. Typed text, titles and PDF text are.";
  if (!s.settings.handwriting) return "Handwriting recognition is off (Settings → Search).";
  return null;
});

function choose(h: SearchHit) {
  emit("open", h);
}

async function saveCorrection(text: string) {
  const h = correcting.value;
  correcting.value = null;
  if (!h?.pageId || !h.regionKey) return;
  try {
    await searchApi.correct(h.notebookId, h.pageId, h.regionKey, text);
    ui.success("Saved. Search uses your reading for this handwriting from now on.");
    await run();
  } catch (e: any) {
    ui.error(`Could not save the correction: ${e?.message ?? e}`);
  }
}

const snippetText = (h: SearchHit) => h.snippet.map((s) => s.text).join("").replace(/^…|…$/g, "");
</script>

<template>
  <Modal title="Search notebooks" wide @close="emit('close')">
    <form role="search" @submit.prevent="run()">
      <label class="relative block">
        <span class="sr-only">Search all notebooks</span>
        <Search :size="18" class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-muted" />
        <input v-model="search.query" autofocus type="search" enterkeyhint="search" placeholder="Words, “a phrase”, a title…" maxlength="200"
          class="h-11 w-full rounded-xl border border-line bg-surface pl-10 pr-10 text-base outline-none focus:border-brand epaper:border-2" />
        <LoaderCircle v-if="busy" :size="18" class="absolute right-3 top-1/2 -translate-y-1/2 animate-spin text-muted" />
      </label>
    </form>
    <p v-if="indexingNote" class="mt-2 text-xs text-muted" aria-live="polite">{{ indexingNote }}. Results may be incomplete.</p>
    <p v-if="handwritingNote" class="mt-2 text-xs text-muted">{{ handwritingNote }}</p>

    <div class="mt-4 min-h-24" aria-live="polite">
      <p v-if="error" class="rounded-xl border border-line p-3 text-sm text-danger" role="alert">{{ error }}</p>
      <p v-else-if="searched && !hits.length && !busy" class="py-8 text-center text-sm text-muted">No matches for “{{ searched }}”.</p>
      <p v-else-if="!search.query.trim()" class="py-8 text-center text-sm text-muted">Search runs on this device; nothing you type is sent anywhere.</p>
      <div v-for="g in groups" :key="g.id" class="mb-4">
        <h3 class="mb-1 truncate px-1 text-xs font-semibold uppercase tracking-wide text-muted">{{ g.title }}</h3>
        <ul class="divide-y divide-line overflow-hidden rounded-xl border border-line">
          <li v-for="(h, i) in g.hits" :key="i" class="flex items-start gap-2 bg-surface hover:bg-surface-2">
            <button type="button" class="flex min-w-0 flex-1 items-start gap-3 px-3 py-2.5 text-left" @click="choose(h)">
              <component :is="icons[h.kind]" :size="18" class="mt-0.5 shrink-0 text-muted" :aria-label="kindLabel[h.kind]" />
              <span class="min-w-0 flex-1">
                <span class="block text-sm leading-snug">
                  <template v-for="(s, j) in h.snippet" :key="j"><mark v-if="s.hit" class="rounded bg-warn/30 px-0.5 text-ink epaper:bg-transparent epaper:font-bold epaper:underline">{{ s.text }}</mark><template v-else>{{ s.text }}</template></template>
                </span>
                <span class="mt-0.5 block text-xs text-muted">
                  {{ kindLabel[h.kind] }}<template v-if="h.pageIndex !== null && h.kind !== 'title'"> · page {{ h.pageIndex + 1 }}</template><template v-if="h.fuzzy"> · similar spelling</template>
                </span>
              </span>
            </button>
            <button v-if="h.kind === 'ink' && h.regionKey" type="button" class="touch-target mr-1 mt-1 inline-flex shrink-0 items-center justify-center rounded-lg text-muted hover:bg-surface"
              :aria-label="`Correct the recognized text “${snippetText(h)}”`" title="Correct the recognized text" @click="correcting = h">
              <Pencil :size="16" />
            </button>
          </li>
        </ul>
      </div>
    </div>
  </Modal>
  <PromptDialog v-if="correcting" title="Correct recognized text" label="What does this handwriting say?" :value="snippetText(correcting)" confirm="Save"
    @close="correcting = null" @submit="saveCorrection" />
</template>
