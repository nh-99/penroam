// Shared brush geometry, identical across Kotlin/Swift/TS/Rust (reference implementation:
// tests/fixtures/generate-fixtures.mjs; golden: geometry-golden.json; Rust
// port: crates/penroam-core/src/geometry.rs).
import type { InkPoint } from "../generated/InkPoint";

export type Piece =
  | { kind: "dot"; x: number; y: number; width: number }
  | { kind: "line"; x0: number; y0: number; x1: number; y1: number; width: number }
  | { kind: "quad"; x0: number; y0: number; cx: number; cy: number; x1: number; y1: number; width: number };

/** Minimal stroke shape the renderer needs (InkStroke satisfies it). */
export interface StrokeLike {
  points: InkPoint[];
  width: number;
  pressure: boolean;
  color: string;
  opacity: number;
  tool: "pen" | "highlighter";
}

export function widthFor(base: number, pressure: number, pressureEnabled: boolean): number {
  if (!pressureEnabled) return base;
  const p = pressure < 0 ? 0 : pressure > 1 ? 1 : pressure;
  return Math.max(0.5, base * (0.4 + 1.2 * p));
}

export function pieceCount(n: number): number {
  return n === 0 ? 0 : n <= 2 ? 1 : n;
}

/** Pieces that cannot change once n points are known (all but the trailing line). */
export function stablePieceCount(n: number): number {
  return n < 3 ? 0 : n - 1;
}

export function piece(pts: InkPoint[], k: number, base: number, pe: boolean): Piece {
  const n = pts.length;
  const w = (p: InkPoint) => widthFor(base, p.pressure, pe);
  if (n === 1) return { kind: "dot", x: pts[0].x, y: pts[0].y, width: w(pts[0]) };
  if (n === 2) return { kind: "line", x0: pts[0].x, y0: pts[0].y, x1: pts[1].x, y1: pts[1].y, width: w(pts[0]) };
  if (k === 0) {
    const a = pts[0], b = pts[1];
    return { kind: "line", x0: a.x, y0: a.y, x1: (a.x + b.x) / 2, y1: (a.y + b.y) / 2, width: w(a) };
  }
  if (k === n - 1) {
    const a = pts[n - 2], b = pts[n - 1];
    return { kind: "line", x0: (a.x + b.x) / 2, y0: (a.y + b.y) / 2, x1: b.x, y1: b.y, width: w(b) };
  }
  const p0 = pts[k - 1], p1 = pts[k], p2 = pts[k + 1];
  return {
    kind: "quad",
    x0: (p0.x + p1.x) / 2, y0: (p0.y + p1.y) / 2,
    cx: p1.x, cy: p1.y,
    x1: (p1.x + p2.x) / 2, y1: (p1.y + p2.y) / 2,
    width: w(p1),
  };
}

export function strokePieces(pts: InkPoint[], base: number, pe: boolean): Piece[] {
  const out: Piece[] = [];
  for (let k = 0; k < pieceCount(pts.length); k++) out.push(piece(pts, k, base, pe));
  return out;
}

/** Whether a stroke is rendered as one constant-width path. */
export function isUniform(s: StrokeLike): boolean {
  return !s.pressure || s.tool === "highlighter";
}

/**
 * Draws pieces [from, to) of a stroke. `ctx` carries the doc->pixel
 * transform; lineCap/lineJoin must be "round". Colour is set here.
 */
export function drawPieces(ctx: CanvasRenderingContext2D, s: StrokeLike, from: number, to: number): void {
  const pts = s.points;
  const n = pts.length;
  if (n === 0 || from >= to) return;
  ctx.strokeStyle = s.color;
  ctx.fillStyle = s.color;
  const base = s.width;
  if (isUniform(s) && n >= 2) {
    ctx.lineWidth = base;
    ctx.beginPath();
    for (let k = from; k < to; k++) {
      const pc = piece(pts, k, base, false);
      if (pc.kind === "line") {
        if (k === from) ctx.moveTo(pc.x0, pc.y0);
        ctx.lineTo(pc.x1, pc.y1);
      } else if (pc.kind === "quad") {
        if (k === from) ctx.moveTo(pc.x0, pc.y0);
        ctx.quadraticCurveTo(pc.cx, pc.cy, pc.x1, pc.y1);
      }
    }
    ctx.stroke();
    return;
  }
  // Variable width: consecutive pieces whose widths round to the same 1/16 pt
  // share one path (round joins == the round caps of separate pieces), so a
  // stroke costs a few stroke() calls instead of one per sample.
  const pe = s.pressure && s.tool !== "highlighter";
  let curW = -1;
  let open = false;
  for (let k = from; k < to; k++) {
    const pc = piece(pts, k, base, pe);
    if (pc.kind === "dot") {
      if (open) ctx.stroke();
      open = false;
      curW = -1;
      dot(ctx, pc.x, pc.y, pc.width);
      continue;
    }
    const w = Math.round(pc.width * 16) / 16;
    if (w !== curW) {
      if (open) ctx.stroke();
      ctx.lineWidth = w;
      ctx.beginPath();
      ctx.moveTo(pc.x0, pc.y0);
      curW = w;
      open = true;
    }
    if (pc.kind === "line") ctx.lineTo(pc.x1, pc.y1);
    else ctx.quadraticCurveTo(pc.cx, pc.cy, pc.x1, pc.y1);
  }
  if (open) ctx.stroke();
}

