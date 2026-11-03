<script setup lang="ts">
// Compact sync state for the editor header. Never green unless everything is
// really up to date. Hidden when sync is not set up.
import { computed } from "vue";
import { CloudCheck, CloudUpload, CloudOff, CloudAlert, Cloud } from "@lucide/vue";
import { useSync } from "../stores/sync";
import { describeStatus, statusIcon } from "../platform/sync";

const emit = defineEmits<{ open: [] }>();
const sync = useSync();
const icon = computed(() => statusIcon(sync.status));
const label = computed(() => {
  const s = sync.status;
  const base = describeStatus(s).text;
  return s && s.conflicts > 0 ? `${base} · ${s.conflicts} conflict(s)` : base;
});
</script>

<template>
  <button v-if="sync.available && sync.configured" type="button" class="touch-target inline-flex items-center gap-1 rounded-xl px-1.5 text-xs hover:bg-surface-2" :aria-label="`Sync: ${label}`" :title="label" @click="emit('open')">
    <CloudCheck v-if="icon === 'check'" :size="18" class="text-ok" />
    <template v-else-if="icon === 'upload'"><CloudUpload :size="18" class="text-muted" /><span class="tabular-nums text-muted">{{ sync.status?.queued }}</span></template>
    <CloudOff v-else-if="icon === 'off'" :size="18" class="text-muted" />
    <CloudAlert v-else-if="icon === 'alert'" :size="18" class="text-danger" />
    <Cloud v-else :size="18" class="text-muted" />
  </button>
</template>
