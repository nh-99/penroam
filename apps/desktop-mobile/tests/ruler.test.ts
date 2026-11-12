import { describe, expect, it } from "vitest";
import {
  CM_PT,
  fingerTurn,
  moveBy,
  normaliseAngle,
  projectToEdge,
  reachFor,
  rotateAbout,
  RULER_HALF_WIDTH_PX,
  rulerPress,
  signedDistance,
  snapAngle,
  stepAngle,
  tickLayout,
  twoFingerTransform,
  type Ruler,
} from "../src/editor/ruler";

const flat: Ruler = { cx: 400, cy: 300, angle: 0 };

describe("ruler angles", () => {
  it("normalises to [0, 180)", () => {
    expect(normaliseAngle(0)).toBe(0);
    expect(normaliseAngle(180)).toBe(0);
    expect(normaliseAngle(190)).toBe(10);
    expect(normaliseAngle(-30)).toBe(150);
    expect(normaliseAngle(-180)).toBe(0);
    expect(normaliseAngle(359.9999999999)).toBe(0);
  });
  it("snaps within 4 degrees of a 15 degree detent, and only then", () => {
    expect(snapAngle(43)).toBe(45);
    expect(snapAngle(49)).toBe(45);
    expect(snapAngle(49.5)).toBe(49.5);
    expect(snapAngle(178)).toBe(0);
    expect(snapAngle(-2)).toBe(0);
    expect(snapAngle(97)).toBe(97);
  });
  it("steps to the next and previous detent", () => {
    expect(stepAngle(0, 1)).toBe(15);
    expect(stepAngle(0, -1)).toBe(165);
    expect(stepAngle(165, 1)).toBe(0);
    expect(stepAngle(20, 1)).toBe(30);
    expect(stepAngle(20, -1)).toBe(15);
    expect(stepAngle(45.0000000001, 1)).toBe(60);
  });
});

describe("ruler hit tests", () => {
  it("turns on the dial for every pointer", () => {
    for (const p of ["pen", "mouse", "touch"] as const) expect(rulerPress(flat, 410, 310, p, true)).toEqual({ kind: "rotate" });
  });
  it("moves under a mouse or finger on the band, never under the pen", () => {
    expect(rulerPress(flat, 600, 320, "mouse", true)).toEqual({ kind: "move" });
    expect(rulerPress(flat, 600, 280, "touch", false)).toEqual({ kind: "move" });
    expect(rulerPress(flat, 600, 320, "pen", true)).toEqual({ kind: "draw", side: 1 });
    expect(rulerPress(flat, 600, 299, "pen", true)).toEqual({ kind: "draw", side: -1 });
    expect(rulerPress(flat, 600, 320, "pen", false)).toBeNull();
  });
  it("draws along an edge from up to 16 px outside it, and not further", () => {
    expect(rulerPress(flat, 100, 300 + 28 + 16, "mouse", true)).toEqual({ kind: "draw", side: 1 });
    expect(rulerPress(flat, 100, 300 - 28 - 10, "touch", true)).toEqual({ kind: "draw", side: -1 });
    expect(rulerPress(flat, 100, 300 + 28 + 17, "pen", true)).toBeNull();
    expect(rulerPress(flat, 100, 300 + 40, "mouse", false)).toBeNull();
  });
  it("is infinitely long", () => {
    expect(rulerPress(flat, -5000, 320, "mouse", true)).toEqual({ kind: "move" });
  });
  it("measures distance across a turned ruler", () => {
    const r: Ruler = { cx: 0, cy: 0, angle: 90 };
    // turned a quarter clockwise, side 1 is to the left on screen
    expect(signedDistance(r, -10, 50)).toBeCloseTo(10);
    expect(rulerPress(r, 30, 200, "mouse", true)).toEqual({ kind: "draw", side: -1 });
  });
});

