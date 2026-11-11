// Penroam page editor engine (framework-free). Vue renders the chrome and
// passes tool/settings in; per-sample input and rendering never go through
// Vue reactivity or IPC.
//
//   input samples   -> this.active (outside any reactive state)
//   live preview    -> screen-space `active` (stable pieces) + `tail` (provisional piece, predicted samples)
//   committed model -> PageSession (Rust is authoritative; ops persisted in order)
//   rendered cache  -> PageRenderer page-sized layers, CSS-transformed for pan/zoom
//   other pages     -> PageStrip pictures around the open page (continuous scrolling)
//
// Infinite canvas (`setCanvas`): the page has no edges. The renderer's layers then cover an
// extent around the content and the view instead of the page, and the extent follows the view
// when it settles: it grows in `CANVAS_CHUNK_PT` steps while that stays within the pixel
// budget, and is otherwise a window around the view, so memory stays bounded however far the
// canvas reaches. Panning is free; "fit" fits the content. PDF pages sit on a canvas as image
// elements with `pdfPage`, rendered sharp near the view and coarse elsewhere (`syncPdfPages`).
// Ruled paper makes the canvas a column (`column`): the page's width with its header and margin
// line, infinite only downward. The extent then spans the column's width and grows down, the
// view is kept on the column, and text is placed and sized as on a sheet.
import type { Element } from "../generated/Element";
import type { InkPoint } from "../generated/InkPoint";
import type { InkStroke } from "../generated/InkStroke";
import type { ImageElement } from "../generated/ImageElement";
import type { PdfBackgroundRef } from "../generated/PdfBackgroundRef";
import type { Template } from "../generated/Template";
import type { TextBlock } from "../generated/TextBlock";
import type { TextFont } from "../generated/TextFont";
import type { NativeInkSurface, NativeTool } from "../platform/nativeInk";
import {
  drawPieces,
  drawStroke,
  hitTest,
  hitTestSegment,
  isUniform,
  lassoHitsStroke,
  pieceCount,
  pointInPolygon,
  rectsIntersect,
  stablePieceCount,
  strokeBounds,
  unionRect,
  type Rect,
} from "./geometry";
import { newId } from "./ids";
import { classifyPointer, PALM_CONTACT_PX, PENCIL_CURVE, penErases, pressureArrived, pressureCapable, samplePoint, type PenOnlyMode } from "./input";
import { PdfCache, PdfPageRenderer, renderPdfPage } from "./pdf";
import { drawText, elementBounds, linkAt, PageRenderer, paintOf, pdfPageKey, shownColor, taskAt, textBounds, type Ink, type Text } from "./renderer";
import type { PageSession, SaveState } from "./session";
import { rowsFor, toggledTask, type TaskBox } from "./markdown";
import { MarkdownEditor } from "./markdownEditor";
import type { FormatAction } from "./markdownFormat";
import { onMathReady } from "./math";
import { alignment, SNAP_DISTANCE_PX, type Guide, type TextEdges } from "./snap";
import { isCanvasColumn, rowPitchOf, snapToRow } from "./templates";
import { ascentOf, contentHeight, descentOf, firstBaseline, fontFamilyOf, lineHeight, metrics } from "./textLayout";
import { PageStrip, type StripPage } from "./pageStrip";
import { backingScale, clampScale, constrain, constrainTo, fitPage, fitWidth, toDoc, zoomAbout, type ViewTransform } from "./viewport";

/** The font all the text blocks among `sel` share, or null when they differ or there are none. */
export function sharedTextFont(sel: Element[]): TextFont | null {
  let font: TextFont | null = null;
  for (const e of sel) {
    if (e.type !== "text") continue;
    const f = e.font ?? "formatted";
    if (font !== null && font !== f) return null;
    font = f;
  }
  return font;
}

export type Tool = "pen" | "highlighter" | "eraser" | "select" | "lasso" | "text" | "hand";
export const DRAW_TOOLS: Tool[] = ["pen", "highlighter", "eraser"];

export interface ToolSettings {
  penColor: string;
  penWidth: number;
  highlighterColor: string;
  highlighterWidth: number;
  textColor: string;
  textSize: number;
  /** Font of new text. */
  textFont: TextFont;
  /** New text goes anywhere on ruled and dotted paper instead of locking to its rows. */
  textFreePlacement: boolean;
  pressure: boolean;
}

export interface EditorSettings {
  penOnly: PenOnlyMode;
  mono: boolean;
  /** Dark theme: the page is shown inverted (see `RenderOptions.dark`). */
  dark: boolean;
  /** max backing pixels per page layer */
  maxCanvasPixels: number;
  useNativeInk: boolean;
  /** Touches wider than this (CSS px) count as palms; `Infinity` turns size filtering off (iOS). */
  palmContactPx: number;
  /** Pens are Apple Pencils, whose force needs the Pencil curve (iOS). */
  pencilCurve: boolean;
  /** Use the detail layer for any zoom past fit width, because the page layers blur there too (iOS). */
  detailWhenZoomed: boolean;
}

export interface EditorState {
  tool: Tool;
  scale: number;
  canUndo: boolean;
  canRedo: boolean;
  selectionCount: number;
  selectionHasText: boolean;
  /** Every selected text block is placed freely (as opposed to locked to rows). */
  selectionTextFree: boolean;
  /** The font every selected text block shares; null when they differ or none is selected. */
  selectionTextFont: TextFont | null;
  /** The current page's paper has rows that text can lock to. */
  pageHasRows: boolean;
  editingText: boolean;
  /** Formatting commands that are on at the caret of the text box being edited. */
  textFormats: FormatAction[];
  saveState: SaveState;
  saveError: string | null;
  nativeActive: boolean;
  penSeen: boolean;
}

export interface EditorCallbacks {
  onState(s: Partial<EditorState>): void;
  onError(message: string): void;
  /** Debounced: the page content changed (thumbnail refresh). */
  onPageEdited(): void;
  /** Bytes of a notebook attachment, for images on the page. */
  readAsset?(hash: string): Promise<Uint8Array>;
  /** A link in a text block was tapped: open `url` outside the app. */
  openLink?(url: string): void;
  /**
   * Continuous scrolling: another page is now in the middle of the view, or was pressed.
   * Open it with `setSession(…, { keepView: true })` so that it stays where it is on screen.
   */
  onScrollToPage?(pageId: string): void;
  /**
   * Continuous scrolling: pages near the view have no picture, or one too coarse for the
   * zoom. Each wants a canvas `width` pixels wide passed to `setPageImage`. A new call
   * replaces the previous list, nearest page first.
   */
  wantPageImages?(wants: { id: string; width: number }[]): void;
}

export const ERASER_RADIUS_PX = 8;
const SELECT_TOLERANCE_PX = 8;
const HIGHLIGHTER_OPACITY = 0.35;
/** A finger stroke younger than this was the start of a pinch when a second finger lands. */
const PINCH_GRACE_MS = 250;
/** Contacts this soon after the pen lifts are the writing hand, not a deliberate gesture. */
const PEN_LIFT_GRACE_MS = 400;
/** A press that moves less than this (screen pixels) before it lifts is a tap. */
const TAP_SLOP_PX = 8;
/** How far (screen pixels) outside a task's small checkbox a tap still reaches it. */
const TASK_TAP_PAD_PX = 6;
/** Infinite canvas: how far past the view, as a fraction of its size, the layers reach. */
const CANVAS_VIEW_PAD = 0.5;
/** Infinite canvas: extents snap to this many points, so small pans don't redraw. */
const CANVAS_CHUNK_PT = 512;
/** Space between content and PDF pages placed below it, before rounding up to a row. */
const CANVAS_PLACE_GAP_PT = 48;
/** Widest a text block may be made (the model's longest page side). */
const MAX_TEXT_WIDTH_PT = 14_400;
/** PDF pages away from the view are kept at this resolution (device px per pt). */
const PDF_FAR_RES = 0.5;
/** Sharpest a PDF page on a canvas is rendered (device px per pt); the detail layer stretches it past that. */
const PDF_MAX_RES = 3;

type PdfImage = ImageElement & { type: "image"; pdfPage: PdfBackgroundRef };
const isPdfPage = (e: Element): e is PdfImage => e.type === "image" && !!e.pdfPage;

/** The size a PDF page shows at: its crop box, turned by its rotation. */
export function pdfDisplaySize(p: PdfBackgroundRef): { width: number; height: number } {
  const [x0, y0, x1, y1] = p.cropBox;
  const w = Math.abs(x1 - x0), h = Math.abs(y1 - y0);
  return p.rotation % 180 === 90 ? { width: h, height: w } : { width: w, height: h };
}

const containsRect = (o: Rect, i: Rect) => o[0] <= i[0] && o[1] <= i[1] && o[2] >= i[2] && o[3] >= i[3];
const rectArea = (r: Rect) => (r[2] - r[0]) * (r[3] - r[1]);
const snapOut = (r: Rect, c: number): Rect => [Math.floor(r[0] / c) * c, Math.floor(r[1] / c) * c, Math.ceil(r[2] / c) * c, Math.ceil(r[3] / c) * c];

/** A task checkbox under a page point, with the text its position was worked out from. */
interface TaskHit {
  id: string;
  text: string;
  box: TaskBox;
}

/** A press on a task's checkbox; it ticks the box if the press lifts as a tap. */
interface TaskTap extends TaskHit {
  pointerId: number;
  x: number;
  y: number;
  /** With the select tool nothing starts until the press moves; this is where to start it from. */
  selectFrom?: PointerEvent;
}

interface ActiveStroke {
  stroke: Ink;
  pointerId: number;
  t0: number;
  stableDrawn: number;
  coalesced: number;
  predicted: InkPoint[];
}

interface EraseGesture {
  pointerId: number;
  last: [number, number];
  erased: Map<string, Element>;
  dirty: Rect | null;
}

type SelectDrag =
  | { kind: "move"; pointerId: number; start: [number, number]; dx: number; dy: number; moved: boolean; guides: Guide[]; tapEdits?: boolean }
  | { kind: "resize"; pointerId: number; start: [number, number]; orig: TextBlock; width: number; height: number }
  | { kind: "band"; pointerId: number; start: [number, number]; end: [number, number] }
  | { kind: "lasso"; pointerId: number; points: [number, number][] };

interface TextEdit {
  id: string;
  isNew: boolean; // not yet committed to the session
  committed: Text | null; // last committed version
  draft: Text;
  timer: number | null;
  composing: boolean;
}

export class InkEditor {
  readonly world: HTMLDivElement;
  readonly renderer: PageRenderer;
  session: PageSession | null = null;
  transform: ViewTransform = { scale: 1, ox: 0, oy: 0 };
  tool: Tool = "pen";
  tools: ToolSettings = {
    penColor: "#111111",
    penWidth: 2.2,
    highlighterColor: "#facc15",
    highlighterWidth: 14,
    textColor: "#111111",
    textSize: 16,
    textFont: "handwritten",
    textFreePlacement: false,
    pressure: true,
  };
  settings: EditorSettings = { penOnly: "auto", mono: false, dark: false, maxCanvasPixels: 16_000_000, useNativeInk: true, palmContactPx: PALM_CONTACT_PX, pencilCurve: false, detailWhenZoomed: false };
  selection = new Set<string>();
  penSeen = false;

  private overlay: HTMLDivElement;
  private selBox: HTMLDivElement;
  private selHandle: HTMLDivElement;
  /** Holds the text box editor, positioned over the block; the editor exists during an edit. */
  private textHost: HTMLDivElement;
  private textEditor: MarkdownEditor | null = null;
  /** The strip around the box being edited. Dragging it moves the box. */
  private editFrame: HTMLDivElement;
  private activeCv: HTMLCanvasElement;
  private tailCv: HTMLCanvasElement;
  private actx: CanvasRenderingContext2D;
  private tctx: CanvasRenderingContext2D;
  private dpr = 1;
  private viewW = 0;
  private viewH = 0;
  private rect: DOMRect | null = null;
  private active: ActiveStroke | null = null;
  private erase: EraseGesture | null = null;
  private drag: SelectDrag | null = null;
  /** The scale when a platform pinch began. */
  private pinchStart: number | null = null;
  private edit: TextEdit | null = null;
  private touches = new Map<number, { x: number; y: number }>();
  private pinch: { dist: number; scale: number; ax: number; ay: number } | null = null;
  private pan: { pointerId: number; x: number; y: number } | null = null;
  /** A navigating press that started on a link; it opens if the press lifts as a tap. */
  private linkTap: { pointerId: number; x: number; y: number; href: string } | null = null;
  private taskTap: TaskTap | null = null;
  /** The finger behind the stroke, erase or drag in progress, in viewport coordinates. */
  private fingerDraw: { pointerId: number; x: number; y: number } | null = null;
  private penDown = false;
  private penLiftedAt = -Infinity;
  /** The tool before the last change, for the Pencil's "switch to previous tool". */
  private previousTool: Tool = "pen";
  private hoverShown = false;
  private dprQuery: MediaQueryList | null = null;
  private onDprChange = () => this.watchDpr();
  private spaceDown = false;
  private frameRequested = false;
  private settleTimer: number | null = null;
  private editedTimer: number | null = null;
  private pdfCache: PdfCache | null = null;
  private pdf = new PdfPageRenderer();
  private background: PdfBackgroundRef | null = null;
  private ro: ResizeObserver;
  private disposers: (() => void)[] = [];
  private sampleEvent: "pointerrawupdate" | "pointermove";
  private hasCoalesced: boolean;
  private hasPredicted: boolean;
  // native ink surface
  private native: NativeInkSurface | null = null;
  private nativeMounted = false;
  private nativeVisible = false;
  private nativeEcho = false;
  private overlayBlocked = false;
  private nativeQueue: Promise<unknown> = Promise.resolve();
  // continuous scrolling
  private strip = new PageStrip();
  /** Continuous scrolling is on: the setting, and the page isn't an infinite canvas. */
  private continuous = false;
  private continuousSetting = false;
  // infinite canvas
  private canvas = false;
  /** Whether the open page was laid out as a column, to notice its paper changing to or from ruled. */
  private wasColumn = false;
  /** PDF page bitmaps waiting to be rendered, nearest the view first. */
  private pdfQueue: { key: string; ref: PdfBackgroundRef; res: number }[] = [];
  private pdfBusy = false;
  /** The page last handed to `onScrollToPage`, until a session opens. */
  private scrollRequested: string | null = null;

