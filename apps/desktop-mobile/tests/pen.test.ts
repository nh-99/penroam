import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InkEditor } from "../src/editor/engine";
import { lassoHitsStroke, pointInPolygon } from "../src/editor/geometry";
import { pencilPressure, penErases, pressureArrived, samplePoint } from "../src/editor/input";
import { PageRenderer } from "../src/editor/renderer";
import { PageSession } from "../src/editor/session";
import type { Element } from "../src/generated/Element";
import type { Page } from "../src/generated/Page";
import { drawCalls } from "./setup";

function page(): Page {
  return { schemaVersion: 1, id: "p1", paper: "letter", size: { width: 612, height: 792 }, template: { kind: "blank", spacing: 24 }, revision: 0, createdAt: 0, updatedAt: 0, elements: [] };
}

function ink(id: string, z: number, x: number, y: number, n = 6) {
  const points = Array.from({ length: n }, (_, i) => ({ x: x + i * 4, y, pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: i }));
  return { type: "ink", id, z, tool: "pen", color: "#000000", opacity: 1, width: 2, pressure: false, source: "stylus", input: {}, points } as unknown as Extract<Element, { type: "ink" }>;
}

describe("pen buttons", () => {
  const pen = { pointerType: "pen", button: 0, buttons: 1 };
  it("the eraser end erases", () => {
    expect(penErases({ ...pen, button: 5, buttons: 33 })).toBe(true);
    expect(penErases({ ...pen, buttons: 32 })).toBe(true);
  });
  it("a side button erases while held, as on the S Pen", () => {
    expect(penErases({ ...pen, button: 2, buttons: 2 })).toBe(true);
    expect(penErases({ ...pen, buttons: 3 })).toBe(true);
  });
  it("a plain tip does not, and a mouse's right button is not a pen button", () => {
    expect(penErases(pen)).toBe(false);
    expect(penErases({ pointerType: "mouse", button: 2, buttons: 2 })).toBe(false);
  });
  it("pressure can arrive after contact", () => {
    expect(pressureArrived("pen", 0.31)).toBe(true);
    expect(pressureArrived("pen", 0)).toBe(false);
    expect(pressureArrived("pen", 0.5)).toBe(false);
    expect(pressureArrived("touch", 0.31)).toBe(false);
  });
});

describe("Apple Pencil pressure curve", () => {
  it("puts ordinary writing mid-range and keeps the ends", () => {
    expect(pencilPressure(0)).toBe(0);
    expect(pencilPressure(1)).toBeCloseTo(1, 9);
    // measured median and 95th percentile of real writing, as force fractions
    expect(pencilPressure(0.111)).toBeCloseTo(0.404, 2);
    expect(pencilPressure(0.218)).toBeCloseTo(0.639, 2);
  });
  it("always increases, so harder never reads lighter", () => {
    for (let f = 0; f < 1; f += 0.01) expect(pencilPressure(f + 0.01)).toBeGreaterThan(pencilPressure(f));
  });
  it("applies to pens only when asked", () => {
    const t = { scale: 1, ox: 0, oy: 0 };
    const e = { clientX: 0, clientY: 0, pressure: 0.2, timeStamp: 0, pointerType: "pen" };
    expect(samplePoint(e, { left: 0, top: 0 }, t, 0).pressure).toBe(0.2);
    expect(samplePoint(e, { left: 0, top: 0 }, t, 0, false, true).pressure).toBeCloseTo(pencilPressure(0.2), 3);
    expect(samplePoint({ ...e, pointerType: "mouse" }, { left: 0, top: 0 }, t, 0, false, true).pressure).toBe(0.5);
    expect(samplePoint({ ...e, pressure: 0 }, { left: 0, top: 0 }, t, 0, false, true).pressure).toBe(0.5);
  });
});

describe("lasso geometry", () => {
  const square: [number, number][] = [[0, 0], [10, 0], [10, 10], [0, 10]];
  it("tests points against a closed polygon", () => {
    expect(pointInPolygon(5, 5, square)).toBe(true);
    expect(pointInPolygon(15, 5, square)).toBe(false);
  });
  it("catches a stroke mostly inside and leaves one it only clips", () => {
    const inside = ink("a", 1, 1, 5, 2);
    const clipped = ink("b", 2, 8, 5, 6);
    expect(lassoHitsStroke(inside, square)).toBe(true);
    expect(lassoHitsStroke(clipped, square)).toBe(false);
  });
});

