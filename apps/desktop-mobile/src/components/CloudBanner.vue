<script setup lang="ts">
// Storage and payment prompt for Penroam Cloud, shown over the library and the editor:
// storage nearly full, storage full (uploads paused), or a failed payment. On desktop builds
// it links to the plan picker or the billing portal; mobile builds show the facts only.
// Closing it hides that kind of prompt until the app restarts.
import { CreditCard, HardDrive, TriangleAlert, X } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import { useSync } from "../stores/sync";

defineProps<{ compact?: boolean }>();
const sync = useSync();
</script>

<template>
  <div
    v-if="sync.notice"
    class="flex flex-wrap items-center gap-x-3 gap-y-1.5 border-b text-sm epaper:border-black"
    :class="[
      compact ? 'px-3 py-1.5' : 'px-4 py-2.5 sm:px-6',
      sync.notice.kind === 'nearlyFull' ? 'border-line bg-surface-2' : 'border-warn/40 bg-warn/10',
    ]"
    :role="sync.notice.kind === 'nearlyFull' ? 'status' : 'alert'"
  >
    <TriangleAlert v-if="sync.notice.kind !== 'nearlyFull'" :size="18" class="shrink-0 text-warn" aria-hidden="true" />
    <HardDrive v-else :size="18" class="shrink-0 text-muted" aria-hidden="true" />
    <p class="min-w-0 flex-1 leading-snug">{{ sync.notice.text }}</p>
    <div class="flex items-center gap-1">
      <Btn v-if="sync.notice.canUpgrade" variant="primary" @click="sync.showUpgrade()"><HardDrive :size="16" />Get more storage</Btn>
      <Btn v-if="sync.notice.canManage" variant="primary" :disabled="sync.accountBusy" @click="sync.openBilling('manage_billing')">
        <CreditCard :size="16" />Update payment method
      </Btn>
      <button class="touch-target inline-flex items-center justify-center rounded-lg text-muted hover:bg-surface-2" aria-label="Hide this message" @click="sync.dismissNotice(sync.notice.kind)">
        <X :size="18" />
      </button>
    </div>
  </div>
</template>
