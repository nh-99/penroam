// Canvas 2D ink engine. Deliberately framework-free: React only renders the
// toolbar/debug UI and never sees per-sample data.
//
// Layers (separating the four concepts from PLAN.md §5):
//   raw input samples   -> this.active.points (+ per-sample timestamps)
//   active preview      -> `active` canvas (stable pieces, incremental) and
//                          `tail` canvas (unstable trailing piece + predicted
//                          points, cleared every frame)
//   committed strokes   -> this.strokes (vector data, source of truth)
//   rendered cache      -> `committed` canvas (rebuilt only on transform,
//                          erase, load, resize)
import { drawPieces, drawStroke, hitTest, pieceCount, RenderStyle, stablePieceCount, strokeBounds } from "./geometry";
import { analyzeSamples, Metrics } from "./metrics";
import { InkPoint, InkStroke, newId } from "./model";

export type Tool = "pen" | "eraser";

export interface ViewTransform {
  scale: number;
  ox: number; // CSS px
  oy: number;
}

export interface EngineSettings {
  width: number;
  pressure: boolean;
  tool: Tool;
  renderStyle: RenderStyle;
  fingerDraws: boolean;
  usePredicted: boolean;
}

interface ActiveStroke {
  stroke: InkStroke;
  pointerId: number;
  device: string;
  eraser: boolean;
  t0: number; // event timestamp of first sample
  ts: number[]; // event timestamps (ms) for gap analysis
  stableDrawn: number;
  coalesced: number;
  predicted: InkPoint[];
  predictedShown: number;
  erased: string[];
}

export const ERASER_RADIUS = 8;
const PALM_CONTACT_CSS_PX = 40;

export interface ApiSupport {
  pointerEvents: boolean;
  pointerRawUpdate: boolean;
  getCoalescedEvents: boolean;
  getPredictedEvents: boolean;
  setPointerCapture: boolean;
  offscreenCanvas: boolean;
  timestampClockOk: boolean | null;
  userAgent: string;
  devicePixelRatio: number;
}

export function detectApis(): ApiSupport {
  const pe = typeof PointerEvent !== "undefined";
  return {
    pointerEvents: pe,
    pointerRawUpdate: typeof window !== "undefined" && "onpointerrawupdate" in window,
    getCoalescedEvents: pe && typeof PointerEvent.prototype.getCoalescedEvents === "function",
    getPredictedEvents: pe && typeof (PointerEvent.prototype as any).getPredictedEvents === "function",
    setPointerCapture: typeof Element !== "undefined" && typeof Element.prototype.setPointerCapture === "function",
    offscreenCanvas: typeof OffscreenCanvas !== "undefined",
    timestampClockOk: null,
    userAgent: typeof navigator !== "undefined" ? navigator.userAgent : "",
    devicePixelRatio: typeof window !== "undefined" ? window.devicePixelRatio : 1,
  };
}

export class InkEngine {
  strokes: InkStroke[] = [];
  metrics = new Metrics();
  transform: ViewTransform = { scale: 1, ox: 0, oy: 0 };
  settings: EngineSettings = { width: 3, pressure: true, tool: "pen", renderStyle: "smooth", fingerDraws: false, usePredicted: false };
  apis = detectApis();
  inputEnabled = true;
  onChange?: () => void;
  onTransform?: (t: ViewTransform) => void;

  private committed: HTMLCanvasElement;
  private activeCv: HTMLCanvasElement;
  private tail: HTMLCanvasElement;
  private cctx: CanvasRenderingContext2D;
  private actx: CanvasRenderingContext2D;
  private tctx: CanvasRenderingContext2D;
  private dpr = 1;
  private cssW = 0;
  private cssH = 0;
  private rect: DOMRect | null = null;
  private active: ActiveStroke | null = null;
  private frameRequested = false;
  private rebuildRequested = false;
  private pendingSinceEventTs = 0;
  private lastFrameTs = 0;
  private touches = new Map<number, { x: number; y: number }>();
  private pinch: { dist: number; scale: number; ax: number; ay: number } | null = null;
  private panMouse: { x: number; y: number } | null = null;
  private penDown = false;
  private sampleEvent: "pointerrawupdate" | "pointermove";
  private ro: ResizeObserver;
  private disposers: (() => void)[] = [];

