import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InkEditor } from "../src/editor/engine";
import { PageSession } from "../src/editor/session";
import type { Page } from "../src/generated/Page";

function page(): Page {
  return { schemaVersion: 1, id: "p1", paper: "letter", size: { width: 612, height: 792 }, template: { kind: "blank", spacing: 24 }, revision: 0, createdAt: 0, updatedAt: 0, elements: [] };
}

let vp: HTMLDivElement;
let ed: InkEditor;
let session: PageSession;
const persisted: any[] = [];

function ptr(type: string, x: number, y: number, o: Partial<PointerEventInit> & { pointerType?: string } = {}) {
  const e = new PointerEvent(type, { clientX: x, clientY: y, pointerId: o.pointerId ?? 1, pointerType: o.pointerType ?? "pen", pressure: o.pressure ?? 0.6, button: 0, buttons: type === "pointerup" ? 0 : 1, width: o.width ?? 1, height: o.height ?? 1, bubbles: true, cancelable: true });
  vp.dispatchEvent(e);
  return e;
}

/** Browsers with pointerrawupdate deliver both events; the engine consumes one stream. */
function move(x: number, y: number, o: Parameters<typeof ptr>[3] = {}) {
  ptr("pointerrawupdate", x, y, o);
  ptr("pointermove", x, y, o);
}

function drawLine(x0: number, y0: number, x1: number, y1: number, o: Parameters<typeof ptr>[3] = {}) {
  ptr("pointerdown", x0, y0, o);
  for (let i = 1; i <= 8; i++) move(x0 + ((x1 - x0) * i) / 8, y0 + ((y1 - y0) * i) / 8, o);
  ptr("pointerup", x1, y1, o);
}

beforeEach(async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  vp = document.createElement("div");
  document.body.appendChild(vp);
  vp.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600, x: 0, y: 0, toJSON() {} }) as DOMRect;
  (vp as any).setPointerCapture = () => {};
  persisted.length = 0;
  let rev = 0;
  session = new PageSession("nb", page(), async (b) => {
    persisted.push(b);
    return { revision: ++rev, updatedAt: 0, durable: true };
  });
  ed = new InkEditor(vp, { onState: () => {}, onError: (m) => { throw new Error(m); }, onPageEdited: () => {} });
  await ed.setSession(session, null);
  ed.setTransform({ scale: 1, ox: 0, oy: 0 });
});
afterEach(() => {
  ed.dispose();
  vp.remove();
  vi.useRealTimers();
});

