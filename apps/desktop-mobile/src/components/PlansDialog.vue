<script setup lang="ts">
// Penroam Cloud plan comparison, opened from the storage banner and from Settings -> Sync.
// It is a table: plans as columns, storage, the features every plan shares, then price with the
// buttons. Plans differ only in storage and price; the feature rows come from planFeatures.ts.
// Desktop prices come from the server as display text and choosing one opens the server's checkout
// in the system browser (the plan shows up when the window regains focus). Store builds buy
// through the app store instead, so they also carry the disclosures Apple requires next to the
// subscribe button: title, length, price, auto-renewal terms, terms and privacy links, restore.
import { computed } from "vue";
import { Check, ExternalLink, RefreshCw, RotateCcw } from "@lucide/vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { useSync } from "../stores/sync";
import { diagnosticsApi } from "../platform/diagnostics";
import { SHARED_PLAN_FEATURES } from "../platform/planFeatures";
import { NEARLY_FULL, formatBytes, planTiers, usageFraction, usesStore, type PlanTier } from "../platform/sync";

const sync = useSync();
const a = computed(() => sync.account);
const tiers = computed(() => (a.value ? planTiers(a.value) : []));
const full = computed(() => !!a.value && !a.value.writable && a.value.readOnlyReason === "over_quota");
const nearlyFull = computed(() => !!a.value && (usageFraction(a.value) ?? 0) >= NEARLY_FULL);
const usage = computed(() => {
  const x = a.value;
  if (!x || x.quotaBytes == null) return "";
  return `${formatBytes(x.usedBytes)} of ${formatBytes(x.quotaBytes)} used (${Math.floor((usageFraction(x) ?? 0) * 100)}%).`;
});

/** Column heading: "Free", or the storage size, which is what tells paid plans apart. */
function tierName(t: PlanTier): string {
  return t.free ? "Free" : formatBytes(t.quotaBytes!);
}

function choose(id: string) {
  // the store build never talks to Stripe and the desktop build has no store
  if (a.value && usesStore(a.value)) void sync.buyInStore(id);
  else void sync.openBilling("subscribe", id);
}

function openPage(page: "terms" | "privacy") {
  void diagnosticsApi.openHelp(page);
}
</script>

