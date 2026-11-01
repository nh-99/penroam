// InkBackend adapter for native handwriting surfaces hosted by Tauri plugins:
//   Android: tauri-plugin-boox-ink  (shared Kotlin BooxInkView: Onyx raw drawing / native software)
//   iOS:     tauri-plugin-pencilkit (shared Swift PencilInk: PKCanvasView)
// Both plugins expose the same command names through one generic `call`
// command and report *completed* strokes as events; no pen sample crosses
// the IPC boundary before it is visible.
import type { PluginListener } from "@tauri-apps/api/core";
import { addPluginListener, invoke } from "@tauri-apps/api/core";
import type { InkStroke } from "./ink/model";
import { parseDocument } from "./ink/model";

export type NativeMode = "BOOX_HARDWARE" | "NATIVE_SOFTWARE" | "PENCILKIT";

export interface NativeCallbacks {
  onStrokes(strokes: InkStroke[]): void;
  onErased(ids: string[]): void;
  onViewport(t: { scale: number; offsetX: number; offsetY: number }): void;
  onStatus(s: any): void;
}

export class NativeInkBackend {
  private listeners: PluginListener[] = [];
  constructor(readonly plugin: "boox-ink" | "pencilkit") {}

  static forMode(mode: NativeMode) {
    return new NativeInkBackend(mode === "PENCILKIT" ? "pencilkit" : "boox-ink");
  }

  call<T = any>(method: string, args: Record<string, unknown> = {}): Promise<T> {
    return invoke<T>(`plugin:${this.plugin}|call`, { method, args });
  }

  async listen(cb: NativeCallbacks) {
    const on = async (ev: string, fn: (p: any) => void) => this.listeners.push(await addPluginListener(this.plugin, ev, fn));
    if (this.plugin === "pencilkit") {
      await on("strokes", (p) => cb.onStrokes(parseDocument(p.strokes)));
    } else {
      await on("stroke", (p) => cb.onStrokes(parseDocument(JSON.stringify([p.stroke]))));
    }
    await on("erased", (p) => { this.erasedHook?.(p.ids); cb.onErased(p.ids); });
    await on("viewport", (p) => cb.onViewport(p));
    await on("status", (p) => cb.onStatus(p));
  }

  async attach(args: {
    rect: { x: number; y: number; width: number; height: number; dpr: number };
    mode: NativeMode; width: number; pressure: boolean; tool: string;
    transform: { scale: number; offsetX: number; offsetY: number }; strokesJson: string;
  }): Promise<{ mode: string; capabilities: any; status: any }> {
    if (this.plugin === "pencilkit") {
      return this.call("attach", { ...args.rect, tool: args.tool, penWidth: args.width, strokes: args.strokesJson, transform: args.transform });
    }
    return this.call("attach", { ...args.rect, mode: args.mode, width: args.width, pressure: args.pressure, tool: args.tool,
      transform: args.transform, strokes: args.strokesJson });
  }

  setBounds(rect: { x: number; y: number; width: number; height: number; dpr: number }) { return this.call("setBounds", rect); }
  setBrush(width: number, pressure: boolean, tool: string) { return this.call("setBrush", { width, pressure, tool }); }
  setTransform(scale: number, offsetX: number, offsetY: number) { return this.call("setTransform", { scale, offsetX, offsetY }); }
  clear() { return this.call("clear"); }
  loadStrokes(json: string) { return this.call("loadStrokes", { strokes: json }); }
  getMetrics() { return this.call("getMetrics"); }
  /** Benchmarks: whole-stroke erase along a document-space path [x0, y0, x1, y1, ...]. */
  eraseAlong(points: number[]) { return this.call<{ removed: number }>("eraseAlong", { points }); }
  /** Benchmarks: observes "erased" events before the app callback. */
  erasedHook?: (ids: string[]) => void;

  async detach() {
    this.listeners.forEach((l) => l.unregister());
    this.listeners = [];
    await this.call("detach").catch(() => {});
  }
}
