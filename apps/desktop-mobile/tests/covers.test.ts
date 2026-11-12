// Library covers: saved ones are read, missing ones are drawn (cards on screen first) and saved,
// and nothing is drawn while a notebook is open.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";

const saved = new Map<string, Uint8Array>();
const drawn: string[] = [];
const summary = (id: string, updatedAt = 1) => ({ id, title: id, cover: "#3b5b7a", pageCount: 1, createdAt: 1, updatedAt, firstPageId: `${id}-p1` });
let notebooks = [summary("a"), summary("b"), summary("c")];

vi.mock("../src/platform/ipc", () => ({
  hasHost: () => true,
  CommandError: class extends Error {},
  api: {
    listNotebooks: vi.fn(async () => notebooks),
    listFolders: vi.fn(async () => []),
    libraryStatus: vi.fn(async () => ({ removedTempFiles: 0, orphanedFiles: 0, truncatedJournals: 0, replayedBatches: 0, warnings: [] })),
    getThumbnail: vi.fn(async (nb: string, page: string) => saved.get(`${nb}/${page}`) ?? null),
    saveThumbnail: vi.fn(async (nb: string, page: string, png: Uint8Array) => void saved.set(`${nb}/${page}`, png)),
    getNotebook: vi.fn(async (id: string) => ({ id, pageOrder: [`${id}-p1`], infiniteCanvas: false })),
    loadPage: vi.fn(async (nb: string, id: string) => ({ id, nb, elements: [] })),
    readAsset: vi.fn(),
  },
}));
vi.mock("../src/editor/pdf", () => ({ PdfCache: class { destroy() {} } }));
vi.mock("../src/editor/pagePicture", () => ({
  drawPagePng: vi.fn(async (page: { nb: string }) => {
    drawn.push(page.nb);
    return new Uint8Array([0x89, 0x50, 0x4e, 0x47]);
  }),
}));

const { useLibrary } = await import("../src/stores/library");

/** Lets the painters run to the end (each picture yields to a timer). */
async function settle() {
  for (let i = 0; i < 20; i++) {
    await flushPromises();
    await vi.advanceTimersByTimeAsync(20);
  }
}

describe("library covers", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    setActivePinia(createPinia());
    saved.clear();
    drawn.length = 0;
    notebooks = [summary("a"), summary("b"), summary("c")];
    globalThis.URL.createObjectURL = vi.fn(() => `blob:${Math.random()}`);
    globalThis.URL.revokeObjectURL = vi.fn();
  });

  it("reads saved covers and draws and saves only the missing ones", async () => {
    saved.set("b/b-p1", new Uint8Array([1]));
    const lib = useLibrary();
    await lib.refresh();
    await settle();
    expect(drawn.sort()).toEqual(["a", "c"]);
    expect(saved.has("a/a-p1") && saved.has("c/c-p1")).toBe(true);
    expect(Object.keys(lib.covers).sort()).toEqual(["a", "b", "c"]);
  });

  it("draws the covers on screen first", async () => {
    const lib = useLibrary();
    lib.coverVisible("c", true);
    await lib.refresh();
    await settle();
    expect(drawn[0]).toBe("c");
    expect(drawn).toHaveLength(3);
  });

  it("waits while a notebook is open, and draws a cover again once it changed elsewhere", async () => {
    const lib = useLibrary();
    lib.current = { id: "x" } as never;
    await lib.refresh();
    await settle();
    expect(drawn).toEqual([]);

    lib.current = null;
    await lib.refresh();
    await settle();
    expect(drawn.sort()).toEqual(["a", "b", "c"]);

    // sync installed a new version of b's first page, which dropped its saved picture
    saved.delete("b/b-p1");
    notebooks = [summary("a"), summary("b", 2), summary("c")];
    drawn.length = 0;
    await lib.refresh();
    await settle();
    expect(drawn).toEqual(["b"]);
  });
});
