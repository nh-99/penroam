<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { EllipsisVertical, SquarePen, Download, FileDown, FolderInput, Printer, Trash, Palette, Lock, Copy, Users } from "@lucide/vue";
import type { NotebookSummary } from "../generated/NotebookSummary";
import Menu from "./ui/Menu.vue";
import MenuItem from "./ui/MenuItem.vue";

const props = defineProps<{
  nb: NotebookSummary;
  cover?: string;
  /** The notebook is in a vault; its name, when this device knows it. */
  vaultName?: string;
  /** Offer moving it into a vault. */
  canMoveToVault?: boolean;
  /** Offer filing it in a folder. */
  canMoveToFolder?: boolean;
  /** The owner has shared this notebook: how its copy on this device stands, in words. */
  sharedStatus?: string;
}>();
const emit = defineEmits<{
  open: []; rename: []; duplicate: []; export: []; exportPdf: []; print: []; trash: []; recolor: []; moveToVault: []; moveToFolder: []; share: [];
  /** The card came on screen (true) or left it (false); covers on screen are drawn first. */
  visible: [boolean];
}>();

const root = ref<HTMLElement | null>(null);
let seen: IntersectionObserver | null = null;
onMounted(() => {
  if (typeof IntersectionObserver === "undefined" || !root.value) return;
  // a little ahead of the scroll, so the next row is drawn before it shows
  seen = new IntersectionObserver((entries) => {
    for (const e of entries) emit("visible", e.isIntersecting);
  }, { rootMargin: "200px 0px" });
  seen.observe(root.value);
});
onBeforeUnmount(() => {
  seen?.disconnect();
  emit("visible", false);
});
const updated = computed(() => {
  const d = new Date(props.nb.updatedAt);
  const days = Math.floor((Date.now() - d.getTime()) / 86400000);
  if (days < 1) return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (days < 7) return d.toLocaleDateString([], { weekday: "long" });
  return d.toLocaleDateString();
});
</script>

<template>
  <article ref="root" class="group relative flex flex-col">
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
        <h3 class="flex items-center gap-1 text-sm font-medium" :title="nb.title">
          <Lock v-if="vaultName !== undefined" :size="12" class="shrink-0 text-muted" role="img" :aria-label="`In ${vaultName}`" />
          <span class="truncate">{{ nb.title }}</span>
        </h3>
        <p class="text-xs text-muted">{{ nb.pageCount }} page{{ nb.pageCount === 1 ? '' : 's' }} · {{ updated }}</p>
        <p v-if="sharedStatus" class="mt-0.5 flex items-center gap-1 text-xs text-muted" data-testid="shared-badge">
          <Users :size="12" class="shrink-0" aria-hidden="true" /><span class="truncate">Shared · {{ sharedStatus }}</span>
        </p>
      </div>
      <Menu :label="`${nb.title} actions`">
        <template #trigger="{ toggle, open }">
          <button class="touch-target -mr-2 -mt-1 inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" :aria-expanded="open" aria-haspopup="menu" :aria-label="`More actions for ${nb.title}`" @click="toggle">
            <EllipsisVertical :size="18" />
          </button>
        </template>
        <MenuItem @click="emit('rename')"><SquarePen :size="16" />Rename</MenuItem>
        <MenuItem @click="emit('duplicate')"><Copy :size="16" />Duplicate</MenuItem>
        <MenuItem @click="emit('recolor')"><Palette :size="16" />Change cover colour</MenuItem>
        <MenuItem @click="emit('share')"><Users :size="16" />Share notebook…</MenuItem>
        <MenuItem @click="emit('export')"><Download :size="16" />Export .penroam…</MenuItem>
        <MenuItem @click="emit('exportPdf')"><FileDown :size="16" />Export annotated PDF…</MenuItem>
        <MenuItem @click="emit('print')"><Printer :size="16" />Print…</MenuItem>
        <MenuItem v-if="canMoveToFolder" @click="emit('moveToFolder')"><FolderInput :size="16" />Move to folder…</MenuItem>
        <MenuItem v-if="canMoveToVault" @click="emit('moveToVault')"><Lock :size="16" />Move to vault…</MenuItem>
        <MenuItem danger @click="emit('trash')"><Trash :size="16" />Move to trash</MenuItem>
      </Menu>
    </div>
  </article>
</template>