describe("ruler edge projection", () => {
  it("puts points on the edge line, keeping their position along it", () => {
    expect(projectToEdge(flat, 1, 120, 340)).toEqual([120, 300 + RULER_HALF_WIDTH_PX]);
    expect(projectToEdge(flat, -1, 120, 240)).toEqual([120, 300 - RULER_HALF_WIDTH_PX]);
  });
  it("works at an angle", () => {
    const r: Ruler = { cx: 100, cy: 100, angle: 45 };
    for (const [x, y] of [[300, 260], [10, 40], [180, 200]]) {
      const [px, py] = projectToEdge(r, 1, x, y);
      expect(signedDistance(r, px, py)).toBeCloseTo(RULER_HALF_WIDTH_PX, 9);
      // the move is along the normal only
      const along = (px - x) * Math.SQRT1_2 + (py - y) * Math.SQRT1_2;
      expect(along).toBeCloseTo(0, 9);
    }
  });
});

describe("ruler gestures", () => {
  it("turns about the dial with detents", () => {
    const r = rotateAbout(flat, [450, 300], [400 + 50 * Math.cos(Math.PI / 4.2), 300 + 50 * Math.sin(Math.PI / 4.2)]);
    expect(r).toEqual({ cx: 400, cy: 300, angle: 45 });
    const free = rotateAbout(flat, [450, 300], [400 + 50 * Math.cos(0.4), 300 + 50 * Math.sin(0.4)]);
    expect(free.angle).toBeCloseTo((0.4 * 180) / Math.PI);
  });
  it("moves by one pointer's travel", () => {
    expect(moveBy(flat, [10, 10], [40, -5])).toEqual({ cx: 430, cy: 285, angle: 0 });
  });
  it("carries and turns with two fingers about their midpoint", () => {
    // fingers on the ruler either side of the centre turn a quarter about it
    const r = twoFingerTransform(flat, [300, 300], [500, 300], [400, 200], [400, 400]);
    expect(r.angle).toBe(90);
    expect(r.cx).toBeCloseTo(400);
    expect(r.cy).toBeCloseTo(300);
    // a pure slide keeps the angle
    expect(twoFingerTransform(flat, [300, 300], [500, 300], [320, 350], [520, 350])).toEqual({ cx: 420, cy: 350, angle: 0 });
  });
  it("turns the centre by the snapped angle", () => {
    const a0: [number, number] = [400, 300], b0: [number, number] = [500, 300];
    const t = (43 * Math.PI) / 180;
    const r = twoFingerTransform({ cx: 600, cy: 300, angle: 0 }, a0, b0, a0, [400 + 100 * Math.cos(t), 300 + 100 * Math.sin(t)]);
    expect(r.angle).toBe(45);
    // the centre 150 px out from the midpoint (450, 300) turns by 45 about where the midpoint went
    const mx = (400 + 400 + 100 * Math.cos(t)) / 2, my = (300 + 300 + 100 * Math.sin(t)) / 2;
    expect(r.cx).toBeCloseTo(mx + 150 * Math.SQRT1_2);
    expect(r.cy).toBeCloseTo(my + 150 * Math.SQRT1_2);
  });
});

