// PDF.js page backgrounds. The original PDF bytes come from the notebook's
// immutable asset store; pages are rendered at the page canvas resolution and
// re-rendered when the zoom settles. All PDF.js data files are bundled
// (public/pdfjs) so nothing is fetched from the network.
import * as pdfjs from "pdfjs-dist";
import type { PDFDocumentProxy, RenderTask } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import type { PdfBackgroundRef } from "../generated/PdfBackgroundRef";

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

const base = () => new URL("pdfjs/", document.baseURI).href;

export class PdfCache {
  private docs = new Map<string, Promise<PDFDocumentProxy>>();
  private order: string[] = [];

  constructor(
    private load: (hash: string) => Promise<Uint8Array>,
    private max = 3,
  ) {}

  get(hash: string): Promise<PDFDocumentProxy> {
    let p = this.docs.get(hash);
    if (!p) {
      p = this.load(hash).then(
        (data) =>
          pdfjs.getDocument({
            data,
            cMapUrl: `${base()}cmaps/`,
            cMapPacked: true,
            standardFontDataUrl: `${base()}standard_fonts/`,
            wasmUrl: `${base()}wasm/`,
            iccUrl: `${base()}iccs/`,
            enableXfa: false,
          }).promise,
      );
      this.docs.set(hash, p);
      this.order.push(hash);
      while (this.order.length > this.max) {
        const old = this.order.shift()!;
        this.docs.get(old)?.then((d) => d.loadingTask.destroy()).catch(() => {});
        this.docs.delete(old);
      }
      p.catch(() => this.docs.delete(hash));
    }
    return p;
  }

  destroy() {
    for (const p of this.docs.values()) p.then((d) => d.loadingTask.destroy()).catch(() => {});
    this.docs.clear();
    this.order = [];
  }
}

export class PdfPageRenderer {
  private task: RenderTask | null = null;
  private seq = 0;

  /**
   * Renders the background page at `res` device px per pt. Resolves with a
   * canvas (or null if superseded by a newer request).
   */
  async render(cache: PdfCache, bg: PdfBackgroundRef, res: number): Promise<HTMLCanvasElement | null> {
    const seq = ++this.seq;
    this.task?.cancel();
    const doc = await cache.get(bg.assetHash);
    if (seq !== this.seq) return null;
    const page = await doc.getPage(bg.pageIndex + 1);
    if (seq !== this.seq) return null;
    // PDF.js' default rotation is the page's /Rotate and its view box is the
    // crop box ∩ media box: the same display space Penroam stores (see
    // PdfBackgroundRef::doc_to_pdf_matrix for the export direction).
    const viewport = page.getViewport({ scale: res });
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(viewport.width));
    canvas.height = Math.max(1, Math.round(viewport.height));
    this.task = page.render({ canvas, viewport });
    try {
      await this.task.promise;
    } catch (e: any) {
      if (e?.name === "RenderingCancelledException") return null;
      throw e;
    } finally {
      this.task = null;
    }
    return seq === this.seq ? canvas : null;
  }

  cancel() {
    this.seq++;
    this.task?.cancel();
  }
}
