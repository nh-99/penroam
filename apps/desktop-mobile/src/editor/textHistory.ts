// Own-edit tracking for typed text that other people edit at the same time.
//
// A text block's undo must remove only what *this* user typed (or restore only what they deleted),
// even after a collaborator changed the same block. So each own change is stored as ranges of the
// current text, not as "the old whole text": `own` are the stretches this user inserted and
// `restores` are the places where this user deleted something, with the deleted text.
// Every later change to the block, remote or the user's own undo/redo, is a list of splices, and
// every stored edit is mapped through it (`mapEdit`). Undoing is then `invertEdit` on the *current*
// text, which also yields the edit that redoes it.
//
// Everything is in UTF-16 offsets, like JavaScript strings and CodeMirror. Diffs run on code
// points, so a surrogate pair is never split, and are widened to grapheme boundaries where
// `Intl.Segmenter` exists, so emoji sequences and combining marks stay whole.
// Pure and framework-free; see tests/textHistory.test.ts.

/** Replace `[from, to)` of a text with `insert`. A list of splices is ascending and disjoint. */
export interface Splice {
  from: number;
  to: number;
  insert: string;
}

/** What one change by this user did, as ranges of the text as it is now. */
export interface OwnEdit {
  /** Stretches this user inserted: undoing removes them. Sorted, disjoint, never empty. */
  own: [number, number][];
  /** Places where this user deleted text: undoing puts `text` back at `at`. */
  restores: { at: number; text: string }[];
}

/** Applies ascending, disjoint splices to `text`. */
export function applySplices(text: string, splices: Splice[]): string {
  let out = "";
  let at = 0;
  for (const s of splices) {
    out += text.slice(at, s.from) + s.insert;
    at = s.to;
  }
  return out + text.slice(at);
}

const MAX_EDIT_STEPS = 4000;

/** Ascending splices that turn `a` into `b`. Few and minimal for ordinary typing and pasting. */
export function diffSplices(a: string, b: string): Splice[] {
  if (a === b) return [];
  // common prefix and suffix first: nearly every edit is one contiguous change
  let pre = 0;
  const max = Math.min(a.length, b.length);
  while (pre < max && a.charCodeAt(pre) === b.charCodeAt(pre)) pre++;
  // never end inside a surrogate pair
  if (pre > 0 && isHigh(a.charCodeAt(pre - 1))) pre--;
  let suf = 0;
  while (suf < max - pre && a.charCodeAt(a.length - 1 - suf) === b.charCodeAt(b.length - 1 - suf)) suf++;
  if (suf > 0 && isLow(a.charCodeAt(a.length - suf))) suf--;
  const midA = Array.from(a.slice(pre, a.length - suf));
  const midB = Array.from(b.slice(pre, b.length - suf));
  const ops = midA.length && midB.length ? myers(midA, midB) : null;
  let out: Splice[];
  if (!ops) out = [{ from: pre, to: a.length - suf, insert: b.slice(pre, b.length - suf) }];
  else {
    out = [];
    let ai = 0, bi = 0, aoff = pre;
    let cur: Splice | null = null;
    const flush = () => {
      if (cur) out.push(cur);
      cur = null;
    };
    for (const op of ops) {
      const ch = op.ch;
      if (op.kind === "keep") {
        flush();
        aoff += ch.length;
        ai++;
        bi++;
      } else if (op.kind === "del") {
        cur ??= { from: aoff, to: aoff, insert: "" };
        cur.to += ch.length;
        aoff += ch.length;
        ai++;
      } else {
        cur ??= { from: aoff, to: aoff, insert: "" };
        cur.insert += ch;
        bi++;
      }
    }
    flush();
    void ai;
    void bi;
  }
  return snapToGraphemes(a, out);
}

function isHigh(c: number) {
  return c >= 0xd800 && c <= 0xdbff;
}
function isLow(c: number) {
  return c >= 0xdc00 && c <= 0xdfff;
}

interface Op {
  kind: "keep" | "del" | "ins";
  ch: string;
}

/** Myers' O(ND) diff over code points; null when the change is too large to be worth aligning. */
function myers(a: string[], b: string[]): Op[] | null {
  const n = a.length, m = b.length;
  const max = Math.min(n + m, MAX_EDIT_STEPS);
  const off = max + 1;
  const v = new Int32Array(2 * max + 3);
  const trace: Int32Array[] = [];
  for (let d = 0; d <= max; d++) {
    trace.push(v.slice());
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[off + k - 1] < v[off + k + 1]) ? v[off + k + 1] : v[off + k - 1] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[off + k] = x;
      if (x >= n && y >= m) return backtrack(trace, a, b, d, off);
    }
  }
  return null;
}

function backtrack(trace: Int32Array[], a: string[], b: string[], dEnd: number, off: number): Op[] {
  const ops: Op[] = [];
  let x = a.length, y = b.length;
  for (let d = dEnd; d > 0; d--) {
    const v = trace[d];
    const k = x - y;
    const down = k === -d || (k !== d && v[off + k - 1] < v[off + k + 1]);
    const pk = down ? k + 1 : k - 1;
    const px = v[off + pk];
    const py = px - pk;
    while (x > px && y > py) {
      ops.push({ kind: "keep", ch: a[x - 1] });
      x--;
      y--;
    }
    if (down) ops.push({ kind: "ins", ch: b[y - 1] });
    else ops.push({ kind: "del", ch: a[x - 1] });
    x = px;
    y = py;
  }
  while (x > 0 && y > 0) {
    ops.push({ kind: "keep", ch: a[x - 1] });
    x--;
    y--;
  }
  return ops.reverse();
}

