import { describe, expect, it } from "vitest";
import { drawingBox, flowShift, flowUnits, movedElement, shiftOps } from "../src/editor/flow";
import type { Rect } from "../src/editor/geometry";
import type { Element } from "../src/generated/Element";

const ink = (id: string, x0: number, y0: number, x1: number, y1: number): Element =>
  ({ type: "ink", id, z: 1, tool: "pen", color: "#111111", opacity: 1, width: 0, pressure: false, source: "stylus", input: {}, points: [{ x: x0, y: y0, pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: 0 }, { x: x1, y: y1, pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: 1 }] }) as Element;
const text = (id: string, y: number, height: number, x = 0, width = 300): Element => ({ type: "text", id, z: 1, x, y, width, height, text: "t", fontSize: 16, color: "#111111", align: "left", rowPitch: 24 });

function bounds(e: Element): Rect {
  if (e.type === "ink") {
    const xs = e.points.map((p) => p.x), ys = e.points.map((p) => p.y);
    return [Math.min(...xs), Math.min(...ys), Math.max(...xs), Math.max(...ys)];
  }
  return [e.x, e.y, e.x + e.width, e.y + e.height];
}
const ids = (els: Element[]) => flowUnits(els, bounds, 24).map((u) => [...u.ids].sort());

describe("note flow", () => {
  it("makes a drawing box of strokes that meet or come within a row of each other", () => {
    const els = [ink("a", 10, 100, 50, 150), ink("b", 400, 160, 450, 200), ink("c", 10, 300, 50, 320)];
    // b is 10 pt under a, and beside it: the same drawing; c is far below
    expect(ids(els)).toEqual([["a", "b"], ["c"]]);
  });

  it("keeps ink touching a text box with that box, and leaves other text boxes apart", () => {
    const els = [text("t1", 48, 48), ink("underline", 0, 98, 100, 99), text("t2", 120, 24)];
    expect(ids(els)).toEqual([["t1", "underline"], ["t2"]]);
    expect(flowUnits(els, bounds, 24).map((u) => u.text)).toEqual([true, true]);
  });

  it("moves what is under a growing box down by whole rows", () => {
    const units = flowUnits([ink("d", 10, 200, 50, 260), text("t", 300, 24)], bounds, 24);
    // the box above ended at 96 and now ends at 106: one row more
    expect(flowShift(units, 96, 10, 24, 106)).toEqual({ ids: ["d", "t"], dy: 24 });
    // without rows, by exactly as much
    expect(flowShift(units, 96, 10, undefined, 106).dy).toBe(10);
    // nothing under the box: nothing moves
    expect(flowShift(units, 400, 30, 24, 430)).toEqual({ ids: [], dy: 0 });
  });

  it("leaves what sits beside a growing box where it is", () => {
    const beside = ink("margin", 500, 60, 560, 140);
    const units = flowUnits([beside, ink("d", 10, 200, 50, 260)], bounds, 24);
    expect(flowShift(units, 96, 24, 24, 120).ids).toEqual(["d"]);
  });

  it("pulls blocks up when a box shrinks, but not above it or anything that stays", () => {
    const units = flowUnits([ink("d", 10, 200, 50, 260)], bounds, 24);
    expect(flowShift(units, 144, -48, 24, 96).dy).toBe(-48);
    // only 30 pt left between the new bottom and the drawing: one whole row
    expect(flowShift(units, 144, -200, 24, 170).dy).toBe(-24);
    // a drawing beside the box that ends at 190 stops the pull there
    const blocked = flowUnits([ink("side", 500, 100, 560, 190), ink("d", 10, 230, 50, 260)], bounds, 24);
    expect(flowShift(blocked, 144, -48, undefined, 96)).toEqual({ ids: ["d"], dy: -40 });
  });

  it("gives a drawing box the space down to the next block, and none to a text block", () => {
    const units = flowUnits([ink("d", 10, 100, 50, 150), text("t", 300, 24)], bounds, 24);
    expect(drawingBox(units, "d", 12)).toMatchObject({ bottom: 288, next: 300 });
    expect(drawingBox(units, "t", 12)).toBeNull();
    const last = flowUnits([ink("d", 10, 100, 50, 150)], bounds, 24);
    expect(drawingBox(last, "d", 12)).toMatchObject({ bottom: 162, next: null });
  });

  it("moves elements and builds the ops that put them back", () => {
    const d = ink("d", 10.0004, 100, 50, 150);
    expect(movedElement(d, 0, 24)).toMatchObject({ points: [{ x: 10, y: 124 }, { x: 50, y: 174 }] });
    const t = text("t", 300, 24);
    const ops = shiftOps([d, t], ["t"], 48);
    expect(ops.forward).toEqual([{ op: "replaceElements", elements: [{ ...t, y: 348 }] }]);
    expect(ops.inverse).toEqual([{ op: "replaceElements", elements: [t] }]);
    expect(shiftOps([d], [], 10)).toEqual({ forward: [], inverse: [] });
  });
});
