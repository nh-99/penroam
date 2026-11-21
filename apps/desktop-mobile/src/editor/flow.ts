// Note flow on a ruled infinite canvas (a column, see `isCanvasColumn`), where typed text and
// drawings share one long page as in a notes app.
//
// The page is read as a stack of blocks (`flowUnits`). Every text box is a block. Ink and
// images that overlap one another vertically, or come within a gap of each other, form a
// drawing box together, whatever their horizontal position. Ink or an image that touches a text
// box (an underline, a circled word) belongs to that box's block. Blocks are never stored: they
// are worked out from the elements each time, so they need no format change, sync the way any
// element move syncs, and can't disagree between devices.
//
// When a block changes height, every block wholly below its old bottom moves by the same amount
// (`flowShift`), so the gaps between them stay as they were. On ruled paper the move is a whole
// number of rows, which keeps text that sits on the rows on them. A block that shrinks pulls the
// blocks below it up only as far as nothing above stops them. Blocks beside the one that changed,
// even if they reach below it, stay where they are.
//
// See docs/manual/pages-and-canvas.md#typing-and-drawing-on-ruled-paper.
import type { Element } from "../generated/Element";
import type { OpKind } from "../generated/OpKind";
import { rectsIntersect, type Rect } from "./geometry";

/** Slack for rounding when comparing edges, in pt. */
const EPS = 0.5;
/** How close ink must come to a text box to belong to it, in pt. */
const TOUCH_PAD = 2;

/** One block of the note: a text box with its annotations, or a drawing box. */
export interface FlowUnit {
  /** The elements in the block. */
  ids: string[];
  top: number;
  bottom: number;
  /** The block holds a text box. A block without one is a drawing box. */
  text: boolean;
}

/**
 * Groups `elements` into blocks. `joinGap` is the vertical distance (pt) up to which separate
 * strokes still count as one drawing; one row of the paper suits it.
 */
export function flowUnits(elements: readonly Element[], bounds: (e: Element) => Rect, joinGap: number): FlowUnit[] {
  const n = elements.length;
  const rects = elements.map(bounds);
  const parent = elements.map((_, i) => i);
  const find = (i: number): number => {
    while (parent[i] !== i) i = parent[i] = parent[parent[i]];
    return i;
  };
  const join = (a: number, b: number) => {
    const ra = find(a), rb = find(b);
    if (ra !== rb) parent[rb] = ra;
  };
  const texts: number[] = [];
  const others: number[] = [];
  for (let i = 0; i < n; i++) (elements[i].type === "text" ? texts : others).push(i);
  // drawings: a sweep down the page, joining each piece to the run above it while they meet
  others.sort((a, b) => rects[a][1] - rects[b][1]);
  let run = -1, runBottom = -Infinity;
  for (const i of others) {
    if (run >= 0 && rects[i][1] <= runBottom + joinGap) {
      join(run, i);
      runBottom = Math.max(runBottom, rects[i][3]);
    } else {
      run = i;
      runBottom = rects[i][3];
    }
  }
  // annotations: ink touching a text box goes with it (and so does the drawing it is part of)
  for (const t of texts) {
    const r = rects[t];
    const pad: Rect = [r[0] - TOUCH_PAD, r[1] - TOUCH_PAD, r[2] + TOUCH_PAD, r[3] + TOUCH_PAD];
    for (const i of others) if (rectsIntersect(pad, rects[i])) join(t, i);
  }
  const units = new Map<number, FlowUnit>();
  for (let i = 0; i < n; i++) {
    const root = find(i);
    const u = units.get(root) ?? { ids: [], top: Infinity, bottom: -Infinity, text: false };
    u.ids.push(elements[i].id);
    u.top = Math.min(u.top, rects[i][1]);
    u.bottom = Math.max(u.bottom, rects[i][3]);
    if (elements[i].type === "text") u.text = true;
    units.set(root, u);
  }
  return [...units.values()].sort((a, b) => a.top - b.top);
}