describe("pen input in the editor", () => {
  let vp: HTMLDivElement;
  let ed: InkEditor;
  let session: PageSession;

  function ptr(type: string, x: number, y: number, o: { button?: number; buttons?: number; pressure?: number } = {}) {
    vp.dispatchEvent(new PointerEvent(type, { clientX: x, clientY: y, pointerId: 1, pointerType: "pen", pressure: o.pressure ?? 0.6, button: o.button ?? 0, buttons: o.buttons ?? (type === "pointerup" ? 0 : 1), bubbles: true, cancelable: true }));
  }
  function line(x0: number, y0: number, x1: number, y1: number, o: Parameters<typeof ptr>[3] = {}) {
    ptr("pointerdown", x0, y0, o);
    for (let i = 1; i <= 8; i++) ptr("pointermove", x0 + ((x1 - x0) * i) / 8, y0 + ((y1 - y0) * i) / 8, o);
    ptr("pointerup", x1, y1, o);
  }
  const inkCount = () => session.page.elements.filter((e) => e.type === "ink").length;

  beforeEach(async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    vp = document.createElement("div");
    document.body.appendChild(vp);
    vp.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600, x: 0, y: 0, toJSON() {} }) as DOMRect;
    (vp as any).setPointerCapture = () => {};
    session = new PageSession("nb", page(), async () => ({ revision: 1, updatedAt: 0, durable: true }));
    ed = new InkEditor(vp, { onState: () => {}, onError: (m) => { throw new Error(m); }, onPageEdited: () => {} });
    await ed.setSession(session, null);
    ed.setTransform({ scale: 1, ox: 0, oy: 0 });
  });
  afterEach(() => {
    ed.dispose();
    vp.remove();
    vi.useRealTimers();
  });

  it("holding the side button erases, then the pen writes again", () => {
    line(100, 100, 200, 100);
    expect(inkCount()).toBe(1);
    line(150, 60, 150, 140, { button: 2, buttons: 2 });
    expect(inkCount()).toBe(0);
    expect(ed.tool).toBe("pen");
    line(100, 100, 200, 100);
    expect(inkCount()).toBe(1);
  });

  it("a stroke whose first sample has no pressure still uses it once it arrives", () => {
    ptr("pointerdown", 100, 100, { pressure: 0.5 });
    for (const type of ["pointerrawupdate", "pointermove"]) {
      ptr(type, 110, 100, { pressure: 0.3 });
      ptr(type, 120, 100, { pressure: 0.7 });
    }
    ptr("pointerup", 120, 100);
    const s = session.page.elements[0];
    expect(s.type === "ink" && s.pressure).toBe(true);
  });

  it("pressure stays off when the setting is off", () => {
    ed.setToolSettings({ pressure: false });
    line(100, 100, 200, 100, { pressure: 0.3 });
    const s = session.page.elements[0];
    expect(s.type === "ink" && s.pressure).toBe(false);
  });

  it("a hovering pen draws nothing into the page", () => {
    ptr("pointermove", 100, 100, { buttons: 0, pressure: 0 });
    ptr("pointermove", 120, 100, { buttons: 0, pressure: 0 });
    expect(session.page.elements).toHaveLength(0);
  });

  it("the lasso selects what it encloses and leaves the rest", () => {
    line(100, 100, 140, 100);
    line(400, 400, 440, 400);
    ed.setTool("lasso");
    ptr("pointerdown", 80, 80);
    for (const [x, y] of [[160, 80], [160, 120], [80, 120]]) ptr("pointermove", x, y);
    ptr("pointerup", 80, 120);
    expect(ed.selection.size).toBe(1);
    expect(ed.selection.has(session.page.elements[0].id)).toBe(true);
    expect(inkCount()).toBe(2);
  });

  it("a selection made with the lasso moves by dragging inside it", () => {
    line(100, 100, 140, 100);
    ed.setTool("lasso");
    ed.selectAll();
    const before = session.page.elements[0];
    ptr("pointerdown", 120, 100);
    ptr("pointermove", 170, 150);
    ptr("pointerup", 170, 150);
    const after = session.page.elements[0];
    if (before.type !== "ink" || after.type !== "ink") throw new Error("not ink");
    expect(after.points[0].x - before.points[0].x).toBeCloseTo(50);
    expect(ed.selection.size).toBe(1);
  });

  it("follows the Pencil's double-tap preference", () => {
    ed.pencilTap("switchEraser");
    expect(ed.tool).toBe("eraser");
    ed.pencilTap("switchEraser");
    expect(ed.tool).toBe("pen");
    ed.setTool("highlighter");
    ed.pencilTap("switchPrevious");
    expect(ed.tool).toBe("pen");
    ed.pencilTap("showColorPalette");
    expect(ed.tool).toBe("pen");
  });
});

