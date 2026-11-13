// Library + open-notebook UI state. All data operations go through Rust; this
// store only mirrors the latest results for the views.
import { defineStore } from "pinia";
import { computed, ref, shallowRef } from "vue";
import type { Folder } from "../generated/Folder";
import type { Notebook } from "../generated/Notebook";
import type { NotebookSummary } from "../generated/NotebookSummary";
import type { Paper } from "../generated/Paper";
import type { Template } from "../generated/Template";
import type { PdfBackgroundRef } from "../generated/PdfBackgroundRef";
import type { RecoveryReport } from "../generated/RecoveryReport";
import { renderMathTable } from "../editor/math";
import { api, CommandError, hasHost } from "../platform/ipc";
import { pickFile, pickImportFiles, pickSaveLocation, safeFileName, type Picked } from "../platform/files";
import type { Kept } from "../generated/Kept";
import { useSync } from "./sync";
import { useSettings } from "./settings";
import { useUi } from "./ui";

/** A long operation's progress. `fraction` is null while there's no count to show. */
export interface Progress {
  label: string;
  fraction: number | null;
}

/** How one imported file went. */
export interface ImportResult {
  file: string;
  title: string;
  format: string;
  kept?: Kept;
  /** What did not come across, in plain language. */
  lost: string[];
  error?: string;
}

