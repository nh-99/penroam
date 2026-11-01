import { onBeforeUnmount, onMounted, reactive, ref, shallowRef, watch, type Ref } from "vue";
import { InkEditor, type EditorState } from "../editor/engine";
import { PdfCache, PdfPageRenderer } from "../editor/pdf";
import { PageRenderer } from "../editor/renderer";
import type { Page } from "../generated/Page";
import { PageSession, errorMessage } from "../editor/session";
import { api } from "../platform/ipc";
import { detectNativeInk, type NativeInkSurface } from "../platform/nativeInk";
import { registerFlusher } from "../platform/lifecycle";
import { useLibrary } from "../stores/library";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import { benchHooks } from "../bench/hooks";

export function useEditorHost(viewport: Ref<HTMLElement | null>, extraBlocked: Ref<boolean>) {
  const lib = useLibrary();
  const settings = useSettings();
  const ui = useUi();
  const state = reactive<EditorState>({
    tool: "pen",
    scale: 1,
    canUndo: false,
    canRedo: false,
    selectionCount: 0,
    selectionHasText: false,
    editingText: false,
    saveState: "saved",
    saveError: null,
    nativeActive: false,
    penSeen: false,
  });
  const editor = shallowRef<InkEditor | null>(null);
  const native = shallowRef<NativeInkSurface | null>(null);
  const loading = ref(false);
  const pageError = ref<string | null>(null);
  let session: PageSession | null = null;
  let pdfCache: PdfCache | null = null;
  let pdfCacheFor: string | null = null;
  let openSeq = 0;

  function applySettings() {
    const e = editor.value;
    if (!e) return;
    const s = settings.s;
    e.setSettings({
      penOnly: s.penOnly,
      mono: settings.effectiveTheme() === "epaper",
      useNativeInk: s.useNativeInk,
      maxCanvasPixels: /Android|iPad|iPhone/i.test(navigator.userAgent) ? 8_000_000 : 16_000_000,
    });
    e.setToolSettings({
      penColor: s.penColor,
      penWidth: s.penWidth,
      highlighterColor: s.highlighterColor,
      highlighterWidth: s.highlighterWidth,
      textColor: s.textColor,
      textSize: s.textSize,
      pressure: s.pressure,
    });
  }

  async function thumbnailNow() {
    const e = editor.value;
    const pid = session?.page.id;
    if (!e || !pid) return;
    const png = await e.renderer.thumbnail(240);
    if (png && session?.page.id === pid) await lib.saveThumb(pid, png);
  }

  /** Renders a page that is not open into a small PNG (detached canvases). */
  async function renderOffscreenThumb(page: Page): Promise<Uint8Array | null> {
    const host = document.createElement("div");
    const r = new PageRenderer(host, () => page.elements);
    r.setPage(page.size.width, page.size.height, page.template);
    r.opts.mono = settings.effectiveTheme() === "epaper";
    const res = Math.min(2, 480 / page.size.width);
    r.setResolution(res);
    if (page.background && pdfCache) {
      r.pdfBitmap = await new PdfPageRenderer().render(pdfCache, page.background, res).catch(() => null);
    }
    r.renderAll(null);
    return r.thumbnail(240);
  }

  /** Imported notebooks have no thumbnails: render the missing ones in the background. */
  async function fillMissingThumbs(nbId: string) {
    await new Promise((r) => setTimeout(r, 1500));
    for (const pid of [...(lib.current?.pageOrder ?? [])]) {
      if (lib.current?.id !== nbId || !editor.value) return;
      if (lib.thumbs[pid] || pid === session?.page.id) continue;
      if (await api.getThumbnail(nbId, pid).catch(() => null)) continue;
      try {
        const page = await api.loadPage(nbId, pid);
        if (page.elements.length > 20000) continue;
        const png = await renderOffscreenThumb(page);
        if (png && lib.current?.id === nbId) await lib.saveThumb(pid, png);
      } catch (e) {
        console.warn("thumbnail", pid, e);
      }
      await new Promise((r) => setTimeout(r, 30));
    }
  }

  /** Flushes the current page; throws (and keeps the data in memory) if saving fails. */
  async function flush() {
    editor.value?.endTextEdit();
    if (session) {
      await session.flush();
      await thumbnailNow().catch(() => {});
    }
  }

  async function openPage(pageId: string | null) {
    const nb = lib.current;
    const e = editor.value;
    if (!nb || !e) return;
    if (session && session.page.id === pageId && session.notebookId === nb.id) return;
    const seq = ++openSeq;
    if (session) {
      try {
        await flush();
      } catch (err) {
        ui.error(`Changes on this page are not saved yet (${errorMessage(err)}). Staying on the page.`, { label: "Retry", run: () => void session?.retry() });
        if (lib.currentPageId !== session.page.id) lib.currentPageId = session.page.id;
        return;
      }
    }
    if (!pageId) {
      session = null;
      return;
    }
    if (pdfCacheFor !== nb.id) {
      pdfCache?.destroy();
      pdfCache = new PdfCache((hash) => api.readAsset(nb.id, hash));
      pdfCacheFor = nb.id;
    }
    loading.value = true;
    pageError.value = null;
    try {
      const page = await api.loadPage(nb.id, pageId);
      if (seq !== openSeq) return;
      session = new PageSession(nb.id, page, (batch) => api.applyPageOperations(nb.id, page.id, batch));
      await e.setSession(session, pdfCache);
      if (!lib.thumbs[pageId]) setTimeout(() => void thumbnailNow(), page.background ? 1500 : 200);
    } catch (err) {
      if (seq === openSeq) pageError.value = errorMessage(err);
    } finally {
      if (seq === openSeq) loading.value = false;
    }
  }

  let unregister: (() => void) | null = null;
  const onVisibility = () => {
    if (document.visibilityState === "hidden") void flush().catch(() => {});
  };

  onMounted(async () => {
    const e = new InkEditor(viewport.value!, {
      onState: (s) => Object.assign(state, s),
      onError: (m) => ui.error(m),
      onPageEdited: () => void thumbnailNow(),
    });
    editor.value = e;
    benchHooks.editor = e;
    applySettings();
    unregister = registerFlusher(flush);
    document.addEventListener("visibilitychange", onVisibility);
    await openPage(lib.currentPageId);
    if (lib.current) void fillMissingThumbs(lib.current.id);
    const n = await detectNativeInk();
    if (n && editor.value === e) {
      native.value = n;
      e.setNativeSurface(n);
    }
  });

  onBeforeUnmount(() => {
    unregister?.();
    document.removeEventListener("visibilitychange", onVisibility);
    const e = editor.value;
    editor.value = null;
    if (benchHooks.editor === e) benchHooks.editor = null;
    const s = session;
    session = null;
    void s?.flush().catch(() => {});
    e?.dispose();
    pdfCache?.destroy();
  });

  watch(() => lib.currentPageId, (id) => void openPage(id));
  watch(() => settings.s, applySettings, { deep: true });
  watch(() => settings.effectiveTheme(), applySettings);
  watch(
    () => ui.overlays > 0 || extraBlocked.value,
    (b) => editor.value?.setOverlayBlocked(b),
  );
  watch(
    () => state.penSeen,
    (seen) => seen && settings.s.penOnly === "auto" && console.info("pen detected: fingers now pan/zoom"),
  );

  return { state, editor, native, loading, pageError, flush, openPage, retry: () => session?.retry(), session: () => session };
}