/** Widens splices to grapheme-cluster edges of `a` (emoji sequences, combining marks). */
function snapToGraphemes(a: string, splices: Splice[]): Splice[] {
  const Seg = (Intl as { Segmenter?: new (l?: string, o?: object) => { segment(s: string): Iterable<{ index: number; segment: string }> } }).Segmenter;
  if (!Seg || !splices.length) return splices;
  const starts: number[] = [];
  for (const s of new Seg(undefined, { granularity: "grapheme" }).segment(a)) starts.push(s.index);
  starts.push(a.length);
  const down = (p: number) => {
    let lo = 0, hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= p) lo = mid;
      else hi = mid - 1;
    }
    return starts[lo];
  };
  const up = (p: number) => {
    const d = down(p);
    if (d === p) return p;
    const i = starts.indexOf(d);
    return starts[Math.min(i + 1, starts.length - 1)];
  };
  const widened = splices.map((s) => {
    const from = down(s.from), to = up(s.to);
    return { from, to, insert: a.slice(from, s.from) + s.insert + a.slice(s.to, to) };
  });
  // widening can make neighbours touch or overlap: merge them
  const out: Splice[] = [];
  for (const s of widened) {
    const last = out[out.length - 1];
    if (last && s.from <= last.to) {
      last.insert += a.slice(last.to, s.from) + s.insert;
      last.to = Math.max(last.to, s.to);
    } else out.push({ ...s });
  }
  // a splice that changes nothing after widening is dropped
  return out.filter((s) => a.slice(s.from, s.to) !== s.insert);
}

/** The edit that `splices` (from `prev` to `next`) made, as ranges of `next`. */
export function editFromSplices(prev: string, splices: Splice[]): OwnEdit {
  const edit: OwnEdit = { own: [], restores: [] };
  let delta = 0;
  for (const s of splices) {
    const at = s.from + delta;
    if (s.to > s.from) edit.restores.push({ at, text: prev.slice(s.from, s.to) });
    if (s.insert.length) edit.own.push([at, at + s.insert.length]);
    delta += s.insert.length - (s.to - s.from);
  }
  return edit;
}

/** Maps an edit through one splice of the text (positions in the text before it). */
function mapOne(edit: OwnEdit, s: Splice): OwnEdit {
  const delta = s.insert.length - (s.to - s.from);
  const own: [number, number][] = [];
  for (const [a, b] of edit.own) {
    if (b <= s.from) own.push([a, b]);
    else if (a >= s.to) own.push([a + delta, b + delta]);
    else {
      // the splice cuts into this stretch: keep what lies before and after it, never the new text
      if (a < s.from) own.push([a, s.from]);
      if (b > s.to) own.push([s.from + s.insert.length, b + delta]);
    }
  }
  const restores = edit.restores.map((r) => {
    let at = r.at;
    if (at >= s.to && !(at === s.to && s.to === s.from)) at += delta;
    else if (at > s.from) at = s.from; // inside what was replaced: put it where that began
    return { at, text: r.text };
  });
  return { own: mergeRanges(own), restores };
}

function mergeRanges(r: [number, number][]): [number, number][] {
  const out: [number, number][] = [];
  for (const [a, b] of r.filter(([a, b]) => b > a).sort((x, y) => x[0] - y[0])) {
    const last = out[out.length - 1];
    if (last && a <= last[1]) last[1] = Math.max(last[1], b);
    else out.push([a, b]);
  }
  return out;
}

/** Maps an edit through ascending splices of the text (all in the coordinates before them). */
export function mapEdit(edit: OwnEdit, splices: Splice[]): OwnEdit {
  let delta = 0;
  let e = edit;
  for (const s of splices) {
    e = mapOne(e, { from: s.from + delta, to: s.to + delta, insert: s.insert });
    delta += s.insert.length - (s.to - s.from);
  }
  return e;
}

/**
 * Undoes (or redoes) an edit on the current `text`: removes the stretches the user inserted that
 * are still there and puts back what they deleted. Returns the new text, the splices that did it
 * (so other stored edits can be mapped through them) and the edit that reverses this one.
 */
export function invertEdit(text: string, edit: OwnEdit): { text: string; splices: Splice[]; edit: OwnEdit } {
  const raw: Splice[] = [
    ...edit.own.map(([a, b]) => ({ from: a, to: Math.min(b, text.length), insert: "" })),
    ...edit.restores.map((r) => ({ from: Math.min(r.at, text.length), to: Math.min(r.at, text.length), insert: r.text })),
  ].sort((x, y) => x.from - y.from || x.to - y.to);
  // an insertion at or inside a removed stretch is one replacement
  const splices: Splice[] = [];
  for (const s of raw) {
    const last = splices[splices.length - 1];
    if (last && s.from <= last.to) {
      last.insert += s.insert;
      last.to = Math.max(last.to, s.to);
    } else splices.push({ ...s });
  }
  const result = applySplices(text, splices);
  return { text: result, splices, edit: editFromSplices(text, splices) };
}