describe("editor engine", () => {
  it("a pen stroke is committed on pen-up with pressure and source metadata", async () => {
    drawLine(100, 100, 200, 140, { pressure: 0.42 });
    expect(session.page.elements).toHaveLength(1);
    const s = session.page.elements[0] as any;
    expect(s.type).toBe("ink");
    expect(s.pressure).toBe(true);
    expect(s.source).toBe("stylus");
    expect(s.points[0]).toMatchObject({ x: 100, y: 100, pressure: 0.42 });
    expect(s.points.length).toBeGreaterThanOrEqual(9);
    await session.flush();
    expect(persisted[0].ops[0].op).toBe("addElements");
  });

  it("strokes map through zoom/pan into document space", () => {
    ed.setTransform({ scale: 2, ox: 50, oy: 10 });
    drawLine(250, 210, 350, 210);
    const s = session.page.elements[0] as any;
    expect(s.points[0].x).toBe(100);
    expect(s.points[0].y).toBe(100);
  });

  it("mouse strokes are fixed width", () => {
    drawLine(10, 10, 60, 60, { pointerType: "mouse", pressure: 0.5 });
    expect((session.page.elements[0] as any).pressure).toBe(false);
  });

  it("pointercancel on a tap leaves no orphan stroke; long cancelled strokes are kept", () => {
    ptr("pointerdown", 10, 10);
    ptr("pointercancel", 10, 10);
    expect(session.page.elements).toHaveLength(0);
    ptr("pointerdown", 10, 10);
    for (let i = 1; i < 10; i++) move(10 + i * 5, 10);
    ptr("pointercancel", 60, 10);
    expect(session.page.elements).toHaveLength(1);
  });

  it("the whole-stroke eraser removes strokes it crosses and undo restores them", () => {
    drawLine(100, 100, 300, 100);
    drawLine(100, 300, 300, 300);
    ed.setTool("eraser");
    drawLine(200, 50, 200, 150);
    expect(session.page.elements).toHaveLength(1);
    ed.undo();
    expect(session.page.elements).toHaveLength(2);
    ed.redo();
    expect(session.page.elements).toHaveLength(1);
  });

  it("pen eraser button erases without switching tools", () => {
    drawLine(100, 100, 300, 100);
    const e = new PointerEvent("pointerdown", { clientX: 200, clientY: 100, pointerId: 3, pointerType: "pen", buttons: 32, button: 5, bubbles: true });
    vp.dispatchEvent(e);
    ptr("pointerup", 200, 100, { pointerId: 3 });
    expect(session.page.elements).toHaveLength(0);
    expect(ed.tool).toBe("pen");
  });

  it("palm-sized touches never draw", () => {
    drawLine(100, 100, 300, 100, { pointerType: "touch", width: 80, height: 80, pointerId: 9 });
    expect(session.page.elements).toHaveLength(0);
  });

  it("text tool: tap places a block, typing is batched, tap outside commits; no ink is drawn meanwhile", () => {
    ed.setTool("text");
    ptr("pointerdown", 120, 200);
    ptr("pointerup", 120, 200);
    const ta = vp.querySelector("textarea")!;
    expect(ta.classList.contains("hidden")).toBe(false);
    ta.value = "Hello\nworld";
    ta.dispatchEvent(new Event("input"));
    // pen input on the page while editing ends the edit instead of drawing
    ptr("pointerdown", 400, 400);
    ptr("pointerup", 400, 400);
    const els = session.page.elements;
    expect(els).toHaveLength(1);
    expect(els[0]).toMatchObject({ type: "text", text: "Hello\nworld", x: 120 });
    expect((els[0] as any).height).toBe(40); // two lines at 16 pt
    expect(ta.classList.contains("hidden")).toBe(true);
  });

  it("typing pauses become separate undo steps; empty new blocks are discarded", () => {
    ed.setTool("text");
    ptr("pointerdown", 50, 50);
    ptr("pointerup", 50, 50);
    const ta = vp.querySelector("textarea")!;
    ta.value = "first";
    ta.dispatchEvent(new Event("input"));
    vi.advanceTimersByTime(800);
    ta.value = "first second";
    ta.dispatchEvent(new Event("input"));
    ed.endTextEdit();
    expect((session.page.elements[0] as any).text).toBe("first second");
    ed.undo();
    expect((session.page.elements[0] as any).text).toBe("first");
    ed.undo();
    expect(session.page.elements).toHaveLength(0);
    // empty block
    ptr("pointerdown", 300, 300);
    ptr("pointerup", 300, 300);
    ed.endTextEdit();
    expect(session.page.elements).toHaveLength(0);
  });

  it("select tool moves a text block and keeps it fixed relative to ink across zoom", () => {
    ed.setTool("text");
    ptr("pointerdown", 100, 100);
    ptr("pointerup", 100, 100);
    const ta = vp.querySelector("textarea")!;
    ta.value = "move me";
    ta.dispatchEvent(new Event("input"));
    ed.endTextEdit();
    ed.setTool("select");
    const t0 = session.page.elements[0] as any;
    ptr("pointerdown", t0.x + 5, t0.y + 5, { pointerType: "mouse" });
    move(t0.x + 45, t0.y + 25, { pointerType: "mouse" });
    ptr("pointerup", t0.x + 45, t0.y + 25, { pointerType: "mouse" });
    const t1 = session.page.elements[0] as any;
    expect(t1.x).toBeCloseTo(t0.x + 40, 6);
    expect(t1.y).toBeCloseTo(t0.y + 20, 6);
    // zooming changes only the view transform, never document coordinates
    ed.zoomBy(2);
    expect((session.page.elements[0] as any).x).toBe(t1.x);
    ed.undo();
    expect((session.page.elements[0] as any).x).toBeCloseTo(t0.x, 6);
  });

  it("delete selection and template change are undoable", () => {
    drawLine(100, 100, 300, 100);
    ed.setTool("select");
    ed.selectAll();
    ed.deleteSelection();
    expect(session.page.elements).toHaveLength(0);
    ed.undo();
    expect(session.page.elements).toHaveLength(1);
    ed.setTemplate({ kind: "grid", spacing: 18 });
    expect(session.page.template.kind).toBe("grid");
    ed.undo();
    expect(session.page.template.kind).toBe("blank");
  });

  it("wheel pans, ctrl+wheel zooms; neither draws", () => {
    vp.dispatchEvent(new WheelEvent("wheel", { deltaY: 100, clientX: 400, clientY: 300, bubbles: true, cancelable: true }));
    expect(ed.transform.oy).toBe(-100);
    const zoom = new WheelEvent("wheel", { deltaY: -100, clientX: 400, clientY: 300, bubbles: true, cancelable: true });
    Object.defineProperty(zoom, "ctrlKey", { value: true }); // happy-dom ignores ctrlKey in WheelEventInit
    vp.dispatchEvent(zoom);
    expect(ed.transform.scale).toBeGreaterThan(1);
    expect(session.page.elements).toHaveLength(0);
  });

  it("highlighter strokes are translucent and fixed width", () => {
    ed.setTool("highlighter");
    drawLine(100, 100, 300, 100, { pressure: 0.9 });
    expect(session.page.elements[0]).toMatchObject({ tool: "highlighter", pressure: false, opacity: 0.35 });
  });
});
