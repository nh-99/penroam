// Pointer input normalisation (pure functions; unit-tested).
import type { InkPoint } from "../generated/InkPoint";
import { toDoc, type ViewTransform } from "./viewport";

export type PointerKind = "pen" | "mouse" | "touch";

export const FLAG_COALESCED = 2;
/** Contacts wider than this (CSS px) are treated as a palm. */
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

const r3 = (v: number) => Math.round(v * 1000) / 1000;
const r4 = (v: number) => Math.round(v * 10000) / 10000;

/**
 * Maps one pointer sample to a canonical document point. Coordinates are
 * rounded to 0.001 pt and pressure to 1e-4 (capture precision; far below
 * digitiser resolution). Mouse and pressure-less pens record 0.5, as in M00.
 */
export function samplePoint(e: RawSample, rect: { left: number; top: number }, t: ViewTransform, t0: number, coalesced = false): InkPoint {
  const [x, y] = toDoc(t, e.clientX - rect.left, e.clientY - rect.top);
  const kind = e.pointerType;
  const pressure = kind === "mouse" ? 0.5 : e.pressure > 0 ? Math.min(1, e.pressure) : 0.5;
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

/** Whether a stroke from this pointer should use pressure for width. */
export function pressureCapable(kind: string, firstPressure: number): boolean {
  if (kind === "mouse") return false;
  // touch on most hardware reports 0 or a constant 0.5/1: treat as no pressure
  if (kind === "touch") return false;
  return firstPressure > 0 && firstPressure !== 0.5;
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
  state: { penOnly: PenOnlyMode; penSeen: boolean; penDown: boolean; drawingActive: boolean; touches: number },
): "draw" | "navigate" | "ignore" {
  if (e.pointerType === "touch") {
    if (state.penDown) return "ignore";
    if (Math.max(e.width, e.height) > PALM_CONTACT_PX) return "ignore";
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
