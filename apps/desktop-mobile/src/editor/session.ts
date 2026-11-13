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

export interface OpEffect {
  added: Element[];
  removed: Element[];
  templateChanged: boolean;
}

/**
 * Mirrors penroam-core ops (validation of element data is done by Rust).
 * `index` (id -> element) is kept in sync so large pages are not rescanned
 * per stroke; pass none to build one.
 */
export function applyOpLocal(page: Page, op: OpKind, index: Map<string, Element> = new Map(page.elements.map((e) => [e.id, e]))): OpEffect {
  const fx: OpEffect = { added: [], removed: [], templateChanged: false };
  switch (op.op) {
    case "addElements": {
      const seen = new Set<string>();
      for (const e of op.elements) {
        if (index.has(e.id) || seen.has(e.id)) throw new ApplyLocalError(`element ${e.id} already exists`);
        seen.add(e.id);
      }
      const els = page.elements;
      const maxZ = els.length ? els[els.length - 1].z : -Infinity;
      const inOrder = op.elements.every((e, i) => e.z >= (i ? op.elements[i - 1].z : maxZ));
      els.push(...op.elements);
      if (!inOrder) sortByZ(els);
      for (const e of op.elements) index.set(e.id, e);
      fx.added.push(...op.elements);
      break;
    }
    case "removeElements": {
      for (const id of op.ids) if (!index.has(id)) throw new ApplyLocalError(`element ${id} does not exist`);
      const rm = new Set(op.ids);
      for (const id of rm) fx.removed.push(index.get(id)!);
      page.elements = rm.size === 1 ? page.elements.filter((e) => e.id !== op.ids[0]) : page.elements.filter((e) => !rm.has(e.id));
      for (const id of rm) index.delete(id);
      break;
    }
    case "replaceElements": {
      for (const e of op.elements) if (!index.has(e.id)) throw new ApplyLocalError(`element ${e.id} does not exist`);
      let resort = false;
      for (const e of op.elements) {
        const i = page.elements.indexOf(index.get(e.id)!);
        fx.removed.push(page.elements[i]);
        resort ||= page.elements[i].z !== e.z;
        page.elements[i] = e;
        index.set(e.id, e);
        fx.added.push(e);
      }
      if (resort) sortByZ(page.elements);
      break;
    }
    case "setTemplate":
      page.template = op.template;
      fx.templateChanged = true;
      break;
  }
  return fx;
}

/**
 * Drops the parts of an op that no longer apply to `index` (elements a
 * remote change removed, or ids that already exist). Returns null when
 * nothing is left. Used when local edits are re-applied on top of a page
 * that changed underneath them.
 */
export function sanitizeOp(op: OpKind, index: Map<string, Element>): OpKind | null {
  switch (op.op) {
    case "addElements": {
      const elements = op.elements.filter((e) => !index.has(e.id));
      return elements.length ? { op: "addElements", elements } : null;
    }
    case "removeElements": {
      const ids = op.ids.filter((id) => index.has(id));
      return ids.length ? { op: "removeElements", ids } : null;
    }
    case "replaceElements": {
      const elements = op.elements.filter((e) => index.has(e.id));
      return elements.length ? { op: "replaceElements", elements } : null;
    }
    case "setTemplate":
      return op;
  }
}

