// Continuous scrolling: the notebook's pages laid out in one column around the open page.
//
// Only the open page is live. It has an editing session and the editor's canvases. The
// pages around it are pictures (a rendered canvas, or the sidebar thumbnail until that
// arrives) placed in the open page's coordinate space, so one CSS transform pans and zooms
// all of them together with the live page.
//
// Coordinates are page points with the open page's top-left corner at (0, 0). Pages above
// it have negative y. Every page is centred on the open page's vertical axis, and pages are
// `PAGE_GAP` points apart. A page whose size is not known yet takes the open page's size,
// so the layout may shift once it loads. Because the layout is anchored on the open page,
// only pages between the open page and the one loading can move.
//
// The editor (engine.ts) decides when another page becomes the open one. This module only
// lays the pages out, keeps their elements and pictures, and answers where a point falls.
import type { Rect } from "./geometry";

/** Space between pages (page points). */
export const PAGE_GAP = 20;

/** How many pictures to keep, nearest to the open page first. The rest are dropped to bound memory. */
const KEEP_IMAGES = 6;

/** A page of the notebook, in reading order, as far as the strip knows it. */
export interface StripPage {
  id: string;
  /** Page size in points, if loaded; otherwise the open page's size is assumed. */
  width?: number;
  height?: number;
  /** Image URL shown until a rendered picture arrives (the sidebar thumbnail). */
  placeholder?: string | null;
}