/** Draws a whole stroke including its opacity (highlighter: one path, one alpha). */
export function drawStroke(ctx: CanvasRenderingContext2D, s: StrokeLike): void {
  const a = s.opacity ?? 1;
  if (a < 1) ctx.globalAlpha = a;
  drawPieces(ctx, s, 0, pieceCount(s.points.length));
  if (a < 1) ctx.globalAlpha = 1;
}

export function drawPiece(ctx: CanvasRenderingContext2D, pc: Piece): void {
  if (pc.kind === "dot") return dot(ctx, pc.x, pc.y, pc.width);
  ctx.lineWidth = pc.width;
  ctx.beginPath();
  ctx.moveTo(pc.x0, pc.y0);
  if (pc.kind === "line") ctx.lineTo(pc.x1, pc.y1);
  else ctx.quadraticCurveTo(pc.cx, pc.cy, pc.x1, pc.y1);
  ctx.stroke();
}

function dot(ctx: CanvasRenderingContext2D, x: number, y: number, width: number) {
  ctx.beginPath();
  ctx.arc(x, y, width / 2, 0, Math.PI * 2);
  ctx.fill();
}

// ---------------------------------------------------------------- bounds --
export type Rect = [number, number, number, number]; // minX, minY, maxX, maxY
const boundsCache = new WeakMap<object, Rect>();

export function strokeBounds(s: StrokeLike): Rect {
  let b = boundsCache.get(s);
  if (b) return b;
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity, maxW = 0;
  const pe = s.pressure && s.tool !== "highlighter";
  for (const p of s.points) {
    if (p.x < minX) minX = p.x;
    if (p.y < minY) minY = p.y;
    if (p.x > maxX) maxX = p.x;
    if (p.y > maxY) maxY = p.y;
    const w = widthFor(s.width, p.pressure, pe);
    if (w > maxW) maxW = w;
  }
  const pad = maxW / 2 + 1;
  b = [minX - pad, minY - pad, maxX + pad, maxY + pad];
  boundsCache.set(s, b);
  return b;
}

/** Stroke eraser hit test (document units). Same rule as the Kotlin and Swift ink modules. */
export function hitTest(s: StrokeLike, cx: number, cy: number, r: number): boolean {
  const b = strokeBounds(s);
  if (cx < b[0] - r || cx > b[2] + r || cy < b[1] - r || cy > b[3] + r) return false;
  const pts = s.points;
  const half = s.width / 2;
  if (pts.length === 1) return Math.hypot(cx - pts[0].x, cy - pts[0].y) <= r + half;
  for (let i = 1; i < pts.length; i++) {
    if (segDist(cx, cy, pts[i - 1].x, pts[i - 1].y, pts[i].x, pts[i].y) <= r + half) return true;
  }
  return false;
}

/** Eraser swept from (ax,ay) to (bx,by): hit if the stroke comes within r of the segment. */
export function hitTestSegment(s: StrokeLike, ax: number, ay: number, bx: number, by: number, r: number): boolean {
  const len = Math.hypot(bx - ax, by - ay);
  const steps = Math.max(1, Math.ceil(len / Math.max(1, r)));
  for (let i = 0; i <= steps; i++) {
    const t = i / steps;
    if (hitTest(s, ax + (bx - ax) * t, ay + (by - ay) * t, r)) return true;
  }
  return false;
}

export function segDist(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax, dy = by - ay;
  const len2 = dx * dx + dy * dy;
  if (len2 === 0) return Math.hypot(px - ax, py - ay);
  let t = ((px - ax) * dx + (py - ay) * dy) / len2;
  t = t < 0 ? 0 : t > 1 ? 1 : t;
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

export function rectsIntersect(a: Rect, b: Rect): boolean {
  return a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1];
}

export function unionRect(a: Rect | null, b: Rect): Rect {
  if (!a) return [...b] as Rect;
  return [Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.max(a[2], b[2]), Math.max(a[3], b[3])];
}
