<script setup lang="ts">
// Settings → Advanced: the app-menu entry the Linux AppImage adds for itself
// (src-tauri/src/desktop_entry.rs). Hidden everywhere else.
import { onMounted, ref } from "vue";
import { AppWindow, Trash2 } from "@lucide/vue";
import Btn from "./ui/Btn.vue";
import { useUi } from "../stores/ui";
import { desktopEntryApi, type DesktopEntryStatus } from "../platform/desktopEntry";

const ui = useUi();
const status = ref<DesktopEntryStatus | null>(null);
const busy = ref(false);

onMounted(async () => {
  try {
    status.value = await desktopEntryApi.status();
  } catch {
    status.value = null;
  }
});

async function run(what: () => Promise<DesktopEntryStatus>, done: string) {
  busy.value = true;
  try {
    status.value = await what();
    ui.success(done);
  } catch (e: any) {
    ui.error(e?.message ?? String(e));
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div v-if="status?.supported" class="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-line p-3">
    <span class="min-w-0 flex-1">
      <span class="block text-sm">App menu</span>
      <span v-if="status.installed" class="block text-xs text-muted">Penroam added itself to your app menu. If you're uninstalling, remove it here before you delete the AppImage. Your notes are not affected.</span>
      <span v-else class="block text-xs text-muted">Penroam is not in your app menu, and won't add itself again when it starts.</span>
    </span>
    <Btn v-if="status.installed" variant="ghost" class="shrink-0 text-danger" :disabled="busy" @click="run(desktopEntryApi.remove, 'Penroam was removed from the app menu.')"><Trash2 :size="16" />Remove from app menu</Btn>
    <Btn v-else variant="outline" class="shrink-0" :disabled="busy" @click="run(desktopEntryApi.install, 'Penroam was added to the app menu.')"><AppWindow :size="16" />Add to app menu</Btn>
  </div>
</template>
