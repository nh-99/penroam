<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { LoaderCircle } from "@lucide/vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import LibraryView from "./views/LibraryView.vue";
import EditorView from "./views/EditorView.vue";
import Toasts from "./components/Toasts.vue";
import { useLibrary } from "./stores/library";
import { useSettings } from "./stores/settings";
import { useUi } from "./stores/ui";
import { api, hasHost } from "./platform/ipc";
import { flushEverything } from "./platform/lifecycle";
import { detectNativeInk } from "./platform/nativeInk";

const lib = useLibrary();
const settings = useSettings();
const ui = useUi();
const nativeKind = ref<string | null>(null);
let unlisten: UnlistenFn | null = null;

watch(
  () => !!lib.busy,
  (b, old) => {
    if (b && !old) ui.overlays++;
    if (!b && old) ui.overlays--;
  },
);

onMounted(async () => {
  await lib.refresh();
  if (!hasHost()) return;
  try {
    unlisten = await listen("penroam://close-requested", async () => {
      try {
        await flushEverything();
      } catch (e) {
        // Rust still flushes its own state; unsaved in-memory ops are reported
        console.error("flush before exit failed", e);
      }
      await api.exitApp();
    });
  } catch (e) {
    console.error("close handler not installed", e);
  }
  try {
    const n = await detectNativeInk();
    nativeKind.value = n?.kind ?? null;
    if (n?.kind === "boox") settings.preferEpaper();
  } catch (e) {
    console.error("native ink detection failed", e);
  }
  const bench = await api.autobenchRequested().catch((e) => {
    console.error("autobench query failed", e);
    return null;
  });
  if (bench) {
    console.info("autobench", bench);
    const { runAutobench } = await import("./bench/autobench");
    await runAutobench(bench).catch((e) => {
      console.error("benchmark failed", e);
      ui.error(`Benchmark failed: ${e?.message ?? e}`);
    });
  }
});
onBeforeUnmount(() => unlisten?.());
</script>

<template>
  <EditorView v-if="lib.current" :key="lib.current.id" />
  <LibraryView v-else :native-kind="nativeKind" />
  <Toasts />
  <div v-if="lib.busy" class="fixed inset-0 z-[70] flex items-center justify-center bg-black/20 epaper:bg-white/70" role="alert" aria-busy="true">
    <div class="flex items-center gap-3 rounded-xl border border-line bg-surface px-5 py-4 text-sm shadow-lg"><LoaderCircle :size="18" class="animate-spin" />{{ lib.busy }}</div>
  </div>
</template>
