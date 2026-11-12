// Geometry of the on-screen ruler (pure functions; unit-tested).
//
// The ruler lies over the page like a physical one: it lives in viewport CSS pixels, so
// panning and zooming the page leave it where it is. It is a band of infinite length,
// `RULER_HALF_WIDTH_PX` either side of a centre line through (cx, cy) at `angle` degrees
// (screen coordinates, y down, so positive angles turn clockwise). Angles are normalised to
// [0, 180): the band is symmetric, so 180° is the same ruler as 0°.
//
// A stroke that starts close to one of the band's edges locks to that edge for its whole
// life: every sample is projected onto the edge line before it becomes a page point. The
// decision is made once, at the start (`rulerPress`); the engine keeps a copy of the ruler
// with the stroke, so moving the ruler afterwards cannot bend a stroke in progress.
//
// Ticks measure distance on the page, not on screen: their spacing is a centimetre of page
// (`CM_PT` points) times the view's scale, counted outward from the centre dial.

/** Half the width of the ruler's band, in CSS pixels. */
export const RULER_HALF_WIDTH_PX = 28;
/** How far outside the band (CSS px) a stroke may start and still lock to the edge. */
export const RULER_SNAP_PX = 16;
/** Radius of the drawn centre dial, in CSS pixels. */
export const DIAL_RADIUS_PX = 14;
/** Radius around the ruler's centre (CSS px) where a press turns the ruler. */
export const DIAL_HIT_PX = 22;
/**
 * A dial drag ignores the pointer until it is this far (CSS px) from the ruler's centre: closer
 * in, a pixel of movement would swing the angle wildly.
 */
export const DIAL_DEAD_PX = 8;
/** Angles the ruler clicks into, every this many degrees. */
export const DETENT_DEG = 15;
/** How close (degrees) an angle has to come to a detent to snap onto it. */
export const DETENT_SNAP_DEG = 4;
/** One centimetre in page points. */
export const CM_PT = 28.3465;
/** Ticks closer together than this (CSS px) are left out. */
export const MIN_TICK_GAP_PX = 4;
/** Labels closer together than this (CSS px) are thinned to every 2, 5 or 10 cm. */
const MIN_LABEL_GAP_PX = 24;

/** Where the ruler lies, in viewport CSS pixels and degrees. */
export interface Ruler {
  cx: number;
  cy: number;
  /** Degrees in [0, 180). */
  angle: number;
}

/** Which edge of the band: -1 is the side the normal points away from, 1 the side it points to. */
export type RulerSide = 1 | -1;

/** What a press on or near the ruler does. */
export type RulerPress = { kind: "rotate" } | { kind: "move" } | { kind: "draw"; side: RulerSide };

/** The kind of pointer, as far as the ruler cares. */
export type RulerPointer = "pen" | "mouse" | "touch";

const EPS = 1e-9;

/** `a` degrees brought into [0, 180). */
export function normaliseAngle(a: number): number {
  const n = ((a % 180) + 180) % 180;
  // -0 and values within rounding of 180 count as 0
  return n > 180 - EPS || n < EPS ? 0 : n;
}

/** `a` normalised, and snapped onto the nearest detent when within `DETENT_SNAP_DEG` of it. */
export function snapAngle(a: number): number {
  const n = normaliseAngle(a);
  const detent = Math.round(n / DETENT_DEG) * DETENT_DEG;
  return Math.abs(n - detent) <= DETENT_SNAP_DEG ? normaliseAngle(detent) : n;
}

/** The next detent after `a` (`dir` 1) or before it (`dir` -1), normalised; for keyboard rotation. */
export function stepAngle(a: number, dir: 1 | -1): number {
  const n = normaliseAngle(a);
  const k = n / DETENT_DEG;
  // an angle a hair off a detent counts as on it, so a step always moves a full detent
  const r = Math.round(k);
  const at = Math.abs(k - r) < 1e-6 ? r : null;
  const next = dir === 1 ? (at ?? Math.floor(k)) + 1 : (at ?? Math.ceil(k)) - 1;
  return normaliseAngle(next * DETENT_DEG);
}

