<script setup lang="ts">
// Page list with thumbnails. Reorder by drag (mouse/pen) or with the
// per-page menu (touch/keyboard); delete goes to the page trash (restorable).
import { ref } from "vue";
import { Plus, EllipsisVertical, ArrowUp, ArrowDown, Trash, FilePlusCorner, ArchiveArrowUp, FileText } from "@lucide/vue";
import type { Notebook } from "../generated/Notebook";
import Menu from "./ui/Menu.vue";
import MenuItem from "./ui/MenuItem.vue";

const props = defineProps<{ notebook: Notebook; current: string | null; thumbs: Record<string, string> }>();
const emit = defineEmits<{ select: [id: string]; add: [afterIndex: number]; delete: [id: string]; reorder: [order: string[]]; restore: [id: string] }>();
const dragId = ref<string | null>(null);
const overIdx = ref<number | null>(null);
const showTrash = ref(false);

function move(id: string, delta: number) {
  const o = [...props.notebook.pageOrder];
  const i = o.indexOf(id);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= o.length) return;
  o.splice(i, 1);
  o.splice(j, 0, id);
  emit("reorder", o);
}

function onDrop(idx: number) {
  const id = dragId.value;
  dragId.value = null;
  overIdx.value = null;
  if (!id) return;
  const o = props.notebook.pageOrder.filter((p) => p !== id);
  const from = props.notebook.pageOrder.indexOf(id);
  o.splice(idx > from ? idx - 1 : idx, 0, id);
  if (o.join() !== props.notebook.pageOrder.join()) emit("reorder", o);
}
</script>

<template>
  <nav class="flex h-full flex-col" aria-label="Pages">
    <div class="flex items-center justify-between px-3 pb-1 pt-3">
      <h2 class="text-xs font-semibold uppercase tracking-wide text-muted">Pages · {{ notebook.pageOrder.length }}</h2>
      <button class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" aria-label="Add page at end" title="Add page at end" @click="emit('add', notebook.pageOrder.length - 1)">
        <Plus :size="18" />
      </button>
    </div>
    <ol class="flex-1 space-y-1 overflow-y-auto px-2 pb-3" @dragover.prevent>
      <li v-for="(pid, i) in notebook.pageOrder" :key="pid" class="relative"
        @dragover.prevent="overIdx = i" @drop.prevent="onDrop(i)">
        <div v-if="overIdx === i && dragId && dragId !== pid" class="absolute -top-1 left-2 right-2 h-0.5 rounded bg-accent" aria-hidden="true" />
        <div class="group flex items-start gap-1 rounded-xl p-1.5" :class="pid === current ? 'bg-brand-soft epaper:outline epaper:outline-2 epaper:outline-black' : 'hover:bg-surface-2'">
          <button class="flex min-w-0 flex-1 items-start gap-2 text-left" draggable="true" :aria-current="pid === current ? 'page' : undefined" :aria-label="`Page ${i + 1}`"
            @dragstart="dragId = pid" @dragend="dragId = null; overIdx = null" @click="emit('select', pid)">
            <span class="w-5 pt-1 text-right text-xs tabular-nums text-muted">{{ i + 1 }}</span>
            <span class="block aspect-[17/22] w-24 overflow-hidden rounded-md border border-line bg-white">
              <img v-if="thumbs[pid]" :src="thumbs[pid]" alt="" class="h-full w-full object-cover object-top" draggable="false" />
              <FileText v-else :size="18" class="m-auto mt-6 text-gray-300" />
            </span>
          </button>
          <Menu :label="`Page ${i + 1} actions`">
            <template #trigger="{ toggle, open }">
              <button class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface" :aria-expanded="open" aria-haspopup="menu" :aria-label="`Actions for page ${i + 1}`" @click="toggle">
                <EllipsisVertical :size="16" />
              </button>
            </template>
            <MenuItem @click="emit('add', i)"><FilePlusCorner :size="16" />Insert page after</MenuItem>
            <MenuItem :disabled="i === 0" @click="move(pid, -1)"><ArrowUp :size="16" />Move up</MenuItem>
            <MenuItem :disabled="i === notebook.pageOrder.length - 1" @click="move(pid, 1)"><ArrowDown :size="16" />Move down</MenuItem>
            <MenuItem danger @click="emit('delete', pid)"><Trash :size="16" />Delete page</MenuItem>
          </Menu>
        </div>
      </li>
      <li class="h-6" @dragover.prevent="overIdx = notebook.pageOrder.length" @drop.prevent="onDrop(notebook.pageOrder.length)" />
    </ol>
    <div v-if="notebook.trashedPages.length" class="border-t border-line px-2 py-2">
      <button class="flex w-full items-center justify-between rounded-lg px-2 py-1.5 text-xs text-muted hover:bg-surface-2" :aria-expanded="showTrash" @click="showTrash = !showTrash">
        <span>Deleted pages ({{ notebook.trashedPages.length }})</span><span>{{ showTrash ? 'Hide' : 'Show' }}</span>
      </button>
      <ul v-if="showTrash" class="mt-1 space-y-1">
        <li v-for="t in notebook.trashedPages" :key="t.id" class="flex items-center justify-between gap-2 rounded-lg px-2 py-1 text-xs">
          <span class="text-muted">Page {{ t.formerIndex + 1 }} · {{ new Date(t.deletedAt).toLocaleDateString() }}</span>
          <button class="inline-flex min-h-8 items-center gap-1 rounded-md px-2 font-medium text-brand hover:bg-surface-2" @click="emit('restore', t.id)"><ArchiveArrowUp :size="14" />Restore</button>
        </li>
      </ul>
    </div>
  </nav>
</template>
