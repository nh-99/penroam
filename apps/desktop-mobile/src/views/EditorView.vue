<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { ArrowLeft, ChevronLeft, ChevronRight, PanelLeft, Plus, LayoutTemplate, FileUp, Download, FileDown, Printer, Settings, LoaderCircle, TriangleAlert, Search } from "@lucide/vue";
import { useLibrary } from "../stores/library";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import { useEditorHost } from "../composables/useEditorHost";
import Toolbar from "../components/Toolbar.vue";
import PageSidebar from "../components/PageSidebar.vue";
import SaveIndicator from "../components/SaveIndicator.vue";
import SyncIndicator from "../components/SyncIndicator.vue";
import ConflictsDialog from "../components/ConflictsDialog.vue";
import { useSync } from "../stores/sync";
import SettingsDialog from "../components/SettingsDialog.vue";
import CloudBanner from "../components/CloudBanner.vue";
import SearchPanel from "../components/SearchPanel.vue";
import { useSearch } from "../stores/search";
import { highlightRects, type SearchHit } from "../platform/search";
import PromptDialog from "../components/PromptDialog.vue";
import TemplatePreview from "../components/TemplatePreview.vue";
import IconBtn from "../components/ui/IconBtn.vue";
import Menu from "../components/ui/Menu.vue";
import MenuItem from "../components/ui/MenuItem.vue";
import Btn from "../components/ui/Btn.vue";
import type { Tool } from "../editor/engine";
import type { TemplateKind } from "../generated/TemplateKind";
import { modKey } from "../platform/device";

const lib = useLibrary();
const settings = useSettings();
const ui = useUi();
const viewport = ref<HTMLElement | null>(null);
const narrow = ref(window.innerWidth < 768);
const drawerOpen = ref(false);
const showSettings = ref(false);
const showConflicts = ref(false);
const sync = useSync();
function openSync() {
  if ((sync.status?.conflicts ?? 0) > 0) showConflicts.value = true;
  else showSettings.value = true;
}
const renaming = ref(false);
const blocked = computed(() => narrow.value && drawerOpen.value);
const host = useEditorHost(viewport, blocked);
const st = host.state;
const mod = modKey();

const nb = computed(() => lib.current!);
const idx = computed(() => lib.pageIndex);
const sidebarVisible = computed(() => (narrow.value ? drawerOpen.value : settings.s.sidebarOpen));

function onResize() {
  narrow.value = window.innerWidth < 768;
}
onMounted(() => window.addEventListener("resize", onResize));
onBeforeUnmount(() => window.removeEventListener("resize", onResize));

function toggleSidebar() {
  if (narrow.value) drawerOpen.value = !drawerOpen.value;
  else settings.s.sidebarOpen = !settings.s.sidebarOpen;
}

function go(i: number) {
  const id = nb.value.pageOrder[i];
  if (id) lib.currentPageId = id;
}
function selectPage(id: string) {
  lib.currentPageId = id;
  if (narrow.value) drawerOpen.value = false;
}
async function addPage(after?: number) {
  const id = await lib.addPage(after ?? idx.value);
  if (id) lib.currentPageId = id;
}
async function back() {
  try {
    await host.flush();
  } catch (e) {
    ui.error(`Changes are not saved yet: ${(e as Error).message}`, { label: "Retry", run: () => void host.retry() });
    return;
  }
  lib.close();
}
async function importPdf() {
  const first = await lib.importPdf();
  if (first) lib.currentPageId = first;
}
const search = useSearch();
async function openHit(h: SearchHit) {
  search.panelOpen = false;
  search.pendingHighlight = h.pageId ? { pageId: h.pageId, rects: highlightRects(h) } : null;
  if (h.notebookId === nb.value.id) {
    if (h.pageId && nb.value.pageOrder.includes(h.pageId)) lib.currentPageId = h.pageId;
    return;
  }
  // another notebook: same as leaving through the back button, then open it there
  try {
    await host.flush();
  } catch (e) {
    ui.error(`Changes are not saved yet: ${(e as Error).message}`, { label: "Retry", run: () => void host.retry() });
    return;
  }
  await lib.open(h.notebookId, h.pageId ?? undefined);
}
// outline a search match once its page is on screen
watch(
  [() => search.pendingHighlight, () => host.loading.value, () => host.editor.value],
  () => {
    const p = search.pendingHighlight;
    const ed = host.editor.value;
    if (!p || !ed || host.loading.value || host.session()?.page.id !== p.pageId) return;
    search.pendingHighlight = null;
    ed.showMatches(p.rects);
  },
  { immediate: true },
);