/** The backend rejected a batch because the page changed (stale base revision). */
export function isRevisionConflict(e: unknown): boolean {
  return !!e && typeof e === "object" && (e as { code?: string }).code === "conflict" && /revision/i.test(String((e as { message?: string }).message ?? ""));
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
  /** The whole page was replaced (remote changes merged in): re-render everything. */
  onPageReplaced?(): void;
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
  private index: Map<string, Element>;
  private rebases = 0;
  events: SessionEvents = {};
  /**
   * View-only access (a viewer, or access that was removed). This is the central guard: while
   * set, `commit`, `undo` and `redo` refuse, so nothing is applied locally or queued for Rust,
   * whatever the UI above still offers. Rust refuses the same writes; this keeps the page from
   * showing edits that would never be saved.
   */
  private readOnlyFlag = false;
  get readOnly() {
    return this.readOnlyFlag;
  }
  set readOnly(on: boolean) {
    this.readOnlyFlag = on;
    this.emitHistory();
  }
  /**
   * Reloads the page from storage. When set, a batch rejected because the
   * page changed underneath (e.g. synced remote edits) is rebased: the fresh
   * page is loaded and the unsaved local ops are re-applied on top, so local
   * strokes are never lost.
   */
  reload: (() => Promise<Page>) | null = null;

  constructor(
    readonly notebookId: string,
    public page: Page,
    private persist: Persist,
    private historyLimit = 200,
  ) {
    this.revision = page.revision;
    this.index = new Map(page.elements.map((e) => [e.id, e]));
  }

  get canUndo() {
    return !this.readOnlyFlag && this.undoStack.length > 0;
  }
  get canRedo() {
    return !this.readOnlyFlag && this.redoStack.length > 0;
  }
  get pendingCount() {
    return this.pending.length;
  }

  elementById(id: string): Element | undefined {
    return this.index.get(id);
  }

  nextZ(): number {
    const els = this.page.elements; // sorted by z
    return (els.length ? Math.max(0, els[els.length - 1].z) : 0) + 1;
  }

  /** Applies a change locally, records it for undo and queues it for saving. */
  commit(change: Change): void {
    if (this.closed) throw new Error("session closed");
    if (this.readOnlyFlag || change.forward.length === 0) return;
    this.applyLocal(change.forward);
    this.undoStack.push(change);
    if (this.undoStack.length > this.historyLimit) this.undoStack.shift();
    this.redoStack.length = 0;
    this.enqueue(change.forward);
    this.emitHistory();
  }

  undo(): Change | null {
    if (this.readOnlyFlag) return null;
    const c = this.undoStack.pop();
    if (!c) return null;
    const ops = this.tolerant(c.inverse);
    this.applyLocal(ops);
    this.redoStack.push(c);
    this.enqueue(ops);
    this.emitHistory();
    return c;
  }

  redo(): Change | null {
    if (this.readOnlyFlag) return null;
    const c = this.redoStack.pop();
    if (!c) return null;
    const ops = this.tolerant(c.forward);
    this.applyLocal(ops);
    this.undoStack.push(c);
    this.enqueue(ops);
    this.emitHistory();
    return c;
  }

  /** Undo/redo ops minus targets that disappeared since (remote changes). */
  private tolerant(ops: OpKind[]): OpKind[] {
    const probe = new Map(this.index);
    const out: OpKind[] = [];
    for (const op of ops) {
      const s = sanitizeOp(op, probe);
      if (!s) continue;
      if (s.op === "addElements") for (const e of s.elements) probe.set(e.id, e);
      if (s.op === "removeElements") for (const id of s.ids) probe.delete(id);
      out.push(s);
    }
    return out;
  }

  /**
   * Replaces the page with a fresh copy from storage and re-applies `ops`
   * (unsaved local edits) on top, dropping parts that no longer apply.
   * Returns the ops that were applied (to be saved).
   */
  replacePage(fresh: Page, ops: OpKind[] = []): OpKind[] {
    this.page = fresh;
    this.revision = fresh.revision;
    this.index = new Map(fresh.elements.map((e) => [e.id, e]));
    const kept: OpKind[] = [];
    for (const op of ops) {
      const s = sanitizeOp(op, this.index);
      if (!s) continue;
      applyOpLocal(this.page, s, this.index);
      kept.push(s);
    }
    this.events.onPageReplaced?.();
    return kept;
  }

  /** True while nothing is waiting to be saved. */
  get idle(): boolean {
    return !this.inflight && this.pending.length === 0;
  }

  private applyLocal(ops: OpKind[]) {
    const added: Element[] = [];
    const removed: Element[] = [];
    let templateChanged = false;
    for (const op of ops) {
      const fx = applyOpLocal(this.page, op, this.index);
      added.push(...fx.added);
      removed.push(...fx.removed);
      templateChanged ||= fx.templateChanged;
    }
    // report net effect: what is on the page now vs what left it
    this.events.onElementsChanged?.({
      added: added.filter((e) => this.index.get(e.id) === e),
      removed: removed.filter((e) => this.index.get(e.id) !== e),
      templateChanged,
    });
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
        this.rebases = 0;
        this.revision = out.revision;
        this.page.revision = out.revision;
        this.inflight = null;
        if (this.pending.length) this.pump();
        else this.setState("saved");
      },
      (err) => {
        if (isRevisionConflict(err) && this.reload && this.rebases < 3) {
          // the page changed underneath (merged remote edits): reload it and
          // re-apply every unsaved local op on top
          this.rebases++;
          const local = [...ops, ...this.pending.splice(0, this.pending.length)];
          const reload = this.reload;
          this.inflight = (async () => {
            const fresh = await reload();
            // edits committed while reloading were applied to the old page: re-apply them too
            const all = [...local, ...this.pending.splice(0, this.pending.length)];
            this.pending.unshift(...this.replacePage(fresh, all));
          })().then(
            () => {
              this.inflight = null;
              if (this.pending.length) this.pump();
              else this.setState("saved");
            },
            (e2) => {
              this.pending.unshift(...local);
              this.inflight = null;
              this.setState("error", errorMessage(e2));
            },
          );
          return;
        }
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