describe("ruler two-finger turns past 90 degrees", () => {
  const start: Ruler = { cx: 400, cy: 200, angle: 0 };
  const a0: [number, number] = [200, 200], b0: [number, number] = [400, 200];
  // the fingers turned `deg` about their midpoint (300, 200), one small step per event
  function turnTo(deg: number): Ruler {
    let turn = 0, a = a0, b = b0;
    const steps = Math.ceil(Math.abs(deg) / 10);
    for (let i = 1; i <= steps; i++) {
      const t = ((deg * i) / steps) * (Math.PI / 180);
      a = [300 - 100 * Math.cos(t), 200 - 100 * Math.sin(t)];
      b = [300 + 100 * Math.cos(t), 200 + 100 * Math.sin(t)];
      turn = fingerTurn(turn, a0, b0, a, b);
    }
    return twoFingerTransform(start, a0, b0, a, b, turn);
  }
  it("turns the centre with the fingers at 89 and 120 degrees", () => {
    const r89 = turnTo(89);
    expect(r89.angle).toBe(90);
    expect(r89.cx).toBeCloseTo(300);
    expect(r89.cy).toBeCloseTo(300);
    const r120 = turnTo(120);
    expect(r120.angle).toBe(120);
    expect(r120.cx).toBeCloseTo(250);
    expect(r120.cy).toBeCloseTo(200 + 100 * Math.sin((120 * Math.PI) / 180));
  });
  it("follows the fingers continuously across 180 degrees and back", () => {
    for (const deg of [170, 185, 200, 270, 350, -100, -200]) {
      const r = turnTo(deg);
      const t = (Math.round(deg / 15) * 15 * Math.PI) / 180;
      const snapped = Math.abs(deg - Math.round(deg / 15) * 15) <= 4;
      const rad = snapped ? t : (deg * Math.PI) / 180;
      expect(r.cx).toBeCloseTo(300 + 100 * Math.cos(rad));
      expect(r.cy).toBeCloseTo(200 + 100 * Math.sin(rad));
      expect(r.angle).toBeCloseTo(normaliseAngle((rad * 180) / Math.PI));
    }
  });
  it("unwraps the finger angle to the turn nearest the last one", () => {
    const a: [number, number] = [0, 0];
    expect(fingerTurn(170, a, [1, 0], a, [-1, -0.1])).toBeCloseTo(185.71, 2);
    expect(fingerTurn(-170, a, [1, 0], a, [-1, 0.1])).toBeCloseTo(-185.71, 2);
    expect(fingerTurn(0, a, [1, 0], a, [0, 1])).toBeCloseTo(90);
  });
});

describe("ruler dial dead zone", () => {
  it("does not turn while the pointer is near the centre", () => {
    expect(rotateAbout(flat, [403, 300], [400, 305])).toBe(flat);
    expect(rotateAbout(flat, [450, 300], [401, 302])).toBe(flat);
    expect(rotateAbout(flat, [409, 300], [400, 309]).angle).toBe(90);
  });
});

describe("ruler ticks", () => {
  it("spaces centimetres by a page centimetre at the view's scale", () => {
    const ticks = tickLayout(2, 300);
    const cms = ticks.filter((t) => t.kind === "cm");
    expect(cms[0].at).toBeCloseTo(-5 * CM_PT * 2);
    const one = cms.find((t) => t.label === 1 && t.at > 0)!;
    expect(one.at).toBeCloseTo(CM_PT * 2);
    expect(ticks.some((t) => t.kind === "mm")).toBe(true);
    expect(ticks.some((t) => t.at === 0)).toBe(false);
  });
  it("drops millimetres, then half centimetres, when they crowd", () => {
    const at1 = tickLayout(1, 200); // mm 2.8 px, half 14 px
    expect(at1.some((t) => t.kind === "mm")).toBe(false);
    expect(at1.some((t) => t.kind === "half")).toBe(true);
    const at02 = tickLayout(0.2, 200); // half 2.8 px
    expect(at02.every((t) => t.kind === "cm")).toBe(true);
    for (const ticks of [at1, at02, tickLayout(6, 500)]) {
      const at = ticks.map((t) => t.at).sort((a, b) => a - b);
      for (let i = 1; i < at.length; i++) expect(at[i] - at[i - 1]).toBeGreaterThanOrEqual(4);
    }
  });
  it("thins labels when centimetres are small on screen", () => {
    const labels = tickLayout(0.2, 400).filter((t) => t.label !== undefined).map((t) => t.label!);
    expect(labels.every((l) => l % 5 === 0)).toBe(true);
    expect(tickLayout(1, 100).filter((t) => t.label !== undefined).map((t) => t.label)).toEqual([3, 2, 1, 1, 2, 3]);
  });
  it("reaches the view's farthest corner", () => {
    expect(reachFor({ cx: 0, cy: 0, angle: 0 }, 300, 400)).toBe(500);
  });
});
