<script setup lang="ts">
import { Cloud, LoaderCircle } from "@lucide/vue";
import type { ErasedNotebook } from "../platform/sync";

defineProps<{ nb: ErasedNotebook; downloading: boolean }>();
const emit = defineEmits<{ download: [] }>();
</script>

<template>
  <article class="flex flex-col">
    <button
      class="relative block aspect-[17/22] w-full overflow-hidden rounded-xl border border-dashed border-line text-left transition hover:-translate-y-0.5 hover:shadow-md focus-visible:-translate-y-0.5 disabled:translate-y-0 disabled:shadow-none epaper:border-2 epaper:border-black"
      :aria-label="downloading ? `Downloading ${nb.title}` : `Download ${nb.title}`"
      :disabled="downloading"
      @click="emit('download')"
    >
      <div class="absolute inset-y-0 left-0 w-3 opacity-40" :style="{ background: nb.cover }" aria-hidden="true" />
      <div class="flex h-full w-full items-center justify-center pl-3 text-muted">
        <LoaderCircle v-if="downloading" :size="32" class="animate-spin" />
        <Cloud v-else :size="32" />
      </div>
    </button>
    <div class="mt-2 min-w-0">
      <h3 class="truncate text-sm font-medium text-muted" :title="nb.title">{{ nb.title }}</h3>
      <p class="text-xs text-muted">{{ nb.pageCount }} page{{ nb.pageCount === 1 ? "" : "s" }} · {{ downloading ? "Downloading…" : "Not on this device" }}</p>
    </div>
  </article>
</template>
