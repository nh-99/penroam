<script setup lang="ts">
// Version history of the open page, grouped into writing sessions. Restoring adds a new edit,
// so it syncs and the version it replaced stays in this list.
import { computed, onBeforeUnmount, onMounted, ref, shallowRef } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import RestoreDialog from "./RestoreDialog.vue";
import { elementBounds } from "../editor/renderer";
import type { Element } from "../generated/Element";
import type { HistoryStats } from "../generated/HistoryStats";
import type { Notebook } from "../generated/Notebook";
import type { Page } from "../generated/Page";
import { api, type HistoryRow } from "../platform/ipc";
import { authorsLine, fromAccount, groupSessions, historyApi, versionRef, type HistorySession, type VersionCompare, type VersionRef } from "../platform/history";
import { describeSharingError } from "../platform/sharing";
import { formatBytes } from "../platform/search";
import { useSync } from "../stores/sync";
import { useSharing } from "../stores/sharing";
import { useUi } from "../stores/ui";

const props = defineProps<{
  notebook: Notebook;
  pageId: string;
  render: (page: Page, width: number) => Promise<Uint8Array | null>;
}>();
const emit = defineEmits<{ close: []; restored: [pages: string[]] }>();
const ui = useUi();
const sync = useSync();
const isShared = computed(() => useSharing().byNotebook.has(props.notebook.id));
const points = ref<HistoryRow[]>([]);
const stats = ref<HistoryStats | null>(null);
const loading = ref(true);
// shallow: `choose` compares identity, and a deep ref would hand back a proxy
const chosen = shallowRef<HistorySession | null>(null);
const previewUrl = ref<string | null>(null);
const previewing = ref(false);
const confirm = ref<"clear" | null>(null);
/** The restore flow with a preview of what it would change (shared notebooks). */
const restoring = ref<{ position?: number; revision?: number; at?: number; elementIds?: string[] } | null>(null);
/** The chosen version's page, for picking elements and for the highlights. */
const versionPage = shallowRef<Page | null>(null);
const picking = ref(false);
const picked = ref<Set<string>>(new Set());
/** Comparing the chosen version with another: `head` is the current page. */
const compareWith = ref<"" | "head" | string>("");
const comparing = ref(false);
const diff = shallowRef<VersionCompare | null>(null);
const fromPage = shallowRef<Page | null>(null);
const toPage = shallowRef<Page | null>(null);
const compareError = ref<string | null>(null);

const sessions = computed(() => groupSessions(points.value).reverse());
const when = (ms: number) => new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });

function changes(s: HistorySession): string {
  if (s === sessions.value[0]) return "Current version";
  const parts = [];
  if (s.added) parts.push(`${s.added} added`);
  if (s.removed) parts.push(`${s.removed} removed`);
  const what = parts.join(", ") || "starting point";
  return fromAccount(s) ? `${what}, from your account` : what;
}

const clearMessage = computed(() =>
  sync.configured
    ? "Old versions recorded on this device are removed, including anything you erased. Your encrypted account keeps everything already synced, so versions from your account stay available. Your current pages aren't affected."
    : "All old versions of every page in this notebook are permanently removed from this device, including anything you erased. Your current pages aren't affected.",
);

function clearPreview() {
  if (previewUrl.value) URL.revokeObjectURL(previewUrl.value);
  previewUrl.value = null;
}

async function load() {
  loading.value = true;
  try {
    points.value = await api.pageHistory(props.notebook.id, props.pageId);
    stats.value = await api.historyStats(props.notebook.id);
  } catch (e: any) {
    ui.error(`Could not read the history: ${e?.message ?? e}`);
  } finally {
    loading.value = false;
  }
}
onMounted(load);
onBeforeUnmount(clearPreview);

async function choose(s: HistorySession) {
  chosen.value = s;
  clearPreview();
  previewing.value = true;
  try {
    const page = await api.pageVersion(props.notebook.id, props.pageId, s.revision, s.position);
    versionPage.value = page;
    picking.value = false;
    picked.value = new Set();
    resetCompare();
    const png = await props.render(page, 520);
    if (chosen.value === s && png) previewUrl.value = URL.createObjectURL(new Blob([png as BlobPart], { type: "image/png" }));
  } catch (e: any) {
    ui.error(`Could not show that version: ${e?.message ?? e}`);
  } finally {
    previewing.value = false;
  }
}

const refOf = (s: HistorySession) => versionRef(s);
function resetCompare() {
  compareWith.value = "";
  diff.value = null;
  fromPage.value = toPage.value = null;
  compareError.value = null;
}