export const useLibrary = defineStore("library", () => {
  const ui = useUi();
  const notebooks = ref<NotebookSummary[]>([]);
  const loaded = ref(false);
  const loadError = ref<string | null>(null);
  const current = shallowRef<Notebook | null>(null);
  const currentPageId = ref<string | null>(null);
  const recovery = ref<RecoveryReport | null>(null);
  const busy = ref<string | null>(null);
  /** Results of the last import that has something to report. */
  const importResults = ref<ImportResult[] | null>(null);
  /** Shown as a bar with `busy`, for operations that report progress. */
  const progress = ref<Progress | null>(null);
  /** Answers the plaintext export question. Set while it's on screen. */
  const plaintextQuestion = ref<((ok: boolean) => void) | null>(null);

  /**
   * Exported files leave end-to-end encryption, so with sync on the user is asked first.
   * It's their data and they may export it: this only makes sure they know.
   */
  async function confirmPlaintext(): Promise<boolean> {
    const settings = useSettings();
    if (!useSync().configured || settings.s.plaintextExportOk) return true;
    return new Promise((resolve) => {
      plaintextQuestion.value = (ok) => {
        plaintextQuestion.value = null;
        resolve(ok);
      };
    });
  }
  /** pageId -> object URL */
  const thumbs = ref<Record<string, string>>({});
  /** notebookId -> object URL of its first page */
  const covers = ref<Record<string, string>>({});

  const folders = ref<Folder[]>([]);
  /** The folder being browsed; `null` is the top level. */
  const openFolderId = ref<string | null>(null);
  /** The folder being browsed, or `null` once it's gone (deleted on another device, say). */
  const openFolder = computed(() => folders.value.find((f) => f.id === openFolderId.value) ?? null);
  /** From the top-most folder down to the one being browsed. */
  const breadcrumb = computed(() => {
    const chain: Folder[] = [];
    for (let f = openFolder.value; f; f = folders.value.find((x) => x.id === f!.parent) ?? null) chain.unshift(f);
    return chain;
  });
  const subfolders = computed(() => folders.value.filter((f) => (f.parent ?? null) === (openFolder.value?.id ?? null)));

  const live = computed(() => notebooks.value.filter((n) => n.deletedAt == null));
  /** The live notebooks in the folder being browsed. */
  const shelf = computed(() => live.value.filter((n) => (n.folder ?? null) === (openFolder.value?.id ?? null)));
  /** How many notebooks and subfolders are directly inside a folder. */
  function folderCount(id: string): number {
    return live.value.filter((n) => n.folder === id).length + folders.value.filter((f) => f.parent === id).length;
  }
  const trashed = computed(() => notebooks.value.filter((n) => n.deletedAt != null));
  const pageIndex = computed(() => (current.value && currentPageId.value ? current.value.pageOrder.indexOf(currentPageId.value) : -1));

  function fail(e: unknown, what: string) {
    const msg = e instanceof CommandError ? e.message : String((e as Error)?.message ?? e);
    ui.error(`${what}: ${msg}`);
  }

  async function refresh() {
    try {
      [notebooks.value, folders.value] = await Promise.all([api.listNotebooks(), api.listFolders()]);
      loadError.value = null;
      if (!recovery.value) {
        recovery.value = await api.libraryStatus();
        const r = recovery.value;
        if (r.replayedBatches > 0 || r.truncatedJournals > 0) {
          ui.toast("info", `Recovered ${r.replayedBatches} saved edit batch(es) after an unexpected exit.`, { ms: 6000 });
        }
        for (const w of r.warnings.filter((w) => !w.includes("replayed"))) console.warn("recovery:", w);
      }
      void loadCovers();
    } catch (e) {
      loadError.value = e instanceof CommandError ? e.message : String(e);
    } finally {
      loaded.value = true;
    }
  }

  // ------------------------------------------------------------------ covers --
  //
  // A cover is the saved thumbnail of a notebook's first page. Thumbnails are a cache on each
  // device: they don't travel with sync or in archives, and storage drops a page's thumbnail
  // when sync installs a new version of the page. So a notebook that arrived from another
  // device, or changed there, has no cover here until one is drawn. The library draws those
  // itself, notebooks on screen first, and saves them like the editor's own thumbnails.

  /** notebookId -> the `updatedAt` its cover was read or drawn for. */
  const coverFor = new Map<string, number>();
  /** Notebooks whose cover is missing and waits to be drawn, in shelf order. */
  let toPaint: string[] = [];
  /** Notebooks whose card is on screen; their covers are drawn first. */
  const onScreen = new Set<string>();
  /** notebookId -> the `updatedAt` a cover could not be drawn for; not tried again until it changes. */
  const paintFailed = new Map<string, number>();
  let painters = 0;
  /** Covers drawn side by side. Reading a page and its PDFs overlaps with drawing another. */
  const PAINTERS = 2;
  /** Cached covers read side by side. */
  const READERS = 4;

  async function loadCovers() {
    const want = notebooks.value.filter((n) => n.firstPageId && n.deletedAt == null && coverFor.get(n.id) !== n.updatedAt);
    const missing: string[] = [];
    let next = 0;
    const read = async () => {
      for (let n = want[next++]; n; n = want[next++]) {
        const b = await api.getThumbnail(n.id, n.firstPageId!).catch(() => null);
        if (b) {
          showCover(n.id, b);
          coverFor.set(n.id, n.updatedAt);
        } else missing.push(n.id);
      }
    };
    await Promise.all(Array.from({ length: Math.min(READERS, want.length) }, read));
    if (!hasHost()) return;
    const order = new Map(notebooks.value.map((n, i) => [n.id, i]));
    toPaint = [...new Set([...toPaint, ...missing])].sort((a, b) => (order.get(a) ?? 0) - (order.get(b) ?? 0));
    paintCovers();
  }

  function showCover(nbId: string, png: Uint8Array) {
    const old = covers.value[nbId];
    covers.value = { ...covers.value, [nbId]: URL.createObjectURL(new Blob([png as BlobPart], { type: "image/png" })) };
    if (old) URL.revokeObjectURL(old);
  }

  /** A notebook's card came on screen or left it; covers on screen are drawn first. */
  function coverVisible(nbId: string, visible: boolean) {
    if (visible) onScreen.add(nbId);
    else onScreen.delete(nbId);
  }

  /** Starts drawing the missing covers, unless a notebook is open: the editor comes first. */
  function paintCovers() {
    while (painters < PAINTERS && toPaint.length && !current.value) {
      painters++;
      void (async () => {
        try {
          for (let id = takeNextCover(); id; id = takeNextCover()) await paintCover(id);
        } finally {
          painters--;
        }
      })();
    }
  }

  function takeNextCover(): string | undefined {
    if (current.value) return undefined;
    const i = Math.max(0, toPaint.findIndex((id) => onScreen.has(id)));
    return toPaint.splice(i, 1)[0];
  }

  async function paintCover(nbId: string) {
    const summary = notebooks.value.find((n) => n.id === nbId);
    if (!summary || summary.deletedAt != null || paintFailed.get(nbId) === summary.updatedAt || coverFor.get(nbId) === summary.updatedAt) return;
    const { PdfCache } = await import("../editor/pdf");
    const { drawPagePng } = await import("../editor/pagePicture");
    const pdfCache = new PdfCache((hash) => api.readAsset(nbId, hash), 1);
    try {
      const nb = await api.getNotebook(nbId);
      const pageId = nb.pageOrder[0];
      if (!pageId) return;
      const page = await api.loadPage(nbId, pageId);
      // as in the editor: a page this large takes too long to draw in the background
      if (page.elements.length > 20000) throw new Error("page too large for a background picture");
      const settings = useSettings();
      const png = await drawPagePng(
        page,
        240,
        { notebookId: nbId, infiniteCanvas: !!nb.infiniteCanvas, pdfCache, mono: settings.effectiveTheme() === "epaper", dark: false },
        { paper: true },
      );
      // the notebook was opened meanwhile: its editor draws the page as it is now
      if (!png || current.value?.id === nbId) return;
      await api.saveThumbnail(nbId, pageId, png);
      showCover(nbId, png);
      coverFor.set(nbId, summary.updatedAt);
    } catch (e) {
      paintFailed.set(nbId, summary.updatedAt);
      console.warn("cover", nbId, e);
    } finally {
      pdfCache.destroy();
    }
    // let the library take input between pictures
    await new Promise((r) => setTimeout(r, 16));
  }

  async function createNotebook(title: string, paper: Paper, template: Template, vault?: string, infiniteCanvas = false) {
    try {
      const { notebook, page } = await api.createNotebook(title, paper, template, undefined, vault, infiniteCanvas);
      await refresh();
      current.value = notebook;
      currentPageId.value = page.id;
      return notebook;
    } catch (e) {
      fail(e, "Could not create the notebook");
      return null;
    }
  }

  async function open(id: string, pageId?: string) {
    try {
      const nb = await api.getNotebook(id);
      current.value = nb;
      currentPageId.value = pageId && nb.pageOrder.includes(pageId) ? pageId : (nb.pageOrder[0] ?? null);
      thumbs.value = {};
      void loadThumbs();
    } catch (e) {
      fail(e, "Could not open the notebook");
    }
  }

  function close() {
    current.value = null;
    currentPageId.value = null;
    for (const u of Object.values(thumbs.value)) URL.revokeObjectURL(u);
    thumbs.value = {};
    void refresh();
  }

  async function loadThumbs() {
    const nb = current.value;
    if (!nb) return;
    for (const pid of nb.pageOrder) {
      if (thumbs.value[pid]) continue;
      const b = await api.getThumbnail(nb.id, pid).catch(() => null);
      if (b && current.value?.id === nb.id) setThumb(pid, b);
    }
  }

  function setThumb(pageId: string, png: Uint8Array) {
    const old = thumbs.value[pageId];
    const url = URL.createObjectURL(new Blob([png as BlobPart], { type: "image/png" }));
    thumbs.value = { ...thumbs.value, [pageId]: url };
    if (old) URL.revokeObjectURL(old);
    const nb = current.value;
    if (nb && nb.pageOrder[0] === pageId) {
      showCover(nb.id, png);
      // the editor drew it: nothing left to draw, and the library reads the saved one again
      toPaint = toPaint.filter((id) => id !== nb.id);
      coverFor.delete(nb.id);
    }
  }

  async function saveThumb(pageId: string, png: Uint8Array) {
    const nb = current.value;
    if (!nb) return;
    setThumb(pageId, png);
    await api.saveThumbnail(nb.id, pageId, png).catch((e) => console.warn("thumbnail not saved", e));
  }

  /** Remote (synced) changes touched the open notebook: refresh its metadata. */
  async function reloadCurrent() {
    const nb = current.value;
    if (!nb) return;
    try {
      const fresh = await api.getNotebook(nb.id);
      if (current.value?.id !== nb.id) return;
      current.value = fresh;
      const pid = currentPageId.value;
      if (pid && !fresh.pageOrder.includes(pid)) {
        // the open page was deleted on another device: it is in the trash, show a neighbour
        const old = nb.pageOrder.indexOf(pid);
        currentPageId.value = fresh.pageOrder[Math.min(Math.max(old, 0), fresh.pageOrder.length - 1)] ?? null;
        ui.info("The page you were on was deleted on another device. It is in this notebook's trash.");
      }
      void loadThumbs();
      void refresh();
    } catch (e) {
      if (e instanceof CommandError && e.code === "notFound" && current.value?.id === nb.id) {
        close();
        ui.info("This notebook was deleted on another device. You can restore it from the trash.");
        return;
      }
      console.warn("reload after sync", e);
    }
  }

  async function rename(id: string, title: string) {
    try {
      const nb = await api.renameNotebook(id, title);
      if (current.value?.id === id) current.value = nb;
      await refresh();
    } catch (e) {
      fail(e, "Could not rename");
    }
  }

  /**
   * Shows the open notebook's pages as infinite canvases, or as sheets again. With `collapse`
   * every page is gathered onto the first one, which then opens; the others go to the
   * notebook's trash. Flush the editor first. Returns false if it failed (already reported).
   */
  async function setInfiniteCanvas(infinite: boolean, collapse = false): Promise<boolean> {
    const nb = current.value;
    if (!nb) return false;
    if (collapse) busy.value = "Combining pages…";
    try {
      const updated = await api.setInfiniteCanvas(nb.id, infinite, collapse);
      current.value = updated;
      if (collapse) {
        currentPageId.value = updated.pageOrder[0] ?? null;
        await loadThumbs();
      }
      return true;
    } catch (e) {
      fail(e, infinite ? "Could not turn on the infinite canvas" : "Could not turn off the infinite canvas");
      return false;
    } finally {
      busy.value = null;
    }
  }

  async function setCover(id: string, cover: string) {
    try {
      const nb = await api.setNotebookCover(id, cover);
      if (current.value?.id === id) current.value = nb;
      await refresh();
    } catch (e) {
      fail(e, "Could not change the cover");
    }
  }

  async function createFolder(name: string) {
    try {
      await api.createFolder(name, openFolder.value?.id ?? null);
      await refresh();
    } catch (e) {
      fail(e, "Could not create the folder");
    }
  }

  async function renameFolder(id: string, name: string) {
    try {
      await api.renameFolder(id, name);
      await refresh();
    } catch (e) {
      fail(e, "Could not rename the folder");
    }
  }

  async function moveFolder(id: string, parent: string | null) {
    try {
      await api.moveFolder(id, parent);
      await refresh();
    } catch (e) {
      fail(e, "Could not move the folder");
    }
  }

  async function deleteFolder(id: string) {
    try {
      await api.deleteFolder(id);
      await refresh();
    } catch (e) {
      fail(e, "Could not delete the folder");
    }
  }

  async function fileNotebook(id: string, folder: string | null) {
    try {
      const nb = await api.fileNotebook(id, folder);
      if (current.value?.id === id) current.value = nb;
      await refresh();
    } catch (e) {
      fail(e, "Could not move the notebook");
    }
  }

  /** Makes a copy of a notebook next to it, titled "<title> (copy)". */
  async function duplicate(id: string) {
    busy.value = "Duplicating…";
    try {
      await api.duplicateNotebook(id);
      await refresh();
      ui.toast("info", "Notebook duplicated.");
    } catch (e) {
      fail(e, "Could not duplicate the notebook");
    } finally {
      busy.value = null;
    }
  }

  /** Moves a notebook into a vault; the copy has a new id. */
  async function moveToVault(id: string, vault: string) {
    try {
      await api.moveNotebookToVault(id, vault);
      await refresh();
      await useSync().refreshVaults();
      ui.toast("info", "Notebook moved to the vault.");
    } catch (e) {
      fail(e, "Could not move the notebook");
    }
  }

  async function trash(id: string) {
    try {
      await api.deleteNotebook(id);
      await refresh();
      ui.toast("info", "Notebook moved to the trash.", { action: { label: "Undo", run: () => void restore(id) } });
    } catch (e) {
      fail(e, "Could not delete");
    }
  }

  async function restore(id: string) {
    try {
      await api.restoreNotebook(id);
      await refresh();
    } catch (e) {
      fail(e, "Could not restore");
    }
  }

  async function purge(id: string) {
    try {
      await api.purgeNotebook(id);
      await refresh();
    } catch (e) {
      fail(e, "Could not delete permanently");
    }
  }

  async function addPage(afterIndex?: number, template?: Template) {
    const nb = current.value;
    if (!nb) return null;
    try {
      const idx = afterIndex === undefined ? nb.pageOrder.length : afterIndex + 1;
      const { notebook, page } = await api.createPage(nb.id, idx, template ?? nb.defaults.template, nb.defaults.paper);
      current.value = notebook;
      return page.id;
    } catch (e) {
      fail(e, "Could not add a page");
      return null;
    }
  }

  async function deletePage(pageId: string) {
    const nb = current.value;
    if (!nb) return;
    try {
      const idx = nb.pageOrder.indexOf(pageId);
      current.value = await api.deletePage(nb.id, pageId);
      if (currentPageId.value === pageId) {
        const order = current.value.pageOrder;
        currentPageId.value = order[Math.min(idx, order.length - 1)] ?? null;
      }
      ui.toast("info", "Page moved to the trash.", { action: { label: "Undo", run: () => void restorePage(pageId) } });
    } catch (e) {
      fail(e, "Could not delete the page");
    }
  }

  async function restorePage(pageId: string) {
    const nb = current.value;
    if (!nb) return;
    try {
      current.value = await api.restorePage(nb.id, pageId);
      void loadThumbs();
    } catch (e) {
      fail(e, "Could not restore the page");
    }
  }

  async function reorder(order: string[]) {
    const nb = current.value;
    if (!nb) return;
    const prev = nb;
    current.value = { ...nb, pageOrder: order }; // optimistic
    try {
      current.value = await api.updatePageOrder(nb.id, order);
    } catch (e) {
      current.value = prev;
      fail(e, "Could not reorder pages");
    }
  }

  async function importPdf(): Promise<string | null> {
    const nb = current.value;
    if (!nb) return null;
    const f = await pickFile("pdf");
    if (!f) return null;
    busy.value = "Importing PDF…";
    try {
      const after = pageIndex.value >= 0 ? pageIndex.value + 1 : undefined;
      const { notebook, pages } = await api.importPdf(nb.id, f.path, f.name, after);
      current.value = notebook;
      ui.success(`Imported ${pages.length} page${pages.length === 1 ? "" : "s"} from ${f.name}.`);
      return pages[0]?.id ?? null;
    } catch (e) {
      fail(e, "PDF import failed");
      return null;
    } finally {
      busy.value = null;
    }
  }

  /**
   * Picks a PDF and adds it to the open notebook, for placing its pages on the canvas.
   * Returns references to its pages, or null if nothing was imported.
   */
  async function importPdfForCanvas(): Promise<PdfBackgroundRef[] | null> {
    const nb = current.value;
    if (!nb) return null;
    const f = await pickFile("pdf");
    if (!f) return null;
    busy.value = "Importing PDF…";
    try {
      const { notebook, pages } = await api.addPdfAsset(nb.id, f.path, f.name);
      current.value = notebook;
      ui.success(`Placed ${pages.length} page${pages.length === 1 ? "" : "s"} from ${f.name} on the canvas.`);
      return pages;
    } catch (e) {
      fail(e, "PDF import failed");
      return null;
    } finally {
      busy.value = null;
    }
  }

  async function exportNotebook(id: string, title: string, beforeExport?: () => Promise<void>, includeComments = false, includeAuthors = false) {
    if (!(await confirmPlaintext())) return;
    const path = await pickSaveLocation("penroam", safeFileName(title));
    if (!path) return;
    busy.value = "Exporting…";
    try {
      await beforeExport?.();
      const r = await api.exportNotebook(id, path, includeComments, includeAuthors);
      ui.success(`Exported ${r.pages} page${r.pages === 1 ? "" : "s"} (${(r.bytes / 1024).toFixed(0)} KB).`);
    } catch (e) {
      fail(e, "Export failed");
    } finally {
      busy.value = null;
    }
  }

  async function exportPdf(id: string, title: string, beforeExport?: () => Promise<void>) {
    if (!(await confirmPlaintext())) return;
    const path = await pickSaveLocation("pdf", safeFileName(title));
    if (!path) return;
    busy.value = "Writing PDF…";
    try {
      await beforeExport?.();
      const r = await api.exportAnnotatedPdf(id, path, await renderMathTable(await api.notebookMath(id)));
      ui.success(`PDF saved: ${r.pages} page${r.pages === 1 ? "" : "s"}, ${r.strokes} strokes, ${r.textBlocks} text blocks.`);
      for (const w of r.warnings) ui.toast("info", w, { ms: 9000 });
    } catch (e) {
      fail(e, "PDF export failed");
    } finally {
      busy.value = null;
    }
  }

  async function printNotebook(id: string, currentPage: string | null = null, beforePrint?: () => Promise<void>) {
    // a held Ctrl+P repeats; one dialog is enough
    if (busy.value) return;
    busy.value = "Preparing to print…";
    try {
      await beforePrint?.();
      const { status, report } = await api.printNotebook(id, currentPage, await renderMathTable(await api.notebookMath(id)));
      if (status === "sent") ui.success(`Sent ${report.pages} page${report.pages === 1 ? "" : "s"} to the printer.`);
      else if (status === "viewer") ui.toast("info", "Opened in your PDF viewer. Print from there.", { ms: 9000 });
      if (status !== "cancelled") for (const w of report.warnings) ui.toast("info", w, { ms: 9000 });
    } catch (e) {
      fail(e, "Printing failed");
    } finally {
      busy.value = null;
    }
  }

  /**
   * Imports the chosen files, each into a new notebook. One failing doesn't stop the
   * others, and the library's existing notebooks are never touched.
   */
  async function importFiles() {
    const files = await pickImportFiles();
    if (!files.length) return;
    const results: ImportResult[] = [];
    try {
      for (const [i, f] of files.entries()) {
        busy.value = files.length > 1 ? `Importing ${i + 1} of ${files.length}…` : "Importing…";
        results.push(await importOne(f));
      }
    } finally {
      busy.value = null;
    }
    await refresh();
    const clean = results.length === 1 && !results[0].error && !results[0].lost.length;
    if (clean) ui.success(`Imported “${results[0].title}”.`);
    else importResults.value = results;
  }

  async function importOne(f: Picked): Promise<ImportResult> {
    try {
      if (f.name.toLowerCase().endsWith(".penroam")) {
        const r = await api.importNotebook(f.path);
        return { file: f.name, title: r.notebook.title, format: "Penroam notebook", lost: r.warnings };
      }
      const { notebook, report } = await api.importDocument(f.path, f.name);
      return { file: f.name, title: notebook.title, format: report.format, kept: report.kept, lost: report.lost };
    } catch (e) {
      return { file: f.name, title: "", format: "", lost: [], error: e instanceof Error ? e.message : String(e) };
    }
  }

  /** Lossy exports: the words only, with what's missing spelled out afterwards. */
  async function exportText(id: string, title: string, markdown: boolean, beforeExport?: () => Promise<void>) {
    if (!(await confirmPlaintext())) return;
    const path = await pickSaveLocation(markdown ? "md" : "txt", safeFileName(title));
    if (!path) return;
    busy.value = "Exporting…";
    try {
      await beforeExport?.();
      const r = await api.exportNotebookText(id, path, markdown);
      ui.success(`Saved ${r.textBlocks} text block${r.textBlocks === 1 ? "" : "s"} from ${r.pages} page${r.pages === 1 ? "" : "s"}.`);
      for (const w of r.warnings) ui.toast("info", w, { ms: 9000 });
    } catch (e) {
      fail(e, "Export failed");
    } finally {
      busy.value = null;
    }
  }

  async function exportPageSvg(pageId: string, title: string, beforeExport?: () => Promise<void>) {
    const nb = current.value;
    if (!nb || !(await confirmPlaintext())) return;
    const path = await pickSaveLocation("svg", safeFileName(title));
    if (!path) return;
    try {
      await beforeExport?.();
      const warnings = await api.exportPageSvg(nb.id, pageId, path);
      ui.success("Page saved as SVG.");
      for (const w of warnings) ui.toast("info", w, { ms: 9000 });
    } catch (e) {
      fail(e, "Export failed");
    }
  }

  async function exportPagePng(title: string, render: () => Promise<Uint8Array | null>, beforeExport?: () => Promise<void>) {
    if (!(await confirmPlaintext())) return;
    const path = await pickSaveLocation("png", safeFileName(title));
    if (!path) return;
    busy.value = "Exporting…";
    try {
      await beforeExport?.();
      const png = await render();
      if (!png) throw new Error("the page could not be drawn");
      await api.saveRenderedFile(path, png);
      ui.success("Page saved as PNG.");
    } catch (e) {
      fail(e, "Export failed");
    } finally {
      busy.value = null;
    }
  }

  async function exportPage(pageId: string, title: string, beforeExport?: () => Promise<void>) {
    const nb = current.value;
    if (!nb || !(await confirmPlaintext())) return;
    const path = await pickSaveLocation("penroam", safeFileName(title));
    if (!path) return;
    try {
      await beforeExport?.();
      await api.exportPages(nb.id, [pageId], path);
      ui.success("Page saved as a notebook.");
    } catch (e) {
      fail(e, "Export failed");
    }
  }

  async function copyPageText(pageId: string, beforeCopy?: () => Promise<void>) {
    const nb = current.value;
    if (!nb) return;
    try {
      await beforeCopy?.();
      const r = await api.pagePlainText(nb.id, pageId);
      const body = r.text.split("\n\n").slice(2).join("\n\n").trim();
      if (!body) {
        ui.toast("info", "This page has no typed text to copy.");
        return;
      }
      await navigator.clipboard.writeText(body);
      ui.success("Copied the page's text.");
      for (const w of r.warnings) ui.toast("info", w, { ms: 9000 });
    } catch (e) {
      fail(e, "Could not copy");
    }
  }

  return {
    folders,
    openFolderId,
    openFolder,
    breadcrumb,
    subfolders,
    shelf,
    folderCount,
    createFolder,
    renameFolder,
    moveFolder,
    deleteFolder,
    fileNotebook,
    notebooks,
    live,
    trashed,
    loaded,
    loadError,
    current,
    currentPageId,
    pageIndex,
    recovery,
    busy,
    thumbs,
    covers,
    coverVisible,
    refresh,
    createNotebook,
    moveToVault,
    duplicate,
    open,
    close,
    rename,
    setCover,
    trash,
    restore,
    purge,
    addPage,
    deletePage,
    restorePage,
    reorder,
    importPdf,
    importPdfForCanvas,
    setInfiniteCanvas,
    importFiles,
    importResults,
    progress,
    plaintextQuestion,
    confirmPlaintext,
    exportText,
    exportPageSvg,
    exportPagePng,
    exportPage,
    copyPageText,
    exportNotebook,
    exportPdf,
    printNotebook,
    saveThumb,
    loadThumbs,
    reloadCurrent,
  };
});