  constructor(
    private viewport: HTMLElement,
    private cb: EditorCallbacks,
  ) {
    viewport.style.touchAction = "none";
    viewport.style.userSelect = "none";
    (viewport.style as any).webkitUserSelect = "none";
    viewport.style.overflow = "hidden";
    if (getComputedStyle(viewport).position === "static") viewport.style.position = "relative";

    this.world = document.createElement("div");
    this.world.className = "penroam-page absolute left-0 top-0 origin-top-left";
    viewport.appendChild(this.world);
    viewport.insertBefore(this.strip.el, this.world);
    this.renderer = new PageRenderer(this.world, () => this.session?.page.elements ?? [], viewport);

    this.overlay = document.createElement("div");
    this.overlay.className = "absolute inset-0";
    this.overlay.style.pointerEvents = "none";
    this.world.appendChild(this.overlay);

    this.selBox = document.createElement("div");
    this.selBox.className = "penroam-selection absolute hidden";
    this.overlay.appendChild(this.selBox);
    this.selHandle = document.createElement("div");
    this.selHandle.className = "penroam-handle absolute hidden";
    this.overlay.appendChild(this.selHandle);

    // before the text editor, so the editor stays on top of the strip's middle
    this.editFrame = document.createElement("div");
    this.editFrame.className = "penroam-editframe absolute hidden";
    this.editFrame.title = "Drag to move";
    this.editFrame.style.pointerEvents = "auto";
    this.overlay.appendChild(this.editFrame);
    this.textHost = document.createElement("div");
    this.textHost.className = "penroam-textedit absolute hidden";
    this.textHost.style.pointerEvents = "auto";
    this.overlay.appendChild(this.textHost);

    const mkScreen = () => {
      const c = document.createElement("canvas");
      c.className = "absolute inset-0 h-full w-full";
      c.style.pointerEvents = "none";
      viewport.appendChild(c);
      return c;
    };
    this.activeCv = mkScreen();
    this.tailCv = mkScreen();
    const desync = { desynchronized: true } as CanvasRenderingContext2DSettings;
    this.actx = this.activeCv.getContext("2d", desync)!;
    this.tctx = this.tailCv.getContext("2d", desync)!;

    const pe = typeof PointerEvent !== "undefined";
    this.sampleEvent = typeof window !== "undefined" && "onpointerrawupdate" in window ? "pointerrawupdate" : "pointermove";
    this.hasCoalesced = pe && typeof PointerEvent.prototype.getCoalescedEvents === "function";
    this.hasPredicted = pe && typeof (PointerEvent.prototype as any).getPredictedEvents === "function";

    const on = (target: EventTarget, type: string, fn: (e: any) => void, opts?: AddEventListenerOptions) => {
      target.addEventListener(type, fn, opts);
      this.disposers.push(() => target.removeEventListener(type, fn, opts));
    };
    // formulas show their source until MathJax has loaded, and then change size
    this.disposers.push(
      onMathReady(() => {
        this.renderer.renderText(null);
        this.updateSelectionBox();
      }),
    );
    on(viewport, "pointerdown", (e: PointerEvent) => this.onDown(e));
    on(viewport, "pointermove", (e: PointerEvent) => this.onMove(e));
    if (this.sampleEvent === "pointerrawupdate") on(viewport, "pointerrawupdate", (e: PointerEvent) => this.onMove(e));
    on(viewport, "pointerup", (e: PointerEvent) => this.onUp(e, false));
    on(viewport, "pointercancel", (e: PointerEvent) => this.onUp(e, true));
    on(viewport, "pointerleave", () => {
      this.clearHover();
      this.showLinkHover(null, false);
    });
    on(viewport, "lostpointercapture", (e: PointerEvent) => this.onLostCapture(e));
    on(viewport, "wheel", (e: WheelEvent) => this.onWheel(e), { passive: false });
    on(viewport, "contextmenu", (e: Event) => e.preventDefault());
    on(viewport, "dblclick", (e: MouseEvent) => this.onDoubleClick(e));
    on(this.textHost, "compositionstart", () => this.edit && (this.edit.composing = true));
    on(this.textHost, "compositionend", () => {
      if (this.edit) this.edit.composing = false;
      if (this.textEditor) this.onTextInput(this.textEditor.view.state.doc.toString());
    });
    on(this.textHost, "keydown", (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        this.endTextEdit();
      }
      e.stopPropagation(); // typing never triggers editor shortcuts
    });
    on(this.textHost, "focusout", (e: FocusEvent) => {
      if (this.textHost.contains(e.relatedTarget as Node | null)) return;
      // A control removed while it had focus (a closed menu, a redrawn button) takes focus with
      // it, which is not leaving the box: give the text the focus back instead.
      if (e.target instanceof Node && !e.target.isConnected && this.textHost.isConnected) {
        queueMicrotask(() => {
          if (this.edit && (document.activeElement === document.body || document.activeElement === null)) this.textEditor?.focus();
        });
        return;
      }
      this.endTextEdit();
    });
    on(window, "keydown", (e: KeyboardEvent) => {
      if (e.code === "Space" && !this.edit && !(e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement)) this.spaceDown = true;
    });
    on(window, "keyup", (e: KeyboardEvent) => {
      if (e.code === "Space") this.spaceDown = false;
    });
    on(window, "blur", () => this.cancelGestures());
    on(document, "visibilitychange", () => {
      if (document.visibilityState === "hidden") {
        this.cancelGestures();
        this.endTextEdit();
      }
    });