/** The unit vector along the ruler and the unit normal to it. */
function axes(r: Ruler): { ux: number; uy: number; nx: number; ny: number } {
  const t = (r.angle * Math.PI) / 180;
  const ux = Math.cos(t), uy = Math.sin(t);
  return { ux, uy, nx: -uy, ny: ux };
}

/** Signed distance (CSS px) of (x, y) from the ruler's centre line, positive towards side 1. */
export function signedDistance(r: Ruler, x: number, y: number): number {
  const { nx, ny } = axes(r);
  return (x - r.cx) * nx + (y - r.cy) * ny;
}

/** Distance (CSS px) of (x, y) along the ruler from its centre. */
export function alongDistance(r: Ruler, x: number, y: number): number {
  const { ux, uy } = axes(r);
  return (x - r.cx) * ux + (y - r.cy) * uy;
}

/** Whether (x, y) is on the centre dial. */
export function hitDial(r: Ruler, x: number, y: number): boolean {
  return Math.hypot(x - r.cx, y - r.cy) <= DIAL_HIT_PX;
}

/** Whether (x, y) is on the ruler's band (edges included). */
export function inBand(r: Ruler, x: number, y: number): boolean {
  return Math.abs(signedDistance(r, x, y)) <= RULER_HALF_WIDTH_PX;
}

/** The edge nearer to (x, y). Points on the centre line go to side 1. */
export function nearerSide(r: Ruler, x: number, y: number): RulerSide {
  return signedDistance(r, x, y) < 0 ? -1 : 1;
}

/** Whether (x, y) is outside the band but within `RULER_SNAP_PX` of it. */
export function inSnapZone(r: Ruler, x: number, y: number): boolean {
  const d = Math.abs(signedDistance(r, x, y));
  return d > RULER_HALF_WIDTH_PX && d <= RULER_HALF_WIDTH_PX + RULER_SNAP_PX;
}

/**
 * What a press at (x, y) does, or null when the ruler leaves it alone. The dial turns the
 * ruler for every pointer. A mouse or finger on the band moves it; a pen on the band draws
 * along the nearer edge, since the pen only ever moves the ruler by its dial. Just outside the
 * band, any pointer draws along that edge. Drawing applies only when `canSnap` (the pen and
 * highlighter tools, drawing); without it a press outside the band is ordinary.
 */
export function rulerPress(r: Ruler, x: number, y: number, pointer: RulerPointer, canSnap: boolean): RulerPress | null {
  if (hitDial(r, x, y)) return { kind: "rotate" };
  const band = inBand(r, x, y);
  if (band && pointer !== "pen") return { kind: "move" };
  if (canSnap && (band || inSnapZone(r, x, y))) return { kind: "draw", side: nearerSide(r, x, y) };
  return null;
}

/** (x, y) moved perpendicularly onto the line of edge `side`. */
export function projectToEdge(r: Ruler, side: RulerSide, x: number, y: number): [number, number] {
  const { nx, ny } = axes(r);
  const off = signedDistance(r, x, y) - side * RULER_HALF_WIDTH_PX;
  return [x - nx * off, y - ny * off];
}

/**
 * The ruler turned by the dial: `from` is where the drag started, `to` where it is now. While
 * either is within `DIAL_DEAD_PX` of the centre the ruler stays as it is; the engine starts
 * measuring from the first point outside that.
 */
export function rotateAbout(start: Ruler, from: [number, number], to: [number, number]): Ruler {
  if (!outsideDialDeadZone(start, from) || !outsideDialDeadZone(start, to)) return start;
  const a0 = Math.atan2(from[1] - start.cy, from[0] - start.cx);
  const a1 = Math.atan2(to[1] - start.cy, to[0] - start.cx);
  return { ...start, angle: snapAngle(start.angle + ((a1 - a0) * 180) / Math.PI) };
}

/** Whether (x, y) is far enough from the ruler's centre for a dial drag to measure an angle. */
export function outsideDialDeadZone(r: Ruler, [x, y]: [number, number]): boolean {
  return Math.hypot(x - r.cx, y - r.cy) > DIAL_DEAD_PX;
}

/** The ruler moved by one finger or the mouse from `from` to `to`. */
export function moveBy(start: Ruler, from: [number, number], to: [number, number]): Ruler {
  return { ...start, cx: start.cx + to[0] - from[0], cy: start.cy + to[1] - from[1] };
}

