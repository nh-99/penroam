import { describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";

// In-memory stand-in for the Rust command layer (tests only).
const store: any = { notebooks: [] as any[], pages: new Map<string, any>() };
vi.mock("../src/platform/ipc", () => {
  const nb = (title: string) => ({ schemaVersion: 1, id: `nb-${store.notebooks.length + 1}`, title, createdAt: 1, updatedAt: 1, pageOrder: ["pg-1"], trashedPages: [], defaults: { paper: "letter", template: { kind: "ruled", spacing: 24 } }, cover: "#3b5b7a", assets: [] });
  const page = { schemaVersion: 1, id: "pg-1", paper: "letter", size: { width: 612, height: 792 }, template: { kind: "ruled", spacing: 24 }, revision: 0, createdAt: 1, updatedAt: 1, elements: [] };
  return {
    hasHost: () => false,
    CommandError: class extends Error {},
    api: {
      listNotebooks: vi.fn(async () => store.notebooks.map((n: any) => ({ id: n.id, title: n.title, cover: n.cover, pageCount: n.pageOrder.length, createdAt: 1, updatedAt: 1, firstPageId: "pg-1" }))),
      libraryStatus: vi.fn(async () => ({ removedTempFiles: 0, orphanedFiles: 0, truncatedJournals: 0, replayedBatches: 0, warnings: [] })),
      createNotebook: vi.fn(async (title: string) => {
        const n = nb(title);
        store.notebooks.push(n);
        return { notebook: n, page };
      }),
      getNotebook: vi.fn(async (id: string) => store.notebooks.find((n: any) => n.id === id)),
      loadPage: vi.fn(async () => structuredClone(page)),
      applyPageOperations: vi.fn(async () => ({ revision: 1, updatedAt: 1, durable: true })),
      getThumbnail: vi.fn(async () => null),
      saveThumbnail: vi.fn(async () => {}),
      autobenchRequested: vi.fn(async () => null),
    },
  };
});

describe("notebook UI navigation", () => {
  it("empty library -> create notebook -> editor -> back to library", async () => {
    setActivePinia(createPinia());
    const App = (await import("../src/App.vue")).default;
    const w = mount(App, { attachTo: document.body });
    await flushPromises();
    expect(w.text()).toContain("Your shelf is empty");
    await w.findAll("button").find((b) => b.text().includes("New notebook"))!.trigger("click");
    await flushPromises();
    const input = document.body.querySelector<HTMLInputElement>('input[placeholder="Untitled notebook"]')!;
    input.value = "Lab notes";
    input.dispatchEvent(new Event("input"));
    document.body.querySelector<HTMLFormElement>("form")!.dispatchEvent(new Event("submit"));
    await flushPromises();
    expect(w.text()).toContain("Lab notes");
    expect(w.text()).toContain("1 / 1");
    expect(w.find('[role="toolbar"]').exists()).toBe(true);
    await w.find('button[aria-label="Back to notebooks"]').trigger("click");
    await flushPromises();
    expect(w.text()).toContain("Lab notes");
    expect(w.find('[aria-label="Open Lab notes"]').exists()).toBe(true);
    w.unmount();
  });
});