  constructor(private container: HTMLElement) {
    const mk = () => {
      const c = document.createElement("canvas");
      c.style.cssText = "position:absolute;inset:0;width:100%;height:100%;touch-action:none;";
      container.appendChild(c);
      return c;
    };
    this.committed = mk();
    this.activeCv = mk();
    this.tail = mk();
    const ctx = (c: HTMLCanvasElement) => {
      const x = c.getContext("2d", { desynchronized: true } as CanvasRenderingContext2DSettings)!;
      return x;
    };
    this.cctx = ctx(this.committed);
    this.actx = ctx(this.activeCv);
    this.tctx = ctx(this.tail);
    container.style.touchAction = "none";
    container.style.userSelect = "none";
    (container.style as any).webkitUserSelect = "none";

    this.sampleEvent = this.apis.pointerRawUpdate ? "pointerrawupdate" : "pointermove";
    this.metrics.info.apis = this.apis;
    this.metrics.info.sampleEvent = this.sampleEvent;
    for (const [k, v] of Object.entries(this.apis)) {
      if (v === false) this.metrics.event("api_unavailable", { api: k });
    }

    const on = <K extends keyof HTMLElementEventMap>(type: K | string, fn: (e: any) => void, opts?: AddEventListenerOptions) => {
      container.addEventListener(type, fn, opts);
      this.disposers.push(() => container.removeEventListener(type, fn, opts));
    };
    on("pointerdown", (e: PointerEvent) => this.onDown(e));
    on("pointermove", (e: PointerEvent) => this.onMove(e));
    if (this.apis.pointerRawUpdate) on("pointerrawupdate", (e: PointerEvent) => this.onMove(e));
    on("pointerup", (e: PointerEvent) => this.onUp(e, false));
    on("pointercancel", (e: PointerEvent) => this.onUp(e, true));
    on("lostpointercapture", (e: PointerEvent) => { if (this.active?.pointerId === e.pointerId) this.onUp(e, true); });
    on("wheel", (e: WheelEvent) => this.onWheel(e), { passive: false });
    on("contextmenu", (e: Event) => e.preventDefault());

    this.ro = new ResizeObserver(() => this.resize());
    this.ro.observe(container);
    this.resize();
  }

  dispose() {
    this.ro.disconnect();
    this.disposers.forEach((d) => d());
    this.container.replaceChildren();
  }

  // ------------------------------------------------------------ public --
  setSettings(s: Partial<EngineSettings>) {
    const styleChanged = s.renderStyle !== undefined && s.renderStyle !== this.settings.renderStyle;
    this.settings = { ...this.settings, ...s };
    if (styleChanged) this.requestRebuild();
  }

  setTransform(t: ViewTransform, notify = false) {
    this.transform = { scale: Math.min(8, Math.max(0.25, t.scale)), ox: t.ox, oy: t.oy };
    this.requestRebuild();
    if (notify) this.onTransform?.(this.transform);
  }

  setStrokes(strokes: InkStroke[]): { renderMs: number } {
    this.strokes = strokes;
    const t0 = performance.now();
    this.rebuildNow();
    return { renderMs: performance.now() - t0 };
  }

  addCommittedStroke(s: InkStroke) {
    this.strokes.push(s);
    this.drawCommitted(s);
    this.onChange?.();
  }

  removeStrokes(ids: string[]) {
    const set = new Set(ids);
    this.strokes = this.strokes.filter((s) => !set.has(s.id));
    this.requestRebuild();
    this.onChange?.();
  }

  clear() {
    this.strokes = [];
    this.active = null;
    this.rebuildNow();
    this.clearLayer(this.actx);
    this.clearLayer(this.tctx);
    this.metrics.event("clear_canvas");
    this.onChange?.();
  }

  /** Forces the GPU/compositor to finish pending canvas work (for timing). */
  flush(): void {
    this.cctx.getImageData(0, 0, 1, 1);
  }

