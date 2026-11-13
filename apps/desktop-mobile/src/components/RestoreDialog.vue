<script setup lang="ts">
// Restoring from an earlier version without throwing away other people's newer work. Nothing is
// changed until "Restore": first a plan is shown, item by item. What only you changed is ticked;
// what others changed since is unticked with their name, and anything added since stays. If the
// page moved on while this was open, the plan is made again and you check it again. The result is
// one ordinary new edit, so it syncs and can be undone.
import { computed, onMounted, ref } from "vue";
import { TriangleAlert } from "@lucide/vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import AuthorBadge from "./AuthorBadge.vue";
import { changedSincePreview, defaultTicks, historyApi, informational, othersChanged, restoreItemText, toggleItem, type RestorePlan } from "../platform/history";
import { describeSharingError } from "../platform/sharing";

const props = defineProps<{
  notebookId: string;
  /** A shared notebook: newer work by others is kept. Otherwise the notebook goes back exactly. */
  shared?: boolean;
  /** The notebook's pages in order, to name them by number. */
  pageOrder?: string[];
  /** Whole-notebook restore when absent. */
  pageId?: string;
  /** The version's place in the account's log, or its revision on this device (page restore). */
  position?: number;
  revision?: number;
  /** The time to go back to (notebook restore). */
  at?: number;
  /** Only these elements (restore from a selection in the preview). */
  elementIds?: string[];
  /** A short name for the version, for the heading. */
  when: string;
}>();
const emit = defineEmits<{ close: []; restored: [] }>();
const plan = ref<RestorePlan | null>(null);
const ticked = ref<Set<string>>(new Set());
const loading = ref(true);
const busy = ref(false);
const error = ref<string | null>(null);
const refreshed = ref(false);

const notebookLevel = computed(() => !props.pageId);
const pageNumbers = computed(() => Object.fromEntries((props.pageOrder ?? []).map((id, i) => [id, i + 1])));
/** What this restore removes or trashes: only offered in a personal notebook. */
const removes = computed(() => (plan.value?.items ?? []).filter((i) => i.kind === "removeAdditions" || i.kind === "trashNewer" || i.kind === "pageOrder"));
const others = computed(() => (plan.value ? othersChanged(plan.value) : []));
const actionable = computed(() => (plan.value?.items ?? []).filter((i) => !informational(i)));
// what stays: the plan's count (a page preview doesn't list these any more), or the pages a notebook plan lists
const kept = computed(() => Math.max(plan.value?.keptAdditions ?? 0, (plan.value?.items ?? []).filter((i) => i.kind === "keptAddition").length));
const unavailable = computed(() => (plan.value?.items ?? []).filter((i) => i.kind === "unavailable"));
/** A block offered both as "put it back" and "as a copy": the choices exclude each other. */
const choiceIds = computed(() => {
  const by = new Map<string, number>();
  for (const i of actionable.value) if (i.elementId && (i.kind === "revert" || i.kind === "recoveredCopy")) by.set(i.elementId, (by.get(i.elementId) ?? 0) + 1);
  return new Set([...by].filter(([, n]) => n > 1).map(([id]) => id));
});
const count = computed(() => actionable.value.filter((i) => ticked.value.has(i.itemId)).length);

async function preview() {
  loading.value = true;
  error.value = null;
  try {
    plan.value = notebookLevel.value ? await historyApi.notebookRestorePreview(props.notebookId, props.at!) : await historyApi.restorePreview(props.notebookId, props.pageId!, { position: props.position, revision: props.revision }, props.elementIds);
    ticked.value = defaultTicks(plan.value);
  } catch (e) {
    error.value = describeSharingError(e);
  } finally {
    loading.value = false;
  }
}
onMounted(preview);

function toggle(id: string, on: boolean) {
  if (plan.value) ticked.value = toggleItem(plan.value, ticked.value, id, on);
}

