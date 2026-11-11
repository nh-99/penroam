// Page renderer: three page-sized canvases inside the CSS-transformed page
// element ("world"). Pan and zoom only change the CSS transform, so they never
// redraw; the backing resolution is re-rendered once a zoom settles.
//
//   bg    paper, PDF page bitmap, template      (rarely redrawn)
//   ink   ink strokes (by z) and images          (dirty-rect redraw / incremental append)
//   text  text blocks (by z), always above ink   (dirty-rect redraw)
//
// Paint order (ink, then images, then text) is the same in the PDF exporter.
//
// Large pages take hundreds of milliseconds to draw, which would stall pen input on the web
// path, so redraws after a zoom run in time slices of a few strokes (`SLICE_MS`). `rescale`
// draws the visible region into a detached canvas and swaps it in whole, so the old bitmap stays
// on screen until the sharper one is ready. The rest of the page follows in bands, nearest first.
// A slice works from a snapshot of the elements, so anything that changes the ink while one is
// running restarts it (or, for a stroke added on top, queues it on the job).
//
// The page canvases are capped by pixel area, which limits how sharp they can be: about 200 % on
// an iPad with a Letter page. Past that, `showDetail` adds two canvases covering just the part of
// the page around the viewport at full resolution, and a clip-path cuts their area out of the page
// layers so that nothing shows twice. The page layers stay complete underneath, so a pan past the
// detail area shows softer ink until the next settle, never missing ink.
//
// On an infinite canvas the layers cover an *extent* instead of the page: a rectangle of the
// unbounded surface around the content and the view, whose top-left corner (`org`) need not be
// the origin. The engine moves and grows it as the view moves; everything here draws in
// document coordinates offset by `org`, so a sheet is simply an extent at the origin.
//
// The dark theme draws the page as its negative (`RenderOptions.dark`): the paper and any PDF
// pages are inverted, and black, white and grey ink and text swap lightness (`shownColor`).
// Coloured ink and raster images are left as they are. This is display only: the page keeps
// its colours, and exports and saved thumbnails are drawn with `dark` off.
//
// A PDF page placed as an element (an image with `pdfPage`) is drawn from `pdfPages`, bitmaps
// the engine renders at a resolution that suits how close the page is to the view.
//
// The detail canvases live in the viewport, not in the scaled page element. The compositor can
// rasterise a `will-change: transform` element at a fixed scale and stretch it (seen as blur on
// iPad), so anything inside it may not reach the screen at its own resolution. At rest these
// canvases are shown 1:1 with device pixels, with no scale between them and the display.
import type { Element } from "../generated/Element";
import type { InkStroke } from "../generated/InkStroke";
import type { TextBlock } from "../generated/TextBlock";
import type { Template } from "../generated/Template";
import { drawStroke, rectsIntersect, strokeBounds, unionRect, type Rect } from "./geometry";
import { drawCanvasTemplate, drawTemplate, isCanvasColumn } from "./templates";
import { layoutMarkdownWithTargets, type LinkBox, type Paint, type TaskBox } from "./markdown";
import { drawMath, ensureMath, mathBox, mathEpoch, renderedMath } from "./math";
import { drawRun } from "./textLayout";

export type Ink = { type: "ink" } & InkStroke;
export type Text = { type: "text" } & TextBlock;

// Bounds are read on every hit test and dirty-rect redraw, so layouts are kept.
const PAINT_CACHE_ENTRIES = 2048;
const layouts = new Map<string, TextLayout>();

type Layoutable = Pick<TextBlock, "text" | "font" | "fontSize" | "width" | "align" | "rowPitch">;
type TextLayout = { paint: Paint; links: LinkBox[]; tasks: TaskBox[] };

function layoutOf(t: Layoutable): TextLayout {
  const font = t.font ?? "formatted";
  const key = `${mathEpoch()}|${font}|${t.fontSize}|${t.width}|${t.rowPitch ?? ""}|${t.align}|${t.text}`;
  let l = layouts.get(key);
  if (!l) {
    l = layoutMarkdownWithTargets(t.text, font, t.fontSize, t.width, t.rowPitch ?? null, t.align, mathBox);
    if (layouts.size >= PAINT_CACHE_ENTRIES) layouts.clear();
    layouts.set(key, l);
    // Until MathJax has loaded formulas show their source.
    if (l.paint.runs.some((r) => r.face === "mono" && r.text.includes("$"))) void ensureMath();
  }
  return l;
}

/** The cached Markdown layout of a block. */
export function paintOf(t: Layoutable): Paint {
  return layoutOf(t).paint;
}