/** The blocks wholly at or below `y`. */
export function unitsBelow(units: readonly FlowUnit[], y: number): FlowUnit[] {
  return units.filter((u) => u.top >= y - EPS);
}

/** `v` rounded away from zero to a whole number of `pitch` (unchanged without one). */
const awayFromZero = (v: number, pitch?: number) => (pitch ? Math.sign(v) * Math.ceil(Math.abs(v) / pitch - 1e-6) * pitch : v);
/** `v` rounded toward zero to a whole number of `pitch` (unchanged without one). */
const towardZero = (v: number, pitch?: number) => (pitch ? Math.sign(v) * Math.floor(Math.abs(v) / pitch + 1e-6) * pitch : v);

/**
 * How the blocks below `below` (pt) move when what is above them grows by `delta` (shrinks when
 * negative). Growth moves them down by at least `delta`, rounded up to whole rows of `pitch`.
 * Shrinking moves them up by at most `-delta`, rounded down to whole rows, and never above
 * `floor` (the new bottom of what shrank) or the bottom of a block that stays put above them.
 * Returns the elements to move and by how much; `dy` is 0 when nothing moves.
 */
export function flowShift(units: readonly FlowUnit[], below: number, delta: number, pitch: number | undefined, floor: number): { ids: string[]; dy: number } {
  const moving = unitsBelow(units, below);
  if (!moving.length || Math.abs(delta) < EPS) return { ids: [], dy: 0 };
  let dy: number;
  if (delta > 0) dy = awayFromZero(delta, pitch);
  else {
    const top = Math.min(...moving.map((u) => u.top));
    let limit = floor;
    for (const u of units) if (u.top < below - EPS && u.bottom <= top + EPS) limit = Math.max(limit, u.bottom);
    dy = towardZero(Math.max(delta, Math.min(0, limit - top)), pitch);
  }
  if (Math.abs(dy) < EPS) return { ids: [], dy: 0 };
  return { ids: moving.flatMap((u) => u.ids), dy };
}

/** `e` moved by (`dx`, `dy`), with ink points kept at the stored precision. */
export function movedElement(e: Element, dx: number, dy: number): Element {
  if (e.type === "ink") return { ...e, points: e.points.map((p) => ({ ...p, x: Math.round((p.x + dx) * 1000) / 1000, y: Math.round((p.y + dy) * 1000) / 1000 })) };
  return { ...e, x: e.x + dx, y: e.y + dy };
}

/** The ops that move `ids` of `elements` down by `dy`, and the ops that put them back. */
export function shiftOps(elements: readonly Element[], ids: readonly string[], dy: number): { forward: OpKind[]; inverse: OpKind[] } {
  if (!ids.length || dy === 0) return { forward: [], inverse: [] };
  const want = new Set(ids);
  const before = elements.filter((e) => want.has(e.id));
  return {
    forward: [{ op: "replaceElements", elements: before.map((e) => movedElement(e, 0, dy)) }],
    inverse: [{ op: "replaceElements", elements: before }],
  };
}

/**
 * The drawing box `id` belongs to, with the bottom it shows: down to just above the next block
 * (`margin` pt above it), or `margin` below its ink when nothing follows. Null when `id` is part
 * of a text block. `next` is the top of the first block below, if any.
 */
export function drawingBox(units: readonly FlowUnit[], id: string, margin: number): { unit: FlowUnit; bottom: number; next: number | null } | null {
  const unit = units.find((u) => u.ids.includes(id));
  if (!unit || unit.text) return null;
  const tops = unitsBelow(units, unit.bottom).filter((u) => u !== unit).map((u) => u.top);
  const next = tops.length ? Math.min(...tops) : null;
  const bottom = next === null ? unit.bottom + margin : Math.max(unit.bottom, next - margin);
  return { unit, bottom, next };
}
