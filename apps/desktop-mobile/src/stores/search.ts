import { defineStore } from "pinia";
import { ref, shallowRef } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "../platform/ipc";
import { modelNotice, modelsApi, type ModelsStatus } from "../platform/models";
import { searchApi, type PageRect, type SearchSettings, type SearchStatus } from "../platform/search";
import { useUi } from "./ui";

/**
 * Search UI state: the worker status (pushed on `search://status`), the model downloader's
 * status (`models://status`), whether the search panel is open, and a highlight waiting for the
 * editor to open its page.
 */
export const useSearch = defineStore("search", () => {
  const status = shallowRef<SearchStatus | null>(null);
  const models = shallowRef<ModelsStatus | null>(null);
  const panelOpen = ref(false);
  const query = ref("");
  /** Outline these rectangles once `pageId` is shown in the editor. */
  const pendingHighlight = shallowRef<{ pageId: string; rects: PageRect[] } | null>(null);
  let unlisten: UnlistenFn | null = null;
  let unlistenModels: UnlistenFn | null = null;

  async function init() {
    if (!hasHost() || unlisten) return;
    unlisten = await searchApi.onStatus((s) => (status.value = s));
    const ui = useUi();
    unlistenModels = await modelsApi.onStatus((next) => {
      const notice = modelNotice(models.value, next);
      models.value = next;
      if (notice) ui.info(notice);
    });
    try {
      status.value = await searchApi.status();
      models.value = await modelsApi.status();
    } catch (e) {
      console.error("search status", e);
    }
  }

  async function update(patch: Partial<SearchSettings>) {
    if (!status.value) return;
    const settings = { ...status.value.settings, ...patch };
    status.value = { ...status.value, settings };
    await searchApi.setSettings(settings);
  }

  function dispose() {
    unlisten?.();
    unlisten = null;
    unlistenModels?.();
    unlistenModels = null;
  }

  return { status, models, panelOpen, query, pendingHighlight, init, update, dispose };
});
