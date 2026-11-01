import { describe, expect, it, vi } from "vitest";
import { PageSession, applyOpLocal, type SaveState } from "../src/editor/session";
import type { Page } from "../src/generated/Page";
import type { Element } from "../src/generated/Element";
import type { OperationBatch } from "../src/generated/OperationBatch";

function page(): Page {
  return { schemaVersion: 1, id: "p1", paper: "letter", size: { width: 612, height: 792 }, template: { kind: "ruled", spacing: 24 }, revision: 0, createdAt: 0, updatedAt: 0, elements: [] };
}
const text = (id: string, z: number, t = id): Element => ({ type: "text", id, z, x: 0, y: 0, width: 100, height: 20, text: t, fontSize: 16, color: "#000000", align: "left" });

function harness(fail?: () => boolean) {
  const batches: OperationBatch[] = [];
  let rev = 0;
  const persist = vi.fn(async (b: OperationBatch) => {
    await Promise.resolve();
    if (fail?.()) throw { code: "storageFull", message: "disk full" };
    expect(b.baseRevision).toBe(rev);
    batches.push(b);
    rev++;
    return { revision: rev, updatedAt: 1, durable: true };
  });
  const s = new PageSession("nb", page(), persist);
  const states: SaveState[] = [];
  s.events.onSaveState = (st) => states.push(st);
  return { s, batches, persist, states };
}

describe("PageSession", () => {
  it("commits, undoes and redoes semantically and persists every step in order", async () => {
    const { s, batches } = harness();
    s.commit({ label: "add", forward: [{ op: "addElements", elements: [text("a", 1)] }], inverse: [{ op: "removeElements", ids: ["a"] }] });
    s.commit({ label: "add", forward: [{ op: "addElements", elements: [text("b", 2)] }], inverse: [{ op: "removeElements", ids: ["b"] }] });
    expect(s.page.elements.map((e) => e.id)).toEqual(["a", "b"]);
    s.undo();
    expect(s.page.elements.map((e) => e.id)).toEqual(["a"]);
    s.redo();
    s.undo();
    s.undo();
    expect(s.page.elements).toEqual([]);
    expect(s.canUndo).toBe(false);
    expect(s.canRedo).toBe(true);
    await s.flush();
    // replaying every persisted batch on a fresh page reproduces the local state
    const replay = page();
    for (const b of batches) for (const op of b.ops) applyOpLocal(replay, op);
    expect(replay.elements).toEqual(s.page.elements);
    // op ids and batch ids are unique
    const ids = batches.flatMap((b) => [b.batchId, ...b.ops.map((o) => o.opId)]);
    expect(new Set(ids).size).toBe(ids.length);
    expect(s.saveState).toBe("saved");
  });

  it("a new edit clears the redo stack", () => {
    const { s } = harness();
    s.commit({ label: "a", forward: [{ op: "addElements", elements: [text("a", 1)] }], inverse: [{ op: "removeElements", ids: ["a"] }] });
    s.undo();
    s.commit({ label: "b", forward: [{ op: "addElements", elements: [text("b", 1)] }], inverse: [{ op: "removeElements", ids: ["b"] }] });
    expect(s.canRedo).toBe(false);
  });

  it("reports dirty -> saving -> saved, and coalesces queued ops into one batch", async () => {
    const { s, batches, states } = harness();
    for (let i = 0; i < 5; i++) s.commit({ label: "a", forward: [{ op: "addElements", elements: [text(`t${i}`, i)] }], inverse: [{ op: "removeElements", ids: [`t${i}`] }] });
    await s.flush();
    expect(states[0]).toBe("dirty");
    expect(states).toContain("saving");
    expect(states[states.length - 1]).toBe("saved");
    expect(batches.length).toBe(2); // first op immediately, the other four coalesced
    expect(batches[1].ops.length).toBe(4);
  });

  it("keeps unsaved ops after a failed save and retries them", async () => {
    let failing = true;
    const { s, batches } = harness(() => failing);
    s.commit({ label: "a", forward: [{ op: "addElements", elements: [text("a", 1)] }], inverse: [{ op: "removeElements", ids: ["a"] }] });
    await expect(s.flush()).rejects.toThrow("disk full");
    expect(s.saveState).toBe("error");
    expect(s.pendingCount).toBe(1);
    failing = false;
    await s.retry();
    expect(s.saveState).toBe("saved");
    expect(batches[0].ops[0]).toMatchObject({ op: "addElements" });
  });

  it("local apply mirrors Rust semantics (duplicate/missing ids throw, z order is stable)", () => {
    const p = page();
    applyOpLocal(p, { op: "addElements", elements: [text("a", 2), text("b", 1), text("c", 2)] });
    expect(p.elements.map((e) => e.id)).toEqual(["b", "a", "c"]);
    expect(() => applyOpLocal(p, { op: "addElements", elements: [text("a", 3)] })).toThrow();
    expect(() => applyOpLocal(p, { op: "removeElements", ids: ["zz"] })).toThrow();
    applyOpLocal(p, { op: "replaceElements", elements: [text("b", 5, "B")] });
    expect(p.elements.map((e) => e.id)).toEqual(["a", "c", "b"]);
    applyOpLocal(p, { op: "setTemplate", template: { kind: "grid", spacing: 18 } });
    expect(p.template.kind).toBe("grid");
  });
});
