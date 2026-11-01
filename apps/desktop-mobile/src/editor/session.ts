// Page editing session: the in-memory page, semantic undo/redo, and the
// ordered persistence queue to Rust. Framework-free and unit-tested.
//
// Every user edit is a Change = forward ops + inverse ops. The page is
// mutated locally with the same semantics as penroam-core's apply_batch, the
// change goes on the undo stack, and the forward ops are queued for Rust.
// Undo/redo enqueue the inverse/forward ops as new batches with fresh ids, so
// the persisted state is always "all batches applied in order".
import type { Element } from "../generated/Element";
import type { OpKind } from "../generated/OpKind";
import type { OperationBatch } from "../generated/OperationBatch";
import type { Page } from "../generated/Page";
import type { ApplyOutcome } from "../generated/ApplyOutcome";
import { newId } from "./ids";

export interface Change {
  label: string;
  forward: OpKind[];
  inverse: OpKind[];
}

export type SaveState = "saved" | "dirty" | "saving" | "error";

export interface Persist {
  (batch: OperationBatch): Promise<ApplyOutcome>;
}

export class ApplyLocalError extends Error {}

/** Mirrors penroam-core ops::apply_op (validation is done by Rust). */
export function applyOpLocal(page: Page, op: OpKind): void {
  switch (op.op) {
    case "addElements": {
      const ids = new Set(page.elements.map((e) => e.id));
      for (const e of op.elements) {
        if (ids.has(e.id)) throw new ApplyLocalError(`element ${e.id} already exists`);
        ids.add(e.id);
      }
      page.elements.push(...op.elements);
      sortByZ(page.elements);
      break;
    }
    case "removeElements": {
      const rm = new Set(op.ids);
      for (const id of rm) if (!page.elements.some((e) => e.id === id)) throw new ApplyLocalError(`element ${id} does not exist`);
      page.elements = page.elements.filter((e) => !rm.has(e.id));
      break;
    }
    case "replaceElements": {
      for (const e of op.elements) {
        const i = page.elements.findIndex((x) => x.id === e.id);
        if (i < 0) throw new ApplyLocalError(`element ${e.id} does not exist`);
        page.elements[i] = e;
      }
      sortByZ(page.elements);
      break;
    }
    case "setTemplate":
      page.template = op.template;
      break;
  }
}

function sortByZ(els: Element[]) {
  // Array.prototype.sort is stable, matching Rust's sort_by_key
  els.sort((a, b) => a.z - b.z);
}

export interface SessionEvents {
  /** Elements changed (ids whose rendering must update; null = everything). */
  onElementsChanged?(changed: { added: Element[]; removed: Element[]; all?: boolean; templateChanged?: boolean }): void;
  onSaveState?(state: SaveState, error?: string): void;
  onHistory?(canUndo: boolean, canRedo: boolean): void;
}

export class PageSession {
  readonly undoStack: Change[] = [];
  readonly redoStack: Change[] = [];
  saveState: SaveState = "saved";
  lastError: string | null = null;
  private pending: OpKind[] = [];
  private inflight: Promise<void> | null = null;
  private revision: number;
  private closed = false;
  events: SessionEvents = {};

  constructor(
    readonly notebookId: string,
    public page: Page,
    private persist: Persist,
    private historyLimit = 200,
  ) {
    this.revision = page.revision;
  }

  get canUndo() {
    return this.undoStack.length > 0;
  }
  get canRedo() {
    return this.redoStack.length > 0;
  }
  get pendingCount() {
    return this.pending.length;
  }

  elementById(id: string): Element | undefined {
    return this.page.elements.find((e) => e.id === id);
  }

  nextZ(): number {
    let z = 0;
    for (const e of this.page.elements) if (e.z > z) z = e.z;
    return z + 1;
  }

  /** Applies a change locally, records it for undo and queues it for saving. */
  commit(change: Change): void {
    if (this.closed) throw new Error("session closed");
    if (change.forward.length === 0) return;
    this.applyLocal(change.forward);
    this.undoStack.push(change);
    if (this.undoStack.length > this.historyLimit) this.undoStack.shift();
    this.redoStack.length = 0;
    this.enqueue(change.forward);
    this.emitHistory();
  }

  undo(): Change | null {
    const c = this.undoStack.pop();
    if (!c) return null;
    this.applyLocal(c.inverse);
    this.redoStack.push(c);
    this.enqueue(c.inverse);
    this.emitHistory();
    return c;
  }

  redo(): Change | null {
    const c = this.redoStack.pop();
    if (!c) return null;
    this.applyLocal(c.forward);
    this.undoStack.push(c);
    this.enqueue(c.forward);
    this.emitHistory();
    return c;
  }

  private applyLocal(ops: OpKind[]) {
    const before = new Map(this.page.elements.map((e) => [e.id, e]));
    let templateChanged = false;
    for (const op of ops) {
      applyOpLocal(this.page, op);
      if (op.op === "setTemplate") templateChanged = true;
    }
    const after = new Map(this.page.elements.map((e) => [e.id, e]));
    const added: Element[] = [];
    const removed: Element[] = [];
    for (const [id, e] of after) if (before.get(id) !== e) added.push(e);
    for (const [id, e] of before) if (after.get(id) !== e) removed.push(e);
    this.events.onElementsChanged?.({ added, removed, templateChanged });
  }

  private emitHistory() {
    this.events.onHistory?.(this.canUndo, this.canRedo);
  }

  private setState(s: SaveState, err?: string) {
    this.saveState = s;
    this.lastError = err ?? null;
    this.events.onSaveState?.(s, err);
  }

  private enqueue(ops: OpKind[]) {
    this.pending.push(...ops);
    if (!this.inflight) this.setState("dirty");
    this.pump();
  }

  /** Sends pending ops as one batch at a time, in order. */
  private pump() {
    if (this.inflight || this.pending.length === 0) return;
    const ops = this.pending.splice(0, this.pending.length);
    const batch: OperationBatch = {
      batchId: newId(),
      baseRevision: this.revision,
      ops: ops.map((kind) => ({ opId: newId(), ...kind })),
    };
    this.setState("saving");
    this.inflight = this.persist(batch).then(
      (out) => {
        this.revision = out.revision;
        this.page.revision = out.revision;
        this.inflight = null;
        if (this.pending.length) this.pump();
        else this.setState("saved");
      },
      (err) => {
        // keep the ops (front of the queue) so nothing is lost; the user can retry
        this.pending.unshift(...ops);
        this.inflight = null;
        this.setState("error", errorMessage(err));
      },
    );
  }

  /** Retries after an error (e.g. storage was full). */
  retry(): Promise<void> {
    if (this.saveState === "error") this.pump();
    return this.flush();
  }

  /** Resolves when everything committed so far is durable (or rejects with the error). */
  async flush(): Promise<void> {
    for (;;) {
      if (this.inflight) await this.inflight;
      if (this.saveState === "error") throw new Error(this.lastError ?? "save failed");
      if (this.pending.length === 0 && !this.inflight) return;
      this.pump();
    }
  }

  async close(): Promise<void> {
    await this.flush();
    this.closed = true;
  }
}

export function errorMessage(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as any).message);
  return String(e);
}
