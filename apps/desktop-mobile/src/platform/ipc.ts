// Typed facade over the Rust commands (apps/desktop-mobile/src-tauri/src/commands.rs).
// Types come from the Rust schema via ts-rs (src/generated, checked in CI).
import { invoke, isTauri } from "@tauri-apps/api/core";
import { isMobile } from "./device";
import type { ApplyOutcome } from "../generated/ApplyOutcome";
import type { ErrorCode } from "../generated/ErrorCode";
import type { ExportReport } from "../generated/ExportReport";
import type { ImportReport } from "../generated/ImportReport";
import type { Notebook } from "../generated/Notebook";
import type { NotebookSummary } from "../generated/NotebookSummary";
import type { OperationBatch } from "../generated/OperationBatch";
import type { Page } from "../generated/Page";
import type { Paper } from "../generated/Paper";
import type { RecoveryReport } from "../generated/RecoveryReport";
import type { Template } from "../generated/Template";
import type { Size } from "../generated/Size";

export class CommandError extends Error {
  constructor(
    readonly code: ErrorCode | "noHost",
    message: string,
  ) {
    super(message);
  }
}

export const hasHost = (): boolean => {
  try {
    return isTauri();
  } catch {
    return false;
  }
};

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasHost()) throw new CommandError("noHost", "Penroam's storage engine is not available (run inside the Penroam app).");
  try {
    return await invoke<T>(cmd, args);
  } catch (e: any) {
    if (e && typeof e === "object" && "code" in e) throw new CommandError(e.code, e.message ?? String(e));
    throw new CommandError("io", String(e));
  }
}

const decoder = new TextDecoder();
const asBytes = (b: ArrayBuffer | Uint8Array | number[]) => (b instanceof Uint8Array ? b : b instanceof ArrayBuffer ? new Uint8Array(b) : Uint8Array.from(b));

export interface NotebookAndPage {
  notebook: Notebook;
  page: Page;
}

export const api = {
  libraryStatus: () => call<RecoveryReport>("library_status"),
  listNotebooks: () => call<NotebookSummary[]>("list_notebooks"),
  getNotebook: (id: string) => call<Notebook>("get_notebook", { id }),
  createNotebook: (title: string, paper: Paper, template: Template, cover?: string) =>
    call<NotebookAndPage>("create_notebook", { title, paper, template, cover }),
  renameNotebook: (id: string, title: string) => call<Notebook>("rename_notebook", { id, title }),
  setNotebookCover: (id: string, cover: string) => call<Notebook>("set_notebook_cover", { id, cover }),
  deleteNotebook: (id: string) => call<Notebook>("delete_notebook", { id }),
  restoreNotebook: (id: string) => call<Notebook>("restore_notebook", { id }),
  purgeNotebook: (id: string) => call<void>("purge_notebook", { id }),
  createPage: (notebookId: string, index?: number, template?: Template, paper?: Paper, size?: Size) =>
    call<NotebookAndPage>("create_page", { notebookId, index, template, paper, size }),
  updatePageOrder: (notebookId: string, order: string[]) => call<Notebook>("update_page_order", { notebookId, order }),
  deletePage: (notebookId: string, pageId: string) => call<Notebook>("delete_page", { notebookId, pageId }),
  restorePage: (notebookId: string, pageId: string) => call<Notebook>("restore_page", { notebookId, pageId }),
  async loadPage(notebookId: string, pageId: string): Promise<Page> {
    const raw = await call<ArrayBuffer>("load_page", { notebookId, pageId });
    return JSON.parse(decoder.decode(asBytes(raw))) as Page;
  },
  applyPageOperations: (notebookId: string, pageId: string, batch: OperationBatch) =>
    call<ApplyOutcome>("apply_page_operations", { notebookId, pageId, batch }),
  flushPage: (notebookId: string, pageId: string) => call<void>("flush_page", { notebookId, pageId }),
  flushAll: () => call<void>("flush_all"),
  async readAsset(notebookId: string, hash: string): Promise<Uint8Array> {
    return asBytes(await call<ArrayBuffer>("read_asset", { notebookId, hash }));
  },
  async saveThumbnail(notebookId: string, pageId: string, png: Uint8Array): Promise<void> {
    if (!hasHost()) return;
    // mobile IPC has no raw-body path: send a plain byte array there
    await invoke("save_thumbnail", isMobile() ? Array.from(png) : png, { headers: { "x-notebook": notebookId, "x-page": pageId } }).catch((e) => {
      throw new CommandError(e?.code ?? "io", e?.message ?? String(e));
    });
  },
  async getThumbnail(notebookId: string, pageId: string): Promise<Uint8Array | null> {
    const b = asBytes(await call<ArrayBuffer>("get_thumbnail", { notebookId, pageId }));
    return b.length ? b : null;
  },
  importPdf: (notebookId: string, path: string, fileName?: string, index?: number) =>
    call<{ notebook: Notebook; pages: Page[] }>("import_pdf", { notebookId, path, fileName, index }),
  exportNotebook: (notebookId: string, path: string) => call<{ bytes: number; pages: number }>("export_notebook", { notebookId, path }),
  importNotebook: (path: string) => call<ImportReport>("import_notebook", { path }),
  exportAnnotatedPdf: (notebookId: string, path: string) => call<ExportReport>("export_annotated_pdf", { notebookId, path }),
  importM00Strokes: (notebookId: string, pageId: string, json: string) => call<ApplyOutcome>("import_m00_strokes", { notebookId, pageId, json }),
  exitApp: () => call<void>("exit_app"),
  // benchmark helpers (M00 methodology)
  systemInfo: () => call<Record<string, unknown>>("system_info"),
  procStats: () => call<{ treeRssBytes: number; rssBytes: number; processes: number; note?: string }>("proc_stats"),
  startupMs: () => call<number>("startup_ms"),
  writePerf: (label: string, json: string) => call<string>("write_perf", { label, json }),
  autobenchRequested: () => call<string | null>("autobench_requested"),
};

export type Api = typeof api;