/** Compares the chosen version with another (or the current page): what was added, removed, changed. */
async function compare() {
  const s = chosen.value;
  const a = s && refOf(s);
  if (!s || !a || !compareWith.value) return;
  const other = compareWith.value === "head" ? null : sessions.value[Number(compareWith.value)];
  const b: VersionRef | null = other ? refOf(other) : "head";
  if (!b) return;
  comparing.value = true;
  compareError.value = null;
  try {
    const [d, from, to] = await Promise.all([
      historyApi.compare(props.notebook.id, props.pageId, a, b),
      Promise.resolve(versionPage.value),
      other ? api.pageVersion(props.notebook.id, props.pageId, other.revision, other.position) : api.loadPage(props.notebook.id, props.pageId),
    ]);
    diff.value = d;
    fromPage.value = from;
    toPage.value = to;
  } catch (e) {
    compareError.value = describeSharingError(e);
  } finally {
    comparing.value = false;
  }
}

const elementLabel = (e: Element | undefined) => (!e ? "An element" : e.type === "text" ? `Text: “${e.text.slice(0, 40)}”` : e.type === "ink" ? "A stroke" : "An image");
const findIn = (page: Page | null, id: string) => page?.elements.find((e) => e.id === id);

/** Boxes over the preview, in percentages of the page, for picking elements or showing a comparison. */
interface Box { id: string; style: Record<string, string>; label: string; mark: string; cls: string }
const boxFor = (e: Element, page: Page): Record<string, string> => {
  const [x0, y0, x1, y1] = elementBounds(e);
  const { width, height } = page.size;
  return { left: `${(x0 / width) * 100}%`, top: `${(y0 / height) * 100}%`, width: `${((x1 - x0) / width) * 100}%`, height: `${((y1 - y0) / height) * 100}%` };
};
const pickBoxes = computed<Box[]>(() => {
  const p = versionPage.value;
  if (!picking.value || !p) return [];
  return p.elements.map((e) => ({ id: e.id, style: boxFor(e, p), label: elementLabel(e), mark: picked.value.has(e.id) ? "✓" : "", cls: picked.value.has(e.id) ? "border-solid bg-black/10" : "border-dashed" }));
});
const diffBoxes = computed<Box[]>(() => {
  const d = diff.value;
  const to = toPage.value, from = fromPage.value;
  if (!d || !to || !from) return [];
  const out: Box[] = [];
  for (const id of d.added) { const e = findIn(to, id); if (e) out.push({ id, style: boxFor(e, to), label: `Added: ${elementLabel(e)}`, mark: "+", cls: "border-solid" }); }
  for (const id of d.removed) { const e = findIn(from, id); if (e) out.push({ id, style: boxFor(e, from), label: `Removed: ${elementLabel(e)}`, mark: "−", cls: "border-dashed" }); }
  for (const id of d.changed) { const e = findIn(to, id); if (e) out.push({ id, style: boxFor(e, to), label: `Changed: ${elementLabel(e)}`, mark: "~", cls: "border-dotted" }); }
  return out;
});
const togglePick = (id: string) => {
  const n = new Set(picked.value);
  if (n.has(id)) n.delete(id);
  else n.add(id);
  picked.value = n;
};

/** Starts the restore flow for the page, a few elements, or the notebook. */
function startRestore(kind: "page" | "elements" | "notebook") {
  const s = chosen.value;
  if (!s) return;
  if (kind === "notebook") restoring.value = { at: s.at };
  else if (!versionRef(s)) ui.error("This version can't be restored.");
  else restoring.value = { position: s.position ?? undefined, revision: s.position === null ? (s.revision ?? undefined) : undefined, elementIds: kind === "elements" ? [...picked.value] : undefined };
}
function restored() {
  const notebookLevel = restoring.value?.at !== undefined;
  restoring.value = null;
  ui.success(notebookLevel ? "Restored. Newer pages were kept." : "Restored as a new edit. You can undo it from the history.");
  emit("restored", notebookLevel ? [...props.notebook.pageOrder] : [props.pageId]);
}

async function clear() {
  confirm.value = null;
  try {
    await api.clearHistory(props.notebook.id);
    chosen.value = null;
    clearPreview();
    ui.success("History deleted.");
    await load();
  } catch (e: any) {
    ui.error(`Could not delete the history: ${e?.message ?? e}`);
  }
}
</script>

