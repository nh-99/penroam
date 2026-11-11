// Typed facade over the Rust commands (apps/desktop-mobile/src-tauri/src/commands.rs).
// Types come from the Rust schema via ts-rs (src/generated, checked in CI).
import { invoke, isTauri } from "@tauri-apps/api/core";
import { isMobile } from "./device";
import type { ApplyOutcome } from "../generated/ApplyOutcome";
import type { ArchiveSummary } from "../generated/ArchiveSummary";
import type { BackupPreview } from "../generated/BackupPreview";
import type { BackupProblem } from "../generated/BackupProblem";
import type { ConflictPolicy } from "../generated/ConflictPolicy";
import type { HistoryStats } from "../generated/HistoryStats";
import type { ImportLoss } from "../generated/ImportLoss";
import type { NotebookRestore } from "../generated/NotebookRestore";
import type { RestoreReport } from "../generated/RestoreReport";
import type { RemoteFile } from "../generated/RemoteFile";
import type { TextExport } from "../generated/TextExport";
import type { ErrorCode } from "../generated/ErrorCode";
import type { RenderedMath } from "../generated/RenderedMath";
import type { ExportReport } from "../generated/ExportReport";
import type { ImportReport } from "../generated/ImportReport";
import type { Notebook } from "../generated/Notebook";
import type { Folder } from "../generated/Folder";
import type { NotebookSummary } from "../generated/NotebookSummary";
import type { OperationBatch } from "../generated/OperationBatch";
import type { Page } from "../generated/Page";
import type { PdfBackgroundRef } from "../generated/PdfBackgroundRef";
import type { Paper } from "../generated/Paper";
import type { RecoveryReport } from "../generated/RecoveryReport";
import type { Template } from "../generated/Template";
import type { Size } from "../generated/Size";

/** One restorable version. Those older than this device's own record come from the account. */
export interface HistoryRow {
  pageId: string;
  revision: number | null;
  position: number | null;
  at: number;
  added: number;
  removed: number;
}

/** Where automatic backups go. */
export type AutoTarget = { kind: "folder"; dir: string } | { kind: "webdav"; url: string; user: string };

export interface BackupResult {
  bytes: number;
  notebooks: number;
  unreadable: BackupProblem[];
}

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

/** How a print request ended. "systemDialog": the system took over and reports nothing further. */
export type PrintStatus = "sent" | "cancelled" | "systemDialog" | "viewer";

export interface NotebookAndPage {
  notebook: Notebook;
  page: Page;
}