/** A page's place in the open page's coordinates (page points). */
export interface Placed {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Lays `pages` out top to bottom around `currentId`, which goes at (0, 0) with the given
 * size. Returns one entry per page, in the same order. If `currentId` is not among `pages`
 * the result holds only the open page.
 */
export function layoutStrip(pages: StripPage[], currentId: string, curW: number, curH: number, gap = PAGE_GAP): Placed[] {
  const at = pages.findIndex((p) => p.id === currentId);
  const self: Placed = { id: currentId, x: 0, y: 0, width: curW, height: curH };
  if (at < 0) return [self];
  const place = (p: StripPage, y: number, above: boolean): Placed => {
    const width = p.width ?? curW, height = p.height ?? curH;
    return { id: p.id, x: (curW - width) / 2, y: above ? y - height : y, width, height };
  };
  const out: Placed[] = new Array(pages.length);
  out[at] = self;
  let y = -gap;
  for (let i = at - 1; i >= 0; i--) {
    out[i] = place(pages[i], y, true);
    y = out[i].y - gap;
  }
  y = curH + gap;
  for (let i = at + 1; i < pages.length; i++) {
    out[i] = place(pages[i], y, false);
    y += out[i].height + gap;
  }
  return out;
}

/**
 * Index of the page that owns the height `y`. The gap between two pages is split halfway,
 * and heights past either end belong to the first or last page.
 */
export function pageAtY(placed: Placed[], y: number): number {
  let lo = 0, hi = placed.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    const next = placed[mid + 1];
    const split = (placed[mid].y + placed[mid].height + next.y) / 2;
    if (y < split) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

/** The DOM side of the strip: one element per page near the view, behind the live page. */
export class PageStrip {
  /** Holds the pictures. The editor gives it the same transform as the live page. */
  readonly el: HTMLDivElement;
  placed: Placed[] = [];
  private pages: StripPage[] = [];
  private currentId: string | null = null;
  private slots = new Map<string, HTMLDivElement>();
  /** Rendered pictures of the other pages. */
  private images = new Map<string, HTMLCanvasElement>();

  constructor() {
    this.el = document.createElement("div");
    this.el.className = "penroam-strip absolute left-0 top-0 origin-top-left";
    this.el.style.pointerEvents = "none";
  }

  /** Replaces the page list (order or known sizes changed). Call `relayout` afterwards. */
  setPages(pages: StripPage[]) {
    this.pages = pages;
    const ids = new Set(pages.map((p) => p.id));
    for (const id of [...this.images.keys()]) if (!ids.has(id)) this.images.delete(id);
  }

  /** Recomputes positions around the open page. */
  relayout(currentId: string, width: number, height: number) {
    this.currentId = currentId;
    this.placed = layoutStrip(this.pages, currentId, width, height);
    for (const [id, slot] of this.slots) {
      const p = this.placed.find((q) => q.id === id);
      if (p && id !== currentId) this.position(slot, p);
    }
  }

  /** The area all pages cover (page points). */
  bounds(): Rect {
    const p = this.placed;
    if (!p.length) return [0, 0, 0, 0];
    let x0 = Infinity, x1 = -Infinity;
    for (const q of p) {
      x0 = Math.min(x0, q.x);
      x1 = Math.max(x1, q.x + q.width);
    }
    return [x0, p[0].y, x1, p[p.length - 1].y + p[p.length - 1].height];
  }

  /** The page that owns the height `y` (see `pageAtY`), or null if there are no pages. */
  pageNearY(y: number): Placed | null {
    return this.placed.length ? this.placed[pageAtY(this.placed, y)] : null;
  }

  /** The page under a point (page points), or null in a gap or beside the pages. */
  pageAt(x: number, y: number): Placed | null {
    if (!this.placed.length) return null;
    const p = this.placed[pageAtY(this.placed, y)];
    return x >= p.x && x <= p.x + p.width && y >= p.y && y <= p.y + p.height ? p : null;
  }

  /**
   * Shows the pages that intersect `visible` (page points), except the open page, and
   * removes the rest. Returns the pages that intersect `near`, nearest to the open page
   * first. Those are the ones worth having pictures of.
   */
  update(visible: Rect, near: Rect): Placed[] {
    const hits = (p: Placed, r: Rect) => p.y + p.height >= r[1] && p.y <= r[3];
    const show = new Set<string>();
    for (const p of this.placed) {
      if (p.id === this.currentId || !hits(p, visible)) continue;
      show.add(p.id);
      let slot = this.slots.get(p.id);
      if (!slot) {
        slot = document.createElement("div");
        slot.className = "penroam-strip-page absolute";
        this.slots.set(p.id, slot);
        this.el.appendChild(slot);
        this.fill(slot, p.id);
      }
      this.position(slot, p);
    }
    for (const [id, slot] of this.slots) {
      if (show.has(id)) continue;
      slot.remove();
      this.slots.delete(id);
    }
    const at = this.placed.findIndex((p) => p.id === this.currentId);
    const wanted = this.placed.filter((p) => p.id !== this.currentId && hits(p, near));
    wanted.sort((a, b) => Math.abs(this.placed.indexOf(a) - at) - Math.abs(this.placed.indexOf(b) - at));
    this.trimImages(at);
    return wanted;
  }

  /** Width in pixels of the picture held for a page, or 0 if there is none. */
  imageWidth(id: string): number {
    return this.images.get(id)?.width ?? 0;
  }

  /** Uses `canvas` as the page's picture. */
  setImage(id: string, canvas: HTMLCanvasElement) {
    canvas.className = "absolute inset-0 h-full w-full";
    this.images.set(id, canvas);
    const slot = this.slots.get(id);
    if (slot) this.fill(slot, id);
  }

  /** Forgets a page's picture (its content changed). The placeholder shows until a new one arrives. */
  dropImage(id: string) {
    this.images.delete(id);
    const slot = this.slots.get(id);
    if (slot) this.fill(slot, id);
  }

  /** Forgets every page's picture (pages now look different); placeholders show until new ones arrive. */
  dropImages() {
    this.images.clear();
    for (const [id, slot] of this.slots) this.fill(slot, id);
  }

  /** Removes every page element; pictures are kept. */
  hide() {
    for (const slot of this.slots.values()) slot.remove();
    this.slots.clear();
  }

  private position(slot: HTMLDivElement, p: Placed) {
    slot.style.left = `${p.x}px`;
    slot.style.top = `${p.y}px`;
    slot.style.width = `${p.width}px`;
    slot.style.height = `${p.height}px`;
  }

  private fill(slot: HTMLDivElement, id: string) {
    const img = this.images.get(id);
    if (img) {
      slot.replaceChildren(img);
      return;
    }
    // the saved thumbnail, in its own element so the dark theme can invert it like the page
    slot.replaceChildren();
    const url = this.pages.find((p) => p.id === id)?.placeholder;
    if (!url) return;
    const ph = document.createElement("div");
    ph.className = "penroam-strip-placeholder absolute inset-0";
    ph.style.backgroundImage = `url("${url}")`;
    slot.appendChild(ph);
  }

  /** Keeps the pictures of the pages nearest the open page (and any on screen). */
  private trimImages(at: number) {
    if (this.images.size <= KEEP_IMAGES) return;
    const index = new Map(this.placed.map((p, i) => [p.id, i]));
    const byDistance = [...this.images.keys()].sort((a, b) => Math.abs((index.get(a) ?? 1e9) - at) - Math.abs((index.get(b) ?? 1e9) - at));
    for (const id of byDistance.slice(KEEP_IMAGES)) {
      if (this.slots.has(id)) continue;
      this.images.delete(id);
    }
  }
}
