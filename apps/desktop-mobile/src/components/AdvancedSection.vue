<script setup lang="ts">
// Settings → Advanced: which recognizer reads handwriting, and the Penroam handwriting model
// (download, update, install from a file, remove). The defaults need none of this, which is why
// it is not under Search. Downloads are carried out by the backend (src-tauri/src/models.rs).
// On the Linux AppImage it also holds the app-menu entry (DesktopEntryRow).
import { computed, onMounted, ref } from "vue";
import { Download, PackagePlus, RefreshCw, Trash2, X } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import ConfirmDialog from "./ConfirmDialog.vue";
import DesktopEntryRow from "./DesktopEntryRow.vue";
import { useSearch } from "../stores/search";
import { useUi } from "../stores/ui";
import { pickFile } from "../platform/files";
import { modelToOffer, modelsApi } from "../platform/models";
import { formatBytes, searchApi, type ModelSummary, type RecognizerChoice } from "../platform/search";

const search = useSearch();
const ui = useUi();
onMounted(() => void search.init());
const st = computed(() => search.status);
const dl = computed(() => search.models);
const offer = computed(() => modelToOffer(dl.value?.available ?? null, st.value?.model ?? null));
const working = computed(() => (dl.value?.phase ?? "idle") !== "idle");
const upToDate = computed(() => !!dl.value?.checked && !dl.value.error && !offer.value && !!st.value?.model);
const percent = computed(() => {
  const d = dl.value;
  return d && d.total ? Math.round((d.received / d.total) * 100) : 0;
});
const phaseText = computed(() => {
  switch (dl.value?.phase) {
    case "checking":
      return "Checking for a model…";
    case "downloading":
      return `Downloading: ${percent.value}%`;
    case "installing":
      return "Installing…";
    default:
      return "";
  }
});

async function setRecognizer(recognizer: RecognizerChoice) {
  try {
    await search.update({ recognizer });
  } catch (e: any) {
    ui.error(`Could not change search settings: ${e?.message ?? e}`);
  }
}

function start(what: () => Promise<void>) {
  what().catch((e: any) => ui.error(`The handwriting model could not be fetched: ${e?.message ?? e}`));
}

// Installing from a file: pick it, show what it is (size, licence), then install.
const pendingModel = ref<{ path: string; model: ModelSummary } | null>(null);
const removingModel = ref(false);
const fileBusy = ref(false);
const busy = computed(() => fileBusy.value || working.value);

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
  fileBusy.value = true;
  try {
    const m = await searchApi.installModel(p.path);
    ui.success(`Installed ${m.name} ${m.version}. Handwriting is being recognized again in the background.`);
  } catch (e: any) {
    ui.error(`The model could not be installed: ${e?.message ?? e}`);
  } finally {
    fileBusy.value = false;
  }
}

async function removeModel() {
  removingModel.value = false;
  const m = st.value?.model;
  if (!m) return;
  fileBusy.value = true;
  try {
    await searchApi.removeModel(m.id);
    ui.success("Handwriting model removed.");
  } catch (e: any) {
    ui.error(`The model could not be removed: ${e?.message ?? e}`);
  } finally {
    fileBusy.value = false;
  }
}
</script>

<template>
  <div class="space-y-2">
    <section v-if="st" class="space-y-2">
      <p v-if="!st.settings.enabled" class="px-1 text-xs text-muted">Search is off, so handwriting is not being read. Turn it on under Search to use these settings.</p>
      <template v-else>
        <label v-if="st.platform" class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
          <span>
            <span class="block text-sm">Handwriting recognizer</span>
            <span class="block text-xs text-muted">Changing this reads your handwriting again in the background.</span>
          </span>
          <select class="max-w-[50%] rounded-lg border border-line bg-surface px-2 py-1 text-sm" :value="st.settings.recognizer" @change="setRecognizer(($event.target as HTMLSelectElement).value as RecognizerChoice)">
            <option value="auto">Automatic</option>
            <option value="platform">{{ st.platform.name }}</option>
            <option value="model" :disabled="!st.model">Penroam model</option>
          </select>
        </label>
        <div class="space-y-2 rounded-xl border border-line p-3">
          <div class="min-w-0">
            <span class="block text-sm">Handwriting model</span>
            <template v-if="st.model">
              <span class="block text-xs text-muted">{{ st.model.name }} {{ st.model.version }} · {{ formatBytes(st.model.bytes) }}</span>
              <span class="mt-1 block text-xs text-muted">{{ st.model.license }}</span>
            </template>
            <span v-else class="block text-xs text-muted">Used where this device has no handwriting recognizer, and as a fallback when it fails. It runs on this device, and your notes are never uploaded.</span>
            <span v-if="st.modelError" class="mt-1 block text-xs text-danger">The installed model could not be loaded: {{ st.modelError }}</span>
          </div>
          <div v-if="offer" class="text-xs text-muted">
            <span class="block">Available: {{ offer.name }} {{ offer.version }} · {{ formatBytes(offer.bytes) }}</span>
            <span class="block">{{ offer.license }}</span>
          </div>
          <p v-if="upToDate" class="text-xs text-muted">This is the latest model.</p>
          <p v-if="dl?.checked && !dl.error && !offer && !st.model" class="text-xs text-muted">No model is available right now.</p>
          <p v-if="dl?.error" class="text-xs text-danger">{{ dl.error }}</p>
          <div v-if="working" class="flex items-center justify-between gap-3">
            <p class="text-xs text-muted" aria-live="polite">{{ phaseText }}</p>
            <Btn v-if="dl?.phase === 'downloading'" variant="ghost" class="shrink-0" @click="start(modelsApi.cancel)"><X :size="16" />Cancel</Btn>
          </div>
          <div v-else class="flex flex-wrap gap-2">
            <template v-if="dl?.enabled">
              <Btn v-if="offer || !st.model" variant="outline" :disabled="busy" @click="start(modelsApi.download)"><Download :size="16" />{{ st.model ? "Update" : "Download" }}</Btn>
              <Btn v-else variant="outline" :disabled="busy" @click="start(modelsApi.check)"><RefreshCw :size="16" />Check for updates</Btn>
            </template>
            <Btn variant="ghost" :disabled="busy" @click="chooseModel"><PackagePlus :size="16" />Install from file…</Btn>
            <Btn v-if="st.model" variant="ghost" class="text-danger" :disabled="busy" @click="removingModel = true"><Trash2 :size="16" />Remove</Btn>
          </div>
        </div>
      </template>
      <ConfirmDialog v-if="pendingModel" title="Install handwriting model?" confirm="Install"
        :message="`${pendingModel.model.name} ${pendingModel.model.version} (${formatBytes(pendingModel.model.bytes)}). Licence: ${pendingModel.model.license} It is checked against its checksums, stored on this device and runs only here. Handwriting is recognized again in the background.`"
        @close="pendingModel = null" @confirm="installModel" />
      <ConfirmDialog v-if="removingModel" title="Remove handwriting model?" danger confirm="Remove"
        message="Handwriting recognized by the model drops out of search as pages are re-indexed. Your notes are not affected."
        @close="removingModel = false" @confirm="removeModel" />
    </section>
    <DesktopEntryRow />
  </div>
</template>
