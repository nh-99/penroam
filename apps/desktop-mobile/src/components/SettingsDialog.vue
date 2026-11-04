<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { getVersion } from "@tauri-apps/api/app";
import { Monitor, Sun, Moon, Tablet, ShieldCheck } from "@lucide/vue";
import Modal from "./ui/Modal.vue";
import SyncSection from "./SyncSection.vue";
import { useSync } from "../stores/sync";
import { useSettings, type ThemePref } from "../stores/settings";
import { useLibrary } from "../stores/library";
import type { PenOnlyMode } from "../editor/input";
import { hasHost } from "../platform/ipc";
import { tapCounter } from "../platform/sync";
import { useUi } from "../stores/ui";

const props = defineProps<{ nativeKind: string | null }>();
const emit = defineEmits<{ close: [] }>();
const settings = useSettings();
const lib = useLibrary();
const sync = useSync();
const themes: { v: ThemePref; label: string; icon: any }[] = [
  { v: "system", label: "System", icon: Monitor },
  { v: "light", label: "Light", icon: Sun },
  { v: "dark", label: "Dark", icon: Moon },
  { v: "epaper", label: "ePaper", icon: Tablet },
];
const penModes: { v: PenOnlyMode; label: string; hint: string }[] = [
  { v: "auto", label: "Automatic", hint: "Fingers draw until a pen is used, then fingers pan and zoom" },
  { v: "on", label: "Pen only", hint: "Fingers always pan and zoom" },
  { v: "off", label: "Fingers draw", hint: "One finger draws; two fingers pan and zoom" },
];
const nativeLabel = computed(() =>
  props.nativeKind === "boox" ? "BOOX hardware pen (Onyx SDK)" : props.nativeKind === "ios-pencil" ? "Native Apple Pencil layer" : null,
);
const rec = computed(() => lib.recovery);

const ui = useUi();
const version = ref<string | null>(null);
onMounted(async () => {
  if (!hasHost()) return;
  try {
    version.value = await getVersion();
  } catch {
    /* no version: the line is not shown */
  }
});
// Debug builds: five taps on the version turn developer mode (custom sync servers) on or off.
const tap = tapCounter();
function versionTapped() {
  if (!sync.devBuild || !tap(Date.now())) return;
  settings.s.developerMode = !settings.s.developerMode;
  if (settings.s.developerMode) ui.success("Developer mode on: sync setup asks for a server address.");
  else ui.success("Developer mode off.");
}
</script>

<template>
  <Modal title="Settings" @close="emit('close')">
    <div class="space-y-6">
      <section>
        <h3 class="mb-2 text-sm font-medium">Appearance</h3>
        <div class="grid grid-cols-4 gap-2" role="radiogroup" aria-label="Theme">
          <button v-for="t in themes" :key="t.v" type="button" role="radio" :aria-checked="settings.s.theme === t.v"
            class="flex flex-col items-center gap-1 rounded-xl border p-2.5 text-xs"
            :class="settings.s.theme === t.v ? 'border-accent bg-accent-soft text-accent epaper:border-2 epaper:border-black epaper:bg-white epaper:font-bold' : 'border-line hover:bg-surface-2'"
            @click="settings.s.theme = t.v">
            <component :is="t.icon" :size="20" />{{ t.label }}
          </button>
        </div>
      </section>
      <section>
        <h3 class="mb-2 text-sm font-medium">Touch input</h3>
        <div class="space-y-1.5" role="radiogroup" aria-label="Finger input">
          <label v-for="m in penModes" :key="m.v" class="flex cursor-pointer items-start gap-3 rounded-xl border border-line p-3" :class="settings.s.penOnly === m.v ? 'border-accent epaper:border-2 epaper:border-black' : ''">
            <input v-model="settings.s.penOnly" type="radio" name="penOnly" :value="m.v" class="mt-1" />
            <span><span class="block text-sm">{{ m.label }}</span><span class="block text-xs text-muted">{{ m.hint }}</span></span>
          </label>
        </div>
      </section>
      <section class="space-y-2">
        <label class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
          <span><span class="block text-sm">Pressure-sensitive pen</span><span class="block text-xs text-muted">Line width follows pen pressure when the device reports it</span></span>
          <input v-model="settings.s.pressure" type="checkbox" class="h-5 w-5" />
        </label>
        <label v-if="nativeLabel" class="flex items-center justify-between gap-4 rounded-xl border border-line p-3">
          <span><span class="block text-sm">{{ nativeLabel }}</span><span class="block text-xs text-muted">Low-latency native pen rendering. Turn off to use the web canvas.</span></span>
          <input v-model="settings.s.useNativeInk" type="checkbox" class="h-5 w-5" />
        </label>
      </section>
      <SyncSection v-if="sync.available" />
      <section class="flex gap-3 rounded-xl bg-surface-2 p-3 text-xs leading-relaxed text-muted epaper:border">
        <ShieldCheck :size="18" class="shrink-0 text-ok" />
        <p>
          <template v-if="sync.configured">
            Penroam works fully offline. Notebooks are stored on this device in an open format and, with the sync preview above,
            uploaded end-to-end encrypted. There are no analytics or AI models. Export still works without sync.
          </template>
          <template v-else-if="sync.available">
            Penroam works fully offline. Notebooks are stored only on this device, in an open format. Sync (above) is optional and
            off until you set it up; there are no analytics or AI models. Use Export to move a notebook to another device as a
            <code>.penroam</code> file.
          </template>
          <template v-else>
            Penroam works fully offline. Notebooks are stored only on this device, in an open format; there is no account, cloud,
            sync, analytics or AI model. Use Export to move a notebook to another device as a <code>.penroam</code> file.
          </template>
          <template v-if="rec && (rec.replayedBatches || rec.truncatedJournals || rec.orphanedFiles)">
            <br />Last start-up recovery: {{ rec.replayedBatches }} journal batch(es) replayed, {{ rec.truncatedJournals }} incomplete record(s) discarded, {{ rec.orphanedFiles }} unreferenced file(s) set aside.
          </template>
        </p>
      </section>
      <p v-if="version" class="text-center text-xs text-muted">
        <button type="button" class="select-none px-2 py-1" @click="versionTapped">Penroam {{ version }}<template v-if="sync.devBuild && settings.s.developerMode"> · developer mode</template></button>
      </p>
    </div>
  </Modal>
</template>
