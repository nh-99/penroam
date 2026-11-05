<script setup lang="ts">
// Settings → Search: the local index, handwriting recognition and its power controls.
import { computed, onMounted, ref } from "vue";
import { RefreshCw, PackagePlus, Trash2 } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import { useSearch } from "../stores/search";
import { useUi } from "../stores/ui";
import { pickFile } from "../platform/files";
import { describeState, formatBytes, searchApi, type ModelSummary } from "../platform/search";

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

// Installing a model: pick the file, show what it is (size, licence), then install.
const pendingModel = ref<{ path: string; model: ModelSummary } | null>(null);
const removingModel = ref(false);
const modelBusy = ref(false);

async function chooseModel() {
  try {
    const f = await pickFile("model");
    if (!f) return;
    pendingModel.value = { path: f.path, model: await searchApi.inspectModel(f.path) };
  } catch (e: any) {
    ui.error(`This is not a Penroam handwriting model: ${e?.message ?? e}`);
  }
}

async function installModel() {
  const p = pendingModel.value;
  pendingModel.value = null;
  if (!p) return;
  modelBusy.value = true;
  try {
    const m = await searchApi.installModel(p.path);
    ui.success(`Installed ${m.name} ${m.version}. Handwriting is being recognized again in the background.`);
  } catch (e: any) {
    ui.error(`The model could not be installed: ${e?.message ?? e}`);
  } finally {
    modelBusy.value = false;
  }
}

async function removeModel() {
  removingModel.value = false;
  const m = st.value?.model;
  if (!m) return;
  modelBusy.value = true;
  try {
    await searchApi.removeModel(m.id);
    ui.success("Handwriting model removed.");
  } catch (e: any) {
    ui.error(`The model could not be removed: ${e?.message ?? e}`);
  } finally {
    modelBusy.value = false;
  }
}

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
    <h3 class="text-sm font-medium">Search</h3>
    <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
      <span><span class="block text-sm">Search index</span><span class="block text-xs text-muted">A private index on this device for searching titles, typed text, PDF text and handwriting. Turning it off deletes the index; your notes are not affected.</span></span>
      <input type="checkbox" class="h-5 w-5" :checked="s.enabled" @change="set({ enabled: ($event.target as HTMLInputElement).checked })" />
    </label>
    <template v-if="s.enabled">
      <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3" :class="!st.recognizer ? 'opacity-60' : ''">
        <span>
          <span class="block text-sm">Recognize handwriting</span>
          <span v-if="st.platform" class="block text-xs text-muted">Uses {{ st.platform.name }} on this device<template v-if="st.model">, with the Penroam handwriting model as a fallback</template>. Your ink is never changed or uploaded.</span>
          <span v-else-if="st.model" class="block text-xs text-muted">Uses the Penroam handwriting model on this device. Your ink is never changed or uploaded.</span>
          <span v-else class="block text-xs text-muted">This device has no built-in handwriting recognizer. Install the Penroam handwriting model below to search your handwriting.</span>
        </span>
        <input type="checkbox" class="h-5 w-5" :disabled="!st.recognizer" :checked="s.handwriting && !!st.recognizer" @change="set({ handwriting: ($event.target as HTMLInputElement).checked })" />
      </label>
      <label v-if="st.recognizer && s.handwriting && st.recognizer.languages.length > 1" class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
        <span class="text-sm">Handwriting language</span>
        <select class="max-w-[60%] rounded-lg border border-line bg-surface px-2 py-1 text-sm" :value="st.language ?? ''" @change="set({ language: ($event.target as HTMLSelectElement).value })">
          <option v-for="l in st.recognizer.languages" :key="l" :value="l">{{ l }}</option>
        </select>
      </label>
      <div class="rounded-xl border border-line p-3">
        <div class="flex items-start justify-between gap-3">
          <span class="min-w-0">
            <span class="block text-sm">Handwriting model</span>
            <template v-if="st.model">
              <span class="block text-xs text-muted">{{ st.model.name }} {{ st.model.version }} · {{ formatBytes(st.model.bytes) }}</span>
              <span class="mt-1 block text-xs text-muted">{{ st.model.license }}</span>
            </template>
            <span v-else class="block text-xs text-muted">Optional. Used where this device has no handwriting recognizer, and as a fallback when it fails. Runs on this device; nothing is downloaded or uploaded.</span>
            <span v-if="st.modelError" class="mt-1 block text-xs text-danger">The installed model could not be loaded: {{ st.modelError }}</span>
          </span>
          <Btn v-if="st.model" variant="ghost" class="shrink-0 text-danger" :disabled="modelBusy" @click="removingModel = true"><Trash2 :size="16" />Remove</Btn>
          <Btn v-else variant="outline" class="shrink-0" :disabled="modelBusy" @click="chooseModel"><PackagePlus :size="16" />Install…</Btn>
        </div>
      </div>
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
    <ConfirmDialog v-if="pendingModel" title="Install handwriting model?" confirm="Install"
      :message="`${pendingModel.model.name} ${pendingModel.model.version} (${formatBytes(pendingModel.model.bytes)}). Licence: ${pendingModel.model.license} It is checked against its checksums, stored on this device and runs only here. Handwriting is recognized again in the background.`"
      @close="pendingModel = null" @confirm="installModel" />
    <ConfirmDialog v-if="removingModel" title="Remove handwriting model?" danger confirm="Remove"
      message="Handwriting recognized by the model drops out of search as pages are re-indexed. Your notes are not affected."
      @close="removingModel = false" @confirm="removeModel" />
  </section>
</template>
