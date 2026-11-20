// Reopening the app where it was left: the saved place, and the library going back to it.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { createPinia, setActivePinia } from "pinia";
import { api } from "../src/platform/ipc";
import { readResume, setPendingView, takePendingView, writeResume } from "../src/platform/resume";

const KEY = "penroam.resume.v1";
const nb = (id: string, extra: Record<string, unknown> = {}) => ({ id, title: id, pageCount: 2, cover: "#123456", updatedAt: 0, createdAt: 0, ...extra }) as any;
const notebook = (id: string) => ({ id, title: id, pageOrder: ["p1", "p2"], trashedPages: [] }) as any;

async function library() {
  setActivePinia(createPinia());
  const { useLibrary } = await import("../src/stores/library");
  const lib = useLibrary();
  lib.notebooks = [nb("a"), nb("gone", { deletedAt: 5 })];
  lib.folders = [{ id: "f1", name: "Work" } as any];
  return lib;
}

beforeEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
  setPendingView(null);
});

describe("the saved place", () => {
  it("reads nothing saved, or anything malformed, as the library's top level", () => {
    expect(readResume()).toEqual({ notebookId: null, pageId: null, folderId: null, view: null });
    localStorage.setItem(KEY, "{not json");
    expect(readResume().notebookId).toBeNull();
    localStorage.setItem(KEY, JSON.stringify({ notebookId: 7, view: { notebookId: "a", pageId: "p1", scale: -1, x: 0, y: 0 } }));
    expect(readResume()).toEqual({ notebookId: null, pageId: null, folderId: null, view: null });
  });

  it("keeps the parts it isn't given", () => {
    writeResume({ notebookId: "a", pageId: "p2" });
    writeResume({ view: { notebookId: "a", pageId: "p2", scale: 1.5, x: 10, y: 400 } });
    expect(readResume()).toEqual({ notebookId: "a", pageId: "p2", folderId: null, view: { notebookId: "a", pageId: "p2", scale: 1.5, x: 10, y: 400 } });
  });

  it("hands the start-up view to the first page opened only, and only if it's that page", () => {
    setPendingView({ notebookId: "a", pageId: "p2", scale: 2, x: 1, y: 2 });
    expect(takePendingView("a", "p1")).toBeNull();
    expect(takePendingView("a", "p2")).toBeNull();
    setPendingView({ notebookId: "a", pageId: "p2", scale: 2, x: 1, y: 2 });
    expect(takePendingView("a", "p2")).toMatchObject({ scale: 2, x: 1, y: 2 });
    expect(takePendingView("a", "p2")).toBeNull();
  });
});

describe("going back to it", () => {
  it("writes down the folder, notebook and page as the user moves", async () => {
    vi.spyOn(api, "getNotebook").mockResolvedValue(notebook("a"));
    vi.spyOn(api, "getThumbnail").mockResolvedValue(null as any);
    const lib = await library();
    lib.openFolderId = "f1";
    await lib.open("a", "p2");
    await nextTick();
    expect(readResume()).toMatchObject({ notebookId: "a", pageId: "p2", folderId: "f1" });
    vi.spyOn(api, "listNotebooks").mockResolvedValue([]);
    vi.spyOn(api, "listFolders").mockResolvedValue([]);
    lib.close();
    await nextTick();
    expect(readResume()).toMatchObject({ notebookId: null, pageId: null, folderId: "f1" });
  });

  it("reopens the notebook and page, and leaves the view for the editor", async () => {
    const get = vi.spyOn(api, "getNotebook").mockResolvedValue(notebook("a"));
    vi.spyOn(api, "getThumbnail").mockResolvedValue(null as any);
    writeResume({ notebookId: "a", pageId: "p2", folderId: "f1", view: { notebookId: "a", pageId: "p2", scale: 1.2, x: 0, y: 300 } });
    const lib = await library();
    await lib.resume();
    expect(get).toHaveBeenCalledWith("a");
    expect(lib.current?.id).toBe("a");
    expect(lib.currentPageId).toBe("p2");
    expect(lib.openFolderId).toBe("f1");
    expect(takePendingView("a", "p2")).toMatchObject({ scale: 1.2, y: 300 });
  });

  it("stays in the library when the notebook or folder has gone", async () => {
    const get = vi.spyOn(api, "getNotebook");
    writeResume({ notebookId: "gone", pageId: "p1", folderId: "f-deleted", view: { notebookId: "gone", pageId: "p1", scale: 1, x: 0, y: 0 } });
    const lib = await library();
    await lib.resume();
    expect(get).not.toHaveBeenCalled();
    expect(lib.current).toBeNull();
    expect(lib.openFolderId).toBeNull();
    expect(takePendingView("gone", "p1")).toBeNull();
  });
});
