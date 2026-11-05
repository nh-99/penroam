<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { LoaderCircle } from "@lucide/vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import LibraryView from "./views/LibraryView.vue";
import EditorView from "./views/EditorView.vue";
import Toasts from "./components/Toasts.vue";
import UpgradeDialog from "./components/UpgradeDialog.vue";
import { useLibrary } from "./stores/library";
import { useSettings } from "./stores/settings";
import { useUi } from "./stores/ui";
import { useSync } from "./stores/sync";
import { useSearch } from "./stores/search";
import { api, hasHost } from "./platform/ipc";
import { flushEverything } from "./platform/lifecycle";
import { detectNativeInk } from "./platform/nativeInk";

const lib = useLibrary();
const settings = useSettings();
const ui = useUi();
const sync = useSync();
const search = useSearch();
const nativeKind = ref<string | null>(null);
let unlisten: UnlistenFn | null = null;

watch(
  () => !!lib.busy,
  (b, old) => {
    if (b && !old) ui.overlays++;
    if (!b && old) ui.overlays--;
  },
);

/** Rejects after `ms` so one stuck platform call can't block start-up. */
function withTimeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  return Promise.race([p, new Promise<T>((_, rej) => setTimeout(() => rej(new Error(`${what} timed out after ${ms} ms`)), ms))]);
}

onMounted(async () => {
  await lib.refresh();
  if (!hasHost()) return;
  console.info("startup: library loaded");
  void search.init();
  // sync (developer preview): one query at start-up; nothing more unless it is available
  void sync.init().then(() => {
    if (!sync.available) return;
    watch(() => lib.notebooks, (ns) => sync.arrived(ns.map((n) => n.id)));
    // an install can land in a reconcile that then fails offline, which reports no change
    watch(() => sync.status, () => {
      if (sync.downloading.length && !lib.current) void lib.refresh();
    });
    sync.onChanged((c) => {
      // remote changes were merged into the library
      if (!lib.current) void lib.refresh();
      else if (c.notebookIds.includes(lib.current.id)) void lib.reloadCurrent();
    });
  });
  const bench = await withTimeout(api.autobenchRequested(), 5000, "autobench query").catch((e) => {
    console.error("autobench query failed", e);
    return null;
  });
  try {
    unlisten = await withTimeout(
      listen("penroam://close-requested", async () => {
        try {
          await flushEverything();
        } catch (e) {
          // Rust still flushes its own state; unsaved in-memory ops are reported
          console.error("flush before exit failed", e);
        }
        await api.exitApp();
      }),
      5000,
      "close listener",
    );
  } catch (e) {
    console.error("close handler not installed", e);
  }
  console.info("startup: close listener ok");
  try {
    const n = await withTimeout(detectNativeInk(), 8000, "native ink detection");
    nativeKind.value = n?.kind ?? null;
    if (n?.kind === "boox") settings.preferEpaper();
    console.info("startup: native ink", n?.kind ?? "none");
  } catch (e) {
    console.error("native ink detection failed", e);
  }
  if (bench) {
    console.info("autobench", bench);
    const { runAutobench } = await import("./bench/autobench");
    await runAutobench(bench).catch((e) => {
      console.error("benchmark failed", e);
      ui.error(`Benchmark failed: ${e?.message ?? e}`);
    });
  }
});

onBeforeUnmount(() => {
  unlisten?.();
  sync.dispose();
  search.dispose();
});
</script>

<template>
  <EditorView v-if="lib.current" :key="lib.current.id" />
  <LibraryView v-else :native-kind="nativeKind" />
  <Toasts />
  <UpgradeDialog v-if="sync.upgradeOpen" />
  <div v-if="lib.busy" class="fixed inset-0 z-[70] flex items-center justify-center bg-black/20 epaper:bg-white/70" role="alert" aria-busy="true">
    <div class="flex items-center gap-3 rounded-xl border border-line bg-surface px-5 py-4 text-sm shadow-lg"><LoaderCircle :size="18" class="animate-spin" />{{ lib.busy }}</div>
  </div>
</template>
