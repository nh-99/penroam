<script setup lang="ts">
import { CircleCheck, LoaderCircle, TriangleAlert, Circle } from "@lucide/vue";
import type { SaveState } from "../editor/session";
defineProps<{ state: SaveState; error: string | null }>();
const emit = defineEmits<{ retry: [] }>();
</script>

<template>
  <div class="flex items-center gap-1.5 text-xs" role="status" aria-live="polite">
    <template v-if="state === 'saved'"><CircleCheck :size="15" class="text-ok" /><span class="hidden text-muted sm:inline">Saved</span><span class="sr-only sm:hidden">Saved</span></template>
    <template v-else-if="state === 'saving'"><LoaderCircle :size="15" class="animate-spin text-muted" /><span class="hidden text-muted sm:inline">Saving…</span><span class="sr-only sm:hidden">Saving</span></template>
    <template v-else-if="state === 'dirty'"><Circle :size="15" class="text-warn" /><span class="hidden text-muted sm:inline">Unsaved</span><span class="sr-only sm:hidden">Unsaved changes</span></template>
    <template v-else>
      <TriangleAlert :size="15" class="text-danger" />
      <span class="max-w-48 truncate text-danger" :title="error ?? ''">Not saved</span>
      <button class="rounded-md px-1.5 py-0.5 font-semibold text-brand hover:bg-surface-2" @click="emit('retry')">Retry</button>
    </template>
  </div>
</template>
