// Page renderer: three page-sized canvases inside the CSS-transformed page
// element ("world"). Pan and zoom only change the CSS transform, so they never
// redraw; the backing resolution is re-rendered once a zoom settles.
//
//   bg    paper, PDF page bitmap, template      (rarely redrawn)
//   ink   ink strokes (by z) and images          (dirty-rect redraw / incremental append)
//   text  text blocks (by z), always above ink   (dirty-rect redraw)
//
// Paint order (ink, then images, then text) is the same in the PDF exporter.
import type { Element } from "../generated/Element";
import type { InkStroke } from "../generated/InkStroke";
import type { TextBlock } from "../generated/TextBlock";
import type { Template } from "../generated/Template";
import { drawStroke, rectsIntersect, strokeBounds, type Rect } from "./geometry";
import { drawTemplate } from "./templates";
import { firstBaseline, layout, lineHeight, TEXT_FONT_FAMILY, contentHeight } from "./textLayout";

export type Ink = { type: "ink" } & InkStroke;
export type Text = { type: "text" } & TextBlock;

export function textBounds(t: TextBlock): Rect {
  const h = Math.max(t.height, contentHeight(t.text, t.fontSize, t.width));
  return [t.x, t.y, t.x + t.width, t.y + h];
}

export function elementBounds(e: Element): Rect {
  if (e.type === "ink") return strokeBounds(e);
  if (e.type === "text") return textBounds(e);
  return [e.x, e.y, e.x + e.width, e.y + e.height];
}

export function drawText(ctx: CanvasRenderingContext2D, t: TextBlock, colorOverride?: string): void {
  ctx.save();
  ctx.font = `${t.fontSize}px ${TEXT_FONT_FAMILY}`;
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = colorOverride ?? t.color;
  const lh = lineHeight(t.fontSize);
  const base = firstBaseline(t.fontSize);
  layout(t.text, t.fontSize, t.width).forEach((line, i) => {
    const dx = t.align === "center" ? (t.width - line.width) / 2 : t.align === "right" ? t.width - line.width : 0;
    ctx.fillText(line.text.replace(/\t/g, "    "), t.x + dx, t.y + base + i * lh);
  });
  ctx.restore();
}

export interface RenderOptions {
  /** ePaper: grey highlighter, no colour templates */
  mono: boolean;
}

export class PageRenderer {
  readonly bg: HTMLCanvasElement;
  readonly ink: HTMLCanvasElement;
  readonly text: HTMLCanvasElement;
  private bctx: CanvasRenderingContext2D;
  private ictx: CanvasRenderingContext2D;
  private tctx: CanvasRenderingContext2D;
  width = 612;
  height = 792;
  res = 1; // device px per pt
  template: Template = { kind: "blank", spacing: 24 };
  pdfBitmap: CanvasImageSource | null = null;
  images = new Map<string, CanvasImageSource>();
  hidden = new Set<string>();
  opts: RenderOptions = { mono: false };
  /** last full ink redraw duration (ms, flushed) */
  lastFullRedrawMs = 0;

  constructor(
    host: HTMLElement,
    private elements: () => Element[],
  ) {
    const mk = (cls: string) => {
      const c = document.createElement("canvas");
      c.className = `absolute inset-0 h-full w-full ${cls}`;
      c.style.pointerEvents = "none";
      host.appendChild(c);
      return c;
    };
    this.bg = mk("penroam-bg");
    this.ink = mk("penroam-ink");
    this.text = mk("penroam-text");
    const ctx = (c: HTMLCanvasElement) => c.getContext("2d", { alpha: c !== this.bg })!;
    this.bctx = ctx(this.bg);
    this.ictx = ctx(this.ink);
    this.tctx = ctx(this.text);
  }

  setPage(width: number, height: number, template: Template) {
    this.width = width;
    this.height = height;
    this.template = template;
    this.pdfBitmap = null;
    this.hidden.clear();
  }

  /** Resizes the backing stores; returns true if they changed. */
  setResolution(res: number): boolean {
    const w = Math.max(1, Math.round(this.width * res));
    const h = Math.max(1, Math.round(this.height * res));
    if (this.ink.width === w && this.ink.height === h && this.res === res) return false;
    this.res = res;
    for (const c of [this.bg, this.ink, this.text]) {
      c.width = w;
      c.height = h;
    }
    return true;
  }

