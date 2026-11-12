<script setup lang="ts">
// Penroam Cloud plan comparison, opened from the storage banner and from Settings -> Sync.
// Plans differ only in storage and price; the features list below the cards is shared by all.
// Desktop prices come from the server as display text and choosing one opens the server's checkout
// in the system browser (the plan shows up when the window regains focus). Store builds buy
// through the app store instead, so they also carry the disclosures Apple requires next to the
// subscribe button: title, length, price, auto-renewal terms, terms and privacy links, restore.
import { computed } from "vue";
import { Check, CreditCard, ExternalLink, RefreshCw, RotateCcw } from "@lucide/vue";
import Modal from "./ui/Modal.vue";
import Btn from "./ui/Btn.vue";
import { useSync } from "../stores/sync";
import { diagnosticsApi } from "../platform/diagnostics";
import { SHARED_PLAN_FEATURES } from "../platform/planFeatures";
import { NEARLY_FULL, formatBytes, planTiers, usageFraction, usesStore } from "../platform/sync";

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

    <!-- stacked on phones, side by side from the small breakpoint; never scrolls sideways -->
    <ul class="mt-4 grid gap-3 sm:grid-cols-[repeat(auto-fit,minmax(10rem,1fr))]" aria-label="Plans">
      <li
        v-for="t in tiers"
        :key="t.free ? 'free' : t.quotaBytes!"
        class="flex min-w-0 flex-col gap-2 rounded-xl border p-4 epaper:border-2 epaper:border-black"
        :class="t.current ? 'border-accent bg-surface-2' : 'border-line'"
      >
        <div class="flex flex-wrap items-center gap-2">
          <h3 class="text-xl font-semibold">{{ t.quotaBytes == null ? "Free" : formatBytes(t.quotaBytes) }}</h3>
          <span v-if="t.current" class="inline-flex items-center gap-1 rounded-full border border-current px-2 py-0.5 text-xs font-medium epaper:border-black epaper:font-semibold">
            <Check :size="12" />Current plan
          </span>
        </div>
        <p class="text-xs text-muted">{{ t.free ? "Free cloud storage" : "Cloud storage" }}</p>
        <p v-if="t.free && t.quotaBytes != null" class="text-sm font-medium">Free</p>
        <div v-else-if="!t.free" class="mt-auto flex flex-col gap-2 pt-1">
          <div v-for="p in t.prices" :key="p.id" class="flex flex-col gap-1">
            <p v-if="p.name" class="text-sm font-semibold">{{ p.name }}</p>
            <Btn variant="primary" :disabled="sync.accountBusy" @click="choose(p.id)">
              <CreditCard :size="16" />{{ usesStore(a) ? "Subscribe" : "Choose" }}<span class="sr-only"> {{ formatBytes(t.quotaBytes!) }},</span> · {{ p.label }}
            </Btn>
          </div>
        </div>
      </li>
    </ul>

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
    <h3 class="mt-5 text-sm font-semibold">Included in every plan</h3>
    <ul class="mt-2 space-y-1.5">
      <li v-for="f in SHARED_PLAN_FEATURES" :key="f.title" class="flex gap-2 text-sm leading-relaxed">
        <Check :size="16" class="mt-0.5 shrink-0" aria-hidden="true" />
        <span><span class="font-medium">{{ f.title }}.</span> <span class="text-muted">{{ f.detail }}</span></span>
      </li>
    </ul>

    <template #actions>
      <Btn v-if="!usesStore(a)" variant="ghost" :disabled="sync.accountBusy" @click="sync.refreshAccount()"><RefreshCw :size="16" />I've paid, check again</Btn>
      <Btn autofocus @click="sync.upgradeOpen = false">Not now</Btn>
    </template>
  </Modal>
</template>