const exportNb = () => lib.exportNotebook(nb.value.id, nb.value.title, host.flush);
const exportPdf = () => lib.exportPdf(nb.value.id, nb.value.title, host.flush);
const print = () => lib.printNotebook(nb.value.id, lib.currentPageId, host.flush);

const templates: { kind: TemplateKind; label: string }[] = [
  { kind: "blank", label: "Blank" },
  { kind: "ruled", label: "Ruled" },
  { kind: "grid", label: "Grid" },
  { kind: "dotted", label: "Dotted" },
];
function setTemplate(kind: TemplateKind) {
  host.editor.value?.setTemplate({ kind, spacing: kind === "dotted" ? 20 : kind === "grid" ? 18 : 24 });
}

function setTool(t: Tool) {
  host.editor.value?.setTool(t);
}

function onKey(e: KeyboardEvent) {
  const t = e.target as HTMLElement;
  if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
  if (ui.overlays > 0) return;
  const ed = host.editor.value;
  if (!ed) return;
  const m = e.ctrlKey || e.metaKey;
  const k = e.key.toLowerCase();
  if (m && k === "z") {
    e.preventDefault();
    if (e.shiftKey) ed.redo();
    else ed.undo();
  } else if (m && k === "y") {
    e.preventDefault();
    ed.redo();
  } else if (m && (k === "f" || k === "k")) {
    e.preventDefault();
    search.panelOpen = true;
  } else if (m && k === "s") {
    e.preventDefault();
    host.flush().then(() => ui.success("All changes saved to disk."), (err) => ui.error(`Save failed: ${err.message}`));
  } else if (m && !e.shiftKey && k === "p") {
    e.preventDefault();
    void print();
  } else if (m && e.shiftKey && k === "n") {
    e.preventDefault();
    void addPage();
  } else if (m && (k === "=" || k === "+")) {
    e.preventDefault();
    ed.zoomBy(1.25);
  } else if (m && k === "-") {
    e.preventDefault();
    ed.zoomBy(0.8);
  } else if (m && k === "0") {
    e.preventDefault();
    ed.fit("width");
  } else if (m && k === "a") {
    e.preventDefault();
    ed.setTool("select");
    ed.selectAll();
  } else if (!m && !e.altKey) {
    const tools: Record<string, Tool> = { p: "pen", m: "highlighter", e: "eraser", v: "select", l: "lasso", t: "text", h: "hand" };
    if (tools[k]) ed.setTool(tools[k]);
    else if (k === "delete" || k === "backspace") ed.deleteSelection();
    else if (k === "escape") ed.clearSelection();
    else if (k === "enter" && st.selectionHasText) {
      e.preventDefault();
      ed.editSelectedText();
    } else if (k === "pagedown" || (k === "arrowright" && st.selectionCount === 0)) go(idx.value + 1);
    else if (k === "pageup" || (k === "arrowleft" && st.selectionCount === 0)) go(idx.value - 1);
  }
}
onMounted(() => window.addEventListener("keydown", onKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onKey));

watch(narrow, (n) => {
  if (!n) drawerOpen.value = false;
});
</script>

