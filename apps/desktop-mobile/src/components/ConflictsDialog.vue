<script setup lang="ts">
// Conflicts of one notebook: the visible version and every retained
// alternative. Choosing one is an ordinary edit that syncs everywhere.
import { onMounted, ref } from "vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { syncApi, type SyncConflict } from "../platform/sync";
import { conflictTitle, conflictValuePreview, resolvable } from "../platform/conflicts";
import { useUi } from "../stores/ui";

const props = defineProps<{ notebookId: string; currentTitle: string }>();
const emit = defineEmits<{ close: []; resolved: [] }>();
const ui = useUi();
const conflicts = ref<SyncConflict[]>([]);
const loading = ref(true);

async function load() {
  loading.value = true;
  try {
    conflicts.value = await syncApi.conflicts(props.notebookId);
  } catch (e) {
    ui.error(`Could not load conflicts: ${(e as Error).message}`);
  } finally {
    loading.value = false;
  }
}
onMounted(load);

async function keep(c: SyncConflict, choose: string) {
  try {
    await syncApi.resolveConflict(props.notebookId, c.target, choose);
    ui.success("Kept that version.");
    emit("resolved");
    await load();
  } catch (e) {
    ui.error(`Could not resolve: ${(e as Error).message}`);
  }
}
</script>

<template>
  <Modal title="Sync conflicts" wide @close="emit('close')">
    <p class="mb-3 text-xs leading-relaxed text-muted">
      These were changed on two devices at the same time. Penroam shows one version and keeps the others here, so nothing is lost. Pick the version to keep.
    </p>
    <p v-if="loading" class="text-sm text-muted">Loading…</p>
    <p v-else-if="!conflicts.length" class="text-sm">No conflicts in this notebook.</p>
    <ul v-else class="space-y-3">
      <li v-for="(c, i) in conflicts" :key="i" class="rounded-xl border border-line p-3">
        <p class="mb-2 text-sm font-medium">{{ conflictTitle(c) }}</p>
        <div class="space-y-2">
          <div class="flex items-start gap-2 rounded-lg bg-surface-2 p-2 epaper:border">
            <div class="min-w-0 flex-1">
              <p class="text-[11px] font-semibold uppercase tracking-wide text-muted">Shown now</p>
              <p class="whitespace-pre-wrap break-words text-sm">{{ conflictValuePreview(c, c.winnerValue) }}</p>
            </div>
            <Btn v-if="resolvable(c)" variant="outline" @click="keep(c, c.winner)">Keep this version</Btn>
          </div>
          <div v-for="a in c.alternates" :key="a.opId" class="flex items-start gap-2 rounded-lg border border-line p-2">
            <div class="min-w-0 flex-1">
              <p class="text-[11px] font-semibold uppercase tracking-wide text-muted">Other version</p>
              <p class="whitespace-pre-wrap break-words text-sm">{{ conflictValuePreview(c, a.value) }}</p>
            </div>
            <Btn v-if="resolvable(c)" variant="outline" @click="keep(c, a.opId)">Keep this version</Btn>
          </div>
          <p v-if="!resolvable(c)" class="text-xs text-muted">Resolve this by editing the page or notebook directly; any new edit replaces both versions.</p>
        </div>
      </li>
    </ul>
    <template #actions>
      <Btn @click="emit('close')">Close</Btn>
    </template>
  </Modal>
</template>