/** The link under a page point, if the block has one there. */
export function linkAt(t: TextBlock, x: number, y: number): string | null {
  const lx = x - t.x, ly = y - t.y;
  for (const b of layoutOf(t).links) if (lx >= b.x0 && lx <= b.x1 && ly >= b.y0 && ly <= b.y1) return b.href;
  return null;
}

/** The task checkbox nearest a page point, if one is within `pad` of it. */
export function taskAt(t: TextBlock, x: number, y: number, pad: number): TaskBox | null {
  const lx = x - t.x, ly = y - t.y;
  let best: TaskBox | null = null;
  let bestDist = pad;
  for (const b of layoutOf(t).tasks) {
    const dist = Math.hypot(Math.max(b.x0 - lx, lx - b.x1, 0), Math.max(b.y0 - ly, ly - b.y1, 0));
    if (dist <= bestDist) {
      best = b;
      bestDist = dist;
    }
  }
  return best;
}

export function textBounds(t: TextBlock): Rect {
  const p = paintOf(t);
  return [t.x, t.y, t.x + t.width, t.y + Math.max(t.height, p.bottom)];
}

/** Space around an infinite canvas's content when it is cut to a sheet (`CANVAS_SHEET_MARGIN_PT` in the core crate). */
export const CANVAS_SHEET_MARGIN_PT = 36;

/**
 * The doc rect an infinite canvas page shows as a picture: its content and PDF background with
 * a margin, the corner on a whole template pitch, or the page itself when it is empty. That is
 * the same cut `Page::canvas_as_sheet` makes for PDF and SVG, used for PNG export.
 *
 * With `paper`, for thumbnails and other previews, the rect is at least the page's size from
 * that corner and grown to the page's proportions. A small note then fills part of a page
 * instead of a tiny picture, which library covers would blow up and crop to their page shape.
 *
 * Ruled paper is a column (`isCanvasColumn`): the rect is the page's width from the origin,
 * down to the content's end and at least a page long; with `paper`, the top page of it.
 */
export function canvasSheetRect(page: { size: { width: number; height: number }; template: Template; background?: unknown; elements: Element[] }, paper = false): Rect {
  const { width: pw, height: ph } = page.size;
  let r: Rect | null = page.background ? [0, 0, pw, ph] : null;
  for (const e of page.elements) r = unionRect(r, elementBounds(e));
  if (!r) return [0, 0, pw, ph];
  const m = CANVAS_SHEET_MARGIN_PT;
  const sp = Math.max(1, page.template.spacing);
  if (isCanvasColumn(page.template)) {
    // content off the column, from before the paper was ruled, stays in the picture
    const x0 = Math.min(0, Math.floor(r[0] / sp) * sp), y0 = Math.min(0, Math.floor(r[1] / sp) * sp);
    const x1 = Math.max(pw, r[2]);
    return [x0, y0, x1, paper ? y0 + ((x1 - x0) * ph) / pw : Math.max(ph, r[3] + m)];
  }
  const x0 = Math.floor((r[0] - m) / sp) * sp, y0 = Math.floor((r[1] - m) / sp) * sp;
  if (!paper) return [x0, y0, r[2] + m, r[3] + m];
  let w = Math.max(pw, r[2] + m - x0), h = Math.max(ph, r[3] + m - y0);
  // widen or lengthen, whichever keeps the content whole, to the page's proportions
  if (w / h > pw / ph) h = (w * ph) / pw;
  else w = (h * pw) / ph;
  return [x0, y0, x0 + w, y0 + h];
}

