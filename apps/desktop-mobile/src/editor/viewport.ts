// Viewport math. Screen (CSS px, relative to the viewport element) =
// doc (pt) * scale + offset. Shared by the web canvas and native adapters
// (Android BooxInkView uses viewPx = (doc * scale + offset) * dpr).

export interface ViewTransform {
  scale: number;
  ox: number;
  oy: number;
}

export const MIN_SCALE = 0.2;
export const MAX_SCALE = 6;

export const clampScale = (s: number) => Math.min(MAX_SCALE, Math.max(MIN_SCALE, s));

export function toDoc(t: ViewTransform, x: number, y: number): [number, number] {
  return [(x - t.ox) / t.scale, (y - t.oy) / t.scale];
}

export function toScreen(t: ViewTransform, x: number, y: number): [number, number] {
  return [x * t.scale + t.ox, y * t.scale + t.oy];
}

/** Zoom to `scale` keeping the document point under (fx, fy) fixed. */
export function zoomAbout(t: ViewTransform, scale: number, fx: number, fy: number): ViewTransform {
  const s = clampScale(scale);
  const [dx, dy] = toDoc(t, fx, fy);
  return { scale: s, ox: fx - dx * s, oy: fy - dy * s };
}

/** Fits the page width (with margins) and aligns the top. */
export function fitWidth(viewW: number, viewH: number, pageW: number, pageH: number, margin = 24): ViewTransform {
  const sw = (viewW - margin * 2) / pageW;
  const scale = clampScale(Math.min(sw, 3));
  const ox = (viewW - pageW * scale) / 2;
  const oy = pageH * scale + margin * 2 <= viewH ? (viewH - pageH * scale) / 2 : margin;
  return { scale, ox, oy };
}

export function fitPage(viewW: number, viewH: number, pageW: number, pageH: number, margin = 24): ViewTransform {
  const scale = clampScale(Math.min((viewW - margin * 2) / pageW, (viewH - margin * 2) / pageH));
  return { scale, ox: (viewW - pageW * scale) / 2, oy: (viewH - pageH * scale) / 2 };
}

/** Keeps at least `keep` CSS px of the page visible so it cannot be lost off-screen. */
export function constrain(t: ViewTransform, viewW: number, viewH: number, pageW: number, pageH: number, keep = 80): ViewTransform {
  const w = pageW * t.scale, h = pageH * t.scale;
  const ox = Math.min(viewW - keep, Math.max(keep - w, t.ox));
  const oy = Math.min(viewH - keep, Math.max(keep - h, t.oy));
  return { scale: t.scale, ox, oy };
}

/**
 * Backing-store scale (device px per document pt) for a page-sized canvas,
 * capped by pixel area (WebKit on iOS refuses canvases > ~16.7 Mpx).
 */
export function backingScale(scale: number, dpr: number, pageW: number, pageH: number, maxPixels: number): number {
  const want = scale * dpr;
  const cap = Math.sqrt(maxPixels / (pageW * pageH));
  return Math.max(0.25, Math.min(want, cap));
}
