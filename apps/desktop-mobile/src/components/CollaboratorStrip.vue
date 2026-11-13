<script setup lang="ts">
// Who else is in this shared notebook, in the editor header. Each person is a chip with initials,
// a shape and a colour (never colour alone), and a full accessible name with their role and the
// page they are on. People whose identity isn't known yet are "Collaborator".
import { computed } from "vue";
import { peerColor, peerInitials, peerName, peerShape, type PresencePeer } from "../platform/live";
import { roleLabel } from "../platform/sharing";

const props = defineProps<{ peers: PresencePeer[]; pageNumbers: Record<string, number>; currentPageId: string | null; max?: number }>();
const shown = computed(() => props.peers.slice(0, props.max ?? 4));
const extra = computed(() => props.peers.length - shown.value.length);

function where(p: PresencePeer): string {
  const n = props.pageNumbers[p.pageId];
  if (n === undefined) return "on another page";
  return p.pageId === props.currentPageId ? `on this page (page ${n})` : `on page ${n}`;
}
const label = (p: PresencePeer) => `${peerName(p)}, ${roleLabel(p.role)}, ${where(p)}`;
</script>

<template>
  <ul v-if="peers.length" class="flex items-center -space-x-1" aria-label="People in this notebook now" data-testid="collaborators">
    <li v-for="p in shown" :key="p.sessionId" class="relative" :title="label(p)">
      <span
        class="relative inline-flex h-7 min-w-7 items-center justify-center rounded-full border-2 border-surface px-1 text-[11px] font-semibold text-white epaper:border-black epaper:bg-white epaper:text-black"
        :style="{ background: peerColor(p.colorIndex) }"
        role="img"
        :aria-label="label(p)"
      >
        {{ peerInitials(p) }}
        <span class="absolute -bottom-1 -right-1 rounded bg-surface px-0.5 text-[8px] leading-none text-ink" aria-hidden="true">{{ peerShape(p.colorIndex) }}</span>
      </span>
      <span v-if="p.pageId !== currentPageId && pageNumbers[p.pageId] !== undefined" class="sr-only">{{ label(p) }}</span>
    </li>
    <li v-if="extra > 0" class="pl-2 text-xs text-muted" :aria-label="`${extra} more people`">+{{ extra }}</li>
  </ul>
</template>
