<script setup lang="ts">
import { EllipsisVertical, Folder as FolderIcon, FolderInput, SquarePen, Trash } from "@lucide/vue";
import type { Folder } from "../generated/Folder";
import Menu from "./ui/Menu.vue";
import MenuItem from "./ui/MenuItem.vue";

defineProps<{
  folder: Folder;
  /** Notebooks and subfolders directly inside. */
  count: number;
}>();
const emit = defineEmits<{ open: []; rename: []; move: []; delete: [] }>();
</script>

<template>
  <article class="flex items-center rounded-xl border border-line bg-surface pr-1 epaper:border-2 epaper:border-black">
    <button class="flex min-w-0 flex-1 items-center gap-3 rounded-xl px-3 py-3 text-left hover:bg-surface-2" :aria-label="`Open folder ${folder.name}`" @click="emit('open')">
      <FolderIcon :size="20" class="shrink-0 text-muted" aria-hidden="true" />
      <span class="min-w-0">
        <span class="block truncate text-sm font-medium" :title="folder.name">{{ folder.name }}</span>
        <span class="block text-xs text-muted">{{ count }} item{{ count === 1 ? "" : "s" }}</span>
      </span>
    </button>
    <Menu :label="`${folder.name} actions`">
      <template #trigger="{ toggle, open }">
        <button class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-expanded="open" aria-haspopup="menu" :aria-label="`More actions for folder ${folder.name}`" @click="toggle">
          <EllipsisVertical :size="18" />
        </button>
      </template>
      <MenuItem @click="emit('rename')"><SquarePen :size="16" />Rename</MenuItem>
      <MenuItem @click="emit('move')"><FolderInput :size="16" />Move to folder…</MenuItem>
      <MenuItem danger @click="emit('delete')"><Trash :size="16" />Delete folder</MenuItem>
    </Menu>
  </article>
</template>
