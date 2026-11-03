// A save rejected because the page changed underneath (synced remote edits)
// is rebased onto the fresh page: local edits are never lost.
import { describe, expect, it, vi } from "vitest";
import { PageSession, isRevisionConflict, sanitizeOp } from "../src/editor/session";
import type { Page } from "../src/generated/Page";
import type { Element } from "../src/generated/Element";
import type { OperationBatch } from "../src/generated/OperationBatch";

const text = (id: string, z: number, t = id): Element => ({ type: "text", id, z, x: 0, y: 0, width: 100, height: 20, text: t, fontSize: 16, color: "#000000", align: "left" });
function page(revision: number, elements: Element[]): Page {
  return { schemaVersion: 1, id: "p1", paper: "letter", size: { width: 612, height: 792 }, template: { kind: "ruled", spacing: 24 }, revision, createdAt: 0, updatedAt: 0, elements };
}
const conflict = { code: "conflict", message: "revision conflict: page is at 3, batch was built on 1" };
const add = (e: Element) => ({ label: "add", forward: [{ op: "addElements" as const, elements: [e] }], inverse: [{ op: "removeElements" as const, ids: [e.id] }] });

describe("rebase on revision conflict", () => {
  it("reloads the page and re-applies unsaved local edits on top", async () => {
    // storage: local "a" was saved at rev 1, then a remote install added "r" (rev 3)
    let storage = page(3, [text("a", 1), text("r", 2)]);
    const saved: OperationBatch[] = [];
    let first = true;
    const persist = vi.fn(async (b: OperationBatch) => {
      await Promise.resolve();
      if (b.baseRevision !== storage.revision) {
        expect(first).toBe(true);
        first = false;
        throw conflict;
      }
      saved.push(b);
      storage = { ...storage, revision: storage.revision + 1 };
      return { revision: storage.revision, updatedAt: 1, durable: true };
    });
    const s = new PageSession("nb", page(1, [text("a", 1)]), persist);
    let replaced = 0;
    s.events.onPageReplaced = () => replaced++;
    s.reload = async () => structuredClone(storage);
    s.commit(add(text("local", 5)));
    s.commit({ label: "rm", forward: [{ op: "removeElements", ids: ["a"] }], inverse: [{ op: "addElements", elements: [text("a", 1)] }] });
    await s.flush();
    expect(s.saveState).toBe("saved");
    expect(replaced).toBe(1);
    expect(s.page.elements.map((e) => e.id).sort()).toEqual(["local", "r"]);
    expect(saved).toHaveLength(1);
    expect(saved[0].baseRevision).toBe(3);
    expect(saved[0].ops.map((o) => o.op)).toEqual(["addElements", "removeElements"]);
    expect(s.page.revision).toBe(4);
  });

  it("drops only the parts that no longer apply and keeps undo working", async () => {
    // remote deleted "gone" while this device edited it and drew a stroke
    const storage = page(5, [text("keep", 1)]);
    let calls = 0;
    let rev = 5;
    const persist = vi.fn(async (b: OperationBatch) => {
      await Promise.resolve();
      calls++;
      if (calls === 1) throw conflict;
      expect(b.baseRevision).toBe(rev);
      rev++;
      return { revision: rev, updatedAt: 1, durable: true };
    });
    const s = new PageSession("nb", page(2, [text("keep", 1), text("gone", 2)]), persist);
    s.reload = async () => structuredClone(storage);
    s.commit({ label: "edit", forward: [{ op: "replaceElements", elements: [text("gone", 2, "edited")] }], inverse: [{ op: "replaceElements", elements: [text("gone", 2)] }] });
    s.commit(add(text("new", 3)));
    await s.flush();
    expect(s.page.elements.map((e) => e.id)).toEqual(["keep", "new"]);
    // undoing the edit of an element that no longer exists is a harmless no-op
    s.undo();
    expect(() => s.undo()).not.toThrow();
    await s.flush();
    expect(s.saveState).toBe("saved");
  });

  it("other errors keep the ops queued for a retry (no reload)", async () => {
    const reload = vi.fn();
    const s = new PageSession("nb", page(0, []), async () => {
      throw { code: "storageFull", message: "disk full" };
    });
    s.reload = reload;
    s.commit(add(text("x", 1)));
    await expect(s.flush()).rejects.toThrow("disk full");
    expect(reload).not.toHaveBeenCalled();
    expect(s.pendingCount).toBe(1);
  });

  it("classifies errors and sanitizes ops", () => {
    expect(isRevisionConflict(conflict)).toBe(true);
    expect(isRevisionConflict({ code: "conflict", message: "the library is already open" })).toBe(false);
    expect(isRevisionConflict({ code: "io", message: "revision" })).toBe(false);
    const idx = new Map([["a", text("a", 1)]]);
    expect(sanitizeOp({ op: "removeElements", ids: ["a", "b"] }, idx)).toEqual({ op: "removeElements", ids: ["a"] });
    expect(sanitizeOp({ op: "addElements", elements: [text("a", 1)] }, idx)).toBeNull();
    expect(sanitizeOp({ op: "replaceElements", elements: [text("z", 1)] }, idx)).toBeNull();
  });
});
