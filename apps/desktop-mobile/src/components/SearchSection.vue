<script setup lang="ts">
// Settings → Search: the local index, handwriting recognition and its power controls. Choosing
// a recognizer and managing the handwriting model are in AdvancedSection.vue.
import { computed, onMounted } from "vue";
import { RefreshCw } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import { useSearch } from "../stores/search";
import { useUi } from "../stores/ui";
import { describeState, formatBytes, recognizerSummary, searchApi } from "../platform/search";

const search = useSearch();
const ui = useUi();
onMounted(() => void search.init());
const st = computed(() => search.status);
const s = computed(() => st.value?.settings);

async function set(patch: Parameters<typeof search.update>[0]) {
  try {
    await search.update(patch);
  } catch (e: any) {
    ui.error(`Could not change search settings: ${e?.message ?? e}`);
  }
}

async function rebuild() {
  try {
    await searchApi.rebuild();
    ui.info("Rebuilding the search index in the background.");
  } catch (e: any) {
    ui.error(`Could not rebuild the index: ${e?.message ?? e}`);
  }
}

const usage = computed(() => (st.value ? recognizerSummary(st.value) : null));
const summary = computed(() => {
  const x = st.value;
  if (!x || x.state === "off") return null;
  const parts = [`${x.stats.pages} page${x.stats.pages === 1 ? "" : "s"} indexed`];
  if (x.stats.inkEntries) parts.push(`${x.stats.inkEntries} handwritten line${x.stats.inkEntries === 1 ? "" : "s"}`);
  if (x.stats.corrections) parts.push(`${x.stats.corrections} correction${x.stats.corrections === 1 ? "" : "s"}`);
  parts.push(formatBytes(x.indexBytes));
  return parts.join(" · ");
});
</script>

<template>
  <section v-if="st && s" class="space-y-2">
    <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
      <span><span class="block text-sm">Search index</span><span class="block text-xs text-muted">A private index on this device for searching titles, typed text, PDF text and handwriting. Turning it off deletes the index; your notes are not affected.</span></span>
      <input type="checkbox" class="h-5 w-5" :checked="s.enabled" @change="set({ enabled: ($event.target as HTMLInputElement).checked })" />
    </label>
    <template v-if="s.enabled">
      <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3" :class="!st.recognizer ? 'opacity-60' : ''">
        <span>
          <span class="block text-sm">Recognize handwriting</span>
          <span v-if="usage" class="block text-xs text-muted">{{ usage }} Your ink is never changed or uploaded.</span>
          <span v-else class="block text-xs text-muted">This device has no built-in handwriting recognizer. Add the Penroam handwriting model under Advanced to search your handwriting.</span>
        </span>
        <input type="checkbox" class="h-5 w-5" :disabled="!st.recognizer" :checked="s.handwriting && !!st.recognizer" @change="set({ handwriting: ($event.target as HTMLInputElement).checked })" />
      </label>
      <label v-if="st.recognizer && s.handwriting && st.recognizer.languages.length > 1" class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
        <span class="text-sm">Handwriting language</span>
        <select class="max-w-[60%] rounded-lg border border-line bg-surface px-2 py-1 text-sm" :value="st.language ?? ''" @change="set({ language: ($event.target as HTMLSelectElement).value })">
          <option v-for="l in st.recognizer.languages" :key="l" :value="l">{{ l }}</option>
        </select>
      </label>
      <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
        <span><span class="block text-sm">Pause indexing</span><span class="block text-xs text-muted">Search keeps working with what is already indexed</span></span>
        <input type="checkbox" class="h-5 w-5" :checked="s.paused" @change="set({ paused: ($event.target as HTMLInputElement).checked })" />
      </label>
      <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
        <span>
          <span class="block text-sm">Wait while on battery</span>
          <span class="block text-xs text-muted">
            <template v-if="st.onBattery === null">This device does not report its power source, so indexing always runs.</template>
            <template v-else>Indexes only while charging. Now: {{ st.onBattery ? "on battery" : "on power" }}.</template>
          </span>
        </span>
        <input type="checkbox" class="h-5 w-5" :checked="s.pauseOnBattery" @change="set({ pauseOnBattery: ($event.target as HTMLInputElement).checked })" />
      </label>
      <div class="flex items-center justify-between gap-3 px-1">
        <p class="text-xs text-muted" aria-live="polite">{{ describeState(st) }}<template v-if="summary"><br />{{ summary }}</template></p>
        <Btn variant="outline" class="shrink-0" @click="rebuild"><RefreshCw :size="16" />Rebuild</Btn>
      </div>
      <p v-if="st.lastError && st.state !== 'error'" class="px-1 text-xs text-danger">Last problem: {{ st.lastError }}</p>
    </template>
  </section>
</template>
