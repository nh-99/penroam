// Page background templates. Keep in sync with
// crates/penroam-pdf/src/lib.rs::template_content (PDF export).
//
// On an infinite canvas grid and dotted paper have no edges: the pattern repeats from the
// origin in every direction. Ruled paper keeps the page's width, header and margin line and
// only goes on downward, like a roll of the page (`drawCanvasTemplate`, `isCanvasColumn`).
// Exports cut a canvas to a sheet whose corner is on a whole pitch, so the lines line up.
import type { Template } from "../generated/Template";

export const TEMPLATE_COLORS = {
  ruled: "rgb(199,210,222)",
  margin: "rgb(240,180,180)",
  grid: "rgb(213,221,230)",
  dot: "rgb(154,167,181)",
};

/** Draws `t` in document coordinates (ctx carries the doc->px transform). */
export function drawTemplate(ctx: CanvasRenderingContext2D, t: Template, w: number, h: number, mono = false): void {
  const sp = t.spacing;
  ctx.save();
  ctx.lineCap = "butt";
  const c = (col: string) => (mono ? "rgb(170,170,170)" : col);
  if (t.kind === "ruled") {
    ctx.strokeStyle = c(TEMPLATE_COLORS.ruled);
    ctx.lineWidth = 0.5;
    ctx.beginPath();
    for (let y = sp * 3; y <= h - sp; y += sp) {
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
    }
    ctx.stroke();
    ctx.strokeStyle = c(TEMPLATE_COLORS.margin);
    ctx.lineWidth = 0.6;
    ctx.beginPath();
    ctx.moveTo(sp * 3, 0);
    ctx.lineTo(sp * 3, h);
    ctx.stroke();
  } else if (t.kind === "grid") {
    ctx.strokeStyle = c(TEMPLATE_COLORS.grid);
    ctx.lineWidth = 0.4;
    ctx.beginPath();
    for (let x = sp; x < w; x += sp) {
      ctx.moveTo(x, 0);
      ctx.lineTo(x, h);
    }
    for (let y = sp; y < h; y += sp) {
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
    }
    ctx.stroke();
  } else if (t.kind === "dotted") {
    ctx.fillStyle = c(TEMPLATE_COLORS.dot);
    for (let y = sp; y < h; y += sp) {
      for (let x = sp; x < w; x += sp) {
        ctx.beginPath();
        ctx.arc(x, y, 0.9, 0, Math.PI * 2);
        ctx.fill();
      }
    }
  }
  ctx.restore();
}

/** Fewer device pixels than this between lines or dots and the pattern is left out. */
const MIN_PATTERN_PX = 4;

/**
 * True when an infinite canvas with paper `t` is a column: ruled paper stays the page's width,
 * with its header and margin line, and is infinite only downward. Matches
 * `Page::is_canvas_column` in the core crate.
 */
export function isCanvasColumn(t: Template): boolean {
  return t.kind === "ruled";
}

/**
 * Draws `t` over the doc rect `r` of an infinite canvas (ctx carries the doc->px transform).
 * `res` is device px per pt: a pattern finer than `MIN_PATTERN_PX` would only be a grey
 * wash, so it isn't drawn. Ruled paper is drawn as a column `pageWidth` wide from the origin,
 * with the header rows and margin line of a sheet and rules all the way down.
 */
export function drawCanvasTemplate(ctx: CanvasRenderingContext2D, t: Template, r: readonly [number, number, number, number], res: number, mono = false, pageWidth = 0): void {
  const sp = t.spacing;
  if (t.kind === "blank" || sp * res < MIN_PATTERN_PX) return;
  const [x0, y0, x1, y1] = r;
  const first = (v: number) => Math.ceil(v / sp) * sp;
  const c = (col: string) => (mono ? "rgb(170,170,170)" : col);
  ctx.save();
  ctx.lineCap = "butt";
  if (t.kind === "ruled") {
    const right = Math.min(x1, pageWidth);
    ctx.strokeStyle = c(TEMPLATE_COLORS.ruled);
    ctx.lineWidth = 0.5;
    ctx.beginPath();
    for (let y = Math.max(first(y0), sp * 3); y <= y1; y += sp) {
      ctx.moveTo(Math.max(0, x0), y);
      ctx.lineTo(right, y);
    }
    ctx.stroke();
    if (x0 <= sp * 3 && sp * 3 <= x1 && y1 > 0) {
      ctx.strokeStyle = c(TEMPLATE_COLORS.margin);
      ctx.lineWidth = 0.6;
      ctx.beginPath();
      ctx.moveTo(sp * 3, Math.max(0, y0));
      ctx.lineTo(sp * 3, y1);
      ctx.stroke();
    }
  } else if (t.kind === "grid") {
    ctx.strokeStyle = c(TEMPLATE_COLORS.grid);
    ctx.lineWidth = 0.4;
    ctx.beginPath();
    for (let y = first(y0); y <= y1; y += sp) {
      ctx.moveTo(x0, y);
      ctx.lineTo(x1, y);
    }
    for (let x = first(x0); x <= x1; x += sp) {
      ctx.moveTo(x, y0);
      ctx.lineTo(x, y1);
    }
    ctx.stroke();
  } else if (t.kind === "dotted") {
    ctx.fillStyle = c(TEMPLATE_COLORS.dot);
    // one path per row: a fill per dot is far too slow over a large canvas
    for (let y = first(y0); y <= y1; y += sp) {
      ctx.beginPath();
      for (let x = first(x0); x <= x1; x += sp) {
        ctx.moveTo(x + 0.9, y);
        ctx.arc(x, y, 0.9, 0, Math.PI * 2);
      }
      ctx.fill();
    }
  }
  ctx.restore();
}

/** The pitch text locks to on this paper, or undefined when it has no writing rows. */
export function rowPitchOf(t: Template): number | undefined {
  return t.kind === "ruled" || t.kind === "dotted" ? t.spacing : undefined;
}

/**
 * The top of a row-locked block near `y`. A one-row line's baseline is then one
 * pitch below the top, and on ruled paper the first rule is the third row, on an
 * infinite canvas too. A canvas of dotted paper has rows everywhere, above the origin too.
 */
export function snapToRow(y: number, t: Template, pitch: number, round: (v: number) => number = Math.round, canvas = false): number {
  const first = t.kind === "ruled" ? 2 : canvas ? -Infinity : 0;
  return Math.max(first, round(y / pitch)) * pitch;
}
