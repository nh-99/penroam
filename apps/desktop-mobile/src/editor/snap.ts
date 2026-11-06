// Alignment snapping for text blocks. While a block is dragged or placed, it
// snaps to the blocks already on the page: left, centre and right edges line up
// with their own kind, and so do tops and first baselines. Only the nearest match
// on each axis applies, and it reports guide lines to draw. Pure and unit-tested;
// the engine decides when an axis is free to snap (row-locked text moves by whole
// rows, so its y is already aligned and only x snaps).
import type { Rect } from "./geometry";

/** How close, in CSS pixels, an edge has to come before it snaps. */
export const SNAP_DISTANCE_PX = 8;

/** The lines of a text block that can align. */
export interface TextEdges {
  rect: Rect;
  /** First baseline, in page coordinates. */
  baseline: number;
}

/** A guide segment in page coordinates. */
export interface Guide {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

export interface Alignment {
  dx: number;
  dy: number;
  guides: Guide[];
}

const xLines = (e: TextEdges) => [e.rect[0], (e.rect[0] + e.rect[2]) / 2, e.rect[2]];
const yLines = (e: TextEdges) => [e.rect[1], e.baseline];

/** Smallest correction bringing a line of `moving` onto the same kind of line of `others`. */
function nearest(moving: TextEdges[], others: TextEdges[], lines: (e: TextEdges) => number[], tolerance: number): number | null {
  let best: number | null = null;
  for (const m of moving) {
    const from = lines(m);
    for (const o of others) {
      lines(o).forEach((to, i) => {
        const d = to - from[i];
        if (Math.abs(d) <= tolerance && (best === null || Math.abs(d) < Math.abs(best))) best = d;
      });
    }
  }
  return best;
}

const CLOSE = 1e-6;

/**
 * The move that aligns `moving` with `others`, within `tolerance` points. `snapY`
 * turns off the vertical axis. Guides span every pair that ends up aligned.
 */
export function alignment(moving: TextEdges[], others: TextEdges[], tolerance: number, snapY: boolean): Alignment {
  const dx = nearest(moving, others, xLines, tolerance);
  const dy = snapY ? nearest(moving, others, yLines, tolerance) : null;
  const ox = dx ?? 0;
  const oy = dy ?? 0;
  const guides: Guide[] = [];
  for (const m of moving) {
    const r: Rect = [m.rect[0] + ox, m.rect[1] + oy, m.rect[2] + ox, m.rect[3] + oy];
    const moved: TextEdges = { rect: r, baseline: m.baseline + oy };
    for (const o of others) {
      // Blocks of the same size align on every line at once: one guide says it.
      if (dx !== null) {
        const x = xLines(moved).find((v, i) => Math.abs(v - xLines(o)[i]) < CLOSE);
        if (x !== undefined) guides.push({ x0: x, y0: Math.min(r[1], o.rect[1]), x1: x, y1: Math.max(r[3], o.rect[3]) });
      }
      if (dy !== null) {
        const y = yLines(moved).find((v, i) => Math.abs(v - yLines(o)[i]) < CLOSE);
        if (y !== undefined) guides.push({ x0: Math.min(r[0], o.rect[0]), y0: y, x1: Math.max(r[2], o.rect[2]), y1: y });
      }
    }
  }
  return { dx: ox, dy: oy, guides };
}