  private prep(ctx: CanvasRenderingContext2D) {
    ctx.setTransform(this.res, 0, 0, this.res, 0, 0);
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
  }

  renderAll() {
    this.renderBackground();
    this.renderInk(null);
    this.renderText(null);
  }

  renderBackground() {
    const c = this.bctx;
    c.setTransform(1, 0, 0, 1, 0, 0);
    c.fillStyle = "#ffffff";
    c.fillRect(0, 0, this.bg.width, this.bg.height);
    if (this.pdfBitmap) {
      c.imageSmoothingQuality = "high";
      c.drawImage(this.pdfBitmap, 0, 0, this.bg.width, this.bg.height);
    }
    this.prep(c);
    drawTemplate(c, this.template, this.width, this.height, this.opts.mono);
  }

  private clip(ctx: CanvasRenderingContext2D, rect: Rect | null) {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    if (!rect) {
      ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
      this.prep(ctx);
      return;
    }
    const pad = 2;
    const x0 = Math.max(0, Math.floor(rect[0] * this.res) - pad);
    const y0 = Math.max(0, Math.floor(rect[1] * this.res) - pad);
    const x1 = Math.min(ctx.canvas.width, Math.ceil(rect[2] * this.res) + pad);
    const y1 = Math.min(ctx.canvas.height, Math.ceil(rect[3] * this.res) + pad);
    ctx.save();
    ctx.beginPath();
    ctx.rect(x0, y0, x1 - x0, y1 - y0);
    ctx.clip();
    ctx.clearRect(x0, y0, x1 - x0, y1 - y0);
    this.prep(ctx);
  }

  private strokeStyleFor(s: InkStroke): InkStroke {
    if (this.opts.mono && s.tool === "highlighter") return { ...s, color: "#9a9a9a", opacity: 0.45 };
    return s;
  }

  /** Redraws ink inside `rect` (doc pt) or everything when rect is null. */
  renderInk(rect: Rect | null) {
    const t0 = performance.now();
    const ctx = this.ictx;
    this.clip(ctx, rect);
    for (const e of this.elements()) {
      if (e.type === "text" || this.hidden.has(e.id)) continue;
      if (rect && !rectsIntersect(rect, elementBounds(e))) continue;
      if (e.type === "ink") drawStroke(ctx, this.strokeStyleFor(e));
      else this.drawImageEl(ctx, e);
    }
    if (rect) ctx.restore();
    if (!rect) {
      ctx.getImageData(0, 0, 1, 1); // GPU flush, as in M00's timing method
      this.lastFullRedrawMs = performance.now() - t0;
    }
  }

  renderText(rect: Rect | null) {
    const ctx = this.tctx;
    this.clip(ctx, rect);
    for (const e of this.elements()) {
      if (e.type !== "text" || this.hidden.has(e.id)) continue;
      if (rect && !rectsIntersect(rect, textBounds(e))) continue;
      drawText(ctx, e, this.opts.mono && e.color.toLowerCase() !== "#000000" ? "#000000" : undefined);
    }
    if (rect) ctx.restore();
  }

  /** Appends one stroke on top (valid when it has the highest z). */
  appendStroke(s: InkStroke) {
    this.prep(this.ictx);
    drawStroke(this.ictx, this.strokeStyleFor(s));
  }

  private drawImageEl(ctx: CanvasRenderingContext2D, e: Extract<Element, { type: "image" }>) {
    const img = this.images.get(e.assetHash);
    if (img) ctx.drawImage(img, e.x, e.y, e.width, e.height);
    else {
      ctx.save();
      ctx.strokeStyle = "#9ca3af";
      ctx.setLineDash([4, 4]);
      ctx.strokeRect(e.x, e.y, e.width, e.height);
      ctx.restore();
    }
  }

  /** Small PNG of the current page for the sidebar / library. */
  async thumbnail(maxWidth = 240): Promise<Uint8Array | null> {
    const scale = maxWidth / this.width;
    const c = document.createElement("canvas");
    c.width = Math.max(1, Math.round(this.width * scale));
    c.height = Math.max(1, Math.round(this.height * scale));
    const x = c.getContext("2d")!;
    x.imageSmoothingQuality = "high";
    for (const layer of [this.bg, this.ink, this.text]) x.drawImage(layer, 0, 0, c.width, c.height);
    const blob: Blob | null = await new Promise((r) => c.toBlob(r, "image/png"));
    return blob ? new Uint8Array(await blob.arrayBuffer()) : null;
  }
}
