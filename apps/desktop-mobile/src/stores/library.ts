// Library + open-notebook UI state. All data operations go through Rust; this
// store only mirrors the latest results for the views.
import { defineStore } from "pinia";
import { computed, ref, shallowRef } from "vue";
import type { Notebook } from "../generated/Notebook";
import type { NotebookSummary } from "../generated/NotebookSummary";
import type { Paper } from "../generated/Paper";
import type { Template } from "../generated/Template";
import type { RecoveryReport } from "../generated/RecoveryReport";
import { renderMathTable } from "../editor/math";
import { api, CommandError } from "../platform/ipc";
import { pickFile, pickSaveLocation, safeFileName } from "../platform/files";
import { useUi } from "./ui";

export const useLibrary = defineStore("library", () => {
  const ui = useUi();
  const notebooks = ref<NotebookSummary[]>([]);
  const loaded = ref(false);
  const loadError = ref<string | null>(null);
  const current = shallowRef<Notebook | null>(null);
  const currentPageId = ref<string | null>(null);
  const recovery = ref<RecoveryReport | null>(null);
  const busy = ref<string | null>(null);
  /** pageId -> object URL */
  const thumbs = ref<Record<string, string>>({});
  /** notebookId -> object URL of its first page */
  const covers = ref<Record<string, string>>({});

  const live = computed(() => notebooks.value.filter((n) => n.deletedAt == null));
  const trashed = computed(() => notebooks.value.filter((n) => n.deletedAt != null));
  const pageIndex = computed(() => (current.value && currentPageId.value ? current.value.pageOrder.indexOf(currentPageId.value) : -1));

  function fail(e: unknown, what: string) {
    const msg = e instanceof CommandError ? e.message : String((e as Error)?.message ?? e);
    ui.error(`${what}: ${msg}`);
  }

  async function refresh() {
    try {
      notebooks.value = await api.listNotebooks();
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

  async function loadCovers() {
    for (const n of notebooks.value) {
      if (!n.firstPageId || covers.value[n.id]) continue;
      const b = await api.getThumbnail(n.id, n.firstPageId).catch(() => null);
      if (b) covers.value = { ...covers.value, [n.id]: URL.createObjectURL(new Blob([b as BlobPart], { type: "image/png" })) };
    }
  }

  async function createNotebook(title: string, paper: Paper, template: Template) {
    try {
      const { notebook, page } = await api.createNotebook(title, paper, template);
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
      const oc = covers.value[nb.id];
      covers.value = { ...covers.value, [nb.id]: URL.createObjectURL(new Blob([png as BlobPart], { type: "image/png" })) };
      if (oc) URL.revokeObjectURL(oc);
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

  async function setCover(id: string, cover: string) {
    try {
      const nb = await api.setNotebookCover(id, cover);
      if (current.value?.id === id) current.value = nb;
      await refresh();
    } catch (e) {
      fail(e, "Could not change the cover");
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

  async function exportNotebook(id: string, title: string, beforeExport?: () => Promise<void>) {
    const path = await pickSaveLocation("penroam", safeFileName(title));
    if (!path) return;
    busy.value = "Exporting…";
    try {
      await beforeExport?.();
      const r = await api.exportNotebook(id, path);
      ui.success(`Exported ${r.pages} page${r.pages === 1 ? "" : "s"} (${(r.bytes / 1024).toFixed(0)} KB).`);
    } catch (e) {
      fail(e, "Export failed");
    } finally {
      busy.value = null;
    }
  }

  async function exportPdf(id: string, title: string, beforeExport?: () => Promise<void>) {
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

  async function importNotebook() {
    const f = await pickFile("penroam");
    if (!f) return;
    busy.value = "Importing notebook…";
    try {
      const r = await api.importNotebook(f.path);
      await refresh();
      ui.success(`Imported “${r.notebook.title}”.`);
      for (const w of r.warnings) ui.toast("info", w);
    } catch (e) {
      fail(e, "Import failed");
    } finally {
      busy.value = null;
    }
  }

  return {
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
    refresh,
    createNotebook,
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
    importNotebook,
    exportNotebook,
    exportPdf,
    printNotebook,
    saveThumb,
    loadThumbs,
    reloadCurrent,
  };
});