  /** Full rebuild of the committed cache; returns duration in ms (flushed). */
  rebuildNow(): number {
    const t0 = performance.now();
    this.rebuildRequested = false;
    const c = this.cctx;
    this.clearLayer(c);
    this.applyDocTransform(c);
    const [vx0, vy0] = this.toDoc(0, 0);
    const [vx1, vy1] = this.toDoc(this.cssW, this.cssH);
    let drawn = 0;
    for (const s of this.strokes) {
      const b = strokeBounds(s);
      if (b[2] < vx0 || b[0] > vx1 || b[3] < vy0 || b[1] > vy1) continue;
      drawStroke(c, s, this.settings.renderStyle);
      drawn++;
    }
    this.flush();
    const ms = performance.now() - t0;
    this.metrics.timing("full_rebuild_ms", ms);
    this.metrics.count("full_rebuilds");
    this.metrics.info.lastRebuild = { strokes: this.strokes.length, drawn, ms };
    return ms;
  }

  viewSize() {
    return { width: this.cssW, height: this.cssH, dpr: this.dpr };
  }

  // ------------------------------------------------------------ layout --
  private resize() {
    const r = this.container.getBoundingClientRect();
    this.dpr = window.devicePixelRatio || 1;
    this.cssW = r.width;
    this.cssH = r.height;
    for (const c of [this.committed, this.activeCv, this.tail]) {
      c.width = Math.max(1, Math.round(r.width * this.dpr));
      c.height = Math.max(1, Math.round(r.height * this.dpr));
    }
    for (const x of [this.cctx, this.actx, this.tctx]) this.prepCtx(x);
    this.metrics.event("resize", { w: r.width, h: r.height, dpr: this.dpr });
    this.rebuildNow();
  }

  private prepCtx(x: CanvasRenderingContext2D) {
    x.lineCap = "round";
    x.lineJoin = "round";
    x.strokeStyle = "#000";
    x.fillStyle = "#000";
  }

  private clearLayer(x: CanvasRenderingContext2D) {
    x.setTransform(1, 0, 0, 1, 0, 0);
    x.clearRect(0, 0, x.canvas.width, x.canvas.height);
  }

  private applyDocTransform(x: CanvasRenderingContext2D) {
    const { scale, ox, oy } = this.transform;
    const d = this.dpr;
    x.setTransform(d * scale, 0, 0, d * scale, d * ox, d * oy);
  }

  toDoc(cssX: number, cssY: number): [number, number] {
    const { scale, ox, oy } = this.transform;
    return [(cssX - ox) / scale, (cssY - oy) / scale];
  }

  // ------------------------------------------------------------- input --
  private onDown(e: PointerEvent) {
    const t = performance.now();
    this.metrics.count("pointer_event_received");
    this.recordEventClock(e, t);
    if (!this.inputEnabled) return;
    this.rect = this.container.getBoundingClientRect();

    if (e.pointerType === "touch") {
      if (this.penDown || (this.active && this.active.device !== "touch")) return void this.metrics.count("palm_rejected_while_pen");
      if (Math.max(e.width, e.height) > PALM_CONTACT_CSS_PX) return void this.metrics.count("palm_rejected_large_contact");
      if (!(this.settings.fingerDraws && this.touches.size === 0 && !this.active)) {
        this.touches.set(e.pointerId, this.local(e));
        this.capture(e);
        if (this.touches.size === 2) this.beginPinch();
        return;
      }
    }
    if (e.pointerType === "mouse" && e.button === 1) {
      this.panMouse = this.local(e);
      this.capture(e);
      return;
    }
    if (e.pointerType === "mouse" && e.button !== 0) return;
    if (this.active) return; // single drawing pointer

    if (e.pointerType === "pen") this.penDown = true;
    this.capture(e);
    const eraser = this.settings.tool === "eraser" || (e.buttons & 32) !== 0 || e.button === 5;
    const device = e.pointerType || "unknown";
    const stroke: InkStroke = {
      id: newId(),
      source: device === "pen" ? "stylus" : device,
      input: { api: "web-pointer", device, sampleEvent: this.sampleEvent, coalesced: this.apis.getCoalescedEvents },
      brush: { type: "pen", width: this.settings.width, color: "#000000", pressure: this.settings.pressure && device !== "mouse" },
      points: [],
    };
    this.active = { stroke, pointerId: e.pointerId, device, eraser, t0: e.timeStamp, ts: [], stableDrawn: 0, coalesced: 0, predicted: [], predictedShown: 0, erased: [] };
    this.addSample(e);
    this.afterInput(e, t);
  }

