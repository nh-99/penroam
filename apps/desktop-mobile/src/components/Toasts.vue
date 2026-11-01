<script setup lang="ts">
import { X, CircleCheck, TriangleAlert, Info } from "@lucide/vue";
import { useUi } from "../stores/ui";
const ui = useUi();
</script>

<template>
  <div class="pointer-events-none fixed inset-x-0 bottom-0 z-[60] flex flex-col items-center gap-2 p-4 safe-bottom" aria-live="polite" role="status">
    <div
      v-for="t in ui.toasts"
      :key="t.id"
      class="pointer-events-auto flex max-w-lg items-start gap-3 rounded-xl border border-line bg-surface px-4 py-3 text-sm shadow-lg epaper:border-2"
      :role="t.kind === 'error' ? 'alert' : undefined"
    >
      <TriangleAlert v-if="t.kind === 'error'" :size="18" class="mt-0.5 shrink-0 text-danger" />
      <CircleCheck v-else-if="t.kind === 'success'" :size="18" class="mt-0.5 shrink-0 text-ok" />
      <Info v-else :size="18" class="mt-0.5 shrink-0 text-muted" />
      <span class="flex-1 leading-snug">{{ t.text }}</span>
      <button v-if="t.action" class="shrink-0 font-semibold text-accent underline-offset-2 hover:underline" @click="t.action.run(); ui.dismiss(t.id)">{{ t.action.label }}</button>
      <button class="-m-1 shrink-0 rounded p-1 text-muted hover:bg-surface-2" aria-label="Dismiss" @click="ui.dismiss(t.id)"><X :size="16" /></button>
    </div>
  </div>
</template>
