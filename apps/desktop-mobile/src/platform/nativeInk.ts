// NativeInkSurface: typed boundary to the platform ink views hosted by
// plugins/tauri-plugin-native-ink. Vue never talks to a native view directly;
// the editor engine drives this facade.
//
//  * Android/BOOX ("owner" surface): the M00 BooxInkView (Onyx raw drawing,
//    Mode B) sits *above* the WebView with a transparent background. While
//    visible it renders the committed ink layer itself (so its hardware
//    preview hands over to identical software ink) and the web ink layer is
//    hidden. Page template, PDF and text stay visible through it.
//  * iOS/iPadOS ("preview" surface): a UIKit layer renders only the live
//    Pencil stroke at display rate. On pen-up the stroke is sent here, the web
//    renderer commits and paints it, then `commitAck` lets the native layer
//    drop its preview, so the stroke never flashes or doubles.
//
// Pen samples never cross IPC while a stroke is live: only finalised strokes.
import { addPluginListener, invoke, type PluginListener } from "@tauri-apps/api/core";
import type { InkStroke } from "../generated/InkStroke";
import type { InkPoint } from "../generated/InkPoint";
import { hasHost } from "./ipc";
import { isAndroid, isIOS } from "./device";

export type NativeKind = "boox" | "android-software" | "ios-pencil";

export interface SurfaceRect {
  x: number;
  y: number;
  width: number;
  height: number;
  dpr: number;
}

export interface NativeTool {
  tool: "pen" | "highlighter" | "eraser";
  color: string;
  width: number;
  pressure: boolean;
  opacity: number;
}

export interface NativeHandlers {
  strokeFinalized(stroke: InkStroke): void;
  erased(ids: string[]): void;
  viewport(t: { scale: number; ox: number; oy: number }): void;
  status?(s: Record<string, unknown>): void;
  error(message: string): void;
}

export interface NativeInkSurface {
  readonly kind: NativeKind;
  /** True when the native view renders committed ink while visible (BOOX). */
  readonly ownsCommittedInk: boolean;
  readonly capabilities: Record<string, unknown>;
  mount(rect: SurfaceRect, transform: { scale: number; ox: number; oy: number }, tool: NativeTool, strokes: InkStroke[], handlers: NativeHandlers): Promise<void>;
  unmount(): Promise<void>;
  setBounds(rect: SurfaceRect): Promise<void>;
  setViewportTransform(t: { scale: number; ox: number; oy: number }): Promise<void>;
  setTool(t: NativeTool): Promise<void>;
  setVisible(visible: boolean): Promise<void>;
  loadCommittedInk(strokes: InkStroke[]): Promise<void>;
  addStrokes(strokes: InkStroke[]): Promise<void>;
  removeStrokes(ids: string[]): Promise<void>;
  commitAck(strokeId: string): Promise<void>;
  metrics(): Promise<Record<string, unknown>>;
}

// --------------------------------------------------------- M00 interop --

/** M01 stroke -> M00 canonical stroke JSON (the native modules' format). */
export function inkToM00(s: InkStroke): Record<string, unknown> {
  return {
    id: s.id,
    source: s.source,
    input: s.input,
    brush: { type: s.tool, width: s.width, color: s.color, pressure: s.pressure, opacity: s.opacity },
    points: s.points.map((p) => ({ x: p.x, y: p.y, pressure: p.pressure, tiltX: p.tiltX, tiltY: p.tiltY, timeUs: p.timeUs })),
  };
}

/** M00 canonical stroke JSON -> M01 stroke (keeps every point field and input metadata). */
export function m00ToInk(raw: any, z: number, fallback: NativeTool): InkStroke {
  const brush = raw?.brush ?? {};
  const tool = brush.type === "highlighter" ? "highlighter" : fallback.tool === "highlighter" ? "highlighter" : "pen";
  const color = typeof brush.color === "string" && /^#[0-9a-fA-F]{6}$/.test(brush.color) ? brush.color.toLowerCase() : fallback.color;
  const points: InkPoint[] = (raw?.points ?? []).map((p: any) => {
    const q: InkPoint = {
      x: Number(p.x),
      y: Number(p.y),
      pressure: Math.min(1, Math.max(0, Number(p.pressure ?? 0.5))),
      tiltX: Number(p.tiltX ?? 0),
      tiltY: Number(p.tiltY ?? 0),
      timeUs: Math.round(Number(p.timeUs ?? 0)),
    };
    if (p.twist != null) q.twist = Number(p.twist);
    if (p.flags != null) q.flags = Number(p.flags);
    return q;
  });
  return {
    id: String(raw.id),
    z,
    tool,
    color,
    opacity: typeof brush.opacity === "number" ? brush.opacity : tool === "highlighter" ? fallback.opacity : 1,
    width: Number(brush.width ?? fallback.width),
    pressure: brush.pressure !== false && tool === "pen",
    source: String(raw.source ?? "stylus"),
    input: { ...(raw.input ?? {}) },
    points,
  };
}