  private onMove(e: PointerEvent) {
    const t = performance.now();
    if (this.touches.has(e.pointerId) || this.panMouse) {
      if (e.type === "pointermove") this.navigate(e);
      return;
    }
    // When pointerrawupdate is supported both it and pointermove arrive;
    // only one stream is consumed so samples are never duplicated.
    if (e.type !== this.sampleEvent) return;
    this.metrics.count("pointer_event_received");
    this.recordEventClock(e, t);
    const a = this.active;
    if (!a || a.pointerId !== e.pointerId || !this.inputEnabled) return;
    const list = this.apis.getCoalescedEvents ? e.getCoalescedEvents() : [];
    if (list.length > 0) {
      a.coalesced += list.length;
      for (const ce of list) this.addSample(ce);
    } else this.addSample(e);
    if (this.settings.usePredicted && this.apis.getPredictedEvents) {
      a.predicted = (e as any).getPredictedEvents().map((pe: PointerEvent) => this.samplePoint(pe, a));
    }
    this.afterInput(e, t);
  }

  private onUp(e: PointerEvent, cancelled: boolean) {
    if (e.pointerType === "pen") this.penDown = false;
    if (this.touches.delete(e.pointerId)) {
      if (this.touches.size < 2) this.pinch = null;
      if (this.touches.size === 0) this.onTransform?.(this.transform);
      return;
    }
    if (this.panMouse && e.pointerType === "mouse") {
      this.panMouse = null;
      this.onTransform?.(this.transform);
      return;
    }
    const a = this.active;
    if (!a || a.pointerId !== e.pointerId) return;
    if (!cancelled) {
      const p = this.samplePoint(e, a);
      const last = a.stroke.points[a.stroke.points.length - 1];
      if (!last || last.x !== p.x || last.y !== p.y) this.addSample(e);
    } else this.metrics.count("stroke_cancelled");
    this.finishStroke();
  }

  private onWheel(e: WheelEvent) {
    e.preventDefault();
    const r = this.container.getBoundingClientRect();
    const x = e.clientX - r.left, y = e.clientY - r.top;
    if (e.ctrlKey || e.metaKey) {
      const [ax, ay] = this.toDoc(x, y);
      const scale = Math.min(8, Math.max(0.25, this.transform.scale * Math.exp(-e.deltaY * 0.002)));
      this.setTransform({ scale, ox: x - ax * scale, oy: y - ay * scale }, true);
    } else {
      this.setTransform({ ...this.transform, ox: this.transform.ox - e.deltaX, oy: this.transform.oy - e.deltaY }, true);
    }
  }

  private capture(e: PointerEvent) {
    if (!this.apis.setPointerCapture) return;
    try {
      this.container.setPointerCapture(e.pointerId);
    } catch (err) {
      this.metrics.error(`setPointerCapture: ${err}`);
    }
  }

