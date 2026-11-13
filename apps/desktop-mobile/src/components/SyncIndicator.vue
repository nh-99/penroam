<script setup lang="ts">
// Compact sync state for the editor header. Never green unless everything is
// really up to date. Hidden when sync is not set up.
import { computed } from "vue";
import { CloudCheck, CloudUpload, CloudOff, CloudAlert, Cloud, RefreshCw } from "@lucide/vue";
import { useSync } from "../stores/sync";
import { describeStatus, statusIcon } from "../platform/sync";

// `live`: the live channel's words for a shared notebook ("Live", "Live disconnected — …").
const props = defineProps<{ live?: string | null }>();
const emit = defineEmits<{ open: [] }>();
const sync = useSync();
const icon = computed(() => statusIcon(sync.status));
const label = computed(() => {
  const s = sync.status;
  const base = describeStatus(s).text;
  const withConflicts = s && s.conflicts > 0 ? `${base} · ${s.conflicts} conflict(s)` : base;
  return props.live ? `${withConflicts} · ${props.live}` : withConflicts;
});
// a closed channel is a warning; a live one is not a promise that anything was saved
const liveDown = computed(() => !!props.live && props.live.startsWith("Live disconnected"));
</script>

<template>
  <button v-if="sync.available && sync.configured" type="button" class="touch-target inline-flex items-center gap-1 rounded-xl px-1.5 text-xs hover:bg-surface-2" :aria-label="`Sync: ${label}`" :title="label" @click="emit('open')">
    <CloudCheck v-if="icon === 'check'" :size="18" class="text-ok" />
    <template v-else-if="icon === 'upload'"><CloudUpload :size="18" class="text-muted" /><span class="tabular-nums text-muted">{{ sync.status?.queued }}</span></template>
    <CloudOff v-else-if="icon === 'off'" :size="18" class="text-muted" />
    <CloudAlert v-else-if="icon === 'alert'" :size="18" class="text-danger" />
    <!-- motion says the app is working, not stuck (reduced-motion users get the label) -->
    <template v-else-if="icon === 'syncing'"><RefreshCw :size="18" class="text-muted motion-safe:animate-spin" data-testid="sync-spinner" /><span v-if="sync.status?.queued" class="tabular-nums text-muted">{{ sync.status.queued }}</span></template>
    <Cloud v-else :size="18" class="text-muted" />
    <span v-if="live" class="hidden whitespace-nowrap sm:inline" :class="liveDown ? 'text-warn' : 'text-muted'" data-testid="live-status">{{ live }}</span>
  </button>
</template>
