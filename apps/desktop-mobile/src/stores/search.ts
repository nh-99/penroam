import { defineStore } from "pinia";
import { ref, shallowRef } from "vue";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "../platform/ipc";
import { searchApi, type PageRect, type SearchSettings, type SearchStatus } from "../platform/search";

/**
 * Search UI state: the worker status (pushed on `search://status`), whether the search panel
 * is open, and a highlight waiting for the editor to open its page.
 */
export const useSearch = defineStore("search", () => {
  const status = shallowRef<SearchStatus | null>(null);
  const panelOpen = ref(false);
  const query = ref("");
  /** Outline these rectangles once `pageId` is shown in the editor. */
  const pendingHighlight = shallowRef<{ pageId: string; rects: PageRect[] } | null>(null);
  let unlisten: UnlistenFn | null = null;

  async function init() {
    if (!hasHost() || unlisten) return;
    unlisten = await searchApi.onStatus((s) => (status.value = s));
    try {
      status.value = await searchApi.status();
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
  }

  return { status, panelOpen, query, pendingHighlight, init, update, dispose };
});
