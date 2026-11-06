import { describe, expect, it } from "vitest";
import { classifyPointer, pressureCapable, samplePoint, FLAG_COALESCED } from "../src/editor/input";
import { backingScale, constrain, fitWidth, toDoc, toScreen, zoomAbout } from "../src/editor/viewport";

describe("pointer normalisation", () => {
  const rect = { left: 100, top: 50 };
  it("maps client px to document pt through the view transform", () => {
    const t = { scale: 2, ox: 20, oy: 10 };
    const p = samplePoint({ clientX: 100 + 20 + 2 * 30.5, clientY: 50 + 10 + 2 * 40.25, pressure: 0.7, tiltX: 12, tiltY: -5, twist: 30, timeStamp: 1012.5, pointerType: "pen" }, rect, t, 1000, true);
    expect(p).toEqual({ x: 30.5, y: 40.25, pressure: 0.7, tiltX: 12, tiltY: -5, timeUs: 12500, twist: 30, flags: FLAG_COALESCED });
  });
  it("mouse and zero-pressure pens record 0.5", () => {
    const t = { scale: 1, ox: 0, oy: 0 };
    expect(samplePoint({ clientX: 0, clientY: 0, pressure: 1, timeStamp: 0, pointerType: "mouse" }, rect, t, 0).pressure).toBe(0.5);
    expect(samplePoint({ clientX: 0, clientY: 0, pressure: 0, timeStamp: 0, pointerType: "pen" }, rect, t, 0).pressure).toBe(0.5);
  });
  it("pressure is only used for real pen pressure", () => {
    expect(pressureCapable("mouse", 0.5)).toBe(false);
    expect(pressureCapable("touch", 1)).toBe(false);
    expect(pressureCapable("pen", 0.5)).toBe(false);
    expect(pressureCapable("pen", 0.31)).toBe(true);
  });
  it("palm, pen-only and multi-pointer rules", () => {
    const base = { penOnly: "auto" as const, penSeen: false, penDown: false, drawingActive: false, touches: 0 };
    const touch = { pointerType: "touch", button: 0, width: 10, height: 10 };
    expect(classifyPointer(touch, base)).toBe("draw");
    expect(classifyPointer(touch, { ...base, penSeen: true })).toBe("navigate");
    expect(classifyPointer(touch, { ...base, penOnly: "on" })).toBe("navigate");
    expect(classifyPointer({ ...touch, width: 60 }, base)).toBe("ignore");
    expect(classifyPointer({ ...touch, width: 60 }, { ...base, penSeen: true, palmPx: Infinity })).toBe("navigate");
    expect(classifyPointer(touch, { ...base, penDown: true })).toBe("ignore");
    expect(classifyPointer({ pointerType: "pen", button: 0, width: 1, height: 1 }, { ...base, drawingActive: true })).toBe("ignore");
    expect(classifyPointer({ pointerType: "mouse", button: 1, width: 1, height: 1 }, base)).toBe("navigate");
    expect(classifyPointer({ pointerType: "mouse", button: 2, width: 1, height: 1 }, base)).toBe("ignore");
  });
});

describe("viewport math", () => {
  it("toDoc/toScreen are inverse", () => {
    const t = { scale: 1.7, ox: -33, oy: 12 };
    const [x, y] = toDoc(t, ...toScreen(t, 123.4, 567.8));
    expect(x).toBeCloseTo(123.4, 10);
    expect(y).toBeCloseTo(567.8, 10);
  });
  it("zoom keeps the focus point fixed", () => {
    const t = { scale: 1, ox: 10, oy: 20 };
    const before = toDoc(t, 300, 200);
    const z = zoomAbout(t, 2.5, 300, 200);
    expect(toDoc(z, 300, 200)[0]).toBeCloseTo(before[0], 10);
    expect(toDoc(z, 300, 200)[1]).toBeCloseTo(before[1], 10);
  });
  it("fit width and constrain", () => {
    const f = fitWidth(800, 600, 612, 792, 24);
    expect(f.scale).toBeCloseTo(752 / 612, 10);
    expect(f.ox).toBeCloseTo(24, 6);
    const c = constrain({ scale: 1, ox: -5000, oy: 5000 }, 800, 600, 612, 792);
    expect(c.ox).toBeGreaterThan(-612);
    expect(c.oy).toBeLessThan(600);
  });
  it("backing scale is capped by pixel area", () => {
    expect(backingScale(1, 2, 612, 792, 16e6)).toBe(2);
    const capped = backingScale(4, 2, 612, 792, 16e6);
    expect(capped * capped * 612 * 792).toBeLessThanOrEqual(16e6 + 1);
  });
});
