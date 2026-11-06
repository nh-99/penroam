<script setup lang="ts">
// What happened to each imported file. Other formats never come across perfectly, so this
// says what was kept and what wasn't rather than claiming success.
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import type { ImportResult } from "../stores/library";

defineProps<{ results: ImportResult[] }>();
const emit = defineEmits<{ close: [] }>();

function keptText(r: ImportResult): string {
  const k = r.kept;
  if (!k) return "";
  const part = (n: number, one: string) => (n ? `${n} ${one}${n === 1 ? "" : "s"}` : "");
  return [part(k.pages, "page"), part(k.strokes, "stroke"), part(k.textBlocks, "text block"), part(k.images, "image")].filter(Boolean).join(", ");
}
</script>

<template>
  <Modal title="Import results" wide @close="emit('close')">
    <ul class="max-h-[60dvh] space-y-3 overflow-y-auto">
      <li v-for="r in results" :key="r.file" class="rounded-xl border border-line p-3">
        <p class="truncate text-sm font-medium">{{ r.file }}</p>
        <p v-if="r.error" class="mt-1 text-sm text-danger" role="alert">Not imported: {{ r.error }}</p>
        <template v-else>
          <p class="mt-1 text-xs text-muted">{{ r.format }}, now the notebook “{{ r.title }}”. {{ keptText(r) }}</p>
          <ul v-if="r.lost.length" class="mt-2 list-inside list-disc space-y-1 text-xs">
            <li v-for="l in r.lost" :key="l">{{ l }}</li>
          </ul>
        </template>
      </li>
    </ul>
    <template #actions>
      <Btn variant="primary" autofocus @click="emit('close')">Done</Btn>
    </template>
  </Modal>
</template>
