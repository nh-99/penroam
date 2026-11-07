import { beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { useSync } from "../src/stores/sync";
import type { ErasedNotebook } from "../src/platform/sync";

const api = vi.hoisted(() => ({
  erasedNotebooks: vi.fn(),
  vaults: vi.fn(async () => ({ supported: false, vaults: [], notebooks: {} })),
  restoreErased: vi.fn(),
  storeReconcile: vi.fn(async () => ({ applied: 0, conflicts: 0, waiting: 0, message: null })),
  info: vi.fn(),
  account: vi.fn(async () => null),
}));
const status = vi.hoisted(() => ({ emit: (_: unknown) => {} }));

vi.mock("../src/platform/ipc", async (real) => ({ ...(await real<object>()), hasHost: () => true }));
vi.mock("../src/platform/sync", async (real) => ({
  ...(await real<object>()),
  syncApi: api,
  onSyncStatus: async (cb: (s: unknown) => void) => {
    status.emit = cb;
    return () => {};
  },
  onSyncChanged: async () => () => {},
}));

const nb = (id: string, inTrash = false): ErasedNotebook => ({ notebookId: id, title: id, inTrash, cover: "#123456", pageCount: 2 });

async function connected(list: ErasedNotebook[]) {
  api.erasedNotebooks.mockResolvedValue(list);
  api.info.mockResolvedValue({ available: true, configured: true, defaultServerUrl: "", devBuild: false, status: { phase: "idle" } });
  const sync = useSync();
  await sync.init();
  await vi.waitFor(() => expect(sync.erased.length).toBe(list.length));
  return sync;
}

describe("notebooks held only by the account", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.clearAllMocks();
    api.restoreErased.mockResolvedValue(undefined);
  });

  it("are listed at start-up, on the shelf or in the trash", async () => {
    const sync = await connected([nb("a"), nb("b", true)]);
    expect(sync.cloudNotebooks.map((n) => n.notebookId)).toEqual(["a"]);
    expect(sync.cloudTrash.map((n) => n.notebookId)).toEqual(["b"]);
  });

  it("leave the trash once restored", async () => {
    const sync = await connected([nb("b", true)]);
    await sync.download("b");
    expect(api.restoreErased).toHaveBeenCalledWith("b");
    sync.arrived(["b"]);
    expect(sync.cloudTrash).toEqual([]);
  });

  it("keep their card from the click until the notebook arrives", async () => {
    const sync = await connected([nb("a")]);
    await sync.download("a");
    expect(api.restoreErased).toHaveBeenCalledWith("a");

    // the engine stops listing it as soon as the install is queued
    api.erasedNotebooks.mockResolvedValue([]);
    const before = api.erasedNotebooks.mock.calls.length;
    status.emit({ phase: "idle" });
    await vi.waitFor(() => expect(api.erasedNotebooks.mock.calls.length).toBe(before + 1));
    await new Promise((r) => setTimeout(r));
    expect(sync.cloudNotebooks.map((n) => n.notebookId)).toEqual(["a"]);
    expect(sync.downloading).toEqual(["a"]);

    sync.arrived(["a"]);
    expect(sync.cloudNotebooks).toEqual([]);
    expect(sync.downloading).toEqual([]);
  });

  it("go back to a plain card when the download could not start", async () => {
    const sync = await connected([nb("a")]);
    api.restoreErased.mockRejectedValue(new Error("no"));
    await expect(sync.download("a")).rejects.toThrow("no");
    expect(sync.downloading).toEqual([]);
    expect(sync.cloudNotebooks.map((n) => n.notebookId)).toEqual(["a"]);
  });

  it("are forgotten when the device disconnects", async () => {
    const sync = await connected([nb("a")]);
    status.emit(null);
    expect(sync.erased).toEqual([]);
  });
});
