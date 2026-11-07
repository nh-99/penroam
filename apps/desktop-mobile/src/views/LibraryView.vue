<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { BookPlus, Import, Settings, Trash, ArchiveArrowUp, NotebookPen, TriangleAlert, ArrowLeft, Search, Cloud, LoaderCircle } from "@lucide/vue";
import logoUrl from "../assets/logo.png";
import { useLibrary } from "../stores/library";
import { COVERS } from "../editor/covers";
import NotebookCard from "../components/NotebookCard.vue";
import CloudNotebookCard from "../components/CloudNotebookCard.vue";
import NewNotebookDialog from "../components/NewNotebookDialog.vue";
import MoveToVaultDialog from "../components/MoveToVaultDialog.vue";
import VaultBar from "../components/VaultBar.vue";
import PromptDialog from "../components/PromptDialog.vue";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import SettingsDialog from "../components/SettingsDialog.vue";
import CloudBanner from "../components/CloudBanner.vue";
import SearchPanel from "../components/SearchPanel.vue";
import { useSearch } from "../stores/search";
import { useSync } from "../stores/sync";
import { useUi } from "../stores/ui";
import { highlightRects, type SearchHit } from "../platform/search";
import { modKey } from "../platform/device";
import Btn from "../components/ui/Btn.vue";
import IconBtn from "../components/ui/IconBtn.vue";
import type { Paper } from "../generated/Paper";
import type { TemplateKind } from "../generated/TemplateKind";
import type { NotebookSummary } from "../generated/NotebookSummary";

defineProps<{ nativeKind: string | null }>();
const lib = useLibrary();
const showNew = ref(false);
const showSettings = ref(false);
const showTrash = ref(false);
const renaming = ref<NotebookSummary | null>(null);
const purging = ref<NotebookSummary | null>(null);
const moving = ref<NotebookSummary | null>(null);

const list = computed(() => (showTrash.value ? lib.trashed : lib.live));
const search = useSearch();
const sync = useSync();
const ui = useUi();
const mod = modKey();

// with sync the account keeps a deleted notebook, so this isn't final and the dialog must not say so
const purgeMessage = computed(() => {
  const title = purging.value?.title;
  return sync.configured
    ? `“${title}” will be removed from all your devices. It stays in your account, so you can restore it from the trash.`
    : `“${title}” and all its pages will be permanently removed from this device. This cannot be undone.`;
});

function download(id: string) {
  sync.download(id).catch((e) => ui.error(`Could not download the notebook: ${(e as Error).message ?? e}`));
}

function openHit(h: SearchHit) {
  search.panelOpen = false;
  search.pendingHighlight = h.pageId ? { pageId: h.pageId, rects: highlightRects(h) } : null;
  void lib.open(h.notebookId, h.pageId ?? undefined);
}

function onKey(e: KeyboardEvent) {
  const k = e.key.toLowerCase();
  if ((e.ctrlKey || e.metaKey) && (k === "k" || k === "f") && ui.overlays === 0) {
    e.preventDefault();
    search.panelOpen = true;
  }
}
onMounted(() => window.addEventListener("keydown", onKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onKey));

async function create(title: string, paper: Paper, template: TemplateKind, vault?: string) {
  showNew.value = false;
  await lib.createNotebook(title, paper, { kind: template, spacing: template === "dotted" ? 20 : template === "grid" ? 18 : 24 }, vault);
  if (vault) await sync.refreshVaults();
}

/** The vault a notebook is in: its name, or "a vault" when this device doesn't know it. */
function vaultName(id: string): string | undefined {
  const v = sync.vaults.notebooks[id];
  if (!v) return undefined;
  return sync.vaults.vaults.find((x) => x.vaultId === v)?.name ?? "a vault";
}

function recolor(nb: NotebookSummary) {
  const i = COVERS.indexOf(nb.cover);
  void lib.setCover(nb.id, COVERS[(i + 1) % COVERS.length]);
}
</script>