<template>
  <Modal title="Version history" wide @close="emit('close')">
    <p class="mb-3 text-xs leading-relaxed text-muted">
      {{ sync.configured ? "Old versions of this page, from this device and your encrypted account." : "Old versions of this page, kept on this device." }}
      Restoring shows you what would change first, then adds one new edit, so nothing you have now is lost. Older versions thin out over time, and versions over a year old aren't offered.
    </p>
    <p v-if="loading" class="text-sm text-muted">Loading…</p>
    <p v-else-if="!sessions.length" class="text-sm">No earlier versions yet. They start with your next edit to this page.</p>
    <div v-else class="grid gap-3 sm:grid-cols-2">
      <ul class="max-h-[50dvh] divide-y divide-line overflow-y-auto rounded-xl border border-line" aria-label="Versions">
        <li v-for="s in sessions" :key="`${s.revision}-${s.position}`">
          <button class="w-full px-3 py-2 text-left text-sm hover:bg-surface-2" :class="{ 'bg-surface-2': chosen === s }" :aria-pressed="chosen === s" @click="choose(s)">
            <span class="block">{{ when(s.at) }}</span>
            <span class="block text-xs text-muted">{{ changes(s) }}</span>
            <span v-if="authorsLine(s.authors)" class="block text-xs" data-testid="version-authors">By {{ authorsLine(s.authors) }}</span>
          </button>
        </li>
      </ul>
      <div class="min-h-48 space-y-2 rounded-xl border border-line p-2">
        <p v-if="!chosen" class="py-8 text-center text-sm text-muted">Pick a version to see it.</p>
        <p v-else-if="previewing" class="py-8 text-center text-sm text-muted">Rebuilding that version…</p>
        <template v-else-if="previewUrl">
          <!-- a picture of the old page, not an editor: nothing here takes pen or touch input to draw -->
          <div class="relative mx-auto inline-block max-w-full" data-testid="version-preview">
            <img :src="previewUrl" alt="Preview of the chosen version" class="max-h-[40dvh] max-w-full rounded border border-line bg-white" draggable="false" />
            <button v-for="b in pickBoxes" :key="b.id" type="button" class="absolute flex items-start justify-start border-2 border-black p-0 text-[10px] font-bold leading-none" :class="b.cls" :style="b.style" :aria-label="b.label" :aria-pressed="picked.has(b.id)" @click="togglePick(b.id)">{{ b.mark }}</button>
            <div v-for="b in diffBoxes" :key="`d${b.id}`" class="pointer-events-none absolute border-2 border-black text-[11px] font-bold leading-none" :class="b.cls" :style="b.style" :aria-label="b.label" role="img"><span aria-hidden="true">{{ b.mark }}</span></div>
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <label v-if="refOf(chosen)" class="text-xs">Compare with
              <select v-model="compareWith" class="ml-1 h-11 rounded-xl border border-line bg-surface px-2 text-sm epaper:border-2" @change="compare">
                <option value="">Choose…</option>
                <option value="head">The current page</option>
                <option v-for="(o, i) in sessions" :key="i" :value="String(i)" :disabled="o === chosen || !refOf(o)">{{ when(o.at) }}</option>
              </select>
            </label>
            <Btn variant="outline" :aria-pressed="picking" @click="picking = !picking; picked = new Set()">{{ picking ? "Stop picking" : "Pick elements…" }}</Btn>
            <Btn v-if="picking" variant="primary" :disabled="!picked.size" @click="startRestore('elements')">Restore {{ picked.size || "" }} chosen…</Btn>
          </div>
          <p v-if="comparing" class="text-xs text-muted" role="status">Comparing…</p>
          <p v-if="compareError" class="text-xs text-danger" role="alert">{{ compareError }}</p>
          <div v-if="diff" class="space-y-2 text-sm" data-testid="compare-result">
            <p class="text-xs">
              <span data-testid="count-added">+ {{ diff.added.length }} added</span> ·
              <span data-testid="count-removed">− {{ diff.removed.length }} removed</span> ·
              <span data-testid="count-changed">~ {{ diff.changed.length }} changed</span>
              <template v-if="diff.templateChanged"> · the paper changed</template>
            </p>
            <p v-if="!diff.added.length && !diff.removed.length && !diff.changed.length && !diff.templateChanged" class="text-xs text-muted">No differences.</p>
            <div v-for="t in diff.text" :key="t.elementId" class="rounded-lg border border-line p-2" data-testid="text-diff">
              <p class="text-xs font-medium">Text changed</p>
              <p class="whitespace-pre-wrap break-words text-sm">
                <template v-for="(part, i) in t.diff" :key="i">
                  <ins v-if="part.op === 'insert'" class="font-medium underline decoration-2">{{ part.text }}</ins>
                  <del v-else-if="part.op === 'delete'" class="line-through opacity-70">{{ part.text }}</del>
                  <span v-else>{{ part.text }}</span>
                </template>
              </p>
            </div>
          </div>
        </template>
      </div>
    </div>
    <p v-if="stats?.bytes" class="mt-3 text-xs text-muted">This notebook's history uses {{ formatBytes(stats.bytes) }} on this device.</p>
    <template #actions>
      <Btn v-if="stats?.bytes" variant="ghost" class="mr-auto" @click="confirm = 'clear'">Delete history…</Btn>
      <Btn :disabled="!chosen" @click="startRestore('notebook')">Restore whole notebook…</Btn>
      <Btn variant="primary" :disabled="!chosen" @click="startRestore('page')">Restore this page…</Btn>
    </template>
    <RestoreDialog v-if="restoring && chosen" :notebook-id="notebook.id" :shared="isShared" :page-order="notebook.pageOrder" :page-id="restoring.at === undefined ? pageId : undefined" :position="restoring.position" :revision="restoring.revision" :at="restoring.at" :element-ids="restoring.elementIds" :when="when(chosen.at)"
      @close="restoring = null" @restored="restored" />
    <ConfirmDialog v-if="confirm === 'clear'" danger title="Delete this notebook's history?" :message="clearMessage" confirm="Delete history" @close="confirm = null" @confirm="clear" />
  </Modal>
</template>
