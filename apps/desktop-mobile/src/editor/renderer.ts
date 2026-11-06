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
import { layoutMarkdown, type Paint } from "./markdown";
import { drawMath, ensureMath, mathBox, mathEpoch, renderedMath } from "./math";
import { canvasFont } from "./textLayout";

export type Ink = { type: "ink" } & InkStroke;
export type Text = { type: "text" } & TextBlock;

// Bounds are read on every hit test and dirty-rect redraw, so layouts are kept.
const PAINT_CACHE_ENTRIES = 2048;
const paints = new Map<string, Paint>();

/** The cached Markdown layout of a block. */
export function paintOf(t: Pick<TextBlock, "text" | "fontSize" | "width" | "align" | "rowPitch">): Paint {
  const key = `${mathEpoch()}|${t.fontSize}|${t.width}|${t.rowPitch ?? ""}|${t.align}|${t.text}`;
  let p = paints.get(key);
  if (!p) {
    p = layoutMarkdown(t.text, t.fontSize, t.width, t.rowPitch ?? null, t.align, mathBox);
    if (paints.size >= PAINT_CACHE_ENTRIES) paints.clear();
    paints.set(key, p);
    // Until MathJax has loaded formulas show their source.
    if (p.runs.some((r) => r.face === "mono" && r.text.includes("$"))) void ensureMath();
  }
  return p;
}

export function textBounds(t: TextBlock): Rect {
  const p = paintOf(t);
  return [t.x, t.y, t.x + t.width, t.y + Math.max(t.height, p.bottom)];
}

export function elementBounds(e: Element): Rect {
  if (e.type === "ink") return strokeBounds(e);
  if (e.type === "text") return textBounds(e);
  return [e.x, e.y, e.x + e.width, e.y + e.height];
}

export function drawText(ctx: CanvasRenderingContext2D, t: TextBlock, colorOverride?: string): void {
  const p = paintOf(t);
  const color = colorOverride ?? t.color;
  ctx.save();
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = color;
  ctx.strokeStyle = color;
  ctx.lineCap = "butt";
  for (const r of p.runs) {
    ctx.font = canvasFont(r.face, r.size);
    ctx.fillText(r.text, t.x + r.x, t.y + r.y);
  }
  for (const s of p.segments) {
    ctx.lineWidth = s.width;
    ctx.beginPath();
    ctx.moveTo(t.x + s.x0, t.y + s.y0);
    ctx.lineTo(t.x + s.x1, t.y + s.y1);
    ctx.stroke();
  }
  for (const m of p.math) {
    const r = renderedMath(m.tex, m.display);
    if (r) drawMath(ctx, r, t.x + m.x, t.y + m.y, m.size);
  }
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
  /** last synchronous (visible-first) part of a progressive redraw (ms, flushed) */
  lastVisibleRedrawMs = 0;
  private bands: Rect[] = [];
  private bandTimer: number | null = null;
  private idleWaiters: (() => void)[] = [];

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

  /**
   * Repaints everything. With `visible` (doc pt) the visible region is drawn
   * synchronously and the rest of the page follows in bands over the next
   * frames (tile invalidation by horizontal bands), so large pages show up
   * as fast as a viewport-sized canvas would.
   */
  renderAll(visible: Rect | null = null) {
    this.renderBackground();
    this.renderText(null);
    this.renderInkProgressive(visible);
  }

  renderInkProgressive(visible: Rect | null) {
    this.cancelBands();
    if (!visible) return this.renderInk(null);
    const pad = 8;
    const v: Rect = [Math.max(0, visible[0] - pad), Math.max(0, visible[1] - pad), Math.min(this.width, visible[2] + pad), Math.min(this.height, visible[3] + pad)];
    if (v[2] <= v[0] || v[3] <= v[1] || ((v[2] - v[0]) * (v[3] - v[1])) / (this.width * this.height) > 0.85) {
      this.renderInk(null);
      this.lastVisibleRedrawMs = this.lastFullRedrawMs;
      return;
    }
    const t0 = performance.now();
    const ctx = this.ictx;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
    this.renderInk(v);
    ctx.getImageData(0, 0, 1, 1);
    this.lastVisibleRedrawMs = performance.now() - t0;
    // remaining page area in bands, nearest to the viewport first
    const bandH = 160;
    const cy = (v[1] + v[3]) / 2;
    const bands: Rect[] = [];
    for (let y = 0; y < this.height; y += bandH) {
      const b: Rect = [0, y, this.width, Math.min(this.height, y + bandH)];
      const fullyVisible = b[1] >= v[1] && b[3] <= v[3] && v[0] <= 0 && v[2] >= this.width;
      if (!fullyVisible) bands.push(b);
    }
    bands.sort((a, b) => Math.abs((a[1] + a[3]) / 2 - cy) - Math.abs((b[1] + b[3]) / 2 - cy));
    this.bands = bands;
    this.scheduleBands();
  }

  private scheduleBands() {
    if (this.bandTimer !== null) return;
    this.bandTimer = window.setTimeout(() => {
      this.bandTimer = null;
      const t0 = performance.now();
      while (this.bands.length && performance.now() - t0 < 10) this.renderInk(this.bands.shift()!);
      if (this.bands.length) this.scheduleBands();
      else this.resolveIdle();
    }, 0);
  }

  private cancelBands() {
    if (this.bandTimer !== null) clearTimeout(this.bandTimer);
    this.bandTimer = null;
    this.bands = [];
  }

  private resolveIdle() {
    const w = this.idleWaiters;
    this.idleWaiters = [];
    w.forEach((f) => f());
  }

  /** Resolves once every band of a progressive redraw has been painted. */
  whenComplete(): Promise<void> {
    if (!this.bands.length && this.bandTimer === null) return Promise.resolve();
    return new Promise((r) => this.idleWaiters.push(r));
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
    if (!rect) this.cancelBands();
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
      ctx.getImageData(0, 0, 1, 1); // GPU flush, as in the benchmark protocol's timing method
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
    await this.whenComplete();
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