<template>
  <Modal v-if="a" title="Penroam Cloud plans" wide @close="sync.upgradeOpen = false">
    <p class="text-sm leading-relaxed text-muted">
      <template v-if="full">Your cloud storage is full, so new changes wait on this device instead of syncing. </template>
      <template v-else-if="nearlyFull">Your cloud storage is almost full. </template>
      {{ usage }}
    </p>

    <!--
      Plans as columns, features as rows. Only storage and price differ, so the shared features show
      a tick in every column. The price row with the buttons comes last so Apple's disclosures, just
      below the table, sit next to Subscribe. The wrapper scrolls on its own if a server ever sends
      more tiers than fit a phone, so the page itself never scrolls sideways.
    -->
    <div class="mt-4 overflow-x-auto rounded-xl border border-line epaper:border-2 epaper:border-black">
      <table class="w-full border-collapse text-sm">
        <caption class="sr-only">Compare Penroam Cloud plans</caption>
        <thead>
          <tr class="border-b border-line epaper:border-black">
            <th scope="col" class="p-3 text-left align-bottom text-xs font-medium text-muted">Plan</th>
            <th
              v-for="t in tiers"
              :key="t.free ? 'free' : t.quotaBytes!"
              scope="col"
              class="min-w-[6.5rem] p-3 text-left align-bottom"
              :class="t.current ? 'bg-surface-2' : ''"
            >
              <span class="block text-base font-semibold">{{ tierName(t) }}</span>
              <span v-if="t.current" class="mt-1 inline-flex items-center gap-1 rounded-full border border-current px-2 py-0.5 text-xs font-medium epaper:border-black epaper:font-semibold">
                <Check :size="12" aria-hidden="true" />Current plan
              </span>
            </th>
          </tr>
        </thead>
        <tbody>
          <tr class="border-b border-line epaper:border-black">
            <th scope="row" class="p-3 text-left align-top font-medium">Cloud storage</th>
            <td v-for="t in tiers" :key="t.free ? 'free' : t.quotaBytes!" class="p-3 align-top font-semibold" :class="t.current ? 'bg-surface-2' : ''">
              {{ t.quotaBytes == null ? "Free allowance" : formatBytes(t.quotaBytes) }}
            </td>
          </tr>
          <tr class="border-b border-line bg-surface-2 epaper:border-black epaper:bg-white">
            <th scope="colgroup" :colspan="tiers.length + 1" class="px-3 py-1.5 text-left text-xs font-semibold uppercase tracking-wide text-muted">Included in every plan</th>
          </tr>
          <tr v-for="f in SHARED_PLAN_FEATURES" :key="f.title" class="border-b border-line epaper:border-black">
            <th scope="row" class="p-3 text-left align-top font-medium">
              {{ f.title }}
              <span class="mt-0.5 block text-xs font-normal leading-snug text-muted">{{ f.detail }}</span>
            </th>
            <td v-for="t in tiers" :key="t.free ? 'free' : t.quotaBytes!" class="p-3 align-top" :class="t.current ? 'bg-surface-2' : ''">
              <Check :size="18" aria-hidden="true" /><span class="sr-only">Included</span>
            </td>
          </tr>
        </tbody>
        <tfoot>
          <tr>
            <th scope="row" class="p-3 text-left align-top font-medium">Price</th>
            <td v-for="t in tiers" :key="t.free ? 'free' : t.quotaBytes!" class="p-3 align-top" :class="t.current ? 'bg-surface-2' : ''">
              <span v-if="t.free" class="font-semibold">Free</span>
              <div v-else class="flex flex-col gap-2">
                <div v-for="p in t.prices" :key="p.id" class="flex flex-col gap-1">
                  <span v-if="p.name" class="text-xs font-semibold">{{ p.name }}</span>
                  <Btn variant="primary" class="w-full px-2! py-1.5" :disabled="sync.accountBusy" @click="choose(p.id)">
                    <span class="flex flex-col items-center leading-tight">
                      <span>{{ usesStore(a) ? "Subscribe" : "Choose" }}<span class="sr-only"> {{ tierName(t) }},</span></span>
                      <span class="text-xs font-normal">{{ p.label }}</span>
                    </span>
                  </Btn>
                </div>
              </div>
            </td>
          </tr>
        </tfoot>
      </table>
    </div>

    <!-- next to the buttons that cause them; the status node stays mounted so screen readers announce changes -->
    <p class="mt-3 text-xs leading-relaxed" role="status">{{ sync.storeMessage ?? "" }}</p>
    <p v-if="sync.accountError" class="mt-2 text-xs text-danger" role="alert">{{ sync.accountError }}</p>

    <p v-if="!usesStore(a)" class="mt-4 text-xs leading-relaxed text-muted">
      Choosing a plan opens the payment page in your browser. Nothing is deleted if a plan ends.
    </p>
    <template v-else>
      <p v-if="a.storeOffers.length" class="mt-4 text-xs leading-relaxed text-muted">
        Your Apple account is charged when you confirm. The plan renews automatically until you cancel it in your App Store settings, at least 24 hours before the period ends.
      </p>
      <div class="mt-3 flex flex-wrap gap-2">
        <Btn variant="ghost" :disabled="sync.accountBusy" @click="sync.restorePurchases()"><RotateCcw :size="16" />Restore purchases</Btn>
        <Btn variant="ghost" @click="openPage('terms')"><ExternalLink :size="16" />Terms of use</Btn>
        <Btn variant="ghost" @click="openPage('privacy')"><ExternalLink :size="16" />Privacy</Btn>
      </div>
    </template>

    <template #actions>
      <Btn v-if="!usesStore(a)" variant="ghost" :disabled="sync.accountBusy" @click="sync.refreshAccount()"><RefreshCw :size="16" />I've paid, check again</Btn>
      <Btn autofocus @click="sync.upgradeOpen = false">Not now</Btn>
    </template>
  </Modal>
</template>