/** Names one page of one PDF asset, for `PageRenderer.pdfPages`. */
export function pdfPageKey(p: { assetHash: string; pageIndex: number }): string {
  return `${p.assetHash}#${p.pageIndex}`;
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
  for (const r of p.runs) drawRun(ctx, r.face, r.size, r.text, t.x + r.x, t.y + r.y);
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

/** Longest stretch of ink drawing before yielding to input (ms). */
const SLICE_MS = 8;
/** Clock checks are not free next to a short stroke, so they happen every few elements. */
const SLICE_CHECK_EVERY = 8;

/** One region being drawn across several time slices. */
interface InkJob {
  ctx: CanvasRenderingContext2D;
  res: number;
  rect: Rect;
  items: Element[];
  next: number;
  /** Strokes added on top since the job began; drawn last so they keep their stacking. */
  tail: InkStroke[];
  /** Whether the region has been cleared; a restart clears it again. */
  cleared: boolean;
  /** A rescale: swap this canvas in when the job ends. */
  swap: HTMLCanvasElement | null;
  /** Top-left of the canvas in doc pt: the extent's for page layers, the region's for detail. */
  org: [number, number];
  /** A detail job: show the canvas over this region when the job ends. */
  detail: Detail | null;
}

/** A region of the page drawn sharper than the page layers can be. */
interface Detail {
  rect: Rect;
  res: number;
  /** Device px per CSS px; the canvases are this much denser than their CSS size. */
  dpr: number;
}

const contains = (o: Rect, i: Rect) => o[0] <= i[0] && o[1] <= i[1] && o[2] >= i[2] && o[3] >= i[3];

export interface RenderOptions {
  /** ePaper: grey highlighter, no colour templates */
  mono: boolean;
  /** Dark theme: inverted paper and PDF pages, black and white ink swapped (`shownColor`). */
  dark?: boolean;
}

/** Most difference between the channels of a colour that still counts as grey. */
const GREY_SPREAD = 24;

/**
 * The colour ink or text of colour `color` (`#rrggbb`) is shown in. On a dark page black,
 * white and greys are inverted, so dark ink stays readable on the inverted paper; any other
 * colour, or any colour with `dark` off, is returned unchanged. Applying it twice gives the
 * colour back, which lets native ink layers be handed shown colours and return stored ones.
 */
export function shownColor(color: string, dark: boolean): string {
  if (!dark || !/^#[0-9a-fA-F]{6}$/.test(color)) return color;
  const n = parseInt(color.slice(1), 16);
  const [r, g, b] = [n >> 16, (n >> 8) & 255, n & 255];
  if (Math.max(r, g, b) - Math.min(r, g, b) > GREY_SPREAD) return color;
  return `#${(0xffffff - n).toString(16).padStart(6, "0")}`;
}

/** Inverts a rectangle of `ctx` (in its current transform), which must be opaque there. */
function invertRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number) {
  ctx.save();
  ctx.globalCompositeOperation = "difference";
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(x, y, w, h);
  ctx.restore();
}

export class PageRenderer {
  readonly bg: HTMLCanvasElement;
  readonly ink: HTMLCanvasElement;
  readonly text: HTMLCanvasElement;
  private detailInk: HTMLCanvasElement;
  private detailText: HTMLCanvasElement;
  private dictx: CanvasRenderingContext2D;
  private dtctx: CanvasRenderingContext2D;
  private view = { scale: 1, ox: 0, oy: 0 };
  /** What the detail canvases show, or null when they are hidden. */
  private detail: Detail | null = null;
  /** Waiting for the job slot; goes ahead of the remaining bands. */
  private pendingDetail: Detail | null = null;
  private bctx: CanvasRenderingContext2D;
  private ictx: CanvasRenderingContext2D;
  private tctx: CanvasRenderingContext2D;
  width = 612;
  height = 792;
  /** Doc point at the layers' top-left corner: the origin, except on an infinite canvas. */
  org: [number, number] = [0, 0];
  /** Infinite canvas: the template repeats without edges, or ruled paper is a column. */
  canvas = false;
  /** The page's width, which a ruled canvas column keeps. */
  pageWidth = 0;
  res = 1; // device px per pt
  template: Template = { kind: "blank", spacing: 24 };
  pdfBitmap: CanvasImageSource | null = null;
  /** Where `pdfBitmap` goes (doc pt); null fills the layer, which is the page itself on a sheet. */
  pdfBox: Rect | null = null;
  images = new Map<string, CanvasImageSource>();
  /** Bitmaps of PDF page elements by `pdfPageKey`, and the resolution each was rendered at. */
  pdfPages = new Map<string, { canvas: CanvasImageSource; res: number }>();
  /**
   * Drawn under every element in the ink layers. On an infinite canvas the page's own PDF
   * background goes here, as a PDF page element, instead of into `bg`: the background layer's
   * resolution drops as the extent grows, while this one is drawn sharp like any element.
   */
  underlay: Extract<Element, { type: "image" }> | null = null;
  hidden = new Set<string>();
  opts: RenderOptions = { mono: false, dark: false };
  /** last full ink redraw duration (ms, flushed) */
  lastFullRedrawMs = 0;
  /** last synchronous (visible-first) part of a progressive redraw (ms, flushed) */
  lastVisibleRedrawMs = 0;
  /** Longest single slice of the last progressive redraw (ms): the longest input can wait on ink. */
  lastLongestSliceMs = 0;
  private bands: Rect[] = [];
  private job: InkJob | null = null;
  private bandTimer: number | null = null;
  private afterSwap: (() => void) | null = null;
  private idleWaiters: (() => void)[] = [];

  constructor(
    host: HTMLElement,
    private elements: () => Element[],
    /** Unscaled parent for the detail canvases. */
    screen: HTMLElement = host,
  ) {
    const mk = (cls: string, parent = host) => {
      const c = document.createElement("canvas");
      c.className = `absolute inset-0 h-full w-full ${cls}`;
      c.style.pointerEvents = "none";
      parent.appendChild(c);
      return c;
    };
    this.bg = mk("penroam-bg");
    this.ink = mk("penroam-ink");
    this.text = mk("penroam-text");
    this.detailInk = mk("penroam-ink-detail", screen);
    this.detailText = mk("penroam-text-detail", screen);
    for (const c of [this.detailInk, this.detailText]) {
      // sized in `finishDetail` and placed in `placeDetail`, not by the page
      c.className = c.className.replace(" inset-0 h-full w-full", " left-0 top-0");
      c.style.transformOrigin = "0 0";
      c.style.display = "none";
    }
    const ctx = (c: HTMLCanvasElement) => c.getContext("2d", { alpha: c !== this.bg })!;
    this.bctx = ctx(this.bg);
    this.ictx = ctx(this.ink);
    this.tctx = ctx(this.text);
    this.dictx = ctx(this.detailInk);
    this.dtctx = ctx(this.detailText);
  }

  /**
   * Starts drawing a page. The layers cover `width` × `height` pt from `org`; for a sheet
   * that is the page. `canvas` draws the template the infinite canvas way, with ruled paper
   * `pageWidth` wide.
   */
  setPage(width: number, height: number, template: Template, org: [number, number] = [0, 0], canvas = false, pageWidth = width) {
    this.template = template;
    this.canvas = canvas;
    this.pageWidth = pageWidth;
    this.pdfBitmap = null;
    this.pdfBox = null;
    this.underlay = null;
    this.hidden.clear();
    this.setExtent(org, width, height);
    this.resolveIdle();
  }

  /**
   * Moves the layers to cover `width` × `height` pt from `org`, keeping everything else.
   * Nothing is drawn: follow with `setResolution` and `renderAll`.
   */
  setExtent(org: [number, number], width: number, height: number) {
    this.org = org;
    this.width = width;
    this.height = height;
    this.cancelBands();
    this.hideDetail();
    for (const c of [this.bg, this.ink, this.text]) {
      c.style.left = `${org[0]}px`;
      c.style.top = `${org[1]}px`;
      c.style.width = `${width}px`;
      c.style.height = `${height}px`;
    }
  }

  /** The doc rect the layers cover. */
  get extent(): Rect {
    return [this.org[0], this.org[1], this.org[0] + this.width, this.org[1] + this.height];
  }

  /** The resolution the page is at, or is about to change to. */
  get targetRes(): number {
    return this.job?.swap ? this.job.res : this.res;
  }

  /** Resizes the backing stores; returns true if they changed. */
  setResolution(res: number): boolean {
    const w = Math.max(1, Math.round(this.width * res));
    const h = Math.max(1, Math.round(this.height * res));
    if (this.ink.width === w && this.ink.height === h && this.res === res) return false;
    this.cancelBands();
    this.hideDetail();
    this.res = res;
    for (const c of [this.bg, this.ink, this.text]) {
      c.width = w;
      c.height = h;
    }
    return true;
  }

  private prep(ctx: CanvasRenderingContext2D, res = this.res, org: [number, number] = this.org) {
    ctx.setTransform(res, 0, 0, res, -org[0] * res, -org[1] * res);
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
    const v = this.clampVisible(visible);
    if (!v || ((v[2] - v[0]) * (v[3] - v[1])) / (this.width * this.height) > 0.85) {
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
    this.queueBands(v);
    this.scheduleBands();
  }

  /**
   * Changes the backing resolution without blocking input: the visible region is drawn into a
   * detached canvas over several slices and swapped in, and `done` runs once it is on screen.
   * Everything stays usable at the old resolution until then.
   */
  rescale(res: number, visible: Rect, done?: () => void) {
    this.cancelBands();
    const off = document.createElement("canvas");
    off.width = Math.max(1, Math.round(this.width * res));
    off.height = Math.max(1, Math.round(this.height * res));
    const ctx = off.getContext("2d");
    if (!ctx) {
      // no memory for a second canvas: the blocking path still works
      this.setResolution(res);
      this.renderAll(visible);
      return done?.();
    }
    this.afterSwap = done ?? null;
    this.lastLongestSliceMs = 0;
    this.job = this.startJob(ctx, res, this.clampVisible(visible) ?? this.extent, off);
    this.scheduleBands();
  }

  /**
   * Shows the region around `visible` at `res`, for zooms past what the page layers can hold. The
   * current detail stays up until the new one is ready. `maxPixels` caps each detail canvas, so
   * an unusually large window gets slightly softer rather than failing to allocate.
   */
  showDetail(visible: Rect, res: number, maxPixels: number, dpr = 1) {
    const v = this.clampVisible(visible, 0);
    if (!v) return this.hideDetail();
    const covered = (d: Detail | null) => !!d && contains(d.rect, v) && Math.abs(d.res - res) / res <= 0.08;
    const coming = this.pendingDetail ?? this.job?.detail ?? null;
    if (coming ? covered(coming) : covered(this.detail)) return;
    // A margin, so that small pans need no redraw.
    const mx = (v[2] - v[0]) * 0.25;
    const my = (v[3] - v[1]) * 0.25;
    const [ex0, ey0, ex1, ey1] = this.extent;
    const rect: Rect = [Math.max(ex0, Math.floor(v[0] - mx)), Math.max(ey0, Math.floor(v[1] - my)), Math.min(ex1, Math.ceil(v[2] + mx)), Math.min(ey1, Math.ceil(v[3] + my))];
    const px = (rect[2] - rect[0]) * (rect[3] - rect[1]) * res * res;
    this.pendingDetail = { rect, res: px > maxPixels ? res * Math.sqrt(maxPixels / px) : res, dpr };
    if (this.job?.detail) this.job = null; // superseded
    this.scheduleBands();
  }

  /** Back to the page layers alone, once they are sharp enough again. */
  hideDetail() {
    this.pendingDetail = null;
    if (this.job?.detail) this.job = null;
    if (!this.detail) return;
    this.detail = null;
    this.detailInk.style.display = "none";
    this.detailText.style.display = "none";
    // free the memory: an iPad allows only so much canvas in total
    for (const c of [this.detailInk, this.detailText]) c.width = c.height = 1;
    this.ink.style.clipPath = "";
    this.text.style.clipPath = "";
  }

  /** The page's pan and zoom, which the detail canvases follow themselves. */
  setView(view: { scale: number; ox: number; oy: number }) {
    this.view = view;
    this.placeDetail();
  }

  private placeDetail() {
    const d = this.detail;
    if (!d) return;
    const { scale, ox, oy } = this.view;
    // Exactly 1:1 at rest, and on a device pixel: any resampling here is the blur this layer exists to remove.
    const k = scale * d.dpr / d.res;
    const fit = Math.abs(k - 1) < 0.002 ? 1 : k;
    const snap = (v: number) => Math.round(v * d.dpr) / d.dpr;
    const t = `translate(${snap(ox + d.rect[0] * scale)}px, ${snap(oy + d.rect[1] * scale)}px) scale(${fit})`;
    this.detailInk.style.transform = t;
    this.detailText.style.transform = t;
  }

  /** Hides the ink layers while a native surface draws the committed ink. */
  setInkHidden(hidden: boolean) {
    const v = hidden ? "hidden" : "";
    this.ink.style.visibility = v;
    this.detailInk.style.visibility = v;
  }

  private startDetail(d: Detail): InkJob | null {
    const off = document.createElement("canvas");
    off.width = Math.max(1, Math.round((d.rect[2] - d.rect[0]) * d.res));
    off.height = Math.max(1, Math.round((d.rect[3] - d.rect[1]) * d.res));
    const ctx = off.getContext("2d");
    if (!ctx) return null; // out of canvas memory: stay with the softer page layers
    return { ...this.startJob(ctx, d.res, d.rect, off), org: [d.rect[0], d.rect[1]], detail: d };
  }

  /** Puts a finished detail job on screen, in one task so that nothing shows twice or not at all. */
  private finishDetail(job: InkJob) {
    const d = job.detail!;
    const off = job.swap!;
    const [x0, y0, x1, y1] = d.rect;
    for (const c of [this.detailInk, this.detailText]) {
      c.width = off.width;
      c.height = off.height;
      c.style.width = `${off.width / d.dpr}px`;
      c.style.height = `${off.height / d.dpr}px`;
      c.style.display = "";
    }
    this.dictx.setTransform(1, 0, 0, 1, 0, 0);
    this.dictx.drawImage(off, 0, 0);
    this.detail = d;
    this.placeDetail();
    this.renderDetailText(null);
    const [ox, oy] = this.org;
    const pct = (v: number, of: number) => `${(v / of) * 100}%`;
    const [l, r] = [pct(x0 - ox, this.width), pct(x1 - ox, this.width)];
    const [t, b] = [pct(y0 - oy, this.height), pct(y1 - oy, this.height)];
    const hole = `polygon(evenodd, 0 0, 100% 0, 100% 100%, 0 100%, 0 0, ${l} ${t}, ${r} ${t}, ${r} ${b}, ${l} ${b}, ${l} ${t})`;
    this.ink.style.clipPath = hole;
    this.text.style.clipPath = hole;
  }

  private renderDetailInk(rect: Rect | null) {
    const d = this.detail;
    if (!d) return;
    const r = rect ?? d.rect;
    if (!rectsIntersect(r, d.rect)) return;
    const ctx = this.dictx;
    this.clip(ctx, r, d.res, true, [d.rect[0], d.rect[1]]);
    for (const e of this.itemsIn(r)) this.drawItem(ctx, e);
    ctx.restore();
  }

  private renderDetailText(rect: Rect | null) {
    const d = this.detail;
    if (!d) return;
    const r = rect ?? d.rect;
    if (!rectsIntersect(r, d.rect)) return;
    const ctx = this.dtctx;
    this.clip(ctx, r, d.res, true, [d.rect[0], d.rect[1]]);
    for (const e of this.elements()) {
      if (e.type !== "text" || this.hidden.has(e.id) || !rectsIntersect(r, textBounds(e))) continue;
      drawText(ctx, e, this.textColorFor(e));
    }
    ctx.restore();
  }

  private clampVisible(visible: Rect, pad = 8): Rect | null {
    const [x0, y0, x1, y1] = this.extent;
    const v: Rect = [Math.max(x0, visible[0] - pad), Math.max(y0, visible[1] - pad), Math.min(x1, visible[2] + pad), Math.min(y1, visible[3] + pad)];
    return v[2] <= v[0] || v[3] <= v[1] ? null : v;
  }

  /** Remaining page area in bands, nearest to the viewport first. */
  private queueBands(v: Rect) {
    const bandH = 160;
    const cy = (v[1] + v[3]) / 2;
    const bands: Rect[] = [];
    const [x0, y0, x1, y1] = this.extent;
    for (let y = y0; y < y1; y += bandH) {
      const b: Rect = [x0, y, x1, Math.min(y1, y + bandH)];
      const fullyVisible = b[1] >= v[1] && b[3] <= v[3] && v[0] <= x0 && v[2] >= x1;
      if (!fullyVisible) bands.push(b);
    }
    bands.sort((a, b) => Math.abs((a[1] + a[3]) / 2 - cy) - Math.abs((b[1] + b[3]) / 2 - cy));
    this.bands = bands;
  }

  private startJob(ctx: CanvasRenderingContext2D, res: number, rect: Rect, swap: HTMLCanvasElement | null): InkJob {
    return { ctx, res, rect, items: this.itemsIn(rect), next: 0, tail: [], cleared: false, swap, org: this.org, detail: null };
  }

  private itemsIn(rect: Rect): Element[] {
    const items = this.elements().filter((e) => e.type !== "text" && !this.hidden.has(e.id) && rectsIntersect(rect, elementBounds(e)));
    const u = this.underlay;
    return u && rectsIntersect(rect, elementBounds(u)) ? [u, ...items] : items;
  }

  private drawItem(ctx: CanvasRenderingContext2D, e: Element) {
    if (e.type === "ink") drawStroke(ctx, this.strokeStyleFor(e));
    else if (e.type === "image") this.drawImageEl(ctx, e);
  }

  /** Works on `job` until `deadline`; returns true when it has finished. */
  private stepJob(job: InkJob, deadline: number): boolean {
    const ctx = job.ctx;
    this.clip(ctx, job.rect, job.res, !job.cleared, job.org);
    job.cleared = true;
    let n = 0;
    while (job.next < job.items.length) {
      this.drawItem(ctx, job.items[job.next++]);
      if (++n % SLICE_CHECK_EVERY === 0 && performance.now() >= deadline) break;
    }
    const finished = job.next >= job.items.length;
    if (finished) for (const s of job.tail) drawStroke(ctx, this.strokeStyleFor(s));
    ctx.restore();
    return finished;
  }

  /** Puts a finished rescale on screen, in one task so that no blank frame can show. */
  private finishRescale(job: InkJob) {
    const off = job.swap!;
    // What the job did not cover keeps the old pixels, scaled, until its band is redrawn.
    const c = job.ctx;
    const r = job.res;
    c.setTransform(1, 0, 0, 1, 0, 0);
    c.save();
    c.beginPath();
    c.rect(0, 0, off.width, off.height);
    c.rect((job.rect[0] - job.org[0]) * r, (job.rect[1] - job.org[1]) * r, (job.rect[2] - job.rect[0]) * r, (job.rect[3] - job.rect[1]) * r);
    c.clip("evenodd");
    c.drawImage(this.ink, 0, 0, off.width, off.height);
    c.restore();
    this.res = r;
    for (const l of [this.bg, this.ink, this.text]) {
      l.width = off.width;
      l.height = off.height;
    }
    this.renderBackground();
    this.renderText(null);
    this.ictx.setTransform(1, 0, 0, 1, 0, 0);
    this.ictx.drawImage(off, 0, 0);
    const v = job.rect;
    if (v[2] - v[0] < this.width || v[3] - v[1] < this.height) this.queueBands(v);
    const done = this.afterSwap;
    this.afterSwap = null;
    done?.();
  }

  private scheduleBands() {
    if (this.bandTimer !== null) return;
    this.bandTimer = window.setTimeout(() => {
      this.bandTimer = null;
      const t0 = performance.now();
      const deadline = t0 + SLICE_MS;
      do {
        if (!this.job) {
          const b = this.pendingDetail ? null : this.bands.shift();
          if (this.pendingDetail) {
            this.job = this.startDetail(this.pendingDetail);
            this.pendingDetail = null;
          } else if (b) this.job = this.startJob(this.ictx, this.res, b, null);
          else break;
          if (!this.job) continue;
        }
        const job = this.job;
        if (this.stepJob(job, deadline)) {
          this.job = null;
          if (job.detail) this.finishDetail(job);
          else if (job.swap) this.finishRescale(job);
        }
      } while (performance.now() < deadline);
      this.lastLongestSliceMs = Math.max(this.lastLongestSliceMs, performance.now() - t0);
      if (this.job || this.pendingDetail || this.bands.length) this.scheduleBands();
      else this.resolveIdle();
    }, 0);
  }

  private cancelBands() {
    if (this.bandTimer !== null) clearTimeout(this.bandTimer);
    this.bandTimer = null;
    this.bands = [];
    this.job = null;
    this.pendingDetail = null;
    this.afterSwap = null;
  }

  /** The ink changed under a running job, whose snapshot is now out of date. */
  private restartJob() {
    const j = this.job;
    if (!j) return;
    j.items = this.itemsIn(j.rect);
    j.next = 0;
    j.tail = [];
    j.cleared = false;
  }

  private resolveIdle() {
    const w = this.idleWaiters;
    this.idleWaiters = [];
    w.forEach((f) => f());
  }

  /** Resolves once every band of a progressive redraw has been painted. */
  whenComplete(): Promise<void> {
    if (!this.bands.length && !this.job && !this.pendingDetail && this.bandTimer === null) return Promise.resolve();
    return new Promise((r) => this.idleWaiters.push(r));
  }

  renderBackground() {
    const c = this.bctx;
    c.setTransform(1, 0, 0, 1, 0, 0);
    c.fillStyle = "#ffffff";
    c.fillRect(0, 0, this.bg.width, this.bg.height);
    if (this.pdfBitmap) {
      c.imageSmoothingQuality = "high";
      const b = this.pdfBox;
      const [ox, oy] = this.org;
      const r = this.bg.width / this.width;
      if (b) c.drawImage(this.pdfBitmap, (b[0] - ox) * r, (b[1] - oy) * r, (b[2] - b[0]) * r, (b[3] - b[1]) * r);
      else c.drawImage(this.pdfBitmap, 0, 0, this.bg.width, this.bg.height);
    }
    // the paper and the PDF on it are opaque, so the whole layer can be inverted at once
    if (this.opts.dark) invertRect(c, 0, 0, this.bg.width, this.bg.height);
    this.prep(c);
    const look = this.opts.mono ? "mono" : this.opts.dark ? "dark" : "light";
    if (this.canvas) drawCanvasTemplate(c, this.template, this.extent, this.res, look, this.pageWidth);
    else drawTemplate(c, this.template, this.width, this.height, look);
  }

  private clip(ctx: CanvasRenderingContext2D, rect: Rect | null, res = this.res, clear = true, org: [number, number] = this.org) {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    if (!rect) {
      ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
      this.prep(ctx);
      return;
    }
    const pad = 2;
    const x0 = Math.max(0, Math.floor((rect[0] - org[0]) * res) - pad);
    const y0 = Math.max(0, Math.floor((rect[1] - org[1]) * res) - pad);
    const x1 = Math.min(ctx.canvas.width, Math.ceil((rect[2] - org[0]) * res) + pad);
    const y1 = Math.min(ctx.canvas.height, Math.ceil((rect[3] - org[1]) * res) + pad);
    ctx.save();
    ctx.beginPath();
    ctx.rect(x0, y0, x1 - x0, y1 - y0);
    ctx.clip();
    if (clear) ctx.clearRect(x0, y0, x1 - x0, y1 - y0);
    this.prep(ctx, res, org);
  }

  /** Stroke `s` as this page shows it: the ePaper highlighter, or dark theme colours. */
  strokeStyleFor(s: InkStroke): InkStroke {
    if (this.opts.mono && s.tool === "highlighter") return { ...s, color: "#9a9a9a", opacity: 0.45 };
    const color = shownColor(s.color, !!this.opts.dark);
    return color === s.color ? s : { ...s, color };
  }

  /** The colour this page shows text `t` in, or undefined for its own. */
  textColorFor(t: TextBlock): string | undefined {
    if (this.opts.mono) return t.color.toLowerCase() !== "#000000" ? "#000000" : undefined;
    const color = shownColor(t.color, !!this.opts.dark);
    return color === t.color ? undefined : color;
  }

  /** Redraws ink inside `rect` (doc pt) or everything when rect is null. */
  renderInk(rect: Rect | null) {
    if (!rect) this.cancelBands();
    else this.restartJob();
    const t0 = performance.now();
    const ctx = this.ictx;
    this.clip(ctx, rect);
    if (this.underlay && (!rect || rectsIntersect(rect, elementBounds(this.underlay)))) this.drawItem(ctx, this.underlay);
    for (const e of this.elements()) {
      if (e.type === "text" || this.hidden.has(e.id)) continue;
      if (rect && !rectsIntersect(rect, elementBounds(e))) continue;
      this.drawItem(ctx, e);
    }
    if (rect) ctx.restore();
    if (!rect) {
      ctx.getImageData(0, 0, 1, 1); // GPU flush, as in the benchmark protocol's timing method
      this.lastFullRedrawMs = performance.now() - t0;
      this.resolveIdle();
    }
    this.renderDetailInk(rect);
  }

  renderText(rect: Rect | null) {
    const ctx = this.tctx;
    this.clip(ctx, rect);
    for (const e of this.elements()) {
      if (e.type !== "text" || this.hidden.has(e.id)) continue;
      if (rect && !rectsIntersect(rect, textBounds(e))) continue;
      drawText(ctx, e, this.textColorFor(e));
    }
    if (rect) ctx.restore();
    this.renderDetailText(rect);
  }

  /** Appends one stroke on top (valid when it has the highest z). */
  appendStroke(s: InkStroke) {
    this.prep(this.ictx);
    drawStroke(this.ictx, this.strokeStyleFor(s));
    this.job?.tail.push(s);
    const d = this.detail;
    if (d && rectsIntersect(strokeBounds(s), d.rect)) {
      this.prep(this.dictx, d.res, [d.rect[0], d.rect[1]]);
      drawStroke(this.dictx, this.strokeStyleFor(s));
    }
  }

  private drawImageEl(ctx: CanvasRenderingContext2D, e: Extract<Element, { type: "image" }>) {
    if (e.pdfPage) {
      const pdf = this.pdfPages.get(pdfPageKey(e.pdfPage));
      // a PDF page is paper: white until its bitmap is ready, never a dashed hole
      ctx.save();
      ctx.fillStyle = "#ffffff";
      ctx.fillRect(e.x, e.y, e.width, e.height);
      if (pdf) {
        ctx.imageSmoothingQuality = "high";
        ctx.drawImage(pdf.canvas, e.x, e.y, e.width, e.height);
      }
      // the white fill makes the rect opaque, so only the PDF page is inverted
      if (this.opts.dark) invertRect(ctx, e.x, e.y, e.width, e.height);
      ctx.strokeStyle = this.opts.dark ? "#3f3f46" : "#d4d4d8";
      ctx.lineWidth = 0.75;
      ctx.strokeRect(e.x, e.y, e.width, e.height);
      ctx.restore();
      return;
    }
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

  /**
   * Decodes the images a page uses that aren't loaded yet. Returns true if any were added,
   * meaning the ink layer needs a redraw. An unreadable image keeps its placeholder.
   */
  async loadImages(elements: Element[], read: (hash: string) => Promise<Uint8Array>): Promise<boolean> {
    let added = false;
    for (const e of elements) {
      if (e.type !== "image" || e.pdfPage || this.images.has(e.assetHash)) continue;
      try {
        const bytes = await read(e.assetHash);
        this.images.set(e.assetHash, await createImageBitmap(new Blob([bytes as BlobPart])));
        added = true;
      } catch (err) {
        console.warn("image", e.assetHash, err);
      }
    }
    return added;
  }

  /** Small PNG of the current page for the sidebar / library. */
  async thumbnail(maxWidth = 240): Promise<Uint8Array | null> {
    await this.whenComplete();
    const c = this.snapshot(maxWidth);
    const blob: Blob | null = await new Promise((r) => c.toBlob(r, "image/png"));
    return blob ? new Uint8Array(await blob.arrayBuffer()) : null;
  }

  /**
   * The page layers flattened into one canvas `width` pixels wide (the layers' own width
   * if omitted), as drawn so far. Call `whenComplete` first for a finished picture.
   */
  snapshot(width = this.bg.width): HTMLCanvasElement {
    const scale = width / this.width;
    const c = document.createElement("canvas");
    c.width = Math.max(1, Math.round(this.width * scale));
    c.height = Math.max(1, Math.round(this.height * scale));
    const x = c.getContext("2d")!;
    x.imageSmoothingQuality = "high";
    for (const layer of [this.bg, this.ink, this.text]) x.drawImage(layer, 0, 0, c.width, c.height);
    return c;
  }
}
