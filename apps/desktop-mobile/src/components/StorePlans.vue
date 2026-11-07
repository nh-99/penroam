<script setup lang="ts">
// Buying a plan through the App Store. Apple requires the title, length and price, the auto-renewal
// terms, links to the terms and privacy policy, and a way to restore purchases, next to the button.
// The backend only fills `storeOffers` from what the store can really sell on this device.
import { computed } from "vue";
import { CreditCard, ExternalLink, RotateCcw } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import { diagnosticsApi } from "../platform/diagnostics";
import { formatBytes, type StoreOffer } from "../platform/sync";
import { useSync } from "../stores/sync";

const sync = useSync();
const a = computed(() => sync.account);

function priceLine(o: StoreOffer) {
  return `${formatBytes(o.quotaBytes)} of cloud storage · ${o.price}${o.period ? ` per ${o.period}` : ""}`;
}

function open(page: "terms" | "privacy") {
  void diagnosticsApi.openHelp(page);
}
</script>

<template>
  <div v-if="a?.storePurchases" class="space-y-2 pt-1">
    <ul v-if="a.storeOffers.length" class="space-y-2">
      <li v-for="o in a.storeOffers" :key="o.id" class="flex flex-wrap items-center gap-3 rounded-xl border border-line p-3 epaper:border-2 epaper:border-black">
        <div class="min-w-0 flex-1">
          <p class="text-sm font-semibold">{{ o.name }}</p>
          <p class="text-xs text-muted">{{ priceLine(o) }}</p>
        </div>
        <Btn variant="primary" :disabled="sync.accountBusy" @click="sync.buyInStore(o.id)"><CreditCard :size="16" />Subscribe</Btn>
      </li>
    </ul>
    <p v-if="a.storeOffers.length" class="text-xs leading-relaxed text-muted">
      Your Apple account is charged when you confirm. The plan renews automatically until you cancel it in your App Store settings, at least 24 hours before the period ends.
    </p>
    <p v-if="sync.storeMessage" class="text-xs leading-relaxed" role="status">{{ sync.storeMessage }}</p>
    <div class="flex flex-wrap gap-2">
      <Btn variant="ghost" :disabled="sync.accountBusy" @click="sync.restorePurchases()"><RotateCcw :size="16" />Restore purchases</Btn>
      <Btn variant="ghost" @click="open('terms')"><ExternalLink :size="16" />Terms of use</Btn>
      <Btn variant="ghost" @click="open('privacy')"><ExternalLink :size="16" />Privacy</Btn>
    </div>
  </div>
</template>