async function commit() {
  const p = plan.value;
  if (!p || !count.value) return;
  busy.value = true;
  try {
    const ids = actionable.value.filter((i) => ticked.value.has(i.itemId)).map((i) => i.itemId);
    if (notebookLevel.value) await historyApi.notebookRestoreCommit(props.notebookId, p.planId, ids);
    else await historyApi.restoreCommit(props.notebookId, p.planId, ids);
    emit("restored");
  } catch (e) {
    if (changedSincePreview(e)) {
      // somebody (or another device) changed the page: what was shown may no longer be true
      await preview();
      refreshed.value = true;
    } else error.value = describeSharingError(e);
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <Modal :title="notebookLevel ? 'Restore the notebook?' : elementIds?.length ? 'Restore the chosen elements?' : 'Restore this page?'" wide @close="emit('close')">
    <p class="mb-3 text-sm leading-relaxed">
      <template v-if="shared && notebookLevel">Pages deleted since {{ when }} come back. Pages added since then are kept, and nothing newer is thrown away.</template>
      <template v-else-if="shared">Bring back what was on this page on {{ when }}. Anything added since stays, and what others changed since is yours to choose.</template>
      <template v-else-if="notebookLevel">Put the notebook back the way it was on {{ when }}. You choose below what to bring back and what to move out of the way.</template>
      <template v-else-if="elementIds?.length">Bring back the chosen elements as they were on {{ when }}.</template>
      <template v-else>Put this page back the way it was on {{ when }}. You choose below what to bring back and what to remove.</template>
      This is one new edit: it {{ shared ? "syncs, and" : "" }} can be undone from the history.
    </p>
    <p v-if="refreshed" class="mb-3 rounded-xl border border-line bg-surface-2 p-3 text-sm" role="alert" data-testid="changed-since">
      This changed while you were looking (or the preview timed out), so the list below is new. Check it again before restoring.
    </p>
    <p v-if="loading" class="text-sm text-muted" role="status">Working out what would change…</p>
    <p v-else-if="error" class="text-sm text-danger" role="alert">{{ error }}</p>
    <template v-else-if="plan">
      <div v-if="!shared && removes.length" class="mb-3 flex items-start gap-2 rounded-xl border border-line p-3 text-sm epaper:border-2" role="note" data-testid="personal-warning">
        <TriangleAlert :size="18" class="mt-0.5 shrink-0" aria-hidden="true" />
        <p>This puts the notebook back as it was. Later changes go to the trash or are removed, and you can undo it by restoring again.</p>
      </div>
      <div v-if="others.length" class="mb-3 flex items-start gap-2 rounded-xl border border-line p-3 text-sm epaper:border-2" role="note" data-testid="collaborator-warning">
        <TriangleAlert :size="18" class="mt-0.5 shrink-0" aria-hidden="true" />
        <p>Collaborators changed {{ others.length === 1 ? "one thing" : `${others.length} things` }} here since. {{ others.some((i) => i.selected) ? "" : "They are unticked, so their work stays." }} Restoring one replaces what they did.</p>
      </div>
      <p v-if="!actionable.length" class="text-sm" data-testid="nothing-to-restore">There is nothing to restore: the current version already has everything from then.</p>
      <ul v-else class="divide-y divide-line rounded-xl border border-line" aria-label="What would be restored">
        <li v-for="i in actionable" :key="i.itemId" class="px-3 py-2">
          <label class="flex min-h-11 items-start gap-3 text-sm">
            <input type="checkbox" class="mt-0.5 h-5 w-5 shrink-0" :checked="ticked.has(i.itemId)" :disabled="!i.editable" @change="toggle(i.itemId, ($event.target as HTMLInputElement).checked)" />
            <span class="min-w-0 flex-1">
              <span class="block">{{ restoreItemText(i, { pageNumbers }) }}</span>
              <span v-if="i.authors.length" class="mt-0.5 flex flex-wrap gap-x-3 text-xs text-muted"><span>Changed since by</span><AuthorBadge v-for="(a, n) in i.authors" :key="n" :author="a" /></span>
              <span v-if="i.elementId && choiceIds.has(i.elementId)" class="block text-xs text-muted" data-testid="exclusive">Choose this or the other way of restoring this text, not both.</span>
              <span v-if="!i.editable" class="block text-xs text-muted">This can't be changed.</span>
            </span>
          </label>
        </li>
      </ul>
      <p v-if="kept" class="mt-2 text-xs text-muted" data-testid="kept">{{ kept }} {{ notebookLevel ? (kept === 1 ? "page" : "pages") + " added" : kept === 1 ? "addition" : "additions" }} since then {{ kept === 1 ? "stays" : "stay" }}.</p>
      <p v-if="unavailable.length" class="mt-2 text-xs text-muted" data-testid="unavailable">{{ unavailable.length }} {{ unavailable.length === 1 ? "thing" : "things" }} from then can't be brought back.</p>
    </template>
    <template #actions>
      <Btn @click="emit('close')">Cancel</Btn>
      <Btn variant="primary" :disabled="loading || busy || !count" @click="commit">{{ count ? `Restore ${count} ${count === 1 ? "item" : "items"}` : "Restore" }}</Btn>
    </template>
  </Modal>
</template>