const doc = (strokes: InkStroke[]) => JSON.stringify({ format: "m00-strokes", version: 1, meta: {}, strokes: strokes.map(inkToM00) });

// ------------------------------------------------------------- plugin --

class PluginSurface implements NativeInkSurface {
  private listeners: PluginListener[] = [];
  private handlers: NativeHandlers | null = null;
  private tool: NativeTool | null = null;
  private zNext = 1;

  constructor(
    readonly kind: NativeKind,
    readonly capabilities: Record<string, unknown>,
  ) {}

  get ownsCommittedInk() {
    return this.kind !== "ios-pencil";
  }

  private call<T = any>(method: string, args: Record<string, unknown> = {}): Promise<T> {
    return invoke<T>("plugin:native-ink|call", { method, args });
  }

  async mount(rect: SurfaceRect, t: { scale: number; ox: number; oy: number }, tool: NativeTool, strokes: InkStroke[], handlers: NativeHandlers) {
    this.handlers = handlers;
    this.tool = tool;
    this.zNext = strokes.reduce((m, s) => Math.max(m, s.z), 0) + 1;
    const on = async (ev: string, fn: (p: any) => void) => this.listeners.push(await addPluginListener("native-ink", ev, fn));
    await on("stroke", (p) => {
      try {
        const raw = typeof p.stroke === "string" ? JSON.parse(p.stroke) : p.stroke;
        this.handlers?.strokeFinalized(m00ToInk(raw, this.zNext++, this.tool!));
      } catch (e) {
        this.handlers?.error(`native stroke could not be read: ${e}`);
      }
    });
    await on("erased", (p) => this.handlers?.erased((p.ids ?? []).map(String)));
    await on("viewport", (p) => this.handlers?.viewport({ scale: p.scale, ox: p.offsetX, oy: p.offsetY }));
    await on("status", (p) => this.handlers?.status?.(p));
    await on("error", (p) => this.handlers?.error(String(p.message ?? p)));
    await this.call("attach", {
      ...rect,
      mode: this.kind === "boox" ? "BOOX_HARDWARE" : "NATIVE_SOFTWARE",
      ...this.toolArgs(tool),
      transform: { scale: t.scale, offsetX: t.ox, offsetY: t.oy },
      strokes: this.ownsCommittedInk ? doc(strokes) : "",
      transparent: true,
    });
  }

  private toolArgs(t: NativeTool) {
    // `penWidth`, not `width`: attach also carries the overlay rect width
    return { tool: t.tool, color: t.color, penWidth: t.width, pressure: t.pressure, opacity: t.opacity };
  }

  async unmount() {
    this.listeners.forEach((l) => l.unregister());
    this.listeners = [];
    this.handlers = null;
    await this.call("detach").catch(() => {});
  }

  setBounds(rect: SurfaceRect) {
    return this.call("setBounds", { ...rect });
  }
  setViewportTransform(t: { scale: number; ox: number; oy: number }) {
    return this.call("setTransform", { scale: t.scale, offsetX: t.ox, offsetY: t.oy });
  }
  setTool(t: NativeTool) {
    this.tool = t;
    return this.call("setBrush", this.toolArgs(t));
  }
  setVisible(visible: boolean) {
    return this.call("setVisible", { visible });
  }
  loadCommittedInk(strokes: InkStroke[]) {
    this.zNext = strokes.reduce((m, s) => Math.max(m, s.z), 0) + 1;
    return this.ownsCommittedInk ? this.call("loadStrokes", { strokes: doc(strokes) }) : Promise.resolve();
  }
  addStrokes(strokes: InkStroke[]) {
    for (const s of strokes) this.zNext = Math.max(this.zNext, s.z + 1);
    return this.ownsCommittedInk && strokes.length ? this.call("addStrokes", { strokes: doc(strokes) }) : Promise.resolve();
  }
  removeStrokes(ids: string[]) {
    return this.ownsCommittedInk && ids.length ? this.call("removeStrokes", { ids }) : Promise.resolve();
  }
  commitAck(strokeId: string) {
    return this.call("commitAck", { id: strokeId });
  }
  metrics() {
    return this.call("getMetrics");
  }
}

let probed: Promise<NativeInkSurface | null> | null = null;

/** Detects the platform ink surface once. Desktop: always null (web canvas). */
export function detectNativeInk(): Promise<NativeInkSurface | null> {
  if (probed) return probed;
  probed = (async () => {
    if (!hasHost() || !(isAndroid() || isIOS())) return null;
    try {
      const caps = await invoke<Record<string, unknown>>("plugin:native-ink|call", { method: "getCapabilities", args: {} });
      if (isIOS()) return caps.pencilSurface ? new PluginSurface("ios-pencil", caps) : null;
      if (caps.nativeModeCandidate) return new PluginSurface("boox", caps);
      return null; // other Android devices: the web canvas is the validated path
    } catch (e) {
      console.warn("native ink unavailable", e);
      return null;
    }
  })();
  return probed;
}