  private local(e: PointerEvent) {
    const r = this.rect ?? this.container.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  private recordEventClock(e: PointerEvent, handlerT: number) {
    const d = handlerT - e.timeStamp;
    if (this.apis.timestampClockOk === null) {
      this.apis.timestampClockOk = d >= 0 && d < 1000;
      this.metrics.event("timestamp_clock_check", { deltaMs: d, ok: this.apis.timestampClockOk });
    }
    if (this.apis.timestampClockOk) this.metrics.timing("event_to_handler_ms", d);
  }

  private samplePoint(e: PointerEvent, a: ActiveStroke): InkPoint {
    const { x, y } = this.local(e);
    const [dx, dy] = this.toDoc(x, y);
    const pressure = a.device === "mouse" ? 0.5 : e.pressure > 0 ? e.pressure : 0.5;
    return { x: dx, y: dy, pressure, tiltX: e.tiltX ?? 0, tiltY: e.tiltY ?? 0, timeUs: Math.round((e.timeStamp - a.t0) * 1000) };
  }

  private addSample(e: PointerEvent) {
    const a = this.active!;
    const p = this.samplePoint(e, a);
    if (a.eraser) {
      a.stroke.points.push(p);
      this.eraseAt(p.x, p.y);
      return;
    }
    a.stroke.points.push(p);
    a.ts.push(e.timeStamp);
    if (e.twist) a.stroke.input.twistSeen = true;
    this.metrics.count("input_samples");
  }

  private afterInput(e: PointerEvent, handlerStart: number) {
    this.metrics.since("input_sample_processed_ms", handlerStart);
    if (!this.pendingSinceEventTs) this.pendingSinceEventTs = e.timeStamp;
    this.scheduleFrame();
  }

  private navigate(e: PointerEvent) {
    const p = this.local(e);
    if (this.panMouse) {
      const dx = p.x - this.panMouse.x, dy = p.y - this.panMouse.y;
      this.panMouse = p;
      return this.setTransform({ ...this.transform, ox: this.transform.ox + dx, oy: this.transform.oy + dy });
    }
    const prev = this.touches.get(e.pointerId);
    if (!prev) return;
    this.touches.set(e.pointerId, p);
    if (this.touches.size >= 2 && this.pinch) {
      const [a, b] = [...this.touches.values()];
      const dist = Math.max(1, Math.hypot(b.x - a.x, b.y - a.y));
      const fx = (a.x + b.x) / 2, fy = (a.y + b.y) / 2;
      const scale = Math.min(8, Math.max(0.25, (this.pinch.scale * dist) / this.pinch.dist));
      this.setTransform({ scale, ox: fx - this.pinch.ax * scale, oy: fy - this.pinch.ay * scale });
    } else if (this.touches.size === 1) {
      this.setTransform({ ...this.transform, ox: this.transform.ox + p.x - prev.x, oy: this.transform.oy + p.y - prev.y });
    }
  }

  private beginPinch() {
    const [a, b] = [...this.touches.values()];
    const fx = (a.x + b.x) / 2, fy = (a.y + b.y) / 2;
    const [ax, ay] = this.toDoc(fx, fy);
    this.pinch = { dist: Math.max(1, Math.hypot(b.x - a.x, b.y - a.y)), scale: this.transform.scale, ax, ay };
  }

  private eraseAt(x: number, y: number) {
    const r = ERASER_RADIUS / this.transform.scale;
    const keep: InkStroke[] = [];
    let removed = 0;
    for (const s of this.strokes) {
      if (hitTest(s, x, y, r)) {
        this.active?.erased.push(s.id);
        removed++;
      } else keep.push(s);
    }
    if (removed) {
      this.strokes = keep;
      this.metrics.count("strokes_erased", removed);
      this.requestRebuild();
    }
  }

  // ------------------------------------------------------------ render --
  private requestRebuild() {
    this.rebuildRequested = true;
    this.scheduleFrame();
  }

  private scheduleFrame() {
    if (this.frameRequested) return;
    this.frameRequested = true;
    this.metrics.count("preview_update_scheduled");
    requestAnimationFrame((ts) => this.frame(ts));
  }

  private frame(ts: number) {
    this.frameRequested = false;
    if (this.lastFrameTs) {
      const dt = ts - this.lastFrameTs;
      if (dt < 250) {
        this.metrics.timing("frame_interval_ms", dt);
        if (dt > 16.7 * 1.5) this.metrics.count("frames_over_budget");
      }
    }
    this.lastFrameTs = ts;
    const t0 = performance.now();
    if (this.rebuildRequested) this.rebuildNow();
    const a = this.active;
    if (a && !a.eraser) {
      const n = a.stroke.points.length;
      const stable = stablePieceCount(n);
      if (stable > a.stableDrawn) {
        this.applyDocTransform(this.actx);
        drawPieces(this.actx, a.stroke, a.stableDrawn, stable, this.settings.renderStyle);
        a.stableDrawn = stable;
      }
      this.clearLayer(this.tctx);
      this.applyDocTransform(this.tctx);
      drawPieces(this.tctx, a.stroke, a.stableDrawn, pieceCount(n), this.settings.renderStyle);
      if (a.predicted.length) {
        // Predicted points are a temporary preview only; never committed.
        const tmp: InkStroke = { ...a.stroke, points: [a.stroke.points[n - 1], ...a.predicted] };
        this.tctx.globalAlpha = 0.5;
        drawPieces(this.tctx, tmp, 0, pieceCount(tmp.points.length), this.settings.renderStyle);
        this.tctx.globalAlpha = 1;
        a.predictedShown += a.predicted.length;
      }
    }
    this.metrics.since("preview_render_submitted_ms", t0);
    if (this.pendingSinceEventTs) {
      if (this.apis.timestampClockOk) this.metrics.timing("event_to_render_ms", performance.now() - this.pendingSinceEventTs);
      this.pendingSinceEventTs = 0;
      // Approximation of "frame presented": the next rAF after submission.
      const submitted = performance.now();
      requestAnimationFrame(() => this.metrics.timing("render_to_next_frame_ms", performance.now() - submitted));
    }
  }

  private finishStroke() {
    const a = this.active!;
    this.active = null;
    if (a.eraser) {
      this.metrics.event("erase_finished", { removed: a.erased.length });
      this.onChange?.();
      return;
    }
    const t0 = performance.now();
    this.metrics.event("stroke_finalized", { points: a.stroke.points.length });
    this.strokes.push(a.stroke);
    this.clearLayer(this.actx);
    this.clearLayer(this.tctx);
    this.drawCommitted(a.stroke);
    const commitMs = performance.now() - t0;
    this.metrics.timing("persistent_render_completed_ms", commitMs);
    const pts = a.stroke.points;
    const g = analyzeSamples(a.ts, pts.map((p) => p.x), pts.map((p) => p.y));
    this.metrics.stroke({
      id: a.stroke.id,
      device: a.device,
      samples: pts.length,
      coalescedSamples: a.coalesced,
      predictedSamplesShown: a.predictedShown,
      durationMs: a.ts.length ? a.ts[a.ts.length - 1] - a.ts[0] : 0,
      commitMs,
      ...g,
    });
    this.onChange?.();
  }

  private drawCommitted(s: InkStroke) {
    this.applyDocTransform(this.cctx);
    drawStroke(this.cctx, s, this.settings.renderStyle);
  }

  // ------------------------------------------------- scripted replay --
  lastReplayIntervals: number[] = [];
  /**
   * Replays a stroke through the same active-preview + commit path, one
   * sample per animation frame (Workload D "responsiveness while adding
   * strokes"). Resolves with per-frame preview render times.
   */
  replay(points: InkPoint[], width = 3): Promise<number[]> {
    return new Promise((resolve) => {
      const stroke: InkStroke = { id: newId(), source: "fixture", input: { api: "replay", device: "script" }, brush: { type: "pen", width, color: "#000000", pressure: true }, points: [] };
      this.active = { stroke, pointerId: -1, device: "script", eraser: false, t0: performance.now(), ts: [], stableDrawn: 0, coalesced: 0, predicted: [], predictedShown: 0, erased: [] };
      const times: number[] = [];
      // Time between successive rAF steps = the frame interval the page actually got
      // (comparable with Flutter's replay interval; `times` is JS draw cost only).
      this.lastReplayIntervals = [];
      let lastStep = -1;
      let i = 0;
      const step = () => {
        const now = performance.now();
        if (lastStep >= 0) this.lastReplayIntervals.push(now - lastStep);
        lastStep = now;
        if (i >= points.length) {
          this.finishStroke();
          return resolve(times);
        }
        stroke.points.push(points[i]);
        this.active!.ts.push(performance.now());
        i++;
        const t0 = performance.now();
        this.frame(performance.now());
        times.push(performance.now() - t0);
        requestAnimationFrame(step);
      };
      requestAnimationFrame(step);
    });
  }
}
