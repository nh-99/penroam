import { beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { useLibrary } from "../src/stores/library";
import { useUi } from "../src/stores/ui";
import { useSettings } from "../src/stores/settings";
import { useSync } from "../src/stores/sync";
import { autoTarget, backupDue } from "../src/platform/backup";
import { groupSessions } from "../src/platform/history";

const importNotebook = vi.fn();
const importDocument = vi.fn();
const listNotebooks = vi.fn(async () => []);
const exportNotebookText = vi.fn();
const pick = vi.fn();
const save = vi.fn();

vi.mock("../src/platform/ipc", () => ({
  hasHost: () => false,
  CommandError: class extends Error {},
  api: {
    importNotebook: (...a: unknown[]) => importNotebook(...a),
    importDocument: (...a: unknown[]) => importDocument(...a),
    listNotebooks: () => listNotebooks(),
    libraryStatus: async () => ({ replayedBatches: 0, truncatedJournals: 0, warnings: [] }),
    exportNotebookText: (...a: unknown[]) => exportNotebookText(...a),
  },
}));
vi.mock("../src/platform/files", async (orig) => ({
  ...(await orig<typeof import("../src/platform/files")>()),
  pickImportFiles: () => pick(),
  pickSaveLocation: () => save(),
}));

const kept = { pages: 2, strokes: 5, textBlocks: 1, images: 0 };

describe("importing files", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    for (const m of [importNotebook, importDocument, pick, save, exportNotebookText]) m.mockReset();
  });

  it("sends each file to the right importer and keeps going after a failure", async () => {
    pick.mockResolvedValue([
      { path: "/a.penroam", name: "a.penroam" },
      { path: "/b.xopp", name: "b.xopp" },
      { path: "/c.xopp", name: "c.xopp" },
    ]);
    importNotebook.mockResolvedValue({ notebook: { title: "A" }, warnings: [] });
    importDocument.mockImplementation(async (_p: string, name: string) => {
      if (name === "c.xopp") throw new Error("the file ends early");
      return { notebook: { title: "B" }, report: { format: "Xournal++", kept, lost: ["1 filled stroke(s) lost their fill."] } };
    });
    const lib = useLibrary();
    await lib.importFiles();
    const r = lib.importResults!;
    expect(r.map((x) => x.file)).toEqual(["a.penroam", "b.xopp", "c.xopp"]);
    expect(r[1].lost).toHaveLength(1);
    expect(r[2].error).toContain("ends early");
    expect(importNotebook).toHaveBeenCalledTimes(1);
  });

  it("a clean single import is just a success message", async () => {
    pick.mockResolvedValue([{ path: "/n.md", name: "n.md" }]);
    importDocument.mockResolvedValue({ notebook: { title: "N" }, report: { format: "Markdown", kept, lost: [] } });
    const lib = useLibrary();
    await lib.importFiles();
    expect(lib.importResults).toBeNull();
    expect(useUi().toasts.some((t) => t.kind === "success")).toBe(true);
  });

  it("a text export always passes on what was left out", async () => {
    save.mockResolvedValue("/out.md");
    exportNotebookText.mockResolvedValue({ pages: 1, textBlocks: 0, warnings: ["4 handwritten strokes were left out"] });
    await useLibrary().exportText("id", "Title", true);
    expect(useUi().toasts.map((t) => t.text).join()).toContain("handwritten");
  });
});

describe("automatic backups", () => {
  const base = { target: "off" as const, dir: "", url: "", user: "", lastAt: 0 };

  it("are due daily, only when a destination is set", () => {
    expect(backupDue(base, 1e12)).toBe(false);
    const folder = { ...base, target: "folder" as const, dir: "/b" };
    expect(backupDue(folder, 1e12)).toBe(true);
    expect(backupDue({ ...folder, lastAt: 1e12 - 1000 }, 1e12)).toBe(false);
    expect(backupDue({ ...folder, dir: "" }, 1e12)).toBe(false);
    expect(autoTarget({ ...base, target: "webdav", url: "https://x", user: "u" })).toEqual({ kind: "webdav", url: "https://x", user: "u" });
  });
});

describe("exporting encrypted notes", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    save.mockReset();
    exportNotebookText.mockReset();
  });

  it("doesn't ask when sync is off", async () => {
    await expect(useLibrary().confirmPlaintext()).resolves.toBe(true);
  });

  it("asks when sync is on, and cancelling stops before any file is chosen", async () => {
    useSync().configured = true;
    const lib = useLibrary();
    const done = lib.exportText("id", "Title", true);
    await vi.waitFor(() => expect(lib.plaintextQuestion).not.toBeNull());
    lib.plaintextQuestion!(false);
    await done;
    expect(save).not.toHaveBeenCalled();
    expect(lib.plaintextQuestion).toBeNull();
  });

  it("goes ahead once the user agrees, and remembers don't ask again", async () => {
    useSync().configured = true;
    useSettings().s.plaintextExportOk = true;
    await expect(useLibrary().confirmPlaintext()).resolves.toBe(true);
  });
});

describe("history sessions", () => {
  const p = (revision: number, minutes: number, added = 1, removed = 0) => ({ pageId: "p", revision, position: null, at: minutes * 60_000, added, removed });
  const cloud = (position: number, minutes: number) => ({ pageId: "p", revision: null, position, at: minutes * 60_000, added: 1, removed: 0 });

  it("groups edits close together and shows the session's end", () => {
    const s = groupSessions([p(1, 0, 0), p(2, 1), p(3, 2, 0, 2), p(4, 40), p(5, 41)]);
    expect(s).toHaveLength(3);
    expect(s[0]).toMatchObject({ revision: 1 });
    expect(s[1]).toMatchObject({ revision: 3, added: 1, removed: 2 });
    expect(s[2]).toMatchObject({ revision: 5, added: 2 });
  });

  it("always offers the state before the first change", () => {
    expect(groupSessions([p(1, 0, 0), p(2, 0.1), p(3, 0.2)]).map((s) => s.revision)).toEqual([1, 3]);
  });

  it("never mixes account versions and this device's into one session", () => {
    const s = groupSessions([cloud(0, 0), cloud(1, 0.1), cloud(2, 0.2), p(1, 0.3), p(2, 0.4)]);
    expect(s.map((x) => [x.position, x.revision])).toEqual([[0, null], [2, null], [null, 2]]);
  });

  it("handles no history", () => expect(groupSessions([])).toEqual([]));
});