describe("redrawing without blocking input", () => {
  let els: Element[];
  let r: PageRenderer;
  let screen: HTMLDivElement;
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    els = Array.from({ length: 40 }, (_, i) => ink(`s${i}`, i, 10, 10 + i * 18));
    screen = document.createElement("div");
    r = new PageRenderer(document.createElement("div"), () => els, screen);
    r.setPage(612, 792, { kind: "blank", spacing: 24 });
    r.setResolution(1);
    r.renderAll(null);
  });
  afterEach(() => void vi.useRealTimers());

  it("keeps the old resolution until the new one is ready, then swaps", async () => {
    const done = vi.fn();
    r.rescale(2, [0, 0, 612, 300], done);
    expect(r.res).toBe(1);
    expect(r.targetRes).toBe(2);
    expect(done).not.toHaveBeenCalled();
    await vi.runAllTimersAsync();
    expect(r.res).toBe(2);
    expect(r.ink.width).toBe(1224);
    expect(done).toHaveBeenCalledOnce();
    await r.whenComplete();
  });

  it("a stroke added during a rescale is drawn exactly once more, on the new canvas", async () => {
    r.rescale(2, [0, 0, 612, 792]);
    const s = ink("new", 99, 300, 300);
    els.push(s);
    r.appendStroke(s);
    drawCalls.length = 0;
    await vi.runAllTimersAsync();
    expect(r.res).toBe(2);
    expect(drawCalls.filter((c) => c === "stroke")).toHaveLength(41);
  });

  it("changing the ink during a rescale redraws from the current elements", async () => {
    r.rescale(2, [0, 0, 612, 792]);
    els.splice(0, 10);
    r.renderInk([0, 0, 612, 792]);
    drawCalls.length = 0;
    await vi.runAllTimersAsync();
    expect(drawCalls.filter((c) => c === "stroke")).toHaveLength(30);
  });

  it("a new page abandons a rescale in progress", async () => {
    const done = vi.fn();
    r.rescale(2, [0, 0, 612, 792], done);
    r.setPage(612, 792, { kind: "blank", spacing: 24 });
    await vi.runAllTimersAsync();
    expect(r.res).toBe(1);
    expect(done).not.toHaveBeenCalled();
    await r.whenComplete();
  });

  describe("detail layer past the page canvas's limit", () => {
    const layers = () => [...(screen.querySelectorAll("canvas") ?? [])];
    const detailInk = () => layers().find((c) => c.className.includes("penroam-ink-detail"))!;
    const detailText = () => layers().find((c) => c.className.includes("penroam-text-detail"))!;

    it("draws just the area around the viewport at full resolution, and cuts it out of the page layers", async () => {
      r.showDetail([100, 100, 200, 200], 12, 8_000_000);
      await vi.runAllTimersAsync();
      const c = detailInk();
      expect(c.style.display).toBe("");
      expect(c.style.transform).toContain("translate(75px, 75px)");
      expect(c.width).toBe(150 * 12);
      expect(r.ink.style.clipPath).toContain("polygon(evenodd");
      expect(r.text.style.clipPath).toBe(r.ink.style.clipPath);
      // the page layers are not touched, so they stay complete underneath
      expect(r.res).toBe(1);
    });

    it("lives outside the scaled page, with text above ink", () => {
      expect(r.ink.parentElement).not.toBe(screen);
      expect(detailInk().parentElement).toBe(screen);
      const order = layers().map((c) => c.className.match(/penroam-[a-z-]+/)?.[0]);
      expect(order).toEqual(["penroam-ink-detail", "penroam-text-detail"]);
    });

    it("is shown 1:1 with device pixels once settled, whatever the pan", async () => {
      r.setView({ scale: 3, ox: -100.3, oy: -40.7 });
      r.showDetail([100, 100, 200, 200], 6, 8_000_000, 2);
      await vi.runAllTimersAsync();
      const c = detailInk();
      expect(c.style.transform).toContain("scale(1)");
      expect(parseFloat(c.style.width) * 2).toBe(c.width);
      const [x] = c.style.transform.match(/-?[\d.]+/g)!.map(Number);
      expect((x * 2) % 1).toBe(0);
    });

    it("follows pan and zoom before the next redraw", async () => {
      r.setView({ scale: 3, ox: 0, oy: 0 });
      r.showDetail([100, 100, 200, 200], 6, 8_000_000, 2);
      await vi.runAllTimersAsync();
      r.setView({ scale: 6, ox: 10, oy: 20 });
      expect(detailInk().style.transform).toBe(`translate(${10 + 75 * 6}px, ${20 + 75 * 6}px) scale(2)`);
      expect(detailText().style.transform).toBe(detailInk().style.transform);
    });

    it("stays within the pixel limit", async () => {
      r.showDetail([0, 0, 612, 792], 12, 8_000_000);
      await vi.runAllTimersAsync();
      expect(detailInk().width * detailInk().height).toBeLessThanOrEqual(8_100_000);
    });

    it("needs no redraw for a pan inside its margin", async () => {
      r.showDetail([100, 100, 200, 200], 12, 8_000_000);
      await vi.runAllTimersAsync();
      drawCalls.length = 0;
      r.showDetail([110, 110, 210, 210], 12, 8_000_000);
      await vi.runAllTimersAsync();
      expect(drawCalls).toHaveLength(0);
    });

    it("redraws when a pan leaves it, or the zoom changes", async () => {
      r.showDetail([100, 100, 200, 200], 12, 8_000_000);
      await vi.runAllTimersAsync();
      r.showDetail([400, 400, 500, 500], 12, 8_000_000);
      await vi.runAllTimersAsync();
      expect(detailInk().style.transform).toContain("translate(375px, 375px)");
      r.showDetail([400, 400, 500, 500], 6, 8_000_000);
      await vi.runAllTimersAsync();
      expect(detailInk().width).toBe(150 * 6);
    });

    it("a stroke written while zoomed in reaches both the page and detail layers", async () => {
      r.showDetail([0, 0, 200, 200], 12, 8_000_000);
      await vi.runAllTimersAsync();
      const s = ink("new", 99, 50, 50);
      els.push(s);
      drawCalls.length = 0;
      r.appendStroke(s);
      expect(drawCalls.filter((c) => c === "stroke")).toHaveLength(2);
    });

    it("an erase inside it redraws both layers", async () => {
      r.showDetail([0, 0, 612, 200], 12, 8_000_000);
      await vi.runAllTimersAsync();
      r.hidden.add("s1");
      drawCalls.length = 0;
      r.renderInk([0, 0, 612, 40]);
      expect(drawCalls.filter((c) => c === "stroke").length % 2).toBe(0);
    });

    it("goes away when zoomed back out, restoring the page layers", async () => {
      r.showDetail([100, 100, 200, 200], 12, 8_000_000);
      await vi.runAllTimersAsync();
      r.hideDetail();
      expect(detailInk().style.display).toBe("none");
      expect(detailText().style.display).toBe("none");
      expect(r.ink.style.clipPath).toBe("");
      expect(detailInk().width).toBe(1);
    });

    it("a new page drops it, including one still being drawn", async () => {
      r.showDetail([100, 100, 200, 200], 12, 8_000_000);
      r.setPage(612, 792, { kind: "blank", spacing: 24 });
      await vi.runAllTimersAsync();
      expect(detailInk().style.display).toBe("none");
      expect(r.ink.style.clipPath).toBe("");
      await r.whenComplete();
    });

    it("hides with the ink when a native surface owns it", async () => {
      r.showDetail([100, 100, 200, 200], 12, 8_000_000);
      await vi.runAllTimersAsync();
      r.setInkHidden(true);
      expect(detailInk().style.visibility).toBe("hidden");
      r.setInkHidden(false);
      expect(detailInk().style.visibility).toBe("");
    });
  });

  it("the part of the page outside the viewport follows in bands", async () => {
    r.rescale(2, [0, 0, 612, 100]);
    await vi.runAllTimersAsync();
    expect(r.res).toBe(2);
    await r.whenComplete();
  });
});
