<script setup lang="ts">
import { computed } from "vue";
import { EllipsisVertical, SquarePen, Download, FileDown, Trash, Palette } from "@lucide/vue";
import type { NotebookSummary } from "../generated/NotebookSummary";
import Menu from "./ui/Menu.vue";
import MenuItem from "./ui/MenuItem.vue";

const props = defineProps<{ nb: NotebookSummary; cover?: string }>();
const emit = defineEmits<{ open: []; rename: []; export: []; exportPdf: []; trash: []; recolor: [] }>();
const updated = computed(() => {
  const d = new Date(props.nb.updatedAt);
  const days = Math.floor((Date.now() - d.getTime()) / 86400000);
  if (days < 1) return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (days < 7) return d.toLocaleDateString([], { weekday: "long" });
  return d.toLocaleDateString();
});
</script>

<template>
  <article class="group relative flex flex-col">
    <button
      class="relative block aspect-[17/22] w-full overflow-hidden rounded-xl border border-line bg-white text-left shadow-sm transition hover:-translate-y-0.5 hover:shadow-md focus-visible:-translate-y-0.5 epaper:border-2 epaper:border-black"
      :aria-label="`Open ${nb.title}`"
      @click="emit('open')"
    >
      <div class="absolute inset-y-0 left-0 w-3" :style="{ background: nb.cover }" aria-hidden="true" />
      <img v-if="cover" :src="cover" alt="" class="h-full w-full object-cover object-top pl-3" draggable="false" />
      <div v-else class="flex h-full w-full items-end pl-3" :style="{ background: `linear-gradient(160deg, ${nb.cover}22, ${nb.cover}10)` }">
        <span class="p-3 text-xs text-muted">{{ nb.pageCount }} page{{ nb.pageCount === 1 ? '' : 's' }}</span>
      </div>
    </button>
    <div class="mt-2 flex items-start gap-1">
      <div class="min-w-0 flex-1">
        <h3 class="truncate text-sm font-medium" :title="nb.title">{{ nb.title }}</h3>
        <p class="text-xs text-muted">{{ nb.pageCount }} page{{ nb.pageCount === 1 ? '' : 's' }} · {{ updated }}</p>
      </div>
      <Menu :label="`${nb.title} actions`">
        <template #trigger="{ toggle, open }">
          <button class="touch-target -mr-2 -mt-1 inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-expanded="open" aria-haspopup="menu" :aria-label="`More actions for ${nb.title}`" @click="toggle">
            <EllipsisVertical :size="18" />
          </button>
        </template>
        <MenuItem @click="emit('rename')"><SquarePen :size="16" />Rename</MenuItem>
        <MenuItem @click="emit('recolor')"><Palette :size="16" />Change cover colour</MenuItem>
        <MenuItem @click="emit('export')"><Download :size="16" />Export .penroam…</MenuItem>
        <MenuItem @click="emit('exportPdf')"><FileDown :size="16" />Export annotated PDF…</MenuItem>
        <MenuItem danger @click="emit('trash')"><Trash :size="16" />Move to trash</MenuItem>
      </Menu>
    </div>
  </article>
</template>