export const api = {
  libraryStatus: () => call<RecoveryReport>("library_status"),
  listNotebooks: () => call<NotebookSummary[]>("list_notebooks"),
  getNotebook: (id: string) => call<Notebook>("get_notebook", { id }),
  createNotebook: (title: string, paper: Paper, template: Template, cover?: string, vault?: string, infiniteCanvas?: boolean) =>
    call<NotebookAndPage>("create_notebook", { title, paper, template, cover, vault, infiniteCanvas }),
  /** Infinite canvases on or off; with `collapse`, every page is first gathered onto the first one. */
  setInfiniteCanvas: (id: string, infinite: boolean, collapse = false) => call<Notebook>("set_infinite_canvas", { id, infinite, collapse }),
  moveNotebookToVault: (id: string, vault: string) => call<Notebook>("move_notebook_to_vault", { id, vault }),
  renameNotebook: (id: string, title: string) => call<Notebook>("rename_notebook", { id, title }),
  listFolders: () => call<Folder[]>("list_folders"),
  createFolder: (name: string, parent: string | null) => call<Folder>("create_folder", { name, parent }),
  renameFolder: (id: string, name: string) => call<void>("rename_folder", { id, name }),
  moveFolder: (id: string, parent: string | null) => call<void>("move_folder", { id, parent }),
  deleteFolder: (id: string) => call<void>("delete_folder", { id }),
  fileNotebook: (id: string, folder: string | null) => call<Notebook>("file_notebook", { id, folder }),
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
  /** Opens a link from a text block in the system browser or mail app (http, https, mailto only). */
  openLink: (url: string) => call<void>("open_link", { url }),
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
  /** Adds a PDF to the notebook without making pages, for placing its pages on a canvas. */
  addPdfAsset: (notebookId: string, path: string, fileName?: string) =>
    call<{ notebook: Notebook; pages: PdfBackgroundRef[] }>("add_pdf_asset", { notebookId, path, fileName }),
  exportNotebook: (notebookId: string, path: string) => call<{ bytes: number; pages: number }>("export_notebook", { notebookId, path }),
  importNotebook: (path: string) => call<ImportReport>("import_notebook", { path }),
  inspectNotebook: (path: string) => call<ArchiveSummary>("inspect_notebook", { path }),
  importDocument: (path: string, fileName: string) => call<{ notebook: Notebook; report: ImportLoss }>("import_document", { path, fileName }),
  exportPages: (notebookId: string, pages: string[], path: string) => call<{ bytes: number; pages: number }>("export_pages", { notebookId, pages, path }),
  exportBackup: (path: string) => call<BackupResult>("export_backup", { path }),
  autoBackup: (target: AutoTarget) => call<BackupResult>("auto_backup", { target }),
  webdavConnect: (url: string, user: string, password: string) => call<RemoteFile[]>("webdav_connect", { url, user, password }),
  webdavDisconnect: () => call<void>("webdav_disconnect"),
  webdavBackups: (url: string, user: string) => call<RemoteFile[]>("webdav_backups", { url, user }),
  webdavFetch: (url: string, user: string, name: string) => call<string>("webdav_fetch", { url, user, name }),
  discardFetched: (path: string) => call<void>("discard_fetched", { path }),
  previewBackup: (path: string) => call<BackupPreview>("preview_backup", { path }),
  restoreBackup: (path: string, onConflict: ConflictPolicy) => call<RestoreReport>("restore_backup", { path, options: { onConflict } }),
  exportNotebookText: (notebookId: string, path: string, markdown: boolean) =>
    call<{ pages: number; textBlocks: number; warnings: string[] }>("export_notebook_text", { notebookId, path, markdown }),
  pagePlainText: (notebookId: string, pageId: string) => call<TextExport>("page_plain_text", { notebookId, pageId }),
  exportPageSvg: (notebookId: string, pageId: string, path: string) => call<string[]>("export_page_svg", { notebookId, pageId, path }),
  async saveRenderedFile(path: string, bytes: Uint8Array): Promise<void> {
    await invoke("save_rendered_file", isMobile() ? Array.from(bytes) : bytes, { headers: { "x-path": encodeURIComponent(path) } }).catch((e) => {
      throw new CommandError(e?.code ?? "io", e?.message ?? String(e));
    });
  },
  pageHistory: (notebookId: string, pageId: string) => call<HistoryRow[]>("page_history", { notebookId, pageId }),
  pageVersion: (notebookId: string, pageId: string, revision: number | null, position: number | null) => call<Page>("page_version", { notebookId, pageId, revision, position }),
  restorePageVersion: (notebookId: string, pageId: string, revision: number | null, position: number | null) =>
    call<ApplyOutcome>("restore_page_version", { notebookId, pageId, revision, position }),
  restoreNotebookVersion: (notebookId: string, at: number, fromAccount: boolean) => call<NotebookRestore>("restore_notebook_version", { notebookId, at, fromAccount }),
  historyStats: (notebookId: string) => call<HistoryStats>("history_stats", { notebookId }),
  clearHistory: (notebookId: string) => call<void>("clear_history", { notebookId }),
  notebookMath: (notebookId: string) => call<{ tex: string; display: boolean }[]>("notebook_math", { notebookId }),
  exportAnnotatedPdf: (notebookId: string, path: string, math: RenderedMath[]) => call<ExportReport>("export_annotated_pdf", { notebookId, path, math }),
  printNotebook: (notebookId: string, currentPage: string | null, math: RenderedMath[]) => call<{ status: PrintStatus; report: ExportReport }>("print_notebook", { notebookId, currentPage, math }),
  importStrokeDocument: (notebookId: string, pageId: string, json: string) => call<ApplyOutcome>("import_stroke_document", { notebookId, pageId, json }),
  exitApp: () => call<void>("exit_app"),
  // benchmark helpers (docs/benchmarks/protocol.md methodology)
  systemInfo: () => call<Record<string, unknown>>("system_info"),
  procStats: () => call<{ treeRssBytes: number; rssBytes: number; processes: number; note?: string }>("proc_stats"),
  startupMs: () => call<number>("startup_ms"),
  writePerf: (label: string, json: string) => call<string>("write_perf", { label, json }),
  autobenchRequested: () => call<string | null>("autobench_requested"),
};

export type Api = typeof api;
