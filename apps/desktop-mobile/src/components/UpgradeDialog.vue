<script setup lang="ts">
// Plan picker for Penroam Cloud (desktop builds only; the store never opens it otherwise).
// Prices and storage come from the server as display text; choosing one opens the server's
// checkout in the system browser, and the plan shows up here when the window regains focus.
import { computed } from "vue";
import { CreditCard, RefreshCw } from "@lucide/vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { useSync } from "../stores/sync";
import { NEARLY_FULL, formatBytes, usageFraction } from "../platform/sync";

const sync = useSync();
const a = computed(() => sync.account);
const full = computed(() => !!a.value && !a.value.writable && a.value.readOnlyReason === "over_quota");
const usage = computed(() => {
  const x = a.value;
  if (!x || x.quotaBytes == null) return "";
  return `${formatBytes(x.usedBytes)} of ${formatBytes(x.quotaBytes)} used (${Math.floor((usageFraction(x) ?? 0) * 100)}%)`;
});
</script>

<template>
  <Modal v-if="a" title="More cloud storage" @close="sync.upgradeOpen = false">
    <p class="text-sm leading-relaxed text-muted">
      <template v-if="full">Your cloud storage is full, so new changes wait on this device instead of syncing.</template>
      <template v-else-if="(usageFraction(a) ?? 0) >= NEARLY_FULL">Your cloud storage is almost full.</template>
      {{ usage }}.
    </p>
    <ul class="mt-4 space-y-2">
      <li v-for="o in a.offers" :key="o.id" class="flex flex-wrap items-center gap-3 rounded-xl border border-line p-3 epaper:border-2">
        <div class="min-w-0 flex-1">
          <p class="text-sm font-semibold">{{ o.name }}</p>
          <p class="text-xs text-muted">{{ formatBytes(o.quotaBytes) }} of cloud storage · {{ o.priceLabel }}</p>
        </div>
        <Btn variant="primary" :disabled="sync.accountBusy" @click="sync.openBilling('subscribe', o.id)"><CreditCard :size="16" />Choose</Btn>
      </li>
    </ul>
    <p class="mt-3 text-xs leading-relaxed text-muted">
      Opens the payment page in your browser. Your notes, editing and export work the same on every plan, and nothing is deleted if a plan ends.
    </p>
    <p v-if="sync.accountError" class="mt-2 text-xs text-danger" role="alert">{{ sync.accountError }}</p>
    <template #actions>
      <Btn variant="ghost" :disabled="sync.accountBusy" @click="sync.refreshAccount()"><RefreshCw :size="16" />I've paid, check again</Btn>
      <Btn autofocus @click="sync.upgradeOpen = false">Not now</Btn>
    </template>
  </Modal>
</template>
