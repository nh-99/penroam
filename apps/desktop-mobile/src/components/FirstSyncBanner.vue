<script setup lang="ts">
// Shown over the library while this device's first sync runs. That sync can download the whole
// library and take minutes, and until it finishes the shelf may look empty or half filled, so
// this says the app is working. It goes away by itself when the first sync succeeds; a problem
// (offline, an error) is shown by the sync status instead.
import { computed } from "vue";
import { RefreshCw } from "@lucide/vue";
import { useSync } from "../stores/sync";
import { isFirstSync } from "../platform/sync";

const sync = useSync();
const show = computed(() => sync.available && sync.configured && isFirstSync(sync.status));
</script>

<template>
  <div v-if="show" class="flex items-center gap-3 border-b border-line bg-surface-2 px-4 py-2.5 text-sm sm:px-6 epaper:border-black" role="status" data-testid="first-sync">
    <RefreshCw :size="18" class="shrink-0 text-muted motion-safe:animate-spin" aria-hidden="true" />
    <p class="min-w-0 flex-1 leading-snug">
      <span class="font-medium">Syncing for the first time.</span>
      Your notebooks are on their way to this device; this can take a few minutes. You can keep working meanwhile.
    </p>
  </div>
</template>
