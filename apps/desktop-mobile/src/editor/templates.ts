// Page background templates. Keep in sync with
// crates/penroam-pdf/src/lib.rs::template_content (PDF export).
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