<template>
  <div class="flex h-full flex-col bg-app safe-x">
    <!-- top strip -->
    <header class="safe-top relative z-30 border-b border-line bg-surface">
      <div class="flex items-center gap-1 px-1.5 py-1">
        <IconBtn label="Back to notebooks" @click="back"><ArrowLeft :size="20" /></IconBtn>
        <IconBtn :label="sidebarVisible ? 'Hide pages' : 'Show pages'" :active="sidebarVisible" @click="toggleSidebar"><PanelLeft :size="20" /></IconBtn>
        <button class="min-w-0 truncate rounded-lg px-2 py-1 text-left text-sm font-semibold hover:bg-surface-2" :title="`Rename “${nb.title}”`" @click="renaming = true">
          {{ nb.title }}
        </button>
        <div class="ml-1 flex items-center">
          <IconBtn label="Previous page" shortcut="PgUp" :disabled="idx <= 0" @click="go(idx - 1)"><ChevronLeft :size="20" /></IconBtn>
          <span class="min-w-12 text-center text-xs tabular-nums text-muted" aria-live="polite">{{ idx + 1 }} / {{ nb.pageOrder.length }}</span>
          <IconBtn label="Next page" shortcut="PgDn" :disabled="idx >= nb.pageOrder.length - 1" @click="go(idx + 1)"><ChevronRight :size="20" /></IconBtn>
        </div>
        <div class="ml-auto flex items-center gap-0.5">
          <SaveIndicator class="mr-1.5" :state="st.saveState" :error="st.saveError" @retry="host.retry()" />
          <SyncIndicator class="mr-1" @open="openSync" />
          <IconBtn label="Search notebooks" :shortcut="`${mod}+F`" @click="search.panelOpen = true"><Search :size="20" /></IconBtn>
          <IconBtn label="Add page after this one" :shortcut="`${mod}+Shift+N`" @click="addPage()"><Plus :size="20" /></IconBtn>
          <Menu label="Page template">
            <template #trigger="{ toggle, open }">
              <IconBtn label="Page template" :aria-expanded="open" aria-haspopup="menu" @click="toggle"><LayoutTemplate :size="20" /></IconBtn>
            </template>
            <MenuItem v-for="t in templates" :key="t.kind" @click="setTemplate(t.kind)">
              <TemplatePreview :kind="t.kind" class="!h-8 !w-6" />{{ t.label }}
            </MenuItem>
          </Menu>
          <IconBtn label="Import PDF as pages" @click="importPdf"><FileUp :size="20" /></IconBtn>
          <Menu label="Export and print">
            <template #trigger="{ toggle, open }">
              <IconBtn label="Export and print" :aria-expanded="open" aria-haspopup="menu" @click="toggle"><Download :size="20" /></IconBtn>
            </template>
            <MenuItem @click="exportNb"><Download :size="16" />Notebook (.penroam, editable)…</MenuItem>
            <MenuItem @click="exportPdf"><FileDown :size="16" />Annotated PDF…</MenuItem>
            <MenuItem @click="print"><Printer :size="16" />Print…</MenuItem>
          </Menu>
          <IconBtn class="hidden sm:inline-flex" label="Settings" @click="showSettings = true"><Settings :size="20" /></IconBtn>
        </div>
      </div>
      <Toolbar v-if="!narrow" class="border-t border-line" :state="st" @tool="setTool" @undo="host.editor.value?.undo()" @redo="host.editor.value?.redo()"
        @zoom-in="host.editor.value?.zoomBy(1.25)" @zoom-out="host.editor.value?.zoomBy(0.8)" @fit="host.editor.value?.fit('width')"
        @delete-selection="host.editor.value?.deleteSelection()" @edit-text="host.editor.value?.editSelectedText()" @restyle="(s) => host.editor.value?.restyleSelection(s)" @free-placement="(f) => host.editor.value?.setSelectionFreePlacement(f)" />
      <CloudBanner compact class="border-t border-b-0" />
    </header>

    <div class="relative flex min-h-0 flex-1">
      <!-- sidebar (wide) / drawer (narrow) -->
      <aside v-if="!narrow && settings.s.sidebarOpen" class="w-44 shrink-0 border-r border-line bg-surface">
        <PageSidebar :notebook="nb" :current="lib.currentPageId" :thumbs="lib.thumbs" @select="selectPage" @add="addPage" @delete="lib.deletePage" @reorder="lib.reorder" @restore="lib.restorePage" />
      </aside>
      <template v-if="narrow && drawerOpen">
        <div class="absolute inset-0 z-20 bg-black/25 epaper:bg-white/60" @click="drawerOpen = false" />
        <aside class="absolute inset-y-0 left-0 z-30 w-56 border-r border-line bg-surface shadow-xl epaper:border-r-2">
          <PageSidebar :notebook="nb" :current="lib.currentPageId" :thumbs="lib.thumbs" @select="selectPage" @add="addPage" @delete="lib.deletePage" @reorder="lib.reorder" @restore="lib.restorePage" />
        </aside>
      </template>

      <!-- canvas -->
      <main class="relative min-w-0 flex-1 bg-desk">
        <div ref="viewport" class="absolute inset-0" aria-label="Page canvas" role="application" />
        <div v-if="host.loading.value" class="pointer-events-none absolute inset-0 flex items-center justify-center"><LoaderCircle :size="28" class="animate-spin text-muted" /></div>
        <div v-if="host.pageError.value" class="absolute inset-0 flex items-center justify-center p-6" role="alert">
          <div class="max-w-sm rounded-2xl border border-line bg-surface p-5 text-center">
            <TriangleAlert :size="28" class="mx-auto mb-2 text-danger" />
            <p class="text-sm font-medium">This page could not be opened</p>
            <p class="mt-1 text-xs text-muted">{{ host.pageError.value }}</p>
            <Btn class="mt-3" variant="outline" @click="host.openPage(lib.currentPageId)">Try again</Btn>
          </div>
        </div>
        <div v-if="nb.pageOrder.length === 0" class="absolute inset-0 flex items-center justify-center">
          <div class="text-center">
            <p class="text-sm text-muted">This notebook has no pages.</p>
            <Btn class="mt-3" variant="primary" @click="addPage(-1)"><Plus :size="18" />Add a page</Btn>
          </div>
        </div>
      </main>
    </div>

    <!-- bottom tray on narrow screens -->
    <footer v-if="narrow" class="safe-bottom relative z-30 border-t border-line bg-surface">
      <Toolbar compact :state="st" @tool="setTool" @undo="host.editor.value?.undo()" @redo="host.editor.value?.redo()"
        @zoom-in="host.editor.value?.zoomBy(1.25)" @zoom-out="host.editor.value?.zoomBy(0.8)" @fit="host.editor.value?.fit('width')"
        @delete-selection="host.editor.value?.deleteSelection()" @edit-text="host.editor.value?.editSelectedText()" @restyle="(s) => host.editor.value?.restyleSelection(s)" @free-placement="(f) => host.editor.value?.setSelectionFreePlacement(f)" />
    </footer>

    <ConflictsDialog v-if="showConflicts" :notebook-id="nb.id" :current-title="nb.title" @close="showConflicts = false" @resolved="lib.reloadCurrent(); lib.currentPageId && host.markChanged([lib.currentPageId])" />
    <SettingsDialog v-if="showSettings" :native-kind="host.native.value?.kind ?? null" @close="showSettings = false" />
    <SearchPanel v-if="search.panelOpen" @close="search.panelOpen = false" @open="openHit" />
    <PromptDialog v-if="renaming" title="Rename notebook" label="Title" :value="nb.title" confirm="Rename" @close="renaming = false" @submit="(t) => { lib.rename(nb.id, t); renaming = false; }" />
  </div>
</template>
