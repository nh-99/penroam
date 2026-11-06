<script setup lang="ts">
// Version history of the open page, grouped into writing sessions. Restoring adds a new edit,
// so it syncs and the version it replaced stays in this list.
import { computed, onBeforeUnmount, onMounted, ref, shallowRef } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import type { HistoryStats } from "../generated/HistoryStats";
import type { Notebook } from "../generated/Notebook";
import type { Page } from "../generated/Page";
import { api, type HistoryRow } from "../platform/ipc";
import { fromAccount, groupSessions, type HistorySession } from "../platform/history";
import { formatBytes } from "../platform/search";
import { useSync } from "../stores/sync";
import { useUi } from "../stores/ui";

const props = defineProps<{ notebook: Notebook; pageId: string; render: (page: Page, width: number) => Promise<Uint8Array | null> }>();
const emit = defineEmits<{ close: []; restored: [pages: string[]] }>();
const ui = useUi();
const sync = useSync();
const points = ref<HistoryRow[]>([]);
const stats = ref<HistoryStats | null>(null);
const loading = ref(true);
// shallow: `choose` compares identity, and a deep ref would hand back a proxy
const chosen = shallowRef<HistorySession | null>(null);
const previewUrl = ref<string | null>(null);
const previewing = ref(false);
const confirm = ref<"page" | "notebook" | "clear" | null>(null);

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
    const png = await props.render(page, 520);
    if (chosen.value === s && png) previewUrl.value = URL.createObjectURL(new Blob([png as BlobPart], { type: "image/png" }));
  } catch (e: any) {
    ui.error(`Could not show that version: ${e?.message ?? e}`);
  } finally {
    previewing.value = false;
  }
}

async function restorePage() {
  const s = chosen.value;
  confirm.value = null;
  if (!s) return;
  try {
    await api.restorePageVersion(props.notebook.id, props.pageId, s.revision, s.position);
    ui.success(`This page is back to ${when(s.at)}.`);
    emit("restored", [props.pageId]);
  } catch (e: any) {
    ui.error(`Could not restore: ${e?.message ?? e}`);
  }
}

async function restoreNotebook() {
  const s = chosen.value;
  confirm.value = null;
  if (!s) return;
  try {
    const r = await api.restoreNotebookVersion(props.notebook.id, s.at, fromAccount(s));
    const unavailable = r.pagesUnavailable ? ` ${r.pagesUnavailable} page(s) from then can't be brought back.` : "";
    ui.success(`The notebook is back to ${when(s.at)}.${unavailable}`);
    emit("restored", [...props.notebook.pageOrder]);
  } catch (e: any) {
    ui.error(`Could not restore: ${e?.message ?? e}`);
  }
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
      Restoring adds a new edit, so nothing you have now is lost. Older versions thin out over time, and versions over a year old aren't offered.
    </p>
    <p v-if="loading" class="text-sm text-muted">Loading…</p>
    <p v-else-if="!sessions.length" class="text-sm">No earlier versions yet. They start with your next edit to this page.</p>
    <div v-else class="grid gap-3 sm:grid-cols-2">
      <ul class="max-h-[50dvh] divide-y divide-line overflow-y-auto rounded-xl border border-line" aria-label="Versions">
        <li v-for="s in sessions" :key="`${s.revision}-${s.position}`">
          <button class="w-full px-3 py-2 text-left text-sm hover:bg-surface-2" :class="{ 'bg-surface-2': chosen === s }" :aria-pressed="chosen === s" @click="choose(s)">
            <span class="block">{{ when(s.at) }}</span>
            <span class="block text-xs text-muted">{{ changes(s) }}</span>
          </button>
        </li>
      </ul>
      <div class="flex min-h-48 items-center justify-center rounded-xl border border-line p-2">
        <p v-if="!chosen" class="text-sm text-muted">Pick a version to see it.</p>
        <p v-else-if="previewing" class="text-sm text-muted">Rebuilding that version…</p>
        <img v-else-if="previewUrl" :src="previewUrl" alt="Preview of the chosen version" class="max-h-[50dvh] max-w-full rounded border border-line bg-white" />
      </div>
    </div>
    <p v-if="stats?.bytes" class="mt-3 text-xs text-muted">This notebook's history uses {{ formatBytes(stats.bytes) }} on this device.</p>
    <template #actions>
      <Btn v-if="stats?.bytes" variant="ghost" class="mr-auto" @click="confirm = 'clear'">Delete history…</Btn>
      <Btn :disabled="!chosen" @click="confirm = 'notebook'">Restore whole notebook…</Btn>
      <Btn variant="primary" :disabled="!chosen" @click="confirm = 'page'">Restore this page…</Btn>
    </template>
    <ConfirmDialog v-if="confirm === 'page'" title="Restore this page?" :message="`The page goes back to how it was on ${when(chosen!.at)}. You can undo this from the history.`" confirm="Restore page" @close="confirm = null" @confirm="restorePage" />
    <ConfirmDialog v-if="confirm === 'notebook'" title="Restore the whole notebook?" :message="`Every page, the page order and the title go back to ${when(chosen!.at)}. Pages added since move to the trash.`" confirm="Restore notebook" @close="confirm = null" @confirm="restoreNotebook" />
    <ConfirmDialog v-if="confirm === 'clear'" danger title="Delete this notebook's history?" :message="clearMessage" confirm="Delete history" @close="confirm = null" @confirm="clear" />
  </Modal>
</template>
