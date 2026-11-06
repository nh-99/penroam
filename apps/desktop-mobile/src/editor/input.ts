// Pointer input normalisation (pure functions; unit-tested).
import type { InkPoint } from "../generated/InkPoint";
import { toDoc, type ViewTransform } from "./viewport";

export type PointerKind = "pen" | "mouse" | "touch";

export const FLAG_COALESCED = 2;
/**
 * Contacts wider than this (CSS px) are treated as a palm. Not valid on iOS, where WebKit reports
 * twice the touch radius and ordinary fingertips exceed it; see `EditorSettings.palmContactPx`.
 */
export const PALM_CONTACT_PX = 40;

export interface RawSample {
  clientX: number;
  clientY: number;
  pressure: number;
  tiltX?: number;
  tiltY?: number;
  twist?: number;
  timeStamp: number;
  pointerType: string;
}

/** Name of the curve below, recorded in each stroke's `input`. */
export const PENCIL_CURVE = "exp-4.6";
const PENCIL_CURVE_K = 4.6;

/**
 * Apple Pencil force fraction -> canonical pressure. Writing uses only the bottom quarter of the
 * Pencil's range, so this puts it mid-range, where the width formula expects it. Same curve as
 * `pencilPressure` in InkKit, so both iPad input paths agree.
 */
export function pencilPressure(fraction: number): number {
  const x = Math.min(1, Math.max(0, fraction));
  return (1 - Math.exp(-PENCIL_CURVE_K * x)) / (1 - Math.exp(-PENCIL_CURVE_K));
}

const r3 = (v: number) => Math.round(v * 1000) / 1000;
const r4 = (v: number) => Math.round(v * 10000) / 10000;

/**
 * Maps one pointer sample to a canonical document point. Coordinates are
 * rounded to 0.001 pt and pressure to 1e-4 (capture precision; far below
 * digitiser resolution). Mouse and pressure-less pens record 0.5, as in the canonical stroke format.
 */
export function samplePoint(e: RawSample, rect: { left: number; top: number }, t: ViewTransform, t0: number, coalesced = false, pencilCurve = false): InkPoint {
  const [x, y] = toDoc(t, e.clientX - rect.left, e.clientY - rect.top);
  const kind = e.pointerType;
  const raw = e.pressure > 0 ? Math.min(1, e.pressure) : 0.5;
  const pressure = kind === "mouse" ? 0.5 : pencilCurve && kind === "pen" && e.pressure > 0 ? pencilPressure(raw) : raw;
  const p: InkPoint = {
    x: r3(x),
    y: r3(y),
    pressure: r4(pressure),
    tiltX: e.tiltX ?? 0,
    tiltY: e.tiltY ?? 0,
    timeUs: Math.max(0, Math.round((e.timeStamp - t0) * 1000)),
  };
  if (e.twist) p.twist = e.twist;
  if (coalesced) p.flags = FLAG_COALESCED;
  return p;
}

/**
 * Pen buttons that turn the pen into an eraser while held. An eraser end reports button 5 /
 * buttons bit 32. A side button reports the right button, which is how the S Pen's button reaches
 * Android WebView and the barrel button of most Windows and Linux pens; on a pen it has no
 * context menu to open, so it does the job that the S Pen has no eraser tip for.
 */
export function penErases(e: { pointerType: string; button: number; buttons: number }): boolean {
  if (e.pointerType !== "pen") return false;
  return (e.buttons & 32) !== 0 || e.button === 5 || (e.buttons & 2) !== 0 || e.button === 2;
}

/** Whether a stroke from this pointer should use pressure for width. */
export function pressureCapable(kind: string, firstPressure: number): boolean {
  if (kind === "mouse") return false;
  // touch on most hardware reports 0 or a constant 0.5/1: treat as no pressure
  if (kind === "touch") return false;
  return firstPressure > 0 && firstPressure !== 0.5;
}

/**
 * Whether a pen stroke that began without usable pressure has turned out to have it. Some
 * digitisers report 0 or 0.5 on contact and real values only after that, so the first sample
 * cannot settle the question.
 */
export function pressureArrived(kind: string, pressure: number): boolean {
  return kind === "pen" && pressure > 0 && pressure !== 0.5;
}

export type PenOnlyMode = "auto" | "on" | "off";

/**
 * Decides what a new pointer does under the drawing tools.
 *  - "draw": start a stroke / erase
 *  - "navigate": pan/zoom (fingers when pen-only is active, middle mouse)
 *  - "ignore": palm contact, second drawing pointer, secondary buttons
 */
export function classifyPointer(
  e: { pointerType: string; button: number; width: number; height: number },
  state: { penOnly: PenOnlyMode; penSeen: boolean; penDown: boolean; drawingActive: boolean; touches: number; palmPx?: number },
): "draw" | "navigate" | "ignore" {
  if (e.pointerType === "touch") {
    if (state.penDown) return "ignore";
    if (Math.max(e.width, e.height) > (state.palmPx ?? PALM_CONTACT_PX)) return "ignore";
    const penOnly = state.penOnly === "on" || (state.penOnly === "auto" && state.penSeen);
    if (penOnly || state.touches > 0) return "navigate";
    return state.drawingActive ? "navigate" : "draw";
  }
  if (e.pointerType === "mouse") {
    if (e.button === 1) return "navigate";
    if (e.button !== 0) return "ignore";
  }
  return state.drawingActive ? "ignore" : "draw";
}
