import { listen } from "@tauri-apps/api/event";
import { onBeforeUnmount, onMounted, reactive, ref, shallowRef, watch, type Ref } from "vue";
import type { Size } from "../generated/Size";
import { InkEditor, type EditorState } from "../editor/engine";
import { PALM_CONTACT_PX } from "../editor/input";
import { PdfCache } from "../editor/pdf";
import { drawPageCanvas, drawPagePng, type PictureContext } from "../editor/pagePicture";
import type { Page } from "../generated/Page";
import { PageSession, errorMessage } from "../editor/session";
import { api, hasHost } from "../platform/ipc";
import { isIOS } from "../platform/device";
import { detectNativeInk, type NativeInkSurface } from "../platform/nativeInk";
import { registerFlusher } from "../platform/lifecycle";
import { useLibrary } from "../stores/library";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import { useSync } from "../stores/sync";
import { useLive } from "../stores/live";
import { useSharing } from "../stores/sharing";
import { AwarenessSender, liveApi } from "../platform/live";
import type { AnchorView } from "../platform/comments";
import { benchHooks } from "../bench/hooks";

interface PinchEvent {
  phase: "begin" | "update" | "end";
  scale: number;
  x: number;
  y: number;
}

export function useEditorHost(viewport: Ref<HTMLElement | null>, extraBlocked: Ref<boolean>) {
  const lib = useLibrary();
  const settings = useSettings();
  const ui = useUi();
  const sync = useSync();
  const live = useLive();
  const sharing = useSharing();
  const state = reactive<EditorState>({
    tool: "pen",
    scale: 1,
    canUndo: false,
    canRedo: false,
    selectionCount: 0,
    selectionHasText: false, selectionTextFree: true, selectionTextFont: null, pageHasRows: false,
    editingText: false,
    textFormats: [],
    rulerShown: false,
    rulerAngle: 0,
    saveState: "saved",
    saveError: null,
    nativeActive: false,
    penSeen: false,
    hasTextRange: false,
    pageHasPdf: false,
    placing: null,
  });
  const editor = shallowRef<InkEditor | null>(null);
  /** An anchor the user just chose for a comment (selection, tap or drag), until the UI takes it. */
  const commentAnchor = shallowRef<AnchorView | null>(null);
  const native = shallowRef<NativeInkSurface | null>(null);
  const loading = ref(false);
  const pageError = ref<string | null>(null);
  let session: PageSession | null = null;
  /** Sends where this user is, at a bounded rate and never during a stroke (shared notebooks only). */
  let awareness: AwarenessSender | null = null;
  let pdfCache: PdfCache | null = null;
  let pdfCacheFor: string | null = null;
  let openSeq = 0;
  /** The page continuous scrolling asked for, so that opening it keeps the view. */
  let scrollTarget: string | null = null;
  /** Sizes of the pages loaded so far, for laying out continuous scrolling. */
  const sizes = new Map<string, Size>();
  let sizesFor: string | null = null;
  /**
   * Sessions of pages left recently, so that scrolling back to one keeps its undo history.
   * Keyed by notebook and page; dropped when the page changes elsewhere.
   */
  const recent = new Map<string, PageSession>();
  const RECENT_SESSIONS = 8;
  const recentKey = (nbId: string, pageId: string) => `${nbId}/${pageId}`;
  function keepRecent(s: PageSession) {
    const k = recentKey(s.notebookId, s.page.id);
    recent.delete(k);
    recent.set(k, s);
    for (const old of recent.keys()) {
      if (recent.size <= RECENT_SESSIONS) break;
      recent.delete(old);
    }
  }

  function applySettings() {
    const e = editor.value;
    if (!e) return;
    const s = settings.s;
    e.setContinuous(s.continuousScroll);
    e.setSettings({
      penOnly: s.penOnly,
      mono: settings.effectiveTheme() === "epaper",
      dark: settings.effectiveTheme() === "dark",
      useNativeInk: s.useNativeInk,
      // iPad fingertips exceed the size limit; palms are rejected by the Pencil-down rules instead
      palmContactPx: isIOS() ? Infinity : PALM_CONTACT_PX,
      pencilCurve: isIOS(),
      detailWhenZoomed: isIOS(),
      maxCanvasPixels: /Android|iPad|iPhone/i.test(navigator.userAgent) ? 8_000_000 : 16_000_000,
    });
    e.setToolSettings({
      penColor: s.penColor,
      penWidth: s.penWidth,
      highlighterColor: s.highlighterColor,
      highlighterWidth: s.highlighterWidth,
      textColor: s.textColor,
      textFreePlacement: s.textFreePlacement,
      textSize: s.textSize,
      textFont: s.textFont,
      pressure: s.pressure,
    });
  }

  async function thumbnailNow() {
    const e = editor.value;
    const pid = session?.page.id;
    if (!e || !pid || !session) return;
    // The layers of a canvas cover the view, not the drawing: picture the drawing instead.
    // Thumbnails are saved, so they show the page as it is, not as the dark theme shows it.
    const png = e.isCanvas || e.renderer.opts.dark ? await renderPagePng(session.page, 240, { paper: true }) : await e.renderer.thumbnail(240);
    if (png && session?.page.id === pid) await lib.saveThumb(pid, png);
  }

  /** Renders a page that is not open into a small PNG (detached canvases). */
  const renderOffscreenThumb = (page: Page) => renderPagePng(page, 240, { paper: true });

  /**
   * Renders any page, including a past version, to a PNG `width` pixels wide. The PDF
   * behind it is drawn at that resolution from the original, so `width` sets the detail.
   * PNGs are exports, thumbnails and history previews, so they are never in dark colours.
   */
  function renderPagePng(page: Page, width: number, opts: { paper?: boolean } = {}): Promise<Uint8Array | null> {
    return drawPagePng(page, width, pictureContext(false), opts);
  }

  /**
   * Like `renderPagePng`, but keeps the picture as a canvas (continuous scrolling). With
   * `asShown` the page is drawn the way the editor shows it in the dark theme.
   */
  function renderPageCanvas(page: Page, width: number, opts: { paper?: boolean; asShown?: boolean } = {}): Promise<HTMLCanvasElement> {
    return drawPageCanvas(page, width, pictureContext(!!opts.asShown), opts);
  }

  /** The open notebook's side of a page picture (see `editor/pagePicture.ts`). */
  function pictureContext(asShown: boolean): PictureContext {
    return {
      notebookId: lib.current?.id ?? "",
      infiniteCanvas: !!lib.current?.infiniteCanvas,
      pdfCache,
      mono: settings.effectiveTheme() === "epaper",
      dark: asShown && settings.effectiveTheme() === "dark",
      pdfPages: editor.value?.renderer.pdfPages,
    };
  }

  /** Tells the editor the notebook's pages, with the sizes and thumbnails known so far. */
  function pushPageList() {
    const e = editor.value;
    const nb = lib.current;
    if (!e || !nb) return;
    if (sizesFor !== nb.id) {
      sizes.clear();
      sizesFor = nb.id;
    }
    e.setPageList(nb.pageOrder.map((id) => ({ id, ...sizes.get(id), placeholder: lib.thumbs[id] ?? null })));
  }

  /** Records a page's size; the layout changes only if it differs from what was assumed. */
  function noteSize(nbId: string, page: Page) {
    if (sizesFor !== nbId) return;
    const old = sizes.get(page.id);
    if (old && old.width === page.size.width && old.height === page.size.height) return;
    sizes.set(page.id, page.size);
    pushPageList();
  }

  /** Pictures of other pages the editor asked for, nearest first; drawn one at a time. */
  let imageQueue: { id: string; width: number }[] = [];
  let imagesBusy = false;
  /** The page being drawn now; not queued again while it is. */
  let drawing: string | null = null;
  async function drawPageImages() {
    if (imagesBusy) return;
    imagesBusy = true;
    try {
      for (let want = imageQueue.shift(); want; want = imageQueue.shift()) {
        drawing = want.id;
        const nb = lib.current;
        const e = editor.value;
        if (!nb || !e) break;
        if (want.id === session?.page.id || !nb.pageOrder.includes(want.id)) continue;
        try {
          const page = await api.loadPage(nb.id, want.id);
          if (lib.current?.id !== nb.id || editor.value !== e) break;
          noteSize(nb.id, page);
          const canvas = await renderPageCanvas(page, want.width, { asShown: true });
          if (lib.current?.id === nb.id) e.setPageImage(want.id, canvas);
        } catch (err) {
          console.warn("page picture", want.id, err);
        }
      }
    } finally {
      imagesBusy = false;
      drawing = null;
    }
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

  /**
   * Opens the current page again from disk with the notebook's way of showing pages (sheets
   * or infinite canvases), after something outside the editor changed it. Flush first.
   */
  async function reload() {
    const e = editor.value;
    if (!e) return;
    recent.clear();
    session = null;
    scrollTarget = null;
    e.setCanvas(!!lib.current?.infiniteCanvas);
    await openPage(lib.currentPageId);
    // a canvas's picture is of its drawing; combining pages changed it, too
    void thumbnailNow().catch(() => {});
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
    const keepView = scrollTarget === pageId;
    scrollTarget = null;
    const seq = ++openSeq;
    if (session && session.page.id === pageId && session.notebookId === nb.id) {
      // back on the open page before another one finished loading: that load is dropped
      loading.value = false;
      return;
    }
    if (session) {
      try {
        await flush();
      } catch (err) {
        ui.error(`Changes on this page are not saved yet (${errorMessage(err)}). Staying on the page.`, { label: "Retry", run: () => void session?.retry() });
        if (lib.currentPageId !== session.page.id) lib.currentPageId = session.page.id;
        return;
      }
      if (seq !== openSeq) return;
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
    // pages of another notebook may have changed since: only this notebook's sessions are kept
    if (session && session.notebookId !== nb.id) recent.clear();
    else if (session) keepRecent(session);
    // scrolled onto: its picture is already on screen, so no spinner over it
    loading.value = !keepView;
    pageError.value = null;
    try {
      const cached = recent.get(recentKey(nb.id, pageId));
      recent.delete(recentKey(nb.id, pageId));
      let next: PageSession;
      if (cached?.idle) next = cached;
      else {
        const page = await api.loadPage(nb.id, pageId);
        if (seq !== openSeq) return;
        next = new PageSession(nb.id, page, (batch) => api.applyPageOperations(nb.id, page.id, batch));
        // a save rejected because synced remote edits changed the page is rebased, not lost
        next.reload = () => api.loadPage(nb.id, page.id);
      }
      session = next;
      const page = next.page;
      noteSize(nb.id, page);
      await e.setSession(next, pdfCache, { keepView });
      if (!lib.thumbs[pageId]) setTimeout(() => void thumbnailNow(), page.background ? 1500 : 200);
    } catch (err) {
      if (seq === openSeq) pageError.value = errorMessage(err);
    } finally {
      if (seq === openSeq) loading.value = false;
    }
  }

  /** Pages changed by synced remote edits, waiting until the editor is idle. */
  const remotePending = new Set<string>();
  let remoteTimer: ReturnType<typeof setTimeout> | null = null;

  async function applyRemoteChanges() {
    remoteTimer = null;
    const nb = lib.current;
    const e = editor.value;
    if (!nb || !e) return;
    const s = session;
    if (s && remotePending.has(s.page.id) && s.notebookId === nb.id) {
      if (!e.canMergeRemote() || !s.idle) {
        // never interrupt a stroke, a drag or an IME composition: try again shortly
        remoteTimer = setTimeout(() => void applyRemoteChanges(), 700);
        return;
      }
      remotePending.delete(s.page.id);
      try {
        // typing so far goes to the session first, so that the box and the page agree before merging
        e.flushText();
        // the session drops a snapshot that anything drawn or saved meanwhile has made older
        const r = await s.mergeRemote(() => api.loadPage(nb.id, s.page.id), () => e.canMergeRemote());
        if (r === "stale" || session !== s) remotePending.add(s.page.id); // read it again once things are quiet
      } catch (err) {
        console.warn("reload after sync", err);
      }
    }
    // other changed pages: refresh their thumbnails in the background
    for (const pid of [...remotePending]) {
      remotePending.delete(pid);
      if (pid === session?.page.id) continue;
      recent.delete(recentKey(nb.id, pid));
      if (!nb.pageOrder.includes(pid)) continue;
      e.dropPageImage(pid);
      try {
        const page = await api.loadPage(nb.id, pid);
        if (page.elements.length > 20000) continue;
        const png = await renderOffscreenThumb(page);
        if (png && lib.current?.id === nb.id) await lib.saveThumb(pid, png);
      } catch (err) {
        console.warn("thumbnail after sync", err);
      }
    }
    if (remotePending.size && !remoteTimer) remoteTimer = setTimeout(() => void applyRemoteChanges(), 700);
  }

  let offChanged: (() => void) | null = null;
  let offPinch: (() => void) | null = null;
  let unregister: (() => void) | null = null;
  const onVisibility = () => {
    if (document.visibilityState === "hidden") void flush().catch(() => {});
  };

  onMounted(async () => {
    const e = new InkEditor(viewport.value!, {
      onState: (s) => Object.assign(state, s),
      onError: (m) => ui.error(m),
      readAsset: (hash) => api.readAsset(lib.current!.id, hash),
      onPageEdited: () => void thumbnailNow(),
      onAwareness: (a) => awareness?.push(a),
      onCommentAnchor: (a) => (commentAnchor.value = a),
      openLink: (url) => void api.openLink(url).catch((err) => ui.error(`Could not open the link (${errorMessage(err)}).`)),
      onScrollToPage: (id) => {
        if (!lib.current?.pageOrder.includes(id) || lib.currentPageId === id) return;
        scrollTarget = id;
        lib.currentPageId = id;
      },
      wantPageImages: (wants) => {
        imageQueue = wants.filter((w) => w.id !== drawing);
        void drawPageImages();
      },
    });
    editor.value = e;
    benchHooks.editor = e;
    e.penSeen = settings.s.penSeen;
    applySettings();
    e.setCanvas(!!lib.current?.infiniteCanvas);
    e.setTool(settings.s.tool);
    unregister = registerFlusher(flush);
    if (sync.available) {
      offChanged = sync.onChanged((c) => {
        const nbId = lib.current?.id;
        for (const [n, p] of c.pages) if (n === nbId) remotePending.add(p);
        if (remotePending.size && !remoteTimer) remoteTimer = setTimeout(() => void applyRemoteChanges(), 50);
      });
    }
    document.addEventListener("visibilitychange", onVisibility);
    // Linux hands touchpad pinches over from GTK; see src-tauri/src/pinch.rs
    if (hasHost()) {
      void listen<PinchEvent>("penroam://pinch", ({ payload: p }) => editor.value?.platformPinch(p.phase, p.scale, p.x, p.y)).then((off) => {
        if (editor.value === e) offPinch = off;
        else off();
      });
    }
    pushPageList();
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
    offChanged?.();
    offPinch?.();
    if (remoteTimer) clearTimeout(remoteTimer);
    awareness?.clear();
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
  // live collaboration: report where I am only in a shared notebook, with presence on
  const sharedHere = () => sharing.byNotebook.get(lib.current?.id ?? "")?.state === "active";
  watch(
    [() => lib.current?.id, () => live.sharePresence, sharedHere, editor],
    () => {
      const e = editor.value;
      const id = lib.current?.id;
      awareness?.clear();
      awareness = null;
      const on = !!e && !!id && live.sharePresence && sharedHere();
      if (on) awareness = new AwarenessSender((p) => liveApi.awareness(id, p), () => !editor.value?.strokeActive());
      e?.setAwareness(on);
    },
    { immediate: true },
  );
  // other people's carets and areas, drawn over the page
  watch(
    [() => live.peersOf(lib.current?.id), editor],
    ([peers]) => editor.value?.setPeers(peers),
    { immediate: true },
  );
  // switched here by `reload`, or on another device and synced
  watch(() => !!lib.current?.infiniteCanvas, (on) => editor.value?.setCanvas(on));
  watch([() => lib.current?.id, () => lib.current?.pageOrder.join(","), () => lib.thumbs], pushPageList);
  watch(() => settings.s, applySettings, { deep: true });
  watch(() => settings.effectiveTheme(), applySettings);
  watch(
    () => ui.overlays > 0 || extraBlocked.value,
    (b) => editor.value?.setOverlayBlocked(b),
  );
  watch(
    () => state.tool,
    // view-only access forces the hand; that isn't the user's choice of tool, so it isn't remembered
    (t) => {
      if (!editor.value?.isReadOnly) settings.s.tool = t;
    },
  );
  watch(
    () => state.penSeen,
    (seen) => {
      if (!seen) return;
      settings.s.penSeen = true;
      if (settings.s.penOnly === "auto") console.info("pen detected: fingers now pan/zoom");
    },
  );

  /** Reloads pages changed outside the editor (sync, conflict resolution) once it is idle. */
  function markChanged(pageIds: string[]) {
    for (const p of pageIds) remotePending.add(p);
    if (remotePending.size && !remoteTimer) remoteTimer = setTimeout(() => void applyRemoteChanges(), 50);
  }

  /** View-only access (viewer, or access removed): the editor refuses every change. */
  function setReadOnly(on: boolean) {
    const e = editor.value;
    if (!e || e.isReadOnly === on) return;
    e.setReadOnly(on);
    if (!on) e.setTool(settings.s.tool);
  }

  return { state, editor, native, setReadOnly, commentAnchor, loading, pageError, flush, openPage, reload, markChanged, renderPagePng, retry: () => session?.retry(), session: () => session };
}