/**
 * How far (degrees) the line from finger a to finger b has turned between (a0, b0) and
 * (a1, b1). `atan2` only tells the turn modulo 360°, so of the candidates this picks the one
 * nearest `prev`, the turn at the previous event: called on every move with the last result,
 * it follows the fingers continuously past 180° and beyond.
 */
export function fingerTurn(prev: number, a0: [number, number], b0: [number, number], a1: [number, number], b1: [number, number]): number {
  const t0 = Math.atan2(b0[1] - a0[1], b0[0] - a0[0]);
  const t1 = Math.atan2(b1[1] - a1[1], b1[0] - a1[0]);
  const raw = ((t1 - t0) * 180) / Math.PI;
  return raw + 360 * Math.round((prev - raw) / 360);
}

/**
 * The ruler carried by two fingers from (a0, b0) to (a1, b1), having turned `turn` degrees
 * (from `fingerTurn`; unwrapped, so it may exceed 180). It moves with the fingers' midpoint and
 * turns about it. Only the ruler's angle is normalised and snapped to a detent; the centre turns
 * by the true turn plus the detent's small correction, so it never jumps as the angle wraps.
 */
export function twoFingerTransform(start: Ruler, a0: [number, number], b0: [number, number], a1: [number, number], b1: [number, number], turn = fingerTurn(0, a0, b0, a1, b1)): Ruler {
  const angle = snapAngle(start.angle + turn);
  // the detent's correction, in (-90, 90]: 178° snapping to 0° is +2°, not -178°
  const fix = angle - normaliseAngle(start.angle + turn);
  const corr = fix - 180 * Math.round(fix / 180);
  const rad = ((turn + corr) * Math.PI) / 180;
  const c = Math.cos(rad), s = Math.sin(rad);
  const m0x = (a0[0] + b0[0]) / 2, m0y = (a0[1] + b0[1]) / 2;
  const m1x = (a1[0] + b1[0]) / 2, m1y = (a1[1] + b1[1]) / 2;
  const vx = start.cx - m0x, vy = start.cy - m0y;
  return { cx: m1x + vx * c - vy * s, cy: m1y + vx * s + vy * c, angle };
}

/** One tick: `at` is its distance from the centre along the ruler, in CSS px. */
export interface Tick {
  at: number;
  kind: "cm" | "half" | "mm";
  /** Centimetres from the centre, on labelled centimetre ticks. */
  label?: number;
}

/**
 * The ticks within `reach` CSS px either side of the centre at view scale `scale`. Millimetre
 * and then half-centimetre ticks drop out when closer than `MIN_TICK_GAP_PX`, and labels thin
 * to every 2, 5 or 10 cm when they would crowd. The centre itself has the dial, so no tick.
 */
export function tickLayout(scale: number, reach: number): Tick[] {
  const cm = CM_PT * scale;
  const sub = cm / 10 >= MIN_TICK_GAP_PX ? 10 : cm / 2 >= MIN_TICK_GAP_PX ? 2 : 1;
  const labelStep = [1, 2, 5, 10].find((s) => s * cm >= MIN_LABEL_GAP_PX) ?? 10;
  const step = cm / sub;
  const n = Math.floor(reach / step);
  const ticks: Tick[] = [];
  for (let i = -n; i <= n; i++) {
    if (i === 0) continue;
    const at = i * step;
    if (i % sub === 0) {
      const c = Math.abs(i / sub);
      ticks.push(c % labelStep === 0 ? { at, kind: "cm", label: c } : { at, kind: "cm" });
    } else ticks.push({ at, kind: sub === 10 && i % 5 === 0 ? "half" : sub === 2 ? "half" : "mm" });
  }
  return ticks;
}

/** The farthest any corner of a `w` x `h` viewport lies from (cx, cy): how far ticks must reach. */
export function reachFor(r: Ruler, w: number, h: number): number {
  return Math.max(Math.hypot(r.cx, r.cy), Math.hypot(w - r.cx, r.cy), Math.hypot(r.cx, h - r.cy), Math.hypot(w - r.cx, h - r.cy));
}
