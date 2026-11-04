<script setup lang="ts">
// Plan and storage of the synced account (servers with account.v1, i.e. Penroam Cloud).
// Nothing here gates a local feature: notes, editing and export work the same on every plan.
// Mobile builds get no offers and no billing links from the backend, so they show the plan
// and storage only.
import { computed, onMounted, ref } from "vue";
import { CreditCard, HardDrive, RefreshCw, Trash2, Undo2 } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import { useSync } from "../stores/sync";
import { describePlan, describeReadOnly, formatBytes, usageFraction } from "../platform/sync";

const sync = useSync();
const deleting = ref(false);

onMounted(() => void sync.refreshAccount());

const a = computed(() => sync.account);
const fraction = computed(() => (a.value ? usageFraction(a.value) : null));
const usage = computed(() => {
  const x = a.value;
  if (!x) return "";
  return x.quotaBytes == null ? `${formatBytes(x.usedBytes)} used` : `${formatBytes(x.usedBytes)} of ${formatBytes(x.quotaBytes)} used`;
});
const readOnly = computed(() => (a.value ? describeReadOnly(a.value) : null));
const eraseDay = computed(() =>
  a.value?.deletionEraseAfterMs ? new Date(a.value.deletionEraseAfterMs).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" }) : "",
);
const unsynced = computed(() => sync.status?.queued ?? 0);
const deleteMessage = computed(
  () =>
    "Your account and its encrypted cloud copy are erased after a waiting period; until then you can change your mind here. " +
    "Notes on this device and your other devices are not deleted, and sync stops. A paid plan stops renewing. " +
    "You will sign in again in your browser to confirm." +
    (unsynced.value > 0 ? ` ${unsynced.value} change(s) on this device were not uploaded yet; they stay on this device.` : ""),
);

async function confirmDelete() {
  deleting.value = false;
  await sync.requestDeletion();
}
</script>

<template>
  <div v-if="a" class="space-y-2 rounded-xl border border-line p-3" aria-labelledby="cloud-h">
    <h4 id="cloud-h" class="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-muted"><HardDrive :size="14" />Penroam Cloud</h4>
    <p class="text-sm">{{ describePlan(a) }}</p>
    <div>
      <div
        v-if="fraction !== null"
        class="h-2 overflow-hidden rounded-full bg-surface-2 epaper:border epaper:border-black"
        role="progressbar"
        :aria-valuenow="Math.round(fraction * 100)"
        aria-valuemin="0"
        aria-valuemax="100"
        aria-label="Cloud storage used"
      >
        <div class="h-full rounded-full" :class="fraction >= 1 ? 'bg-warn' : 'bg-accent'" :style="{ width: `${Math.max(2, fraction * 100)}%` }" />
      </div>
      <p class="mt-1 text-xs text-muted">{{ usage }}</p>
    </div>
    <p v-if="readOnly" class="rounded-lg bg-surface-2 p-2 text-xs leading-relaxed epaper:border" role="status">{{ readOnly }}</p>
    <p v-if="a.planStatus === 'past_due'" class="text-xs leading-relaxed text-warn">
      Sync keeps working until then. Afterwards your account falls back to free storage; nothing is deleted.
    </p>
    <p v-if="a.planStatus === 'canceling'" class="text-xs leading-relaxed text-muted">
      After that date your account has free storage again. Nothing is deleted; if you store more than that, uploads pause until you make room.
    </p>

    <!-- desktop builds only: the backend sends no offers or billing links on mobile -->
    <div v-if="a.offers.length" class="space-y-1.5 pt-1">
      <div v-for="o in a.offers" :key="o.id" class="flex flex-wrap items-center gap-2">
        <Btn variant="primary" :disabled="sync.accountBusy" @click="sync.openBilling('subscribe', o.id)">
          <CreditCard :size="16" />{{ o.name }}: {{ formatBytes(o.quotaBytes) }}, {{ o.priceLabel }}
        </Btn>
      </div>
      <p class="text-xs text-muted">Opens the payment page in your browser. Sync and local notes keep working whatever you choose.</p>
    </div>
    <Btn v-if="a.canManageBilling" variant="outline" :disabled="sync.accountBusy" @click="sync.openBilling('manage_billing')">
      <CreditCard :size="16" />Manage plan and billing
    </Btn>

    <div v-if="a.canCancelDeletion" class="space-y-1.5 rounded-lg border border-danger/40 p-2">
      <p class="text-xs leading-relaxed">This account is scheduled for deletion on {{ eraseDay }}. Download or export anything you need before then; notes on your devices are not deleted.</p>
      <Btn variant="outline" :disabled="sync.accountBusy" @click="sync.cancelDeletion()"><Undo2 :size="16" />Keep my account</Btn>
    </div>

    <p v-if="sync.accountError" class="text-xs text-danger" role="alert">{{ sync.accountError }}</p>
    <p v-if="sync.accountBusy && sync.signingIn" class="flex items-center gap-2 text-xs text-muted" role="status">
      <RefreshCw :size="14" class="animate-spin" />Finish signing in in your browser, then come back here.
    </p>
    <div class="flex flex-wrap gap-2 pt-1">
      <Btn variant="ghost" :disabled="sync.accountBusy" @click="sync.refreshAccount()"><RefreshCw :size="16" />Refresh</Btn>
      <Btn v-if="a.canRequestDeletion" variant="ghost" :disabled="sync.accountBusy" @click="deleting = true"><Trash2 :size="16" />Delete account…</Btn>
    </div>

    <ConfirmDialog v-if="deleting" title="Delete your Penroam Cloud account?" danger confirm="Continue in browser" :message="deleteMessage" @close="deleting = false" @confirm="confirmDelete" />
  </div>
</template>