    this.ro = new ResizeObserver(() => this.resize());
    this.ro.observe(viewport);
    this.resize();
    this.watchDpr();
  }

  /**
   * Dragging the window to a display with another scale keeps its CSS size, so the resize
   * observer stays silent. `resolution` queries match one exact value, so this re-arms each time.
   */
  private watchDpr() {
    this.dprQuery?.removeEventListener("change", this.onDprChange);
    this.dprQuery = null;
    if (typeof matchMedia === "undefined") return;
    if ((window.devicePixelRatio || 1) !== this.dpr) this.resize();
    this.dprQuery = matchMedia(`(resolution: ${this.dpr}dppx)`);
    this.dprQuery.addEventListener?.("change", this.onDprChange);
  }

  dispose() {
    this.endTextEdit();
    this.ro.disconnect();
    this.dprQuery?.removeEventListener?.("change", this.onDprChange);
    this.disposers.forEach((d) => d());
    this.pdf.cancel();
    if (this.settleTimer) clearTimeout(this.settleTimer);
    if (this.editedTimer) clearTimeout(this.editedTimer);
    void this.detachNative();
    this.viewport.replaceChildren();
  }

  private emit(s: Partial<EditorState>) {
    this.cb.onState(s);
  }

  private emitAll() {
    const sel = [...this.selection].map((id) => this.session?.elementById(id)).filter(Boolean) as Element[];
    this.emit({
      tool: this.tool,
      scale: this.transform.scale,
      canUndo: !!this.session?.canUndo,
      canRedo: !!this.session?.canRedo,
      selectionCount: sel.length,
      selectionHasText: sel.some((e) => e.type === "text"),
      selectionTextFree: sel.every((e) => e.type !== "text" || e.rowPitch === undefined),
      selectionTextFont: sharedTextFont(sel),
      pageHasRows: this.pageHasRows(),
      editingText: !!this.edit,
      textFormats: this.textEditor?.formats ?? [],
      saveState: this.session?.saveState ?? "saved",
      saveError: this.session?.lastError ?? null,
      nativeActive: this.nativeVisible,
      penSeen: this.penSeen,
    });
  }

  // ============================================================ page ==

  /**
   * Opens a page session. The previous session must have been flushed by the caller.
   * The view fits the page's width, unless `keepView` is set in continuous scrolling: then
   * the page stays where it was in the column, so scrolling onto it doesn't jump.
   */
  async setSession(session: PageSession, pdfCache: PdfCache | null, opts: { keepView?: boolean } = {}) {
    this.endTextEdit();
    this.cancelGestures();
    this.clearSelection();
    this.clearMatches();
    const prev = this.session;
    let kept: ViewTransform | null = null;
    if (this.continuous && prev && prev.page.id !== session.page.id) {
      // the page being left becomes a picture, drawn as it is now, so it never shows blank
      this.strip.setImage(prev.page.id, this.renderer.snapshot(Math.min(this.renderer.bg.width, this.pictureWidth(this.renderer.width, this.renderer.height))));
      const to = this.strip.placed.find((p) => p.id === session.page.id);
      const { scale, ox, oy } = this.transform;
      if (opts.keepView && to) kept = { scale, ox: ox + to.x * scale, oy: oy + to.y * scale };
    }
    this.scrollRequested = null;
    this.session = session;
    // PDF page bitmaps belong to the notebook the cache reads from
    if (pdfCache !== this.pdfCache) {
      this.renderer.pdfPages.clear();
      this.pdfQueue = [];
    }
    this.pdfCache = pdfCache;
    session.events = {
      onElementsChanged: (c) => this.onElementsChanged(c),
      onSaveState: (s, err) => this.emit({ saveState: s, saveError: err ?? null }),
      onHistory: (u, r) => this.emit({ canUndo: u, canRedo: r }),
      onPageReplaced: () => this.pageReplaced(),
    };
    const p = session.page;
    if (this.canvas) {
      // a first extent around the content; `growExtent` adds the view once the view is known
      const c = this.onColumn(snapOut(this.contentRect() ?? [0, 0, p.size.width, p.size.height], CANVAS_CHUNK_PT));
      this.renderer.setPage(c[2] - c[0], c[3] - c[1], p.template, [c[0], c[1]], true, p.size.width);
      // the page's PDF is drawn like the PDF pages placed on the canvas, so it stays sharp
      if (p.background) this.renderer.underlay = { type: "image", id: "page-background", z: -Infinity, x: 0, y: 0, ...p.size, assetHash: p.background.assetHash, pdfPage: p.background };
    } else this.renderer.setPage(p.size.width, p.size.height, p.template);
    this.renderer.opts.mono = this.settings.mono;
    this.renderer.opts.dark = this.settings.dark;
    this.wasColumn = this.column;
    // a column is a page on the desk; a free canvas is paper everywhere
    this.viewport.classList.toggle("penroam-canvas-view", this.unbounded);
    this.world.style.width = `${p.size.width}px`;
    this.world.style.height = `${p.size.height}px`;
    // the background layer's PDF bitmap is for sheets
    this.background = this.canvas ? null : (p.background ?? null);
    this.relayoutStrip();
    this.transform = kept ? this.constrained(kept) : this.fitTransform("width");
    this.applyTransform(false);
    if (this.canvas) this.growExtent();
    this.renderer.setResolution(this.wantedRes());
    this.renderer.renderAll(this.visibleDocRect());
    this.emitAll();
    if (this.native) {
      if (this.nativeMounted) this.nativeCall((n) => n.loadCommittedInk(this.inkElements()));
      this.syncNativeTransform();
    }
    this.updateNative();
    void this.loadImages();
    this.syncPdfPages();
    if (this.background) await this.renderPdf();
  }

  private async loadImages() {
    const s = this.session;
    if (!s || !this.cb.readAsset) return;
    const read = (hash: string) => this.cb.readAsset!(hash);
    if ((await this.renderer.loadImages(s.page.elements, read)) && this.session === s) {
      this.renderer.renderInkProgressive(this.visibleDocRect());
      this.cb.onPageEdited();
    }
  }

  /** True while a stroke, erase, drag or text edit is in progress. */
  isBusy(): boolean {
    return !!(this.active || this.erase || this.drag || this.edit);
  }

  /** The session's page was replaced (remote changes merged): redraw, keep the view. */
  private pageReplaced() {
    const s = this.session;
    if (!s) return;
    for (const id of [...this.selection]) if (!s.elementById(id)) this.selection.delete(id);
    this.renderer.template = s.page.template;
    const content = this.canvas ? this.contentRect() : null;
    // includeInExtent draws everything again when it moves the layers
    if (!content || !this.includeInExtent(content)) this.renderer.renderAll(this.visibleDocRect());
    void this.loadImages();
    this.syncPdfPages();
    this.scheduleSettle(); // the redraw replaced any change of resolution in progress
    if (this.native?.ownsCommittedInk && this.nativeMounted) this.nativeCall((n) => n.loadCommittedInk(this.inkElements()));
    if (this.selection.size) this.updateSelectionBox();
    this.emitAll();
    this.cb.onPageEdited();
  }

  /** The page's ink for the native layer, in the colours it is shown in. */
  private inkElements(): InkStroke[] {
    return (this.session?.page.elements ?? []).filter((e): e is Ink => e.type === "ink").map((e) => this.nativeShown(e));
  }

  /**
   * Stroke `s` with its colour swapped between stored and shown (`shownColor` is its own
   * inverse), both ways across the native layer: it draws what it is given and hands back
   * strokes in the colours it drew them in.
   */
  private nativeShown<T extends { color: string }>(s: T): T {
    const color = shownColor(s.color, this.settings.dark);
    return color === s.color ? s : { ...s, color };
  }

  private async renderPdf() {
    const bg = this.background;
    if (!bg || !this.pdfCache) return;
    try {
      const canvas = await this.pdf.render(this.pdfCache, bg, this.renderer.res);
      if (canvas && this.background === bg) {
        this.renderer.pdfBitmap = canvas;
        this.renderer.renderBackground();
      }
    } catch (e) {
      this.cb.onError(`The PDF page could not be rendered: ${(e as Error).message ?? e}`);
    }
  }

  setMono(mono: boolean) {
    this.settings.mono = mono;
    this.renderer.opts.mono = mono;
    this.redrawLook();
  }

  /** Shows the page inverted for the dark theme, or as it is. */
  setDark(dark: boolean) {
    this.settings.dark = dark;
    this.renderer.opts.dark = dark;
    this.redrawLook();
    // the native layer draws ink in the colours it was given
    if (this.native && this.nativeMounted) {
      if (this.native.ownsCommittedInk) this.nativeCall((n) => n.loadCommittedInk(this.inkElements()));
      if (this.nativeVisible) this.nativeCall((n) => n.setTool(this.nativeTool()));
    }
  }

  /** Draws everything again after a change in how pages look. */
  private redrawLook() {
    // the other pages' pictures were drawn the old way
    this.strip.dropImages();
    if (this.session) {
      this.renderer.renderAll();
      this.scheduleSettle();
    }
  }

  setSettings(s: Partial<EditorSettings>) {
    const monoChanged = s.mono !== undefined && s.mono !== this.settings.mono;
    const darkChanged = s.dark !== undefined && s.dark !== this.settings.dark;
    this.settings = { ...this.settings, ...s };
    if (monoChanged) this.setMono(this.settings.mono);
    if (darkChanged) this.setDark(this.settings.dark);
    this.updateNative();
  }

  setTemplate(t: Template) {
    const s = this.session;
    if (!s) return;
    const prev = s.page.template;
    if (prev.kind === t.kind && prev.spacing === t.spacing) return;
    // Locked text follows the paper's rows. Paper without rows leaves it where it is.
    const pitch = rowPitchOf(t);
    const before = s.page.elements.filter((e): e is Text => e.type === "text" && e.rowPitch !== undefined);
    const after = pitch === undefined ? [] : before.map((e) => this.relockedText(e, t, pitch));
    const blocks = after.length > 0;
    s.commit({
      label: "Change template",
      forward: [{ op: "setTemplate", template: t }, ...(blocks ? [{ op: "replaceElements" as const, elements: after }] : [])],
      inverse: [...(blocks ? [{ op: "replaceElements" as const, elements: before }] : []), { op: "setTemplate", template: prev }],
    });
  }

  private pageHasRows(): boolean {
    const t = this.session?.page.template;
    return !!t && rowPitchOf(t) !== undefined;
  }

  /** `e` locked to `pitch` on paper `t`, or freed when `pitch` is undefined. */
  private relockedText(e: Text, t: Template, pitch: number | undefined): Text {
    const n: Text = { ...e, rowPitch: pitch };
    if (pitch !== undefined) n.y = snapToRow(e.y, t, pitch, Math.round, this.unbounded);
    n.height = this.textHeight(n);
    return n;
  }

  /** The block's laid-out height. */
  private textHeight(t: TextBlock): number {
    return paintOf(t).height;
  }

  // ======================================================= viewport ==

  private resize() {
    const r = this.viewport.getBoundingClientRect();
    this.dpr = window.devicePixelRatio || 1;
    const first = this.viewW === 0;
    this.viewW = r.width;
    this.viewH = r.height;
    for (const c of [this.activeCv, this.tailCv]) {
      c.width = Math.max(1, Math.round(r.width * this.dpr));
      c.height = Math.max(1, Math.round(r.height * this.dpr));
    }
    if (this.session && first) {
      this.transform = this.fitTransform("width");
      this.applyTransform(false);
    } else if (this.session) {
      this.transform = this.constrained(this.transform);
      this.applyTransform(false);
    }
    if (this.native && this.nativeMounted) this.nativeCall((n) => n.setBounds(this.surfaceRect()));
  }

  private fitTransform(mode: "width" | "page"): ViewTransform {
    const f = mode === "width" ? fitWidth : fitPage;
    const margin = this.viewW < 600 ? 8 : 24;
    if (this.canvas) return this.fitCanvas(mode, margin);
    const t = f(Math.max(1, this.viewW), Math.max(1, this.viewH), this.renderer.width, this.renderer.height, margin);
    // a short page centred in the column would leave the page before it half on screen
    if (this.continuous && mode === "width") t.oy = margin;
    return t;
  }

  /**
   * Infinite canvas: fits the content (and the page's PDF background) at no more than 100 %,
   * so a small drawing isn't blown up. An empty canvas shows its origin at 100 %.
   */
  private fitCanvas(mode: "width" | "page", margin: number): ViewTransform {
    const p = this.session?.page;
    if (this.column && p) {
      // like a sheet: the column's width, or its first page, from the top
      const [x0, y0, x1] = this.columnBounds();
      const t = (mode === "width" ? fitWidth : fitPage)(Math.max(1, this.viewW), Math.max(1, this.viewH), x1 - x0, p.size.height, margin);
      return { scale: t.scale, ox: t.ox - x0 * t.scale, oy: (mode === "width" ? margin : t.oy) - y0 * t.scale };
    }
    const r = this.contentRect();
    if (!r) return { scale: 1, ox: margin, oy: margin };
    const w = Math.max(1, r[2] - r[0]), h = Math.max(1, r[3] - r[1]);
    const fit = (mode === "width" ? fitWidth : fitPage)(Math.max(1, this.viewW), Math.max(1, this.viewH), w, h, margin);
    const scale = Math.min(1, fit.scale);
    const ox = (this.viewW - w * scale) / 2;
    const oy = mode === "page" || h * scale + margin * 2 <= this.viewH ? (this.viewH - h * scale) / 2 : margin;
    return { scale, ox: ox - r[0] * scale, oy: oy - r[1] * scale };
  }

  /** The bounds of everything on the page, with its PDF background; null when there is none. */
  private contentRect(): Rect | null {
    const p = this.session?.page;
    if (!p) return null;
    let r: Rect | null = p.background ? [0, 0, p.size.width, p.size.height] : null;
    for (const e of p.elements) r = unionRect(r, elementBounds(e));
    return r;
  }

  /** True while the open page is an infinite canvas of ruled paper, which is a column (`isCanvasColumn`). */
  private get column(): boolean {
    const t = this.session?.page.template;
    return this.canvas && !!t && isCanvasColumn(t);
  }

  /** True when text and handles go anywhere: a canvas that isn't a column. */
  private get unbounded(): boolean {
    return this.canvas && !this.column;
  }

  /**
   * Column: the doc rect it spans, with the bottom at the end of its content (or of the page).
   * That is the page's width from the origin, widened for any content beside it, such as
   * drawing made before the paper was ruled.
   */
  private columnBounds(): Rect {
    const p = this.session!.page;
    const c = this.contentRect();
    return [Math.min(0, c?.[0] ?? 0), Math.min(0, c?.[1] ?? 0), Math.max(p.size.width, c?.[2] ?? 0), Math.max(p.size.height, c?.[3] ?? 0)];
  }

  /** Column: `r` made as wide as the column and kept from going above its top. Other canvases: `r`. */
  private onColumn(r: Rect): Rect {
    if (!this.column) return r;
    const [x0, y0, x1] = this.columnBounds();
    const top = Math.max(y0, r[1]);
    return [x0, top, x1, Math.max(top + 1, r[3])];
  }

  /** Infinite canvas: the most doc area the layers may cover, given the pixel budget and the lowest backing scale. */
  private get maxExtentArea(): number {
    return this.settings.maxCanvasPixels / (0.25 * 0.25);
  }

  /**
   * Infinite canvas: makes the layers cover the view with a margin around it. Returns true
   * when they moved, which leaves them blank: the caller redraws. The extent takes in all the
   * content when that fits the budget, so panning around a drawing doesn't redraw it.
   */
  private growExtent(): boolean {
    if (!this.canvas || !this.session || !this.viewW) return false;
    const vis = this.visibleDocRect();
    const pad = (k: number): Rect => {
      const px = (vis[2] - vis[0]) * k, py = (vis[3] - vis[1]) * k;
      return [vis[0] - px, vis[1] - py, vis[2] + px, vis[3] + py];
    };
    let want = this.onColumn(pad(CANVAS_VIEW_PAD));
    const max = this.maxExtentArea;
    if (rectArea(want) > max) want = this.onColumn(vis); // zoomed far out: the view itself is all that fits
    const cur = this.renderer.extent;
    if (containsRect(cur, want)) return false;
    const content = this.contentRect();
    const candidates = [unionRect(unionRect(content, want), cur), unionRect(content, want), want];
    const next = candidates.map((r) => this.onColumn(snapOut(r, CANVAS_CHUNK_PT))).find((r) => rectArea(r) <= max) ?? this.onColumn(snapOut(want, CANVAS_CHUNK_PT));
    this.renderer.setExtent([next[0], next[1]], next[2] - next[0], next[3] - next[1]);
    return true;
  }

  /** Infinite canvas: after the layers moved, draws them again at the resolution that suits them. */
  private redrawExtent() {
    this.renderer.setResolution(this.wantedRes());
    this.renderer.renderAll(this.visibleDocRect());
    if (this.background) void this.renderPdf();
    this.syncDetail();
  }

  /** Infinite canvas: makes sure `r` (doc pt) is inside the layers, as after adding content beyond them. */
  private includeInExtent(r: Rect): boolean {
    const cur = this.renderer.extent;
    if (!this.canvas || containsRect(cur, r)) return false;
    const next = this.onColumn(snapOut(unionRect(cur, r), CANVAS_CHUNK_PT));
    // too far to take in: it is off screen, and the layers move there when the view does
    if (rectArea(next) > this.maxExtentArea) return false;
    this.renderer.setExtent([next[0], next[1]], next[2] - next[0], next[3] - next[1]);
    this.redrawExtent();
    return true;
  }

  /** Keeps the page, or in continuous scrolling the column of pages, from leaving the view. */
  private constrained(t: ViewTransform): ViewTransform {
    // a column reaches a page's length past its content; a free canvas has nowhere to get lost from
    if (this.column) {
      const b = this.columnBounds();
      return constrainTo(t, this.viewW, this.viewH, [b[0], b[1], b[2], b[3] + this.session!.page.size.height]);
    }
    if (this.canvas) return t;
    if (this.continuous) return constrainTo(t, this.viewW, this.viewH, this.strip.bounds());
    return constrain(t, this.viewW, this.viewH, this.renderer.width, this.renderer.height);
  }

  /** Fits the page's width or the whole page; on a free infinite canvas, always everything on it. */
  fit(mode: "width" | "page" = "width") {
    this.setTransform(this.fitTransform(this.unbounded ? "page" : mode));
  }

  zoomBy(factor: number) {
    this.setTransform(zoomAbout(this.transform, this.transform.scale * factor, this.viewW / 2, this.viewH / 2));
  }

  /**
   * A pinch the platform reports itself instead of as pointer or wheel input.
   * `scale` is relative to the start of the gesture and (`x`, `y`) is in client
   * coordinates; `phase` "end" just stops following it.
   */
  platformPinch(phase: "begin" | "update" | "end", scale: number, x: number, y: number) {
    if (phase === "begin") this.pinchStart = this.transform.scale;
    const start = this.pinchStart;
    if (phase === "end") this.pinchStart = null;
    if (phase === "end" || start === null || this.active || this.erase || this.drag) return;
    const r = this.viewport.getBoundingClientRect();
    this.setTransform(zoomAbout(this.transform, start * scale, x - r.left, y - r.top));
  }

  setTransform(t: ViewTransform, fromNative = false) {
    this.transform = this.constrained({ ...t, scale: clampScale(t.scale) });
    this.applyTransform(!fromNative);
  }

  private applyTransform(notifyNative: boolean) {
    const { scale, ox, oy } = this.transform;
    const css = `translate(${ox}px, ${oy}px) scale(${scale})`;
    this.world.style.transform = css;
    this.world.style.setProperty("--inv", String(1 / scale));
    this.renderer.setView(this.transform);
    this.emit({ scale });
    if (notifyNative) this.syncNativeTransform();
    if (this.continuous) {
      this.strip.el.style.transform = css;
      this.followScroll();
    }
    this.scheduleSettle();
  }

  // ========================================== continuous scrolling ==

  /**
   * Turns continuous scrolling on or off. On, the other pages of `setPageList` show above
   * and below the open page, and scrolling onto one opens it. Off, only the open page shows.
   */
  setContinuous(on: boolean) {
    this.continuousSetting = on;
    this.applyContinuous();
  }

  /** Infinite canvases show one page at a time whatever the setting. */
  private applyContinuous() {
    const on = this.continuousSetting && !this.canvas;
    if (on === this.continuous) return;
    this.continuous = on;
    this.scrollRequested = null;
    if (!on) this.strip.hide();
    this.strip.el.style.display = on ? "" : "none";
    if (!this.session) return;
    this.relayoutStrip();
    this.transform = this.constrained(this.transform);
    this.applyTransform(true);
  }

  /** True while continuous scrolling is on. */
  get isContinuous(): boolean {
    return this.continuous;
  }

  /**
   * Shows pages as infinite canvases, or as sheets. An open page is shown again the new way,
   * with the view fitted to it.
   */
  setCanvas(on: boolean) {
    if (on === this.canvas) return;
    this.canvas = on;
    this.world.classList.toggle("penroam-canvas", on);
    this.viewport.classList.toggle("penroam-canvas-view", on);
    this.applyContinuous();
    if (this.session) void this.setSession(this.session, this.pdfCache);
  }

  /** True while pages show as infinite canvases. */
  get isCanvas(): boolean {
    return this.canvas;
  }

  /**
   * Places PDF pages on the open canvas below its content, one under the other, as a single
   * undoable change, and scrolls to the first. They go underneath everything already there,
   * like paper.
   */
  placePdfPages(pages: PdfBackgroundRef[]) {
    const s = this.session;
    if (!s || !pages.length) return;
    this.endTextEdit();
    this.cancelGestures();
    const sp = s.page.template.spacing;
    const row = (v: number) => Math.ceil(v / sp) * sp;
    const content = this.contentRect();
    // on a column the pages go at its left edge, like the sheets they come from
    const x = this.column ? this.columnBounds()[0] : content ? Math.floor(content[0] / sp) * sp : 0;
    let y = content ? row(content[3] + CANVAS_PLACE_GAP_PT) : 0;
    const els = s.page.elements;
    const floor = (els.length ? els[0].z : 0) - pages.length;
    const placed: Element[] = pages.map((ref, i) => {
      const { width, height } = pdfDisplaySize(ref);
      const e: Element = { type: "image", id: newId(), z: floor + i, x, y, width, height, assetHash: ref.assetHash, pdfPage: ref };
      y = row(y + height + CANVAS_PLACE_GAP_PT);
      return e;
    });
    const label = pages.length === 1 ? "Import PDF page" : "Import PDF";
    s.commit({ label, forward: [{ op: "addElements", elements: placed }], inverse: [{ op: "removeElements", ids: placed.map((e) => e.id) }] });
    const first = placed[0] as PdfImage;
    const scale = Math.min(this.transform.scale, Math.max(0.2, (this.viewW - 48) / first.width));
    const margin = this.viewW < 600 ? 8 : 24;
    this.setTransform({ scale, ox: (this.viewW - first.width * scale) / 2 - first.x * scale, oy: margin - first.y * scale });
  }

  /**
   * Renders the bitmaps of the page's PDF page elements: sharp near the view, coarse away from
   * it, so that a long document on a canvas doesn't hold every page at full resolution.
   */
  private syncPdfPages() {
    const s = this.session;
    const cache = this.pdfCache;
    if (!s || !cache) return;
    const pdfs = s.page.elements.filter(isPdfPage);
    const under = this.renderer.underlay;
    if (under && isPdfPage(under)) pdfs.push(under);
    const have = this.renderer.pdfPages;
    if (!pdfs.length) {
      have.clear();
      this.pdfQueue = [];
      return;
    }
    const vis = this.visibleDocRect();
    const w = vis[2] - vis[0], h = vis[3] - vis[1];
    const near: Rect = [vis[0] - w, vis[1] - h, vis[2] + w, vis[3] + h];
    const sharp = Math.min(PDF_MAX_RES, Math.max(PDF_FAR_RES, this.transform.scale * this.dpr));
    const cx = (vis[0] + vis[2]) / 2, cy = (vis[1] + vis[3]) / 2;
    const wants = new Map<string, { ref: PdfBackgroundRef; res: number; dist: number }>();
    for (const e of pdfs) {
      const key = pdfPageKey(e.pdfPage);
      const b = elementBounds(e);
      const res = rectsIntersect(near, b) ? sharp : PDF_FAR_RES;
      const dist = Math.hypot(Math.max(b[0] - cx, cx - b[2], 0), Math.max(b[1] - cy, cy - b[3], 0));
      const w0 = wants.get(key);
      if (!w0 || res > w0.res || dist < w0.dist) wants.set(key, { ref: e.pdfPage, res: Math.max(res, w0?.res ?? 0), dist: Math.min(dist, w0?.dist ?? Infinity) });
    }
    for (const key of [...have.keys()]) if (!wants.has(key)) have.delete(key);
    this.pdfQueue = [...wants]
      .filter(([key, want]) => {
        const got = have.get(key);
        // too soft for where it is, or far sharper than it needs to be away from the view
        return !got || got.res < want.res * 0.8 || got.res > want.res * 2;
      })
      .sort((a, b) => a[1].dist - b[1].dist)
      .map(([key, want]) => ({ key, ref: want.ref, res: want.res }));
    void this.drainPdfPages();
  }

  private async drainPdfPages() {
    if (this.pdfBusy) return;
    this.pdfBusy = true;
    try {
      for (let job = this.pdfQueue.shift(); job; job = this.pdfQueue.shift()) {
        const s = this.session;
        const cache = this.pdfCache;
        if (!s || !cache) break;
        try {
          const canvas = await renderPdfPage(cache, job.ref, job.res);
          if (this.session !== s || this.pdfCache !== cache) continue;
          this.renderer.pdfPages.set(job.key, { canvas, res: job.res });
          let r: Rect | null = null;
          const under = this.renderer.underlay;
          for (const e of under ? [under, ...s.page.elements] : s.page.elements) if (isPdfPage(e) && pdfPageKey(e.pdfPage) === job.key) r = unionRect(r, elementBounds(e));
          if (r) this.renderer.renderInk(r);
        } catch (e) {
          console.warn("PDF page", job.key, e);
        }
      }
    } finally {
      this.pdfBusy = false;
    }
  }

  /** The notebook's pages in reading order, for continuous scrolling. */
  setPageList(pages: StripPage[]) {
    this.strip.setPages(pages);
    if (!this.session) return;
    this.relayoutStrip();
    if (!this.continuous) return;
    this.transform = this.constrained(this.transform);
    this.applyTransform(true);
  }

  /** A rendered picture of another page (see `wantPageImages`). */
  setPageImage(id: string, canvas: HTMLCanvasElement) {
    if (id !== this.session?.page.id) this.strip.setImage(id, canvas);
  }

  /** Forgets the picture of a page whose content changed elsewhere; a new one is asked for. */
  dropPageImage(id: string) {
    this.strip.dropImage(id);
    if (this.continuous) this.scheduleSettle();
  }

  private relayoutStrip() {
    const s = this.session;
    if (s) this.strip.relayout(s.page.id, this.renderer.width, this.renderer.height);
  }

  /** Shows the pages on screen and opens the one in the middle of the view. */
  private followScroll() {
    const s = this.session;
    if (!s || !this.viewH) return;
    const vis = this.visibleDocRect();
    // half a screen of margin so that pages are already there when scrolled onto
    const h = (vis[3] - vis[1]) / 2;
    this.strip.update([vis[0], vis[1] - h, vis[2], vis[3] + h], vis);
    if (this.isBusy()) return;
    const mid = this.strip.pageNearY((vis[1] + vis[3]) / 2);
    if (!mid) return;
    if (mid.id !== s.page.id) this.requestPage(mid.id);
    else if (this.scrollRequested !== null) {
      // back on the open page before the one asked for opened: stay here instead
      this.scrollRequested = null;
      this.cb.onScrollToPage?.(s.page.id);
    }
  }

  /**
   * Pixel width for a picture of a `w` × `h` page at the current zoom. Pictures get a
   * quarter of a page layer's pixel budget, since several are kept at once.
   */
  private pictureWidth(w: number, h: number): number {
    return Math.round(w * backingScale(this.transform.scale, this.dpr, w, h, this.settings.maxCanvasPixels / 4));
  }

  private requestPage(id: string) {
    if (id === this.scrollRequested) return;
    this.scrollRequested = id;
    this.cb.onScrollToPage?.(id);
  }

  /** Asks the host for pictures of the pages near the view that lack a sharp enough one. */
  private requestPageImages() {
    if (!this.continuous || !this.session || !this.cb.wantPageImages) return;
    const vis = this.visibleDocRect();
    const h = vis[3] - vis[1];
    const wants: { id: string; width: number }[] = [];
    for (const p of this.strip.update([vis[0], vis[1] - h / 2, vis[2], vis[3] + h / 2], [vis[0], vis[1] - h, vis[2], vis[3] + h])) {
      const width = this.pictureWidth(p.width, p.height);
      // a coarser picture is fine while zooming out; ask again only once it looks soft
      if (this.strip.imageWidth(p.id) < width * 0.7) wants.push({ id: p.id, width });
    }
    this.cb.wantPageImages(wants);
  }

  private wantedRes() {
    return backingScale(this.transform.scale, this.dpr, this.renderer.width, this.renderer.height, this.settings.maxCanvasPixels);
  }

  private scheduleSettle() {
    if (this.settleTimer) clearTimeout(this.settleTimer);
    this.settleTimer = window.setTimeout(() => {
      this.settleTimer = null;
      this.syncPdfPages();
      if (this.growExtent()) return this.redrawExtent();
      const want = this.wantedRes();
      const cur = this.renderer.targetRes;
      this.requestPageImages();
      if (Math.abs(want - cur) / cur > 0.08) {
        this.renderer.rescale(want, this.visibleDocRect(), () => {
          if (this.background) void this.renderPdf();
          this.syncDetail();
        });
      } else this.syncDetail();
    }, 160);
  }

  /** Past what the page layers can hold, the visible area gets its own sharp layer. */
  private syncDetail() {
    const want = this.transform.scale * this.dpr;
    const zoomedIn = this.settings.detailWhenZoomed && this.transform.scale > this.fitTransform("width").scale * 1.1;
    if (!zoomedIn && want <= this.renderer.targetRes * 1.08) return this.renderer.hideDetail();
    this.renderer.showDetail(this.visibleDocRect(), want, this.settings.maxCanvasPixels, this.dpr);
  }

  // ======================================================= search ==

  private matches: HTMLDivElement[] = [];

  /**
   * Outlines search matches (page points) and pans so that they are on screen. The outlines
   * are DOM boxes inside the page element, so they follow pan and zoom; the ink itself is
   * untouched. They go away on the next pointer down or page change.
   */
  showMatches(rects: { x: number; y: number; w: number; h: number }[]) {
    this.clearMatches();
    if (!rects.length) return;
    const pad = 3;
    for (const r of rects) {
      const d = document.createElement("div");
      d.className = "penroam-match absolute";
      d.style.left = `${r.x - pad}px`;
      d.style.top = `${r.y - pad}px`;
      d.style.width = `${r.w + pad * 2}px`;
      d.style.height = `${r.h + pad * 2}px`;
      this.overlay.appendChild(d);
      this.matches.push(d);
    }
    const x0 = Math.min(...rects.map((r) => r.x)), y0 = Math.min(...rects.map((r) => r.y));
    const x1 = Math.max(...rects.map((r) => r.x + r.w)), y1 = Math.max(...rects.map((r) => r.y + r.h));
    const [vx0, vy0, vx1, vy1] = this.visibleDocRect();
    if (x0 < vx0 || y0 < vy0 || x1 > vx1 || y1 > vy1) {
      // centre the matches at the current zoom
      const { scale } = this.transform;
      this.setTransform({ scale, ox: this.viewW / 2 - ((x0 + x1) / 2) * scale, oy: this.viewH / 2 - ((y0 + y1) / 2) * scale });
    }
  }

  clearMatches() {
    for (const d of this.matches) d.remove();
    this.matches = [];
  }

  /** Page area currently on screen (doc pt). */
  visibleDocRect(): Rect {
    const [x0, y0] = toDoc(this.transform, 0, 0);
    const [x1, y1] = toDoc(this.transform, this.viewW, this.viewH);
    return [x0, y0, x1, y1];
  }

  private viewportPoint(e: { clientX: number; clientY: number }): [number, number] {
    const r = this.rect ?? this.viewport.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  }

  private docPoint(e: { clientX: number; clientY: number }): [number, number] {
    const [x, y] = this.viewportPoint(e);
    return toDoc(this.transform, x, y);
  }

  // ========================================================== tools ==

  setTool(tool: Tool) {
    if (tool === this.tool) return;
    this.endTextEdit();
    this.cancelGestures();
    if (tool !== "select" && tool !== "lasso") this.clearSelection();
    this.previousTool = this.tool;
    this.tool = tool;
    this.viewport.dataset.tool = tool;
    this.clearHover();
    this.emit({ tool });
    this.updateNative();
    this.updateSelectionBox();
  }

  /** Follows the system's Apple Pencil double-tap preference. Other actions are not ours to do. */
  pencilTap(action: string) {
    if (action === "switchEraser") this.setTool(this.tool === "eraser" ? this.previousTool : "eraser");
    else if (action === "switchPrevious") this.setTool(this.previousTool);
  }

  setToolSettings(t: Partial<ToolSettings>) {
    // the host sends every setting on any change; only an actual flip moves the block
    const freed = t.textFreePlacement !== undefined && t.textFreePlacement !== this.tools.textFreePlacement;
    const refont = t.textFont !== undefined && t.textFont !== this.tools.textFont;
    this.tools = { ...this.tools, ...t };
    if (this.edit) {
      // live style change for the block being edited
      const d = this.edit.draft;
      if (t.textColor) d.color = t.textColor;
      if (t.textSize) d.fontSize = t.textSize;
      if (refont) d.font = this.tools.textFont;
      if (freed) Object.assign(d, this.placedForMode(d, this.tools.textFreePlacement));
      this.layoutTextEditor();
      this.scheduleTextCommit();
    }
    if (this.native && this.nativeMounted) this.nativeCall((n) => n.setTool(this.nativeTool()));
  }

  /** `d` with the free or row-locked placement. Paper without rows only frees text. */
  private placedForMode(d: Text, free: boolean): Text {
    const t = this.session?.page.template;
    if (!t) return d;
    const pitch = free ? undefined : rowPitchOf(t);
    if (!free && pitch === undefined) return d;
    return this.relockedText(d, t, pitch);
  }

  /** Frees the selected text blocks, or locks them to the paper's rows. */
  setSelectionFreePlacement(free: boolean) {
    const s = this.session;
    if (!s) return;
    const before: Element[] = [];
    const after: Element[] = [];
    for (const id of this.selection) {
      const e = s.elementById(id);
      if (e?.type !== "text") continue;
      const n = this.placedForMode(e, free);
      if (n.rowPitch === e.rowPitch && n.y === e.y) continue;
      before.push(e);
      after.push(n);
    }
    if (after.length) s.commit({ label: free ? "Free text" : "Lock text to rows", forward: [{ op: "replaceElements", elements: after }], inverse: [{ op: "replaceElements", elements: before }] });
    this.updateSelectionBox();
  }

  /** Applies colour/size/font to the current selection (text: all three; ink: colour). */
  restyleSelection(style: { color?: string; fontSize?: number; font?: TextFont }) {
    const s = this.session;
    if (!s || this.selection.size === 0) return;
    const before: Element[] = [];
    const after: Element[] = [];
    for (const id of this.selection) {
      const e = s.elementById(id);
      if (!e) continue;
      let n: Element | null = null;
      if (e.type === "text") n = { ...e, color: style.color ?? e.color, fontSize: style.fontSize ?? e.fontSize, font: style.font ?? e.font };
      else if (e.type === "ink" && style.color) n = { ...e, color: style.color };
      if (n?.type === "text") n.height = Math.max(this.textHeight(n), 1);
      if (n) {
        before.push(e);
        after.push(n);
      }
    }
    if (after.length) s.commit({ label: "Restyle", forward: [{ op: "replaceElements", elements: after }], inverse: [{ op: "replaceElements", elements: before }] });
    this.updateSelectionBox();
  }

  // ================================================== pointer input ==

  private effectiveTool(e: PointerEvent): Tool {
    if (this.spaceDown) return "hand";
    if (penErases(e)) return "eraser";
    return this.tool;
  }

  private capture(e: PointerEvent) {
    try {
      this.viewport.setPointerCapture(e.pointerId);
    } catch {
      /* not capturable (synthetic) */
    }
  }

  /** Records that a stylus is in use: "auto" pen-only mode now gives fingers pan and zoom. */
  private markPenSeen() {
    if (this.penSeen) return;
    this.penSeen = true;
    this.emit({ penSeen: true });
  }

  private onDown(e: PointerEvent) {
    if (this.textHost.contains(e.target as Node)) return;
    if (!this.session) return;
    this.clearMatches();
    this.rect = this.viewport.getBoundingClientRect();
    if (e.pointerType === "pen") {
      this.markPenSeen();
      this.abandonFingerDraw();
      this.clearHover();
      // WKWebView gets the Pencil as well as the native capture, which commits the stroke itself
      if (this.native?.kind === "ios-pencil" && this.nativeVisible) {
        e.preventDefault();
        this.penDown = true;
        return;
      }
    }
    // Ctrl-click (Cmd-click on macOS) opens a link whatever the tool, as in other editors.
    if (e.pointerType === "mouse" && e.button === 0 && (e.ctrlKey || e.metaKey) && !this.edit) {
      const href = this.linkUnder(e);
      if (href) {
        e.preventDefault();
        return this.cb.openLink?.(href);
      }
    }
    const grabbed = e.target === this.editFrame ? this.edit?.id : undefined;
    // finishing a text edit consumes the tap
    if (this.edit && !(e.pointerType === "touch" && this.touches.size > 0)) {
      this.endTextEdit();
      if (this.tool === "text" && !grabbed) return;
    }
    const tool = this.effectiveTool(e);
    const busy = !!(this.active || this.erase || this.drag);
    const penActive = this.penDown || performance.now() - this.penLiftedAt < PEN_LIFT_GRACE_MS;
    const cls = classifyPointer(e, { penOnly: this.settings.penOnly, penSeen: this.penSeen, penDown: penActive, drawingActive: busy, touches: this.touches.size, palmPx: this.settings.palmContactPx });
    if (cls === "ignore") return;
    if (e.pointerType === "pen") this.penDown = true;
    if (e.pointerType === "touch" && this.fingerDraw) {
      // the drawing finger becomes the first half of the pinch, so it must stop drawing
      const first = this.abandonFingerDraw(PINCH_GRACE_MS)!;
      this.touches.set(first.pointerId, { x: first.x, y: first.y });
    }
    if (cls === "navigate" || tool === "hand") return this.beginNavigate(e);
    if (e.pointerType === "touch" && busy) return;
    e.preventDefault();
    if (this.continuous && !busy) {
      // a press on one of the other pages opens it; ink there would belong to this page
      const [x, y] = this.docPoint(e);
      const hit = this.strip.pageAt(x, y);
      if (hit && hit.id !== this.session.page.id) return this.requestPage(hit.id);
    }
    this.capture(e);
    if (e.pointerType === "touch") {
      const [x, y] = this.viewportPoint(e);
      this.fingerDraw = { pointerId: e.pointerId, x, y };
    }
    // an empty box is gone once its edit ended, and then this is an ordinary press
    if (grabbed && this.session.elementById(grabbed)) return this.beginTextMove(grabbed, e);
    // Only the lift can tell a tap from a stroke or a drag. A pen stroke starts anyway so that
    // ink never lags and is dropped if this was a tap; selecting waits, or a tap would flash
    // the selection and the toolbar's selection buttons.
    this.taskTap = this.pressedTask(e, tool);
    if (this.taskTap && tool === "select") {
      this.taskTap.selectFrom = e;
      return;
    }
    switch (tool) {
      case "pen":
      case "highlighter":
        return this.beginStroke(e, tool);
      case "eraser":
        return this.beginErase(e);
      case "select":
      case "lasso":
        return this.beginSelect(e);
      case "text":
        return this.textTap(e);
    }
  }

  /**
   * The nib's footprint while a pen hovers (S Pen, Apple Pencil on M2 iPads, most desktop pens).
   * Off in the ePaper theme, where every update of the ring would be a visible refresh.
   */
  private drawHover(e: PointerEvent) {
    if (this.settings.mono || !DRAW_TOOLS.includes(this.tool)) return;
    const [x, y] = this.docPoint(e);
    const { scale } = this.transform;
    const width = this.tool === "highlighter" ? this.tools.highlighterWidth : this.tool === "pen" ? this.tools.penWidth : (ERASER_RADIUS_PX * 2) / scale;
    const r = Math.max(width / 2, 2 / scale);
    this.clearScreen(this.tctx);
    this.screenCtx(this.tctx);
    this.tctx.strokeStyle = "rgba(100,100,100,0.8)";
    this.tctx.lineWidth = 1 / scale;
    this.tctx.beginPath();
    this.tctx.arc(x, y, r, 0, Math.PI * 2);
    this.tctx.stroke();
    this.hoverShown = true;
  }

  private clearHover() {
    if (!this.hoverShown) return;
    this.hoverShown = false;
    this.clearScreen(this.tctx);
  }

  private onMove(e: PointerEvent) {
    if (e.pointerType === "pen" && e.buttons === 0 && e.type === "pointermove" && !this.active && !this.erase && !this.drag) return this.drawHover(e);
    if (e.pointerType === "mouse" && e.buttons === 0 && e.type === "pointermove") this.updateLinkHover(e);
    if (this.fingerDraw?.pointerId === e.pointerId) [this.fingerDraw.x, this.fingerDraw.y] = this.viewportPoint(e);
    if (this.linkTap?.pointerId === e.pointerId) {
      const [x, y] = this.viewportPoint(e);
      if (Math.hypot(x - this.linkTap.x, y - this.linkTap.y) > TAP_SLOP_PX) this.linkTap = null;
    }
    if (this.taskTap?.pointerId === e.pointerId) {
      const [x, y] = this.viewportPoint(e);
      if (Math.hypot(x - this.taskTap.x, y - this.taskTap.y) > TAP_SLOP_PX) {
        const from = this.taskTap.selectFrom;
        this.taskTap = null;
        if (from) this.beginSelect(from);
      }
    }
    if (this.touches.has(e.pointerId) || this.pan?.pointerId === e.pointerId) {
      if (e.type === "pointermove") this.navigate(e);
      return;
    }
    if (this.active && this.active.pointerId === e.pointerId) {
      // one sample stream only (pointerrawupdate when available), never both
      if (e.type !== this.sampleEvent) return;
      const a = this.active;
      const list = this.hasCoalesced ? e.getCoalescedEvents() : [];
      if (list.length > 0) {
        a.coalesced += list.length;
        for (const ce of list) this.addSample(a, ce, true);
      } else this.addSample(a, e, false);
      if (this.hasPredicted) {
        const pr: PointerEvent[] = (e as any).getPredictedEvents?.() ?? [];
        // predicted samples are an ephemeral preview only; never stored
        a.predicted = pr.map((pe) => samplePoint(pe, this.rect!, this.transform, a.t0, false, this.settings.pencilCurve));
      }
      this.scheduleFrame();
      return;
    }
    if (e.type !== "pointermove") return;
    if (this.erase && this.erase.pointerId === e.pointerId) {
      const list = this.hasCoalesced ? e.getCoalescedEvents() : [];
      for (const ce of list.length ? list : [e]) this.eraseTo(this.docPoint(ce));
      this.drawEraserCursor(this.docPoint(e));
      return;
    }
    if (this.drag && this.drag.pointerId === e.pointerId) this.dragTo(this.docPoint(e), e.altKey);
  }

  private onUp(e: PointerEvent, cancelled: boolean) {
    if (e.pointerType === "pen") this.liftPen();
    if (this.fingerDraw?.pointerId === e.pointerId) this.fingerDraw = null;
    const tap = this.linkTap?.pointerId === e.pointerId ? this.linkTap : null;
    if (tap) {
      this.linkTap = null;
      if (!cancelled) this.cb.openLink?.(tap.href);
    }
    const task = this.taskTap?.pointerId === e.pointerId ? this.taskTap : null;
    if (task) {
      this.taskTap = null;
      if (!cancelled) {
        if (this.active?.pointerId === e.pointerId) this.dropStroke();
        this.toggleTask(task);
      }
    }
    if (this.touches.delete(e.pointerId)) {
      if (this.touches.size < 2) this.pinch = null;
      return;
    }
    if (this.pan?.pointerId === e.pointerId) {
      this.pan = null;
      return;
    }
    if (this.active?.pointerId === e.pointerId) {
      if (cancelled) return this.cancelStroke();
      const a = this.active;
      const p = samplePoint(e, this.rect!, this.transform, a.t0, false, this.settings.pencilCurve);
      const last = a.stroke.points[a.stroke.points.length - 1];
      if (!last || last.x !== p.x || last.y !== p.y) a.stroke.points.push(p);
      return this.finishStroke();
    }
    if (this.erase?.pointerId === e.pointerId) return this.finishErase();
    if (this.drag?.pointerId === e.pointerId) return cancelled ? this.cancelDrag() : this.finishDrag();
  }

  private liftPen() {
    this.penDown = false;
    this.penLiftedAt = performance.now();
  }

  /**
   * Ends a finger's drawing gesture that turned out to be something else: a pinch, or a palm
   * resting before the pen came down. Its stroke is dropped unless it has lasted `keepAfterMs`,
   * so a real line survives a late second finger. Erasing has already happened and is kept.
   * Returns the finger, if there was one.
   */
  private abandonFingerDraw(keepAfterMs = Infinity) {
    const finger = this.fingerDraw;
    if (!finger) return null;
    this.fingerDraw = null;
    this.taskTap = null;
    if (this.active) {
      if (performance.now() - this.active.t0 >= keepAfterMs) this.finishStroke();
      else this.dropStroke();
    }
    if (this.erase) this.finishErase();
    if (this.drag) this.cancelDrag();
    return finger;
  }

  private onLostCapture(e: PointerEvent) {
    // lost capture mid-stroke (e.g. window lost focus): keep what was drawn, never orphan it
    if (this.active?.pointerId === e.pointerId) this.finishStroke();
    else if (this.erase?.pointerId === e.pointerId) this.finishErase();
    else if (this.drag?.pointerId === e.pointerId) this.finishDrag();
  }

  /** App backgrounded / window blurred / page switch: no stuck or orphaned strokes. */
  cancelGestures() {
    if (this.active) this.finishStroke();
    if (this.erase) this.finishErase();
    if (this.drag) this.cancelDrag();
    this.fingerDraw = null;
    this.touches.clear();
    this.pinch = null;
    this.pan = null;
    this.linkTap = null;
    this.taskTap = null;
    this.penDown = false;
    this.spaceDown = false;
  }

  private onWheel(e: WheelEvent) {
    e.preventDefault();
    if (this.active || this.erase || this.drag) return;
    const [x, y] = this.viewportPoint(e);
    if (e.ctrlKey || e.metaKey) {
      this.setTransform(zoomAbout(this.transform, this.transform.scale * Math.exp(-e.deltaY * 0.0025), x, y));
    } else {
      const k = e.deltaMode === 1 ? 16 : 1;
      this.setTransform({ ...this.transform, ox: this.transform.ox - e.deltaX * k, oy: this.transform.oy - e.deltaY * k });
    }
  }

  private onDoubleClick(e: MouseEvent) {
    if (this.tool !== "select" || !this.session) return;
    const [x, y] = this.docPoint(e);
    const t = this.hitText(x, y);
    // quick taps on a checkbox are ticks, not a request to edit the box
    if (t && !this.taskAtPoint(x, y)) this.beginTextEdit(t, false);
  }

  // ----------------------------------------------------- navigation --
  private beginNavigate(e: PointerEvent) {
    e.preventDefault();
    this.capture(e);
    const p = this.viewportPoint(e);
    // Panning is what a hand or finger does on a page, so a tap that doesn't pan is free to
    // open a link. A second finger makes it a pinch.
    const first = e.pointerType !== "touch" || this.touches.size === 0;
    const href = first && e.button <= 0 ? this.linkUnder(e) : null;
    this.linkTap = href ? { pointerId: e.pointerId, x: p[0], y: p[1], href } : null;
    this.taskTap = first && !href ? this.pressedTask(e, this.effectiveTool(e)) : null;
    if (e.pointerType === "touch") {
      this.touches.set(e.pointerId, { x: p[0], y: p[1] });
      if (this.touches.size === 2) {
        const [a, b] = [...this.touches.values()];
        const [ax, ay] = toDoc(this.transform, (a.x + b.x) / 2, (a.y + b.y) / 2);
        this.pinch = { dist: Math.max(1, Math.hypot(b.x - a.x, b.y - a.y)), scale: this.transform.scale, ax, ay };
      }
    } else this.pan = { pointerId: e.pointerId, x: p[0], y: p[1] };
  }

  private navigate(e: PointerEvent) {
    const [x, y] = this.viewportPoint(e);
    if (this.pan && this.pan.pointerId === e.pointerId) {
      const dx = x - this.pan.x, dy = y - this.pan.y;
      this.pan.x = x;
      this.pan.y = y;
      return this.setTransform({ ...this.transform, ox: this.transform.ox + dx, oy: this.transform.oy + dy });
    }
    const prev = this.touches.get(e.pointerId);
    if (!prev) return;
    this.touches.set(e.pointerId, { x, y });
    if (this.touches.size >= 2 && this.pinch) {
      const [a, b] = [...this.touches.values()];
      const dist = Math.max(1, Math.hypot(b.x - a.x, b.y - a.y));
      const fx = (a.x + b.x) / 2, fy = (a.y + b.y) / 2;
      const scale = clampScale((this.pinch.scale * dist) / this.pinch.dist);
      this.setTransform({ scale, ox: fx - this.pinch.ax * scale, oy: fy - this.pinch.ay * scale });
    } else if (this.touches.size === 1) {
      this.setTransform({ ...this.transform, ox: this.transform.ox + x - prev.x, oy: this.transform.oy + y - prev.y });
    }
  }

  // --------------------------------------------------------- strokes --
  private beginStroke(e: PointerEvent, tool: "pen" | "highlighter") {
    const kind = e.pointerType || "mouse";
    const hl = tool === "highlighter";
    const stroke: Ink = {
      type: "ink",
      id: newId(),
      z: 0,
      tool,
      color: hl ? this.tools.highlighterColor : this.tools.penColor,
      opacity: hl ? HIGHLIGHTER_OPACITY : 1,
      width: hl ? this.tools.highlighterWidth : this.tools.penWidth,
      pressure: !hl && this.tools.pressure && pressureCapable(kind, e.pressure),
      source: kind === "pen" ? "stylus" : kind,
      input: {
        api: "web-pointer",
        device: kind,
        sampleEvent: this.sampleEvent,
        coalesced: this.hasCoalesced,
        firstPressure: e.pressure,
        pointerWidth: e.width,
        pointerHeight: e.height,
        userAgent: navigator.userAgent,
        ...(this.settings.pencilCurve && kind === "pen" ? { pressureCurve: PENCIL_CURVE } : {}),
      },
      points: [],
    };
    this.active = { stroke, pointerId: e.pointerId, t0: e.timeStamp, stableDrawn: 0, coalesced: 0, predicted: [] };
    this.addSample(this.active, e, false);
    this.scheduleFrame();
  }

  private addSample(a: ActiveStroke, e: PointerEvent, coalesced: boolean) {
    const p = samplePoint(e, this.rect!, this.transform, a.t0, coalesced, this.settings.pencilCurve);
    const pts = a.stroke.points;
    const last = pts[pts.length - 1];
    if (last && last.x === p.x && last.y === p.y && last.pressure === p.pressure) return;
    pts.push(p);
    const s = a.stroke;
    if (!s.pressure && s.tool === "pen" && this.tools.pressure && pressureArrived(e.pointerType, e.pressure)) s.pressure = true;
    if (e.twist) a.stroke.input.twistSeen = true;
  }

  private screenCtx(ctx: CanvasRenderingContext2D) {
    const { scale, ox, oy } = this.transform;
    const d = this.dpr;
    ctx.setTransform(d * scale, 0, 0, d * scale, d * ox, d * oy);
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
  }

  private clearScreen(ctx: CanvasRenderingContext2D) {
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
  }

  private scheduleFrame() {
    if (this.frameRequested) return;
    this.frameRequested = true;
    requestAnimationFrame(() => this.frame());
  }

  private frame() {
    this.frameRequested = false;
    const a = this.active;
    if (a) this.drawActive(a);
    if (this.drag) this.drawDrag();
  }

  private drawActive(a: ActiveStroke) {
    const s = this.renderer.strokeStyleFor(a.stroke);
    const n = s.points.length;
    this.clearScreen(this.tctx);
    this.screenCtx(this.tctx);
    if (isUniform(s) && s.tool === "highlighter") {
      // translucent: redraw the whole path once per frame (no darker overlaps)
      drawStroke(this.tctx, s);
      return;
    }
    const stable = stablePieceCount(n);
    if (stable > a.stableDrawn) {
      this.screenCtx(this.actx);
      drawPieces(this.actx, s, a.stableDrawn, stable);
      a.stableDrawn = stable;
    }
    drawPieces(this.tctx, s, a.stableDrawn, pieceCount(n));
    if (a.predicted.length) {
      const tmp = { ...s, points: [s.points[n - 1], ...a.predicted] };
      this.tctx.globalAlpha = 0.5;
      drawPieces(this.tctx, tmp, 0, pieceCount(tmp.points.length));
      this.tctx.globalAlpha = 1;
    }
  }

  private finishStroke() {
    const a = this.active;
    this.active = null;
    if (!a || !this.session || a.stroke.points.length === 0) {
      this.clearScreen(this.actx);
      this.clearScreen(this.tctx);
      return;
    }
    a.stroke.z = this.session.nextZ();
    a.stroke.input.coalescedSamples = a.coalesced;
    // Commit renders the stroke into the page cache synchronously (see
    // onElementsChanged), then the preview is cleared in the same task:
    // the committed pixels and the preview removal land in one frame.
    this.session.commit({ label: "Draw", forward: [{ op: "addElements", elements: [a.stroke] }], inverse: [{ op: "removeElements", ids: [a.stroke.id] }] });
    this.clearScreen(this.actx);
    this.clearScreen(this.tctx);
  }

  private cancelStroke() {
    // pointercancel (palm/gesture takeover): the samples so far are kept
    // only if they form a real stroke; a cancelled tap leaves nothing behind
    if (this.active && this.active.stroke.points.length > 3) return this.finishStroke();
    this.dropStroke();
  }

  private dropStroke() {
    this.active = null;
    this.clearScreen(this.actx);
    this.clearScreen(this.tctx);
  }

  // ---------------------------------------------------------- eraser --
  private beginErase(e: PointerEvent) {
    const p = this.docPoint(e);
    this.erase = { pointerId: e.pointerId, last: p, erased: new Map(), dirty: null };
    this.eraseTo(p);
    this.drawEraserCursor(p);
  }

  private eraseTo(p: [number, number]) {
    const g = this.erase!;
    const r = ERASER_RADIUS_PX / this.transform.scale;
    let dirty: Rect | null = null;
    for (const el of this.session!.page.elements) {
      if (el.type !== "ink" || g.erased.has(el.id)) continue;
      if (hitTestSegment(el, g.last[0], g.last[1], p[0], p[1], r)) {
        g.erased.set(el.id, el);
        this.renderer.hidden.add(el.id);
        dirty = unionRect(dirty, strokeBounds(el));
      }
    }
    g.last = p;
    if (dirty) {
      g.dirty = unionRect(g.dirty, dirty);
      this.renderer.renderInk(dirty);
    }
  }

  private drawEraserCursor(p: [number, number]) {
    this.clearScreen(this.tctx);
    this.screenCtx(this.tctx);
    const r = ERASER_RADIUS_PX / this.transform.scale;
    this.tctx.strokeStyle = "rgba(100,100,100,0.8)";
    this.tctx.lineWidth = 1 / this.transform.scale;
    this.tctx.beginPath();
    this.tctx.arc(p[0], p[1], r, 0, Math.PI * 2);
    this.tctx.stroke();
  }

  private finishErase() {
    const g = this.erase;
    this.erase = null;
    this.clearScreen(this.tctx);
    if (!g || !this.session) return;
    const removed = [...g.erased.values()];
    for (const el of removed) this.renderer.hidden.delete(el.id);
    if (removed.length === 0) return;
    this.session.commit({
      label: "Erase",
      forward: [{ op: "removeElements", ids: removed.map((e) => e.id) }],
      inverse: [{ op: "addElements", elements: removed }],
    });
  }

  // --------------------------------------------------------- links --
  private linkUnder(e: { clientX: number; clientY: number }): string | null {
    if (!this.session) return null;
    const [x, y] = this.docPoint(e);
    const t = this.hitText(x, y);
    return t ? linkAt(t, x, y) : null;
  }

  /**
   * Shows a mouse user where a click would act: a link's address as a tooltip, and a pointer
   * cursor over a link a click would open (the hand tool, or Ctrl/Cmd held) or a checkbox it
   * would tick.
   */
  private updateLinkHover(e: PointerEvent) {
    const href = this.edit ? null : this.linkUnder(e);
    const task = !this.edit && !!this.session && (this.tool === "pen" || this.tool === "select" || this.tool === "hand") && !!this.taskAtPoint(...this.docPoint(e));
    this.showLinkHover(href, this.tool === "hand" || e.ctrlKey || e.metaKey, task);
  }

  private showLinkHover(href: string | null, clickable: boolean, task = false) {
    const v = this.viewport;
    if (href) v.title = href;
    else v.removeAttribute("title");
    if ((href && clickable) || task) v.dataset.link = "";
    else delete v.dataset.link;
  }

  // --------------------------------------------------------- tasks --
  private taskAtPoint(x: number, y: number): TaskHit | null {
    const t = this.hitText(x, y);
    const box = t && taskAt(t, x, y, TASK_TAP_PAD_PX / this.transform.scale);
    return t && box ? { id: t.id, text: t.text, box } : null;
  }

  /** The checkbox this press could tick. Ticking belongs to the pen, select and hand tools. */
  private pressedTask(e: PointerEvent, tool: Tool): TaskTap | null {
    if (!this.session || e.button > 0 || (tool !== "pen" && tool !== "select" && tool !== "hand")) return null;
    const hit = this.taskAtPoint(...this.docPoint(e));
    if (!hit) return null;
    const [x, y] = this.viewportPoint(e);
    return { ...hit, pointerId: e.pointerId, x, y };
  }

  private toggleTask(hit: TaskHit) {
    const s = this.session;
    const t = s?.elementById(hit.id);
    // a sync or an undo can change the block under a press, and `at` is only good for its text
    if (!s || t?.type !== "text" || t.text !== hit.text) return;
    const next: Text = { ...t, text: toggledTask(t.text, hit.box) };
    s.commit({ label: hit.box.done ? "Untick task" : "Tick task", forward: [{ op: "replaceElements", elements: [next] }], inverse: [{ op: "replaceElements", elements: [t] }] });
  }

  // -------------------------------------------------------- select --
  private hitText(x: number, y: number): Text | null {
    const els = this.session!.page.elements;
    for (let i = els.length - 1; i >= 0; i--) {
      const e = els[i];
      if (e.type !== "text") continue;
      const b = textBounds(e);
      if (x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]) return e;
    }
    return null;
  }

  private hitElement(x: number, y: number): Element | null {
    const t = this.hitText(x, y);
    if (t) return t;
    const r = SELECT_TOLERANCE_PX / this.transform.scale;
    const els = this.session!.page.elements;
    for (let i = els.length - 1; i >= 0; i--) {
      const e = els[i];
      if (e.type === "ink" && hitTest(e, x, y, r)) return e;
      // a PDF page is the paper written on: a press on it starts a band, as on the page
      if (e.type === "image" && !e.pdfPage && x >= e.x && x <= e.x + e.width && y >= e.y && y <= e.y + e.height) return e;
    }
    return null;
  }

  private selectionBounds(): Rect | null {
    let r: Rect | null = null;
    for (const id of this.selection) {
      const e = this.session?.elementById(id);
      if (e) r = unionRect(r, elementBounds(e));
    }
    return r;
  }

  private singleSelectedText(): Text | null {
    if (this.selection.size !== 1) return null;
    const e = this.session?.elementById([...this.selection][0]);
    return e?.type === "text" ? e : null;
  }

  private beginSelect(e: PointerEvent) {
    const p = this.docPoint(e);
    const tol = SELECT_TOLERANCE_PX / this.transform.scale;
    const t = this.singleSelectedText();
    if (t) {
      const [hx, hy] = this.handlePoint(textBounds(t));
      if (Math.abs(p[0] - hx) <= tol * 1.5 && Math.abs(p[1] - hy) <= tol * 1.5) {
        this.drag = { kind: "resize", pointerId: e.pointerId, start: p, orig: t, width: t.width, height: t.height };
        this.hideSelection(true);
        return;
      }
    }
    const sb = this.selectionBounds();
    if (sb && p[0] >= sb[0] - tol && p[0] <= sb[2] + tol && p[1] >= sb[1] - tol && p[1] <= sb[3] + tol) {
      this.drag = { kind: "move", pointerId: e.pointerId, start: p, dx: 0, dy: 0, moved: false, guides: [] };
      return;
    }
    if (this.tool === "lasso") {
      this.clearSelection();
      this.drag = { kind: "lasso", pointerId: e.pointerId, points: [p] };
      return;
    }
    const hit = this.hitElement(p[0], p[1]);
    if (hit) {
      this.selection = new Set([hit.id]);
      this.updateSelectionBox();
      this.drag = { kind: "move", pointerId: e.pointerId, start: p, dx: 0, dy: 0, moved: false, guides: [] };
      return;
    }
    this.clearSelection();
    this.drag = { kind: "band", pointerId: e.pointerId, start: p, end: p };
  }

  /** Moves one text block from a press, in any tool. */
  private beginTextMove(id: string, e: PointerEvent, tapEdits = false) {
    this.selection = new Set([id]);
    this.updateSelectionBox();
    this.drag = { kind: "move", pointerId: e.pointerId, start: this.docPoint(e), dx: 0, dy: 0, moved: false, guides: [], tapEdits };
  }

  /** The right edge text keeps to: the page's, which on a column is the layers' right edge. */
  private get pageRight(): number {
    return this.column ? this.renderer.extent[2] : this.renderer.width;
  }

  /**
   * Where a block's resize handle goes: its corner, pulled back onto the page.
   * Long text runs off the bottom of the page, and the corner with it, where
   * nothing could reach the handle to make the box wider.
   */
  private handlePoint(b: Rect): [number, number] {
    if (this.unbounded) return [b[2], b[3]];
    const [, , right, bottom] = this.column ? this.renderer.extent : [0, 0, this.renderer.width, this.renderer.height];
    return [Math.min(b[2], right), Math.min(b[3], bottom)];
  }

  private hideSelection(hide: boolean) {
    for (const id of this.selection) {
      if (hide) this.renderer.hidden.add(id);
      else this.renderer.hidden.delete(id);
    }
    const b = this.selectionBounds();
    if (b) {
      this.renderer.renderInk(b);
      this.renderer.renderText(b);
    }
  }

  private dragTo(p: [number, number], freehand = false) {
    const d = this.drag!;
    if (d.kind === "band") d.end = p;
    else if (d.kind === "lasso") d.points.push(p);
    else if (d.kind === "move") {
      d.dx = p[0] - d.start[0];
      d.dy = p[1] - d.start[1];
      if (!d.moved && Math.hypot(d.dx, d.dy) * this.transform.scale > 2) {
        d.moved = true;
        this.hideSelection(true);
      }
      d.dy = this.snappedDy(d.dy);
      this.alignDrag(d, freehand);
    } else {
      // never wider than the page, unless it already was; a free canvas has no edge
      const widest = this.unbounded ? MAX_TEXT_WIDTH_PT : Math.max(d.orig.width, this.pageRight - d.orig.x);
      d.width = Math.max(24, Math.min(widest, d.orig.width + p[0] - d.start[0]));
      const minH = this.textHeight({ ...d.orig, width: d.width });
      const h = Math.max(minH, d.orig.height + p[1] - d.start[1]);
      const pitch = d.orig.rowPitch;
      d.height = pitch === undefined ? h : Math.ceil(h / pitch - 1e-9) * pitch;
    }
    this.scheduleFrame();
  }

  private textEdges(t: Text, dx = 0, dy = 0): TextEdges {
    const y = t.y + dy;
    const x = t.x + dx;
    return { rect: [x, y, x + t.width, y + t.height], baseline: y + (paintOf(t).runs[0]?.y ?? t.height) };
  }

  /** Edges of the page's text blocks, leaving out `skip`. */
  private otherTextEdges(skip: Set<string>): TextEdges[] {
    const out: TextEdges[] = [];
    for (const e of this.session?.page.elements ?? []) if (e.type === "text" && !skip.has(e.id)) out.push(this.textEdges(e));
    return out;
  }

  /** Lines a moved selection's text up with the other text on the page. */
  private alignDrag(d: Extract<SelectDrag, { kind: "move" }>, freehand: boolean) {
    d.guides = [];
    if (!d.moved || freehand) return;
    const moving: TextEdges[] = [];
    let onRows = false;
    for (const id of this.selection) {
      const e = this.session?.elementById(id);
      if (e?.type !== "text") continue;
      moving.push(this.textEdges(e, d.dx, d.dy));
      onRows ||= e.rowPitch !== undefined;
    }
    if (!moving.length) return;
    const a = alignment(moving, this.otherTextEdges(this.selection), SNAP_DISTANCE_PX / this.transform.scale, !onRows);
    d.dx += a.dx;
    d.dy += a.dy;
    d.guides = a.guides;
  }

  /**
   * Moves the selection by whole rows while it holds row-locked text, so the
   * text stays on the rows and the selection keeps its shape.
   */
  private snappedDy(dy: number): number {
    const s = this.session;
    if (!s) return dy;
    for (const id of this.selection) {
      const e = s.elementById(id);
      if (e?.type === "text" && e.rowPitch !== undefined) return snapToRow(e.y + dy, s.page.template, e.rowPitch, Math.round, this.unbounded) - e.y;
    }
    return dy;
  }

  private movedCopy(e: Element, dx: number, dy: number): Element {
    if (e.type === "ink") return { ...e, points: e.points.map((p) => ({ ...p, x: Math.round((p.x + dx) * 1000) / 1000, y: Math.round((p.y + dy) * 1000) / 1000 })) };
    return { ...e, x: e.x + dx, y: e.y + dy };
  }

  private drawDrag() {
    const d = this.drag!;
    this.clearScreen(this.tctx);
    this.screenCtx(this.tctx);
    const ctx = this.tctx;
    if (d.kind === "lasso") {
      ctx.save();
      ctx.strokeStyle = "rgba(37,99,235,0.9)";
      ctx.fillStyle = "rgba(37,99,235,0.08)";
      ctx.lineWidth = 1 / this.transform.scale;
      ctx.setLineDash([4 / this.transform.scale, 3 / this.transform.scale]);
      ctx.beginPath();
      d.points.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
      ctx.closePath();
      ctx.fill();
      ctx.stroke();
      ctx.restore();
      return;
    }
    if (d.kind === "band") {
      ctx.save();
      ctx.strokeStyle = "rgba(37,99,235,0.9)";
      ctx.fillStyle = "rgba(37,99,235,0.08)";
      ctx.lineWidth = 1 / this.transform.scale;
      ctx.setLineDash([4 / this.transform.scale, 3 / this.transform.scale]);
      const x = Math.min(d.start[0], d.end[0]), y = Math.min(d.start[1], d.end[1]);
      const w = Math.abs(d.end[0] - d.start[0]), h = Math.abs(d.end[1] - d.start[1]);
      ctx.fillRect(x, y, w, h);
      ctx.strokeRect(x, y, w, h);
      ctx.restore();
      return;
    }
    if (d.kind === "move" && !d.moved) return;
    for (const id of this.selection) {
      const e = this.session!.elementById(id);
      if (!e) continue;
      if (d.kind === "resize" && e.type === "text") drawText(ctx, { ...e, width: d.width, height: d.height }, this.renderer.textColorFor(e));
      else if (d.kind === "move") {
        const m = this.movedCopy(e, d.dx, d.dy);
        if (m.type === "ink") drawStroke(ctx, this.renderer.strokeStyleFor(m));
        else if (m.type === "text") drawText(ctx, m, this.renderer.textColorFor(m));
      }
    }
    if (d.kind === "move" && d.guides.length) {
      ctx.strokeStyle = "rgba(219,39,119,0.9)";
      ctx.lineWidth = 1 / this.transform.scale;
      ctx.beginPath();
      for (const g of d.guides) {
        ctx.moveTo(g.x0, g.y0);
        ctx.lineTo(g.x1, g.y1);
      }
      ctx.stroke();
    }
    const b = this.selectionBounds();
    if (b) {
      const off = d.kind === "move" ? [d.dx, d.dy] : [0, 0];
      const w = d.kind === "resize" ? d.width : b[2] - b[0];
      const h = d.kind === "resize" ? d.height : b[3] - b[1];
      ctx.strokeStyle = "rgba(37,99,235,0.9)";
      ctx.lineWidth = 1 / this.transform.scale;
      ctx.strokeRect(b[0] + off[0], b[1] + off[1], w, h);
    }
  }

  private finishDrag() {
    const d = this.drag;
    this.drag = null;
    this.clearScreen(this.tctx);
    const s = this.session;
    if (!d || !s) return;
    if (d.kind === "lasso") {
      if (d.points.length < 3) return this.clearSelection();
      const ids = s.page.elements.filter((e) => this.lassoCatches(e, d.points)).map((e) => e.id);
      this.selection = new Set(ids);
      return this.updateSelectionBox();
    }
    if (d.kind === "band") {
      const r: Rect = [Math.min(d.start[0], d.end[0]), Math.min(d.start[1], d.end[1]), Math.max(d.start[0], d.end[0]), Math.max(d.start[1], d.end[1])];
      if (r[2] - r[0] < 1 && r[3] - r[1] < 1) return this.clearSelection();
      const ids = s.page.elements
        .filter((e) => {
          if (!rectsIntersect(r, elementBounds(e))) return false;
          if (e.type === "ink") return e.points.some((p) => p.x >= r[0] && p.x <= r[2] && p.y >= r[1] && p.y <= r[3]);
          // PDF pages only when the band takes in all of one, as with the lasso
          if (isPdfPage(e)) return containsRect(r, elementBounds(e));
          return true;
        })
        .map((e) => e.id);
      this.selection = new Set(ids);
      return this.updateSelectionBox();
    }
    this.hideSelection(false);
    if (d.kind === "move" && d.tapEdits) {
      // the Text tool: a tap edits the block, a drag moves it
      const t = d.moved ? null : this.singleSelectedText();
      if (t) return this.beginTextEdit(t, false);
    }
    const before: Element[] = [];
    const after: Element[] = [];
    for (const id of this.selection) {
      const e = s.elementById(id);
      if (!e) continue;
      before.push(e);
      if (d.kind === "move") after.push(this.movedCopy(e, d.dx, d.dy));
      else if (e.type === "text") after.push({ ...e, width: d.width, height: d.height });
    }
    const changed = d.kind === "move" ? d.moved && (d.dx !== 0 || d.dy !== 0) : d.width !== d.orig.width || Math.abs(d.height - d.orig.height) > 0.01;
    if (changed && after.length) {
      s.commit({ label: d.kind === "move" ? "Move" : "Resize text", forward: [{ op: "replaceElements", elements: after }], inverse: [{ op: "replaceElements", elements: before }] });
    }
    // only the selection tools keep a selection
    if (this.tool !== "select" && this.tool !== "lasso") this.clearSelection();
    this.updateSelectionBox();
  }

  private lassoCatches(e: Element, poly: [number, number][]): boolean {
    if (e.type === "ink") return lassoHitsStroke(e, poly);
    const b = elementBounds(e);
    // a PDF page only when it is all inside, so that circling ink on it takes the ink alone
    if (isPdfPage(e)) return [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]].every(([x, y]) => pointInPolygon(x, y, poly));
    return pointInPolygon((b[0] + b[2]) / 2, (b[1] + b[3]) / 2, poly);
  }

  private cancelDrag() {
    const d = this.drag;
    this.drag = null;
    this.clearScreen(this.tctx);
    if (d && d.kind !== "band" && d.kind !== "lasso") this.hideSelection(false);
  }

  clearSelection() {
    if (this.selection.size === 0) return;
    this.selection = new Set();
    this.updateSelectionBox();
  }

  selectAll() {
    if (!this.session) return;
    this.selection = new Set(this.session.page.elements.map((e) => e.id));
    this.updateSelectionBox();
  }

  deleteSelection() {
    const s = this.session;
    if (!s || this.selection.size === 0) return;
    const els = [...this.selection].map((id) => s.elementById(id)).filter(Boolean) as Element[];
    this.selection = new Set();
    s.commit({ label: "Delete", forward: [{ op: "removeElements", ids: els.map((e) => e.id) }], inverse: [{ op: "addElements", elements: els }] });
    this.updateSelectionBox();
  }

  private updateSelectionBox() {
    // drop ids that no longer exist (undo/redo)
    for (const id of [...this.selection]) if (!this.session?.elementById(id)) this.selection.delete(id);
    const b = this.selectionBounds();
    const show = !!b && (this.tool === "select" || this.tool === "lasso");
    this.selBox.classList.toggle("hidden", !show);
    const t = this.singleSelectedText();
    this.selHandle.classList.toggle("hidden", !show || !t);
    if (b && show) {
      const pad = 3 / this.transform.scale;
      Object.assign(this.selBox.style, { left: `${b[0] - pad}px`, top: `${b[1] - pad}px`, width: `${b[2] - b[0] + 2 * pad}px`, height: `${b[3] - b[1] + 2 * pad}px` });
      const [hx, hy] = this.handlePoint(b);
      Object.assign(this.selHandle.style, { left: `${hx}px`, top: `${hy}px` });
    }
    const sel = [...this.selection].map((id) => this.session?.elementById(id)).filter(Boolean) as Element[];
    this.emit({
      selectionCount: sel.length,
      selectionHasText: sel.some((e) => e.type === "text"),
      selectionTextFree: sel.every((e) => e.type !== "text" || e.rowPitch === undefined),
      selectionTextFont: sharedTextFont(sel),
    });
  }

  // ----------------------------------------------------------- text --
  private textTap(e: PointerEvent) {
    const [x, y] = this.docPoint(e);
    const hit = this.hitText(x, y);
    if (hit) return this.beginTextMove(hit.id, e, true);
    const W = this.pageRight;
    const fs = this.tools.textSize;
    const template = this.session!.page.template;
    const pitch = this.tools.textFreePlacement ? undefined : rowPitchOf(template);
    // on a sheet or a column, text stays on the page; a free canvas has no edges to keep it from
    const free = this.unbounded;
    const lo = free ? -Infinity : 0;
    let bx = free ? x : Math.min(Math.max(0, x), W - 40);
    // a tap between two rules writes on the lower one
    let by = pitch === undefined ? Math.max(lo, y - lineHeight(fs) / 2) : snapToRow(y, template, pitch, Math.floor, free);
    const h = pitch === undefined ? lineHeight(fs) : rowsFor(fs, pitch) * pitch;
    if (!e.altKey) {
      // Lines up with the text already there. A new block's width comes from where it
      // lands, so only left edges, tops and baselines count.
      const here: TextEdges = { rect: [bx, by, bx, by + h], baseline: by + (pitch ?? firstBaseline(fs)) };
      const others = this.otherTextEdges(new Set()).map((o) => ({ ...o, rect: [o.rect[0], o.rect[1], o.rect[0], o.rect[3]] as Rect }));
      const a = alignment([here], others, SNAP_DISTANCE_PX / this.transform.scale, pitch === undefined);
      bx = Math.max(lo, bx + a.dx);
      by = Math.max(lo, by + a.dy);
    }
    const width = free ? 280 : Math.max(60, Math.min(280, W - bx - 12));
    const block: Text = {
      type: "text",
      id: newId(),
      z: this.session!.nextZ(),
      x: Math.round(bx * 100) / 100,
      y: Math.round(by * 100) / 100,
      width,
      height: h,
      text: "",
      fontSize: fs,
      font: this.tools.textFont,
      color: this.tools.textColor,
      align: "left",
      rowPitch: pitch,
    };
    this.beginTextEdit(block, true);
  }

  editSelectedText() {
    const t = this.singleSelectedText();
    if (t) this.beginTextEdit(t, false);
  }

  private beginTextEdit(t: Text, isNew: boolean) {
    this.endTextEdit();
    this.clearSelection();
    this.edit = { id: t.id, isNew, committed: isNew ? null : t, draft: { ...t }, timer: null, composing: false };
    if (!isNew) {
      this.renderer.hidden.add(t.id);
      this.renderer.renderText(textBounds(t));
    }
    this.textHost.classList.remove("hidden");
    this.editFrame.classList.remove("hidden");
    this.layoutTextEditor();
    this.textEditor = new MarkdownEditor(this.textHost, {
      text: t.text,
      fontSize: t.fontSize,
      rowPitch: t.rowPitch,
      onChange: (text) => this.onTextInput(text),
      onFormats: (textFormats) => this.emit({ textFormats }),
      onResize: () => this.layoutTextEditor(),
    });
    this.layoutTextEditor();
    // focusing doesn't scroll the page under the keyboard: CodeMirror focuses with preventScroll
    this.textEditor.focus();
    this.emit({ editingText: true, textFormats: this.textEditor.formats });
    this.updateNative();
  }

  /** Places the text box editor over the block it is editing. */
  private layoutTextEditor() {
    const d = this.edit?.draft;
    if (!d) return;
    const host = this.textHost;
    const fs = d.fontSize;
    const pitch = d.rowPitch;
    const lh = pitch === undefined ? lineHeight(fs) : rowsFor(fs, pitch) * pitch;
    // CSS centres the font in its line box; rendered row-locked text sits on the
    // bottom of it, so the source moves down to the same baselines.
    const font = d.font ?? "formatted";
    const ascent = ascentOf(font), descent = descentOf(font);
    const cssBaseline = ((lh - ((ascent - descent) * fs) / metrics.unitsPerEm) / 2) + (ascent * fs) / metrics.unitsPerEm;
    const top = pitch === undefined ? d.y : d.y + lh - cssBaseline;
    Object.assign(host.style, {
      left: `${d.x}px`,
      top: `${top}px`,
      width: `${d.width}px`,
      fontSize: `${fs}px`,
      lineHeight: `${lh}px`,
      fontFamily: fontFamilyOf(font),
      // as on the canvas: the layout has no kerning or ligatures for the handwritten font
      fontKerning: font === "handwritten" ? "none" : "",
      fontVariantLigatures: font === "handwritten" ? "none" : "",
      color: d.color,
      textAlign: d.align,
    });
    const h = Math.max(this.textEditor?.height || contentHeight(d.text, fs, d.width, lh), lh);
    host.style.height = `${h}px`;
    Object.assign(this.editFrame.style, { left: `${d.x}px`, top: `${top}px`, width: `${d.width}px`, height: `${h}px` });
  }

  private onTextInput(text: string) {
    const ed = this.edit;
    if (!ed) return;
    ed.draft.text = text;
    this.layoutTextEditor();
    if (!ed.composing) this.scheduleTextCommit();
  }

  private scheduleTextCommit() {
    const ed = this.edit;
    if (!ed) return;
    if (ed.timer) clearTimeout(ed.timer);
    // batch rapid typing into one undoable, persisted change
    ed.timer = window.setTimeout(() => this.commitText(false), 700);
  }

  private commitText(final: boolean) {
    const ed = this.edit;
    const s = this.session;
    if (!ed || !s) return;
    if (ed.timer) clearTimeout(ed.timer);
    ed.timer = null;
    const d = ed.draft;
    const text = final ? d.text.replace(/\s+$/u, "") : d.text;
    const next: Text = { ...d, text, height: this.textHeight({ ...d, text }) };
    const prev = ed.committed;
    const empty = text.trim().length === 0;
    if (!prev) {
      if (empty) return;
      s.commit({ label: "Add text", forward: [{ op: "addElements", elements: [next] }], inverse: [{ op: "removeElements", ids: [next.id] }] });
    } else if (empty && final) {
      s.commit({ label: "Delete text", forward: [{ op: "removeElements", ids: [prev.id] }], inverse: [{ op: "addElements", elements: [prev] }] });
      ed.committed = null;
      return;
    } else if (empty) {
      return;
    } else if (prev.text !== next.text || prev.color !== next.color || prev.fontSize !== next.fontSize || prev.font !== next.font || prev.height !== next.height || prev.y !== next.y || prev.rowPitch !== next.rowPitch) {
      s.commit({ label: "Edit text", forward: [{ op: "replaceElements", elements: [next] }], inverse: [{ op: "replaceElements", elements: [prev] }] });
    } else return;
    ed.committed = next;
    ed.isNew = false;
    // keep the edited block hidden on canvas while the text editor shows it
    this.renderer.hidden.add(next.id);
    this.renderer.renderText(textBounds(next));
  }

  /** Ends text editing (blur, Escape, tap outside, tool/page change, background). */
  endTextEdit() {
    const ed = this.edit;
    if (!ed) return;
    this.commitText(true);
    this.edit = null;
    this.renderer.hidden.delete(ed.id);
    this.textEditor?.destroy();
    this.textEditor = null;
    this.textHost.classList.add("hidden");
    this.editFrame.classList.add("hidden");
    const e = this.session?.elementById(ed.id);
    this.renderer.renderText(e ? textBounds(e as Text) : textBounds(ed.draft));
    this.emit({ editingText: false });
    this.updateNative();
  }

  /** Applies a toolbar formatting command to the text box being edited. */
  formatText(action: FormatAction) {
    this.textEditor?.format(action);
  }

  get isEditingText() {
    return !!this.edit;
  }

  // ------------------------------------------------- model changes --
  private onElementsChanged(c: { added: Element[]; removed: Element[]; templateChanged?: boolean }) {
    if (c.templateChanged && this.canvas && this.column !== this.wasColumn) {
      // to or from ruled paper: the canvas becomes a column, or stops being one, and is laid out again
      void this.setSession(this.session!, this.pdfCache);
      return;
    }
    if (c.templateChanged) {
      this.renderer.template = this.session!.page.template;
      this.renderer.renderBackground();
      this.emit({ pageHasRows: this.pageHasRows() });
    }
    const els = this.session!.page.elements;
    const maxZ = els.length ? els[els.length - 1].z : 0;
    const appendOnly = c.removed.length === 0 && c.added.length > 0 && c.added.every((e) => e.type === "ink" && e.z >= maxZ) && c.added.length <= 4;
    let reach: Rect | null = null;
    for (const e of c.added) reach = unionRect(reach, elementBounds(e));
    if (c.added.some(isPdfPage) || c.removed.some(isPdfPage)) this.syncPdfPages();
    if (reach && this.includeInExtent(reach)) {
      // the layers moved to take the change in, and were drawn again whole
    } else if (appendOnly) {
      for (const e of c.added) this.renderer.appendStroke(e as Ink);
    } else {
      let ink: Rect | null = null;
      let text: Rect | null = null;
      for (const e of [...c.added, ...c.removed]) {
        if (e.type === "text") text = unionRect(text, textBounds(e));
        else ink = unionRect(ink, elementBounds(e));
      }
      if (ink) this.renderer.renderInk(ink);
      if (text) this.renderer.renderText(text);
    }
    // keep the BOOX native layer in sync with edits made on the web side
    if (this.native?.ownsCommittedInk && this.nativeMounted && !this.nativeEcho) {
      const rm = c.removed.filter((e) => e.type === "ink").map((e) => e.id);
      const add = c.added.filter((e): e is Ink => e.type === "ink");
      if (rm.length) this.nativeCall((n) => n.removeStrokes(rm));
      if (add.length) this.nativeCall((n) => n.addStrokes(add.map((e) => this.nativeShown(e))));
    }
    if (this.selection.size) this.updateSelectionBox();
    if (this.editedTimer) clearTimeout(this.editedTimer);
    this.editedTimer = window.setTimeout(() => this.cb.onPageEdited(), 1500);
  }

  undo() {
    this.endTextEdit();
    this.cancelGestures();
    this.session?.undo();
  }

  redo() {
    this.endTextEdit();
    this.cancelGestures();
    this.session?.redo();
  }

  // =================================================== native ink ==

  setNativeSurface(n: NativeInkSurface | null) {
    if (this.native === n) return;
    void this.detachNative();
    this.native = n;
    this.updateNative();
  }

  /** Modal dialogs, menus and drawers over the canvas hide the native layer. */
  setOverlayBlocked(blocked: boolean) {
    this.overlayBlocked = blocked;
    this.updateNative();
  }

  private nativeTool(): NativeTool {
    const t = this.tool;
    if (t === "highlighter") return this.nativeShown({ tool: "highlighter", color: this.tools.highlighterColor, width: this.tools.highlighterWidth, pressure: false, opacity: HIGHLIGHTER_OPACITY });
    return this.nativeShown({ tool: t === "eraser" ? "eraser" : "pen", color: this.tools.penColor, width: this.tools.penWidth, pressure: this.tools.pressure, opacity: 1 });
  }

  private surfaceRect() {
    const r = this.viewport.getBoundingClientRect();
    return { x: r.left, y: r.top, width: r.width, height: r.height, dpr: window.devicePixelRatio || 1 };
  }

  private nativeCall(fn: (n: NativeInkSurface) => Promise<unknown>) {
    const n = this.native;
    if (!n) return;
    this.nativeQueue = this.nativeQueue.then(() => fn(n)).catch((e) => this.nativeFailed(e));
  }

  private nativeFailed(e: unknown) {
    this.cb.onError(`Native pen layer error: ${(e as Error)?.message ?? e}. Using the web canvas.`);
    const n = this.native;
    this.native = null;
    this.nativeMounted = false;
    this.nativeVisible = false;
    this.renderer.setInkHidden(false);
    this.emit({ nativeActive: false });
    n?.unmount().catch(() => {});
  }

  private syncNativeTransform() {
    if (this.native && this.nativeMounted) this.nativeCall((n) => n.setViewportTransform(this.transform));
  }

  private wantNative(): boolean {
    if (!this.native || !this.settings.useNativeInk || !this.session || this.overlayBlocked || this.edit) return false;
    if (this.native.kind === "ios-pencil") return this.tool === "pen" || this.tool === "highlighter";
    return DRAW_TOOLS.includes(this.tool);
  }

  private updateNative() {
    const n = this.native;
    if (!n) return;
    const want = this.wantNative();
    if (want && !this.nativeMounted) {
      this.nativeMounted = true;
      this.nativeCall(async (nn) => {
        await nn.mount(this.surfaceRect(), this.transform, this.nativeTool(), this.inkElements(), {
          strokeFinalized: (s) => this.onNativeStroke(s),
          penState: (down) => this.onNativePen(down),
          pencilTap: (action) => this.pencilTap(action),
          erased: (ids) => this.onNativeErased(ids),
          viewport: (t) => this.setTransform(t, true),
          error: (m) => this.cb.onError(m),
        });
      });
    }
    if (!this.nativeMounted) return;
    if (want === this.nativeVisible) {
      if (want) this.nativeCall((nn) => nn.setTool(this.nativeTool()));
      return;
    }
    this.nativeVisible = want;
    const owns = n.ownsCommittedInk;
    if (want) {
      this.nativeCall(async (nn) => {
        await nn.setTool(this.nativeTool());
        await nn.setBounds(this.surfaceRect());
        await nn.setViewportTransform(this.transform);
        await nn.setVisible(true);
        // native now shows the committed ink: hide ours to avoid doubled edges
        if (owns && this.nativeVisible) requestAnimationFrame(() => this.renderer.setInkHidden(true));
      });
    } else {
      // show web ink first, then remove the native layer: no blank frame
      this.renderer.setInkHidden(false);
      this.penDown = false; // the surface can no longer report a pen-up
      this.nativeCall((nn) => nn.setVisible(false));
    }
    this.emit({ nativeActive: want });
  }

  /**
   * The native surface owns the pen and commits its strokes, so `onDown` draws nothing for it;
   * this reports the pen as soon as the native layer sees it. A resting palm usually lands first and looks like a finger, so pen-down also drops any
   * pan or pinch in progress, and any stroke it started: the page must not move under the nib.
   */
  private onNativePen(down: boolean) {
    this.markPenSeen();
    if (!down) return this.liftPen();
    this.penDown = true;
    this.abandonFingerDraw();
    this.taskTap = null;
    this.touches.clear();
    this.pinch = null;
    this.pan = null;
  }

  private onNativeStroke(shown: InkStroke) {
    const stroke = this.nativeShown(shown);
    this.markPenSeen(); // surfaces that only report finished strokes (BOOX)
    const s = this.session;
    if (!s) return;
    const task = this.nativeTaskTap(stroke);
    if (task) {
      this.toggleTask(task);
      // the native layer has already drawn the dot, so it has to be told to let go of it
      return this.nativeCall((n) => (n.ownsCommittedInk ? n.removeStrokes([stroke.id]) : n.commitAck(stroke.id)));
    }
    const el: Ink = { ...stroke, type: "ink", z: s.nextZ() };
    this.nativeEcho = true;
    try {
      s.commit({ label: "Draw", forward: [{ op: "addElements", elements: [el] }], inverse: [{ op: "removeElements", ids: [el.id] }] });
    } finally {
      this.nativeEcho = false;
    }
    // preview surfaces drop their live stroke once our committed pixels are on screen
    if (this.native && !this.native.ownsCommittedInk) {
      requestAnimationFrame(() => requestAnimationFrame(() => this.nativeCall((n) => n.commitAck(el.id))));
    }
  }

  /** The checkbox a finished native pen stroke tapped, if the stroke was only a tap on one. */
  private nativeTaskTap(stroke: InkStroke): TaskHit | null {
    const start = stroke.points[0];
    if (!start || stroke.tool !== "pen" || this.tool !== "pen") return null;
    const slop = TAP_SLOP_PX / this.transform.scale;
    if (stroke.points.some((p) => Math.hypot(p.x - start.x, p.y - start.y) > slop)) return null;
    return this.taskAtPoint(start.x, start.y);
  }

  private onNativeErased(ids: string[]) {
    const s = this.session;
    if (!s) return;
    const els = ids.map((id) => s.elementById(id)).filter(Boolean) as Element[];
    if (!els.length) return;
    this.nativeEcho = true;
    try {
      s.commit({ label: "Erase", forward: [{ op: "removeElements", ids: els.map((e) => e.id) }], inverse: [{ op: "addElements", elements: els }] });
    } finally {
      this.nativeEcho = false;
    }
  }

  private async detachNative() {
    if (!this.native || !this.nativeMounted) return;
    this.nativeMounted = false;
    this.nativeVisible = false;
    this.renderer.setInkHidden(false);
    const n = this.native;
    this.nativeQueue = this.nativeQueue.then(() => n.unmount()).catch(() => {});
    await this.nativeQueue;
  }

  get nativeSurface(): NativeInkSurface | null {
    return this.native;
  }

  get nativeIsVisible(): boolean {
    return this.nativeVisible;
  }

  async nativeMetrics() {
    return this.native && this.nativeMounted ? this.native.metrics() : null;
  }

  // ===================================================== benchmark ==

  /** Benchmark: zoom `scale` at the top-left, backing store at scale * dpr (uncapped up to 64 Mpx). */
  benchView(scale: number) {
    this.transform = { scale, ox: 0, oy: 0 };
    this.applyTransform(false);
    if (this.settleTimer) clearTimeout(this.settleTimer);
    this.renderer.setResolution(scale * this.dpr);
    this.renderer.renderAll(null);
  }

  /** Visible-first redraw (what a zoom settle or page open costs before the first frame); ms, GPU-flushed. */
  visibleRedraw(): number {
    this.renderer.renderInkProgressive(this.visibleDocRect());
    return this.renderer.lastVisibleRedrawMs;
  }

  /** A settled zoom's redraw: the longest slice is the longest that input can be kept waiting; ms. */
  async rescaleBench(): Promise<{ totalMs: number; longestSliceMs: number }> {
    const t0 = performance.now();
    this.renderer.rescale(this.renderer.res * 1.2, this.visibleDocRect());
    await this.renderer.whenComplete();
    return { totalMs: performance.now() - t0, longestSliceMs: this.renderer.lastLongestSliceMs };
  }

  /** Full redraw of all layers' ink at the current resolution; ms (GPU-flushed). */
  fullRedraw(): number {
    this.renderer.renderInk(null);
    return this.renderer.lastFullRedrawMs;
  }

  lastReplayIntervals: number[] = [];
  /** Replays points through the live-preview and commit path, one sample per frame. */
  replay(points: InkPoint[], width = 2.2): Promise<number[]> {
    return new Promise((resolve) => {
      const stroke: Ink = { type: "ink", id: newId(), z: 0, tool: "pen", color: "#111111", opacity: 1, width, pressure: true, source: "fixture", input: { api: "replay", device: "script" }, points: [] };
      this.rect = this.viewport.getBoundingClientRect();
      this.active = { stroke, pointerId: -1, t0: performance.now(), stableDrawn: 0, coalesced: 0, predicted: [] };
      const times: number[] = [];
      this.lastReplayIntervals = [];
      let last = -1;
      let i = 0;
      const step = () => {
        const now = performance.now();
        if (last >= 0) this.lastReplayIntervals.push(now - last);
        last = now;
        if (i >= points.length) {
          this.finishStroke();
          return resolve(times);
        }
        stroke.points.push(points[i++]);
        const t0 = performance.now();
        if (this.active) this.drawActive(this.active);
        times.push(performance.now() - t0);
        requestAnimationFrame(step);
      };
      requestAnimationFrame(step);
    });
  }
}
