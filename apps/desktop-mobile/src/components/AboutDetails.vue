<script setup lang="ts">
// Settings → About: updates, the diagnostic report, help and licences.
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { Check, Copy, ExternalLink, RefreshCw, Trash2 } from "@lucide/vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import Btn from "./ui/Btn.vue";
import Modal from "./ui/Modal.vue";
import { diagnosticsApi } from "../platform/diagnostics";
import { updatesApi, type OfferedUpdate, type UpdateProgress } from "../platform/updates";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const props = defineProps<{ inkPath: string | null }>();
const settings = useSettings();
const ui = useUi();

const canUpdate = ref(false);
const checking = ref(false);
const upToDate = ref(false);
const offer = ref<OfferedUpdate | null>(null);
const progress = ref<UpdateProgress | null>(null);
const installing = ref(false);
const percent = computed(() => {
  const p = progress.value;
  return p?.total ? Math.round((p.received / p.total) * 100) : null;
});
let unlisten: UnlistenFn | null = null;

const report = ref<string | null>(null);
const copied = ref(false);
const notices = ref<string | null>(null);
const showNotices = ref(false);

onMounted(async () => {
  canUpdate.value = await updatesApi.supported().catch(() => false);
  notices.value = await diagnosticsApi.thirdPartyNotices().catch(() => null);
  if (canUpdate.value) unlisten = await updatesApi.onProgress((p) => (progress.value = p));
});
onBeforeUnmount(() => unlisten?.());

async function check() {
  checking.value = true;
  upToDate.value = false;
  offer.value = null;
  try {
    offer.value = await updatesApi.check(settings.s.updateChannel);
    upToDate.value = !offer.value;
  } catch (e: any) {
    ui.error(`Could not check for updates: ${e?.message ?? e}`);
  } finally {
    checking.value = false;
  }
}

async function install() {
  installing.value = true;
  try {
    await updatesApi.install(settings.s.updateChannel);
  } catch (e: any) {
    ui.error(`The update could not be installed: ${e?.message ?? e}`);
  } finally {
    installing.value = false;
    progress.value = null;
  }
}

async function showReport() {
  try {
    report.value = await diagnosticsApi.report(props.inkPath);
    copied.value = false;
  } catch (e: any) {
    ui.error(`Could not make the report: ${e?.message ?? e}`);
  }
}

async function copyReport() {
  try {
    await navigator.clipboard.writeText(report.value ?? "");
    copied.value = true;
  } catch (e: any) {
    ui.error(`Could not copy the report: ${e?.message ?? e}`);
  }
}

async function clearReports() {
  try {
    await diagnosticsApi.clear();
    await showReport();
    ui.success("Crash reports deleted.");
  } catch (e: any) {
    ui.error(`Could not delete the crash reports: ${e?.message ?? e}`);
  }
}

function open(page: "support" | "privacy") {
  diagnosticsApi.openHelp(page).catch((e: any) => ui.error(`Could not open the browser: ${e?.message ?? e}`));
}
</script>

<template>
  <section v-if="canUpdate" class="space-y-2">
    <h3 class="text-sm font-medium">Updates</h3>
    <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
      <span><span class="block text-sm">Check when Penroam starts</span><span class="block text-xs text-muted">Off by default. A check tells GitHub your IP address and sends nothing else.</span></span>
      <input v-model="settings.s.updateCheck" type="checkbox" class="h-5 w-5" />
    </label>
    <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
      <span><span class="block text-sm">Update channel</span><span class="block text-xs text-muted">Beta gets new versions first and may have more bugs.</span></span>
      <select v-model="settings.s.updateChannel" class="rounded-lg border border-line bg-surface px-2 py-1 text-sm">
        <option value="stable">Stable</option>
        <option value="beta">Beta</option>
      </select>
    </label>
    <div class="flex flex-wrap items-center gap-2">
      <Btn variant="outline" :disabled="checking || installing" @click="check"><RefreshCw :size="16" />Check for updates</Btn>
      <span v-if="upToDate" class="text-xs text-muted" role="status">Penroam is up to date.</span>
    </div>
    <div v-if="offer" class="space-y-2 rounded-xl border border-line p-3">
      <p class="text-sm">Penroam {{ offer.version }} is available.</p>
      <p v-if="offer.notes" class="max-h-32 overflow-y-auto whitespace-pre-wrap text-xs text-muted">{{ offer.notes }}</p>
      <Btn variant="primary" :disabled="installing" @click="install">
        <template v-if="!installing">Install and restart</template>
        <template v-else-if="percent !== null">Downloading: {{ percent }}%</template>
        <template v-else>Installing…</template>
      </Btn>
      <p class="text-xs text-muted">Your notes are saved first. They're kept separately from the app and aren't changed by installing it.</p>
    </div>
  </section>

  <section class="space-y-2">
    <h3 class="text-sm font-medium">Problems</h3>
    <p class="text-xs leading-relaxed text-muted">
      If Penroam crashes it keeps a short report on this device. Reports hold no notes, file names or account details, and nothing is sent anywhere. To share one, read it here, then copy it into your bug report.
    </p>
    <div class="flex flex-wrap gap-2">
      <Btn variant="outline" @click="showReport">Show diagnostic report</Btn>
      <Btn variant="outline" @click="open('support')"><ExternalLink :size="16" />Get help</Btn>
    </div>
    <template v-if="report !== null">
      <pre class="max-h-48 overflow-auto whitespace-pre-wrap rounded-xl bg-surface-2 p-3 text-xs epaper:border" tabindex="0" aria-label="Diagnostic report">{{ report }}</pre>
      <div class="flex flex-wrap gap-2">
        <Btn variant="outline" @click="copyReport"><Check v-if="copied" :size="16" /><Copy v-else :size="16" />{{ copied ? "Copied" : "Copy" }}</Btn>
        <Btn variant="ghost" @click="clearReports"><Trash2 :size="16" />Delete crash reports</Btn>
      </div>
    </template>
  </section>

  <section class="flex flex-wrap gap-2">
    <Btn variant="ghost" @click="open('privacy')"><ExternalLink :size="16" />Privacy</Btn>
    <Btn v-if="notices" variant="ghost" @click="showNotices = true">Open-source licences</Btn>
  </section>

  <Modal v-if="showNotices" title="Open-source licences" @close="showNotices = false">
    <pre class="h-[min(34rem,calc(100dvh-12rem))] overflow-auto whitespace-pre-wrap text-xs" tabindex="0">{{ notices }}</pre>
  </Modal>
</template>