<template>
  <div class="flex h-full flex-col safe-top safe-x">
    <header class="flex items-center gap-2 border-b border-line px-4 py-3 sm:px-6">
      <template v-if="!showTrash">
        <img :src="logoUrl" alt="" class="h-8 w-8 rounded-lg" aria-hidden="true" />
        <h1 class="mr-auto text-lg font-semibold tracking-tight">Penroam</h1>
        <IconBtn label="Search notebooks" :shortcut="`${mod}+K`" @click="search.panelOpen = true"><Search :size="20" /></IconBtn>
        <Btn variant="primary" @click="showNew = true"><BookPlus :size="18" /><span class="hidden sm:inline">New notebook</span></Btn>
        <IconBtn label="Import notebooks and documents" @click="lib.importFiles()"><Import :size="20" /></IconBtn>
        <IconBtn :label="`Trash (${lib.trashed.length + sync.cloudTrash.length})`" @click="showTrash = true"><Trash :size="20" /></IconBtn>
        <IconBtn label="Settings" @click="showSettings = true"><Settings :size="20" /></IconBtn>
      </template>
      <template v-else>
        <IconBtn label="Back to notebooks" @click="showTrash = false"><ArrowLeft :size="20" /></IconBtn>
        <h1 class="mr-auto text-lg font-semibold">Trash</h1>
      </template>
    </header>
    <CloudBanner v-if="!showTrash" />

    <main class="flex-1 overflow-y-auto px-4 py-6 sm:px-6">
      <div v-if="lib.loadError" class="mx-auto mt-16 max-w-md text-center" role="alert">
        <TriangleAlert :size="36" class="mx-auto mb-3 text-danger" />
        <h2 class="text-lg font-semibold">The notebook library could not be opened</h2>
        <p class="mt-2 text-sm text-muted">{{ lib.loadError }}</p>
        <Btn class="mt-4" variant="outline" @click="lib.refresh()">Try again</Btn>
      </div>

      <template v-else-if="!showTrash">
        <VaultBar v-if="sync.status?.phase !== 'revoked'" />
        <div v-if="lib.loaded && list.length === 0 && sync.cloudNotebooks.length === 0" class="mx-auto mt-16 max-w-md text-center">
          <NotebookPen :size="40" class="mx-auto mb-4 text-muted" />
          <h2 class="text-lg font-semibold">Your shelf is empty</h2>
          <p class="mt-2 text-sm leading-relaxed text-muted">Create a notebook to start writing by hand or typing on the same page. Everything stays on this device; nothing needs an account.</p>
          <div class="mt-5 flex justify-center gap-2">
            <Btn variant="primary" @click="showNew = true"><BookPlus :size="18" />New notebook</Btn>
            <Btn variant="outline" @click="lib.importFiles()"><Import :size="18" />Import</Btn>
          </div>
        </div>
        <div v-else class="grid grid-cols-2 gap-x-4 gap-y-6 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
          <NotebookCard v-for="nb in list" :key="nb.id" :nb="nb" :cover="lib.covers[nb.id]"
            :vault-name="vaultName(nb.id)" :can-move-to-vault="sync.openVaults.length > 0 && !sync.vaults.notebooks[nb.id]" @move-to-vault="moving = nb"
            @open="lib.open(nb.id)" @rename="renaming = nb" @recolor="recolor(nb)"
            @export="lib.exportNotebook(nb.id, nb.title)" @export-pdf="lib.exportPdf(nb.id, nb.title)" @print="lib.printNotebook(nb.id)" @trash="lib.trash(nb.id)" />
          <CloudNotebookCard v-for="nb in sync.cloudNotebooks" :key="nb.notebookId" :nb="nb"
            :downloading="sync.downloading.includes(nb.notebookId)" @download="download(nb.notebookId)" />
        </div>
      </template>

      <template v-else>
        <p v-if="list.length === 0 && sync.cloudTrash.length === 0" class="mt-16 text-center text-sm text-muted">The trash is empty.</p>
        <ul v-else class="mx-auto max-w-2xl divide-y divide-line rounded-xl border border-line bg-surface">
          <li v-for="nb in list" :key="nb.id" class="flex items-center gap-3 px-4 py-3">
            <span class="h-8 w-2 rounded-full" :style="{ background: nb.cover }" aria-hidden="true" />
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-medium">{{ nb.title }}</p>
              <p class="text-xs text-muted">{{ nb.pageCount }} pages · deleted {{ new Date(nb.deletedAt!).toLocaleString() }}</p>
            </div>
            <Btn variant="outline" @click="lib.restore(nb.id)"><ArchiveArrowUp :size="16" />Restore</Btn>
            <Btn variant="ghost" class="text-danger" @click="purging = nb">{{ sync.configured ? "Delete from devices" : "Delete forever" }}</Btn>
          </li>
          <li v-for="nb in sync.cloudTrash" :key="nb.notebookId" class="flex items-center gap-3 px-4 py-3">
            <span class="h-8 w-2 rounded-full opacity-40" :style="{ background: nb.cover }" aria-hidden="true" />
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-medium text-muted">{{ nb.title }}</p>
              <p class="flex items-center gap-1 text-xs text-muted"><Cloud :size="12" aria-hidden="true" />{{ nb.pageCount }} pages · only in your account</p>
            </div>
            <Btn variant="outline" :disabled="sync.downloading.includes(nb.notebookId)" @click="download(nb.notebookId)">
              <LoaderCircle v-if="sync.downloading.includes(nb.notebookId)" :size="16" class="animate-spin" /><ArchiveArrowUp v-else :size="16" />Restore
            </Btn>
          </li>
        </ul>
      </template>
    </main>

    <NewNotebookDialog v-if="showNew" @close="showNew = false" @create="create" />
    <MoveToVaultDialog v-if="moving" :notebook="moving" @close="moving = null" />
    <SettingsDialog v-if="showSettings" :native-kind="nativeKind" @close="showSettings = false" />
    <SearchPanel v-if="search.panelOpen" @close="search.panelOpen = false" @open="openHit" />
    <PromptDialog v-if="renaming" title="Rename notebook" label="Title" :value="renaming.title" confirm="Rename"
      @close="renaming = null" @submit="(t) => { lib.rename(renaming!.id, t); renaming = null; }" />
    <ConfirmDialog v-if="purging" :title="sync.configured ? 'Delete from all devices?' : 'Delete forever?'" danger :confirm="sync.configured ? 'Delete from devices' : 'Delete forever'"
      :message="purgeMessage"
      @close="purging = null" @confirm="() => { lib.purge(purging!.id); purging = null; }" />
  </div>
</template>
