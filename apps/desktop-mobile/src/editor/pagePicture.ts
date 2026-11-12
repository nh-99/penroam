// Pictures of whole pages, drawn off screen: thumbnails, library covers, history previews,
// exports and the neighbouring pages of continuous scrolling.
//
// A picture is drawn by a throwaway `PageRenderer` on a detached element, so it needs no editor
// and works for any notebook, open or not. The caller supplies what the picture depends on
// besides the page itself (the notebook, its PDF cache, the colours), which keeps this usable
// from the editor and from the library screen alike.
import type { Page } from "../generated/Page";
import { api } from "../platform/ipc";
import { PdfCache, PdfPageRenderer, renderPdfPage } from "./pdf";
import { canvasSheetRect, PageRenderer, pdfPageKey } from "./renderer";

/** Most pixels in a picture of an infinite canvas, which can be very tall. */
const MAX_PICTURE_PIXELS = 16_000_000;
/** Longest side of such a picture; WebKit refuses larger canvases. */
const MAX_PICTURE_SIDE = 16_384;

/** What a page's picture depends on besides the page. */
export interface PictureContext {
  /** The notebook the page is in; its assets (images, PDFs) are read from there. */
  notebookId: string;
  /** The notebook shows infinite canvases rather than sheets. */
  infiniteCanvas: boolean;
  /** PDFs of the notebook, for page backgrounds and PDF pages placed on a canvas. */
  pdfCache: PdfCache | null;
  /** Draw in the e-paper theme's black and white. */
  mono: boolean;
  /** Draw the page the way the dark theme shows it. Saved pictures never are. */
  dark: boolean;
  /** PDF pages already drawn elsewhere (the open editor's), reused when sharp enough. */
  pdfPages?: Map<string, { canvas: CanvasImageSource; res: number }>;
}

/**
 * Draws a page into a canvas `width` pixels wide. On an infinite canvas the picture is of the
 * drawing, cut the way exports cut it (or, with `paper`, at least a page in the page's shape,
 * for previews), and may come out narrower than `width` when the drawing is too long for a
 * canvas that wide.
 */
export async function drawPageCanvas(page: Page, width: number, ctx: PictureContext, opts: { paper?: boolean } = {}): Promise<HTMLCanvasElement> {
  const host = document.createElement("div");
  const r = new PageRenderer(host, () => page.elements);
  const canvas = ctx.infiniteCanvas;
  const [x0, y0, x1, y1] = canvas ? canvasSheetRect(page, opts.paper) : [0, 0, page.size.width, page.size.height];
  const w = x1 - x0, h = y1 - y0;
  if (canvas) {
    width = Math.min(width, Math.sqrt(MAX_PICTURE_PIXELS * (w / h)), (MAX_PICTURE_SIDE * w) / h);
    r.setPage(w, h, page.template, [x0, y0], true, page.size.width);
    if (page.background) r.pdfBox = [0, 0, page.size.width, page.size.height];
  } else r.setPage(w, h, page.template);
  r.opts.mono = ctx.mono;
  r.opts.dark = ctx.dark;
  const res = canvas ? width / w : Math.min(Math.max(2, width / w), (2 * width) / w);
  r.setResolution(res);
  const pdfCache = ctx.pdfCache;
  if (page.background && pdfCache) {
    r.pdfBitmap = await new PdfPageRenderer().render(pdfCache, page.background, res).catch(() => null);
  }
  if (pdfCache) {
    for (const e of page.elements) {
      const key = e.type === "image" && e.pdfPage ? pdfPageKey(e.pdfPage) : null;
      if (!key || e.type !== "image" || !e.pdfPage || r.pdfPages.has(key)) continue;
      // a bitmap of the page drawn elsewhere, when it is sharp enough, saves rendering it again
      const drawn = ctx.pdfPages?.get(key);
      if (drawn && drawn.res >= res) {
        r.pdfPages.set(key, drawn);
        continue;
      }
      const bitmap = await renderPdfPage(pdfCache, e.pdfPage, res).catch(() => null);
      if (bitmap) r.pdfPages.set(key, { canvas: bitmap, res });
    }
  }
  await r.loadImages(page.elements, (hash) => api.readAsset(ctx.notebookId, hash));
  r.renderAll(null);
  await r.whenComplete();
  return r.snapshot(Math.round(width));
}

/** Like `drawPageCanvas`, encoded as a PNG; `null` if the browser could not encode it. */
export async function drawPagePng(page: Page, width: number, ctx: PictureContext, opts: { paper?: boolean } = {}): Promise<Uint8Array | null> {
  const c = await drawPageCanvas(page, width, ctx, opts);
  const blob: Blob | null = await new Promise((r) => c.toBlob(r, "image/png"));
  return blob ? new Uint8Array(await blob.arrayBuffer()) : null;
}
