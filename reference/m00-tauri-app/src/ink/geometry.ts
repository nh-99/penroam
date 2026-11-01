// TypeScript port of the shared brush geometry. Reference implementation and
// spec: fixtures/generate-fixtures.mjs. Verified by tests/geometry.test.ts.
import type { InkPoint, InkStroke } from "./model";

export type Piece =
  | { kind: "dot"; x: number; y: number; width: number }
  | { kind: "line"; x0: number; y0: number; x1: number; y1: number; width: number }
  | { kind: "quad"; x0: number; y0: number; cx: number; cy: number; x1: number; y1: number; width: number };

export type RenderStyle = "smooth" | "polyline";

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

/**
 * Draws pieces [from, to) of a stroke. ctx must carry the doc->pixel
 * transform and have lineCap/lineJoin = "round" and stroke/fill colors set.
 */
export function drawPieces(
  ctx: CanvasRenderingContext2D,
  s: InkStroke,
  from: number,
  to: number,
  style: RenderStyle = "smooth",
): void {
  const pts = s.points;
  const n = pts.length;
  if (n === 0 || from >= to) return;
  const base = s.brush.width;
  if (style === "polyline") {
    ctx.lineWidth = base;
    if (n === 1) return dot(ctx, pts[0].x, pts[0].y, base);
    const start = Math.max(0, from - 1);
    const end = Math.min(n - 1, to);
    ctx.beginPath();
    ctx.moveTo(pts[start].x, pts[start].y);
    for (let i = start + 1; i <= end; i++) ctx.lineTo(pts[i].x, pts[i].y);
    ctx.stroke();
    return;
  }
  if (!s.brush.pressure && n >= 2) {
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
  for (let k = from; k < to; k++) drawPiece(ctx, piece(pts, k, base, s.brush.pressure));
}

export function drawStroke(ctx: CanvasRenderingContext2D, s: InkStroke, style: RenderStyle = "smooth"): void {
  drawPieces(ctx, s, 0, pieceCount(s.points.length), style);
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
const boundsCache = new WeakMap<InkStroke, [number, number, number, number]>();

export function strokeBounds(s: InkStroke): [number, number, number, number] {
  let b = boundsCache.get(s);
  if (b) return b;
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity, maxW = 0;
  for (const p of s.points) {
    if (p.x < minX) minX = p.x;
    if (p.y < minY) minY = p.y;
    if (p.x > maxX) maxX = p.x;
    if (p.y > maxY) maxY = p.y;
    const w = widthFor(s.brush.width, p.pressure, s.brush.pressure);
    if (w > maxW) maxW = w;
  }
  const pad = maxW / 2 + 1;
  b = [minX - pad, minY - pad, maxX + pad, maxY + pad];
  boundsCache.set(s, b);
  return b;
}

/** Stroke eraser hit test (document units). Same rule as Kotlin/Dart. */
export function hitTest(s: InkStroke, cx: number, cy: number, r: number): boolean {
  const b = strokeBounds(s);
  if (cx < b[0] - r || cx > b[2] + r || cy < b[1] - r || cy > b[3] + r) return false;
  const pts = s.points;
  const half = s.brush.width / 2;
  if (pts.length === 1) return Math.hypot(cx - pts[0].x, cy - pts[0].y) <= r + half;
  for (let i = 1; i < pts.length; i++) {
    if (segDist(cx, cy, pts[i - 1].x, pts[i - 1].y, pts[i].x, pts[i].y) <= r + half) return true;
  }
  return false;
}

function segDist(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax, dy = by - ay;
  const len2 = dx * dx + dy * dy;
  if (len2 === 0) return Math.hypot(px - ax, py - ay);
  let t = ((px - ax) * dx + (py - ay) * dy) / len2;
  t = t < 0 ? 0 : t > 1 ? 1 : t;
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}
