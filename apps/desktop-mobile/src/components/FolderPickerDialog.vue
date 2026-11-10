<script setup lang="ts">
// Chooses a destination folder for a notebook or another folder. When moving a folder, the folder
// and everything inside it are left out, because the backend refuses moving one inside itself.
import { computed, ref } from "vue";
import Btn from "./ui/Btn.vue";
import Modal from "./ui/Modal.vue";
import { useLibrary } from "../stores/library";
import type { Folder } from "../generated/Folder";

const props = defineProps<{
  title: string;
  /** Where the item is now; `null` is the top level. */
  current: string | null;
  /** The folder being moved, if it is a folder. */
  moving?: string;
}>();
const emit = defineEmits<{ close: []; choose: [folder: string | null] }>();
const lib = useLibrary();
const chosen = ref<string | null>(props.current);

/** Depth-first, so each folder sits right under its parent. */
const rows = computed(() => {
  const out: { folder: Folder; depth: number }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const folder of lib.folders) {
      if ((folder.parent ?? null) !== parent || folder.id === props.moving) continue;
      out.push({ folder, depth });
      walk(folder.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
});
</script>

<template>
  <Modal :title="title" @close="emit('close')">
    <form class="space-y-4" @submit.prevent="emit('choose', chosen)">
      <div class="max-h-72 overflow-y-auto rounded-xl border border-line epaper:border-2" role="radiogroup" aria-label="Destination">
        <label class="flex items-center gap-2 px-3 py-2.5 text-sm">
          <input v-model="chosen" type="radio" name="folder" :value="null" />
          All notebooks (top level)
        </label>
        <label v-for="r in rows" :key="r.folder.id" class="flex items-center gap-2 py-2.5 pr-3 text-sm" :style="{ paddingLeft: `${0.75 + (r.depth + 1) * 1.25}rem` }">
          <input v-model="chosen" type="radio" name="folder" :value="r.folder.id" />
          <span class="truncate">{{ r.folder.name }}</span>
        </label>
      </div>
      <div class="flex justify-end gap-2">
        <Btn @click="emit('close')">Cancel</Btn>
        <Btn variant="primary" type="submit" :disabled="chosen === current">Move here</Btn>
      </div>
    </form>
  </Modal>
</template>
