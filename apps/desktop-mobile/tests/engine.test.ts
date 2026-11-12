import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InkEditor } from "../src/editor/engine";
import { canvasSheetRect } from "../src/editor/renderer";
import { PageSession } from "../src/editor/session";
import type { Page } from "../src/generated/Page";
import type { PdfBackgroundRef } from "../src/generated/PdfBackgroundRef";
import type { NativeHandlers, NativeInkSurface } from "../src/platform/nativeInk";

function page(): Page {
  return { schemaVersion: 1, id: "p1", paper: "letter", size: { width: 612, height: 792 }, template: { kind: "blank", spacing: 24 }, revision: 0, createdAt: 0, updatedAt: 0, elements: [] };
}

let vp: HTMLDivElement;
let ed: InkEditor;
let session: PageSession;
const persisted: any[] = [];

function ptr(type: string, x: number, y: number, o: Partial<PointerEventInit> & { pointerType?: string } = {}) {
  const e = new PointerEvent(type, { altKey: o.altKey ?? false, clientX: x, clientY: y, pointerId: o.pointerId ?? 1, pointerType: o.pointerType ?? "pen", pressure: o.pressure ?? 0.6, button: 0, buttons: type === "pointerup" ? 0 : 1, width: o.width ?? 1, height: o.height ?? 1, bubbles: true, cancelable: true });
  vp.dispatchEvent(e);
  return e;
}

/** Browsers with pointerrawupdate deliver both events; the engine consumes one stream. */
function move(x: number, y: number, o: Parameters<typeof ptr>[3] = {}) {
  ptr("pointerrawupdate", x, y, o);
  ptr("pointermove", x, y, o);
}

/** Replaces the text of the box being edited, as typing would. */
function typeIn(text: string) {
  const view = (ed as any).textEditor.view;
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
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

describe("native pen surface and fingers", () => {
  const touch = { pointerType: "touch", pointerId: 11, pressure: 0.5 };
  const touch2 = { pointerType: "touch", pointerId: 12, pressure: 0.5 };
  const inkCount = () => session.page.elements.filter((el) => el.type === "ink").length;
  const afterPenGrace = () => ((ed as any).penLiftedAt -= 1000);

  async function mountPreview() {
    let handlers: NativeHandlers | null = null;
    const noop = async () => {};
    const surface: NativeInkSurface = {
      kind: "ios-pencil",
      ownsCommittedInk: false,
      capabilities: {},
      mount: async (_r, _t, _tool, _s, h) => void (handlers = h),
      unmount: noop,
      setBounds: noop,
      setViewportTransform: noop,
      setTool: noop,
      setVisible: noop,
      loadCommittedInk: noop,
      addStrokes: noop,
      removeStrokes: noop,
      commitAck: noop,
      metrics: async () => ({}),
    };
    ed.setNativeSurface(surface);
    await vi.waitFor(() => expect(handlers).not.toBeNull());
    return handlers! as NativeHandlers;
  }

  it("gives fingers pan and zoom once the Pencil has touched down, with no pen pointer events", async () => {
    const native = await mountPreview();
    native.penState(true);
    native.penState(false);
    expect(ed.penSeen).toBe(true);
    afterPenGrace();

    ptr("pointerdown", 100, 100, touch);
    ptr("pointermove", 130, 150, touch);
    ptr("pointerup", 130, 150, touch);
    expect(ed.transform).toMatchObject({ scale: 1, ox: 30, oy: 50 });

    ptr("pointerdown", 100, 100, touch);
    ptr("pointerdown", 200, 100, touch2);
    ptr("pointermove", 300, 100, touch2);
    expect(ed.transform.scale).toBeCloseTo(2);
    ptr("pointerup", 100, 100, touch);
    ptr("pointerup", 300, 100, touch2);
    expect(inkCount()).toBe(0);
  });

  it("drops a resting palm's pan at pen-down and ignores contacts until pen-up", async () => {
    const native = await mountPreview();
    native.penState(true);
    native.penState(false);
    afterPenGrace();
    ptr("pointerdown", 100, 100, touch); // the palm lands first
    native.penState(true);
    ptr("pointermove", 180, 180, touch);
    ptr("pointerdown", 300, 300, touch2);
    ptr("pointermove", 340, 340, touch2);
    expect(ed.transform).toMatchObject({ ox: 0, oy: 0 });
    native.penState(false);
    ptr("pointerup", 180, 180, touch);
    ptr("pointerup", 340, 340, touch2);
    afterPenGrace();
    ptr("pointerdown", 100, 100, touch);
    ptr("pointermove", 110, 100, touch);
    expect(ed.transform.ox).toBe(10);
  });

  it("does not leave fingers ignored if the surface is hidden while the pen is down", async () => {
    const native = await mountPreview();
    await vi.waitFor(() => expect(ed.nativeIsVisible).toBe(true));
    native.penState(true);
    ed.setOverlayBlocked(true);
    ed.setOverlayBlocked(false);
    ptr("pointerdown", 100, 100, touch);
    ptr("pointermove", 110, 100, touch);
    expect(ed.transform.ox).toBe(10);
  });

  it("turns a one-finger stroke into a pinch when a second finger lands", () => {
    ed.setSettings({ penOnly: "off" });
    ptr("pointerdown", 100, 100, touch);
    move(102, 100, touch);
    ptr("pointerdown", 200, 100, touch2);
    ptr("pointermove", 300, 100, touch2);
    expect(ed.transform.scale).toBeCloseTo(198 / 98); // fingers 98 px apart, now 198
    ptr("pointerup", 102, 100, touch);
    ptr("pointerup", 300, 100, touch2);
    vi.advanceTimersByTime(1000);
    expect(inkCount()).toBe(0);
  });

  it("keeps a finished-looking finger stroke when a second finger lands late", () => {
    ed.setSettings({ penOnly: "off" });
    ptr("pointerdown", 100, 100, touch);
    for (let x = 110; x <= 170; x += 10) move(x, 100, touch);
    (ed as any).active.t0 -= 1000;
    ptr("pointerdown", 300, 100, touch2);
    expect(inkCount()).toBe(1);
    expect((ed as any).active).toBeNull();
  });

  it("drops a finger stroke when the Pencil touches down", async () => {
    ed.setSettings({ penOnly: "off" });
    const native = await mountPreview();
    ptr("pointerdown", 100, 100, touch); // the palm lands first
    for (let x = 110; x <= 170; x += 10) move(x, 100, touch);
    native.penState(true);
    move(180, 100, touch);
    native.penState(false);
    ptr("pointerup", 180, 100, touch);
    expect(inkCount()).toBe(0);
  });

  it("ignores a contact that lands right after the Pencil lifts", async () => {
    const native = await mountPreview();
    native.penState(true);
    native.penState(false);
    ptr("pointerdown", 100, 100, touch);
    ptr("pointermove", 130, 100, touch);
    ptr("pointerup", 130, 100, touch);
    expect(ed.transform.ox).toBe(0);
    afterPenGrace();
    ptr("pointerdown", 100, 100, touch);
    ptr("pointermove", 130, 100, touch);
    expect(ed.transform.ox).toBe(30);
  });

  it("commits a Pencil stroke once when the web view also receives it", async () => {
    const native = await mountPreview();
    await vi.waitFor(() => expect(ed.nativeIsVisible).toBe(true));
    drawLine(100, 100, 200, 100, { pointerType: "pen" });
    native.strokeFinalized({ id: "s1", z: 1, tool: "pen", color: "#111111", opacity: 1, width: 2, pressure: true, source: "stylus", input: {}, points: [{ x: 100, y: 100, pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: 0 }] } as any);
    expect(inkCount()).toBe(1);
  });

  it("still draws from pen pointer events while the native surface is hidden", async () => {
    await mountPreview();
    await vi.waitFor(() => expect(ed.nativeIsVisible).toBe(true));
    ed.setOverlayBlocked(true);
    drawLine(100, 100, 200, 100, { pointerType: "pen" });
    expect(inkCount()).toBe(1);
  });

  it("counts a finished native stroke as pen use", async () => {
    const native = await mountPreview();
    native.strokeFinalized({ id: "s1", z: 1, tool: "pen", color: "#111111", opacity: 1, width: 2, pressure: true, source: "stylus", input: {}, points: [{ x: 1, y: 1, pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: 0 }] } as any);
    expect(ed.penSeen).toBe(true);
  });
});

describe("editor engine", () => {
  it("a platform pinch zooms about the fingers, relative to where it began", () => {
    ed.setTransform({ scale: 1, ox: 0, oy: 0 });
    ed.platformPinch("begin", 1, 100, 100);
    ed.platformPinch("update", 1.5, 100, 100);
    ed.platformPinch("update", 2, 100, 100);
    expect(ed.transform.scale).toBe(2);
    expect(ed.transform.ox).toBe(-100);
    expect(ed.transform.oy).toBe(-100);
    ed.platformPinch("end", 2, 100, 100);
    ed.platformPinch("update", 3, 100, 100); // a stray update after the end
    expect(ed.transform.scale).toBe(2);
  });

  it("a platform pinch doesn't disturb a stroke in progress", () => {
    ptr("pointerdown", 100, 100);
    const before = ed.transform.scale;
    ed.platformPinch("begin", 1, 100, 100);
    ed.platformPinch("update", 2, 100, 100);
    expect(ed.transform.scale).toBe(before);
    ptr("pointerup", 100, 100);
  });

  it("outlines search matches without touching the page, pans to them and clears on pen down", () => {
    ed.showMatches([{ x: 100, y: 700, w: 50, h: 20 }]);
    const boxes = vp.querySelectorAll<HTMLElement>(".penroam-match");
    expect(boxes).toHaveLength(1);
    expect(boxes[0].style.left).toBe("97px");
    expect(boxes[0].style.top).toBe("697px");
    expect(session.page.elements).toHaveLength(0);
    // the match (y 700-720) is centred in the 600 px high viewport
    expect(ed.transform.oy).toBeLessThan(0);
    const [, y0, , y1] = ed.visibleDocRect();
    expect(y0).toBeLessThan(700);
    expect(y1).toBeGreaterThan(720);
    ptr("pointerdown", 10, 10, { pointerType: "mouse" });
    ptr("pointerup", 10, 10, { pointerType: "mouse" });
    expect(vp.querySelectorAll(".penroam-match")).toHaveLength(0);
  });

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
    const ta = vp.querySelector<HTMLElement>(".penroam-textedit")!;
    expect(ta.classList.contains("hidden")).toBe(false);
    typeIn("Hello\nworld");
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
    typeIn("first");
    vi.advanceTimersByTime(800);
    typeIn("first second");
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

  describe("text on paper with rows", () => {
    const type = (x: number, y: number, text: string) => {
      ed.setTool("text");
      ptr("pointerdown", x, y);
      ptr("pointerup", x, y);
      typeIn(text);
      ed.endTextEdit();
      return session.page.elements[session.page.elements.length - 1] as any;
    };
    const ruled = () => ed.setTemplate({ kind: "ruled", spacing: 24 });

    it("locks new text to the rule below the tap", () => {
      ruled();
      const t = type(100, 200, "hello\nworld");
      expect(t.rowPitch).toBe(24);
      expect(t.y).toBe(192); // baselines on the rules at 216 and 240
      expect(t.height).toBe(48);
    });

    it("locks to dotted paper's rows, and not to grid or blank paper", () => {
      ed.setTemplate({ kind: "dotted", spacing: 20 });
      const dotted = type(100, 205, "dots");
      expect(dotted).toMatchObject({ rowPitch: 20, y: 200, height: 20 });
      ed.setTemplate({ kind: "grid", spacing: 18 });
      expect(type(100, 300, "grid").rowPitch).toBeUndefined();
      ed.setTemplate({ kind: "blank", spacing: 24 });
      expect(type(100, 400, "blank").rowPitch).toBeUndefined();
    });

    it("keeps text below the header of ruled paper", () => {
      ruled();
      expect(type(100, 5, "top").y).toBe(48); // the first rule is at 72
    });

    it("places text anywhere with free placement on", () => {
      ruled();
      ed.setToolSettings({ textFreePlacement: true });
      const t = type(100, 203, "free");
      expect(t.rowPitch).toBeUndefined();
      expect(t.y).toBe(193);
      expect(t.height).toBe(20);
    });

    it("moves locked text by whole rows and keeps free text free", () => {
      ruled();
      const t = type(100, 200, "locked");
      ed.setTool("select");
      ptr("pointerdown", t.x + 5, t.y + 5, { pointerType: "mouse" });
      move(t.x + 45, t.y + 35, { pointerType: "mouse" });
      ptr("pointerup", t.x + 45, t.y + 35, { pointerType: "mouse" });
      const moved = session.page.elements[0] as any;
      expect(moved.x).toBeCloseTo(t.x + 40, 6);
      expect(moved.y).toBe(t.y + 24);
    });

    it("a selection's toggle frees and relocks a block, and undo restores it", () => {
      ruled();
      const t = type(100, 200, "hello");
      ed.setTool("select");
      ptr("pointerdown", t.x + 5, t.y + 5, { pointerType: "mouse" });
      ptr("pointerup", t.x + 5, t.y + 5, { pointerType: "mouse" });
      ed.setSelectionFreePlacement(true);
      expect(session.page.elements[0]).toMatchObject({ y: t.y, height: 20 });
      expect((session.page.elements[0] as any).rowPitch).toBeUndefined();
      ed.setSelectionFreePlacement(false);
      expect(session.page.elements[0]).toMatchObject({ y: t.y, height: 24, rowPitch: 24 });
      ed.undo();
      expect((session.page.elements[0] as any).rowPitch).toBeUndefined();
    });

    it("an unrelated settings change leaves the block being edited alone", () => {
      ruled();
      ed.setToolSettings({ textFreePlacement: true });
      const t = type(100, 203, "free");
      ptr("pointerdown", t.x + 5, t.y + 5);
      ptr("pointerup", t.x + 5, t.y + 5);
      ed.setToolSettings({ textFreePlacement: true, textColor: "#1d4ed8" });
      ed.endTextEdit();
      const after = session.page.elements[0] as any;
      expect(after.rowPitch).toBeUndefined();
      expect(after.y).toBe(193);
    });

    it("flipping the setting while typing moves the block being edited", () => {
      ruled();
      ed.setToolSettings({ textFreePlacement: true });
      const t = type(100, 203, "free");
      ptr("pointerdown", t.x + 5, t.y + 5);
      ptr("pointerup", t.x + 5, t.y + 5);
      ed.setToolSettings({ textFreePlacement: false });
      ed.endTextEdit();
      expect(session.page.elements[0]).toMatchObject({ rowPitch: 24, y: 192, height: 24 });
    });

    it("locked text follows a change of paper, in one undoable step", () => {
      ruled();
      const t = type(100, 200, "hello");
      ed.setTemplate({ kind: "dotted", spacing: 20 });
      expect(session.page.elements[0]).toMatchObject({ rowPitch: 20, y: 200, height: 20 });
      ed.undo();
      expect(session.page.template.kind).toBe("ruled");
      expect(session.page.elements[0]).toMatchObject({ rowPitch: 24, y: t.y, height: 24 });
    });

    const drag = (x0: number, y0: number, x1: number, y1: number, opts: any = {}) => {
      ed.setTool("select");
      ptr("pointerdown", x0, y0, { pointerType: "mouse", ...opts });
      move((x0 + x1) / 2, (y0 + y1) / 2, { pointerType: "mouse", ...opts });
      move(x1, y1, { pointerType: "mouse", ...opts });
      ptr("pointerup", x1, y1, { pointerType: "mouse", ...opts });
    };
    const last = () => session.page.elements[session.page.elements.length - 1] as any;

    it("a dragged block snaps to another block's left edge", () => {
      ruled();
      type(100, 200, "above");
      const t = type(300, 300, "below");
      drag(t.x + 5, t.y + 5, t.x + 5 - 195, t.y + 5);
      expect(last().x).toBe(100);
      expect(last().y).toBe(t.y);
    });

    it("free text also snaps to tops, and Alt turns snapping off", () => {
      ed.setToolSettings({ textFreePlacement: true });
      type(100, 200, "left");
      const t = type(400, 260, "right");
      drag(t.x + 5, t.y + 5, t.x + 5, t.y + 5 - 57);
      expect(last().y).toBe(190);
      const u = last();
      drag(u.x + 5, u.y + 5, u.x + 5 - 297, u.y + 5, { altKey: true });
      expect(last().x).toBe(u.x - 297);
    });

    it("a new block lands under the left edge of the text beside it", () => {
      ruled();
      type(100, 200, "first");
      expect(type(104, 300, "second").x).toBe(100);
      expect(type(140, 400, "third").x).toBe(140);
    });

    it("row-locked text stays on its row while it aligns", () => {
      ruled();
      type(100, 200, "above");
      const t = type(300, 300, "below");
      drag(t.x + 5, t.y + 5, t.x + 5 - 198, t.y + 5 + 3);
      expect(last().x).toBe(100);
      expect(last().y % 24).toBe(0);
    });

    const press = (target: Element, x: number, y: number) =>
      target.dispatchEvent(new PointerEvent("pointerdown", { clientX: x, clientY: y, pointerId: 1, pointerType: "mouse", button: 0, buttons: 1, bubbles: true, cancelable: true }));

    it("the Text tool edits a block on a tap and moves it on a drag", () => {
      ruled();
      const t = type(300, 300, "hello");
      ptr("pointerdown", t.x + 5, t.y + 5, { pointerType: "mouse" });
      ptr("pointerup", t.x + 5, t.y + 5, { pointerType: "mouse" });
      const ta = vp.querySelector<HTMLElement>(".penroam-textedit")!;
      expect(ta.classList.contains("hidden")).toBe(false);
      ed.endTextEdit();
      ptr("pointerdown", t.x + 5, t.y + 5, { pointerType: "mouse" });
      move(t.x + 45, t.y + 5, { pointerType: "mouse" });
      move(t.x + 85, t.y + 5, { pointerType: "mouse" });
      ptr("pointerup", t.x + 85, t.y + 5, { pointerType: "mouse" });
      expect(ta.classList.contains("hidden")).toBe(true);
      expect(last().x).toBe(t.x + 80);
    });

    it("a box being edited moves when its frame is dragged", () => {
      for (const tool of ["select", "text"] as const) {
        ed.setTool(tool);
        ruled();
        const t = type(300, 300, `hello ${tool}`);
        ed.setTool(tool);
        ptr("pointerdown", t.x + 5, t.y + 5, { pointerType: "mouse" });
        ptr("pointerup", t.x + 5, t.y + 5, { pointerType: "mouse" });
        if (tool === "select") ed.editSelectedText();
        const frame = vp.querySelector(".penroam-editframe")!;
        expect(frame.classList.contains("hidden")).toBe(false);
        press(frame, t.x - 4, t.y + 5);
        move(t.x + 36, t.y + 5, { pointerType: "mouse" });
        move(t.x + 76, t.y + 5, { pointerType: "mouse" });
        ptr("pointerup", t.x + 76, t.y + 5, { pointerType: "mouse" });
        expect(frame.classList.contains("hidden")).toBe(true);
        expect(last().x).toBe(t.x + 80);
        session.page.elements.length = 0;
      }
    });

    it("dragging inside a box being edited leaves it for selecting text", () => {
      ruled();
      const t = type(300, 300, "hello");
      ptr("pointerdown", t.x + 5, t.y + 5, { pointerType: "mouse" });
      ptr("pointerup", t.x + 5, t.y + 5, { pointerType: "mouse" });
      press(vp.querySelector<HTMLElement>(".penroam-textedit")!, t.x + 5, t.y + 5);
      expect(vp.querySelector<HTMLElement>(".penroam-textedit")!.classList.contains("hidden")).toBe(false);
      expect(last().x).toBe(t.x);
    });

    it("text running off the page keeps a resize handle on the page", () => {
      ruled();
      const t = type(100, 200, Array.from({ length: 120 }, (_, i) => `line ${i}`).join("\n"));
      expect(t.y + t.height).toBeGreaterThan(792);
      drag(t.x + 5, t.y + 5, t.x + 5, t.y + 5); // select
      ed.zoomBy(0.5); // the page bottom in view
      const s = ed.transform.scale;
      const screen = (x: number, y: number): [number, number] => [x * s + ed.transform.ox, y * s + ed.transform.oy];
      const handle = vp.querySelector<HTMLElement>(".penroam-handle")!;
      expect(handle.style.top).toBe("792px");
      const [hx, hy] = screen(t.x + t.width, 792);
      ptr("pointerdown", hx, hy, { pointerType: "mouse" });
      move(hx + 100, hy, { pointerType: "mouse" });
      ptr("pointerup", hx + 100, hy, { pointerType: "mouse" });
      expect(last().width).toBeCloseTo(t.width + 100 / s, 6);
    });

    it("a box can't be dragged wider than the page", () => {
      ruled();
      const t = type(100, 200, "short");
      drag(t.x + 5, t.y + 5, t.x + 5, t.y + 5);
      ed.zoomBy(0.5);
      const s = ed.transform.scale;
      const hx = (t.x + t.width) * s + ed.transform.ox;
      const hy = (t.y + t.height) * s + ed.transform.oy;
      ptr("pointerdown", hx, hy, { pointerType: "mouse" });
      move(hx + 2000, hy, { pointerType: "mouse" });
      ptr("pointerup", hx + 2000, hy, { pointerType: "mouse" });
      expect(last().width).toBe(612 - t.x);
    });

    it("sizes a block from its rendered Markdown, not its source", () => {
      ruled();
      expect(type(100, 200, "# Title").height).toBe(48); // 28 pt takes two rows
      expect(type(100, 400, "| a |\n|---|\n| b |").height).toBe(48); // the delimiter row is not a line
    });
  });

  it("select tool moves a text block and keeps it fixed relative to ink across zoom", () => {
    ed.setTool("text");
    ptr("pointerdown", 100, 100);
    ptr("pointerup", 100, 100);
    typeIn("move me");
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

describe("links in text blocks", () => {
  const opened: string[] = [];
  const mouse = { pointerType: "mouse" };
  beforeEach(async () => {
    opened.length = 0;
    ed.dispose();
    ed = new InkEditor(vp, { onState: () => {}, onError: (m) => { throw new Error(m); }, onPageEdited: () => {}, openLink: (u) => opened.push(u) });
    const p = page();
    p.elements.push({ type: "text", id: "t1", z: 1, x: 100, y: 100, width: 300, height: 20, text: "[link](https://a.example/) rest", fontSize: 16, color: "#000000", align: "left" });
    session = new PageSession("nb", p, async () => ({ revision: 1, updatedAt: 0, durable: true }));
    await ed.setSession(session, null);
    ed.setTransform({ scale: 1, ox: 0, oy: 0 });
  });
  const click = (x: number, y: number, o: Parameters<typeof ptr>[3] = mouse) => {
    ptr("pointerdown", x, y, o);
    ptr("pointerup", x, y, o);
  };

  it("opens on a hand-tool tap, not on a drag or off the link", () => {
    ed.setTool("hand");
    click(110, 112);
    expect(opened).toEqual(["https://a.example/"]);
    click(170, 112);
    ptr("pointerdown", 110, 112, mouse);
    move(160, 112, mouse);
    ptr("pointerup", 160, 112, mouse);
    expect(opened).toEqual(["https://a.example/"]);
  });

  it("opens on a finger tap in pen-only mode, not when it becomes a pinch", () => {
    ed.setTool("pen");
    ed.setSettings({ penOnly: "on" });
    const touch = { pointerType: "touch", pointerId: 11, pressure: 0.5 };
    click(110, 112, touch);
    expect(opened).toEqual(["https://a.example/"]);
    ptr("pointerdown", 110, 112, touch);
    ptr("pointerdown", 300, 300, { ...touch, pointerId: 12 });
    ptr("pointerup", 110, 112, touch);
    expect(opened).toHaveLength(1);
  });

  it("opens on Ctrl-click with any tool, and a plain click with the pen draws", () => {
    ed.setTool("pen");
    vp.dispatchEvent(new PointerEvent("pointerdown", { clientX: 110, clientY: 112, pointerId: 3, pointerType: "mouse", button: 0, buttons: 1, ctrlKey: true, bubbles: true, cancelable: true }));
    expect(opened).toEqual(["https://a.example/"]);
    expect(session.page.elements.filter((e) => e.type === "ink")).toHaveLength(0);
  });
});

describe("task checkboxes in text blocks", () => {
  // At 16px with no row lock the first box spans about x 106.7-117.6, y 104-115.
  const BOX = [112, 110] as const;
  const mouse = { pointerType: "mouse" };
  const text = () => (session.page.elements.find((e) => e.id === "t1") as any).text as string;
  const inks = () => session.page.elements.filter((e) => e.type === "ink");
  const tap = (o: Parameters<typeof ptr>[3] = {}) => {
    ptr("pointerdown", BOX[0], BOX[1], o);
    ptr("pointerup", BOX[0], BOX[1], o);
  };
  beforeEach(() => {
    session.commit({ label: "add", forward: [{ op: "addElements", elements: [{ type: "text", id: "t1", z: 1, x: 100, y: 100, width: 300, height: 44, text: "- [ ] one\n- [x] two", fontSize: 16, color: "#000000", align: "left" }] }], inverse: [{ op: "removeElements", ids: ["t1"] }] });
  });

  it("ticks on a pen tap with the pen tool, leaving no ink, and undoes", () => {
    ed.setTool("pen");
    tap();
    expect(text()).toBe("- [x] one\n- [x] two");
    expect(inks()).toHaveLength(0);
    ed.undo();
    expect(text()).toBe("- [ ] one\n- [x] two");
  });

  it("still draws when a pen stroke starts on the box and moves away", () => {
    ed.setTool("pen");
    drawLine(BOX[0], BOX[1], BOX[0] + 60, BOX[1]);
    expect(text()).toBe("- [ ] one\n- [x] two");
    expect(inks()).toHaveLength(1);
  });

  it("toggles on a select-tool click without selecting, and a drag moves the block", () => {
    ed.setTool("select");
    tap(mouse);
    expect(text()).toBe("- [x] one\n- [x] two");
    expect(ed.selection.size).toBe(0);
    tap(mouse);
    expect(text()).toBe("- [ ] one\n- [x] two");
    ptr("pointerdown", BOX[0], BOX[1], mouse);
    move(BOX[0] + 40, BOX[1], mouse);
    ptr("pointerup", BOX[0] + 40, BOX[1], mouse);
    expect(text()).toBe("- [ ] one\n- [x] two");
    expect(ed.selection.has("t1")).toBe(true);
    expect((session.page.elements.find((e) => e.id === "t1") as any).x).toBe(140);
  });

  it("toggles on a finger tap that would otherwise pan and with the hand tool, not with other tools", () => {
    ed.setTool("pen");
    ed.setSettings({ penOnly: "on" });
    const touch = { pointerType: "touch", pointerId: 11, pressure: 0.5 };
    tap(touch);
    expect(text()).toBe("- [x] one\n- [x] two");
    ed.setTool("hand");
    tap(mouse);
    ed.setTool("highlighter");
    tap(mouse);
    expect(text()).toBe("- [ ] one\n- [x] two");
  });

  it("doesn't tick when the block changed under the press", () => {
    ed.setTool("select");
    ptr("pointerdown", BOX[0], BOX[1], mouse);
    const t = session.page.elements[0] as any;
    session.commit({ label: "edit", forward: [{ op: "replaceElements", elements: [{ ...t, text: "x - [ ] one" }] }], inverse: [{ op: "replaceElements", elements: [t] }] });
    ptr("pointerup", BOX[0], BOX[1], mouse);
    expect(text()).toBe("x - [ ] one");
  });

  it("ticks on a tap from a native surface and has it drop the dot", async () => {
    const removed: string[][] = [];
    let handlers: NativeHandlers | null = null;
    const noop = async () => {};
    const surface: NativeInkSurface = {
      kind: "boox",
      ownsCommittedInk: true,
      capabilities: {},
      mount: async (_r, _t, _tool, _s, h) => void (handlers = h),
      unmount: noop,
      setBounds: noop,
      setViewportTransform: noop,
      setTool: noop,
      setVisible: noop,
      loadCommittedInk: noop,
      addStrokes: noop,
      removeStrokes: async (ids) => void removed.push(ids),
      commitAck: noop,
      metrics: async () => ({}),
    };
    ed.setTool("pen");
    ed.setSettings({ useNativeInk: true });
    ed.setNativeSurface(surface);
    await vi.waitFor(() => expect(handlers).not.toBeNull());
    const point = { x: BOX[0], y: BOX[1], pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: 0 };
    handlers!.strokeFinalized({ id: "dot", z: 1, tool: "pen", color: "#111111", opacity: 1, width: 2, pressure: true, source: "stylus", input: {}, points: [point] } as any);
    expect(text()).toBe("- [x] one\n- [x] two");
    expect(inks()).toHaveLength(0);
    await vi.waitFor(() => expect(removed).toEqual([["dot"]]));
  });
});

describe("infinite canvas", () => {
  const settle = () => vi.advanceTimersByTime(200);
  const ref = (pageIndex: number): PdfBackgroundRef => ({ assetHash: "a".repeat(64), pageIndex, mediaBox: [0, 0, 300, 400], cropBox: [0, 0, 300, 400], rotation: 0 });

  it("draws and pans anywhere, past where a sheet would stop", async () => {
    ed.setCanvas(true);
    await vi.waitFor(() => expect(ed.isCanvas).toBe(true));
    ed.setTransform({ scale: 1, ox: 3000, oy: 2500 });
    // a sheet keeps 80 px of the page on screen; a canvas leaves the view where it was put
    expect(ed.transform).toEqual({ scale: 1, ox: 3000, oy: 2500 });
    settle();
    drawLine(100, 100, 200, 160);
    const s = session.page.elements[0];
    expect(s.type).toBe("ink");
    if (s.type !== "ink") return;
    expect(s.points[0].x).toBeCloseTo(-2900);
    expect(s.points[0].y).toBeCloseTo(-2400);
    // the layers moved to cover the view, and the new stroke in it
    const [x0, y0, x1, y1] = ed.renderer.extent;
    expect(x0).toBeLessThanOrEqual(-3000);
    expect(y0).toBeLessThanOrEqual(-2500);
    expect(x1).toBeGreaterThanOrEqual(-2200);
    expect(y1).toBeGreaterThanOrEqual(-1900);
  });

  it("places text where it is tapped, above and left of the origin too", async () => {
    session.page.template = { kind: "dotted", spacing: 24 };
    ed.setCanvas(true);
    await vi.waitFor(() => expect(ed.isCanvas).toBe(true));
    ed.setTransform({ scale: 1, ox: 1000, oy: 1000 });
    ed.setTool("text");
    ptr("pointerdown", 10, 10);
    ptr("pointerup", 10, 10);
    typeIn("hello");
    ed.endTextEdit();
    const t = session.page.elements.find((e) => e.type === "text");
    expect(t?.type).toBe("text");
    if (t?.type !== "text") return;
    expect(t.x).toBeCloseTo(-990);
    // locked to the rows, which run above the origin on a dotted canvas
    expect(t.y).toBeLessThan(0);
    expect(t.y % 24).toBeCloseTo(0);
  });

  it("puts imported PDF pages below the content, under everything, as one undo step", async () => {
    ed.setCanvas(true);
    await vi.waitFor(() => expect(ed.isCanvas).toBe(true));
    drawLine(50, 50, 120, 300);
    ed.placePdfPages([ref(0), ref(1)]);
    const pdfs = session.page.elements.filter((e) => e.type === "image" && e.pdfPage);
    expect(pdfs).toHaveLength(2);
    const ink = session.page.elements.find((e) => e.type === "ink")!;
    for (const p of pdfs) {
      if (p.type !== "image") continue;
      expect(p.z).toBeLessThan(ink.z);
      expect([p.width, p.height]).toEqual([300, 400]);
    }
    const [a, b] = pdfs as Extract<(typeof pdfs)[number], { type: "image" }>[];
    expect(a.y).toBeGreaterThan(300);
    expect(b.y).toBeGreaterThanOrEqual(a.y + a.height);
    // a press on a PDF page selects nothing: it is paper
    ed.setTool("select");
    const { scale, ox, oy } = ed.transform;
    const [sx, sy] = [(a.x + 150) * scale + ox, (a.y + 200) * scale + oy];
    expect(sx > 0 && sx < 800 && sy > 0 && sy < 600).toBe(true);
    ptr("pointerdown", sx, sy, { pointerType: "mouse" });
    ptr("pointerup", sx, sy, { pointerType: "mouse" });
    expect(ed.selection.size).toBe(0);
    ed.undo();
    expect(session.page.elements.filter((e) => e.type === "image")).toHaveLength(0);
  });

  it("fits the content, never above 100 %, and shows sheets again when turned off", async () => {
    ed.setCanvas(true);
    await vi.waitFor(() => expect(ed.isCanvas).toBe(true));
    drawLine(100, 100, 130, 120);
    ed.fit("width");
    expect(ed.transform.scale).toBe(1);
    ed.setCanvas(false);
    await vi.waitFor(() => expect(ed.isCanvas).toBe(false));
    expect(ed.renderer.extent).toEqual([0, 0, 612, 792]);
  });
});

describe("ruled infinite canvas", () => {
  const settle = () => vi.advanceTimersByTime(200);
  const ruled = async () => {
    session.page.template = { kind: "ruled", spacing: 24 };
    ed.setCanvas(true);
    await vi.waitFor(() => expect(ed.isCanvas).toBe(true));
  };

  it("is the page's width and goes on downward", async () => {
    await ruled();
    ed.fit("width");
    // fitted like a sheet, from the top
    expect(ed.transform.oy).toBeGreaterThanOrEqual(0);
    const { scale } = ed.transform;
    ed.setTransform({ scale, ox: 0, oy: -1000 * scale });
    settle();
    const [x0, y0, x1, y1] = ed.renderer.extent;
    expect([x0, y0, x1]).toEqual([0, 0, 612]);
    expect(y1).toBeGreaterThan(1000);
    // nowhere to go above the top or off to the side
    ed.setTransform({ scale, ox: 5000, oy: 5000 });
    expect(ed.transform.ox).toBeLessThan(800);
    expect(ed.transform.oy).toBeLessThan(600);
  });

  it("can go a page past its content, however long that is", async () => {
    await ruled();
    const { scale } = ed.transform;
    ed.setTransform({ scale, ox: 0, oy: -1e6 });
    expect(-ed.transform.oy / scale).toBeLessThan(792 * 2);
    session.page.elements.push({ type: "ink", id: "far", z: 1, tool: "pen", color: "#111111", opacity: 1, width: 2, pressure: true, source: "stylus", input: {}, points: [{ x: 100, y: 5000, pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: 0 }] } as any);
    ed.setTransform({ scale, ox: 0, oy: -1e6 });
    expect(-ed.transform.oy / scale).toBeGreaterThan(5000 - 600 / scale);
    expect(-ed.transform.oy / scale).toBeLessThan(5000 + 792 * 2);
  });

  it("keeps text on the page and below the header", async () => {
    await ruled();
    ed.setTransform({ scale: 1, ox: 0, oy: 0 });
    ed.setTool("text");
    ptr("pointerdown", 5, 5);
    ptr("pointerup", 5, 5);
    typeIn("hello");
    ed.endTextEdit();
    const t = session.page.elements.find((e) => e.type === "text");
    if (t?.type !== "text") throw new Error("no text");
    expect(t.y).toBe(48);
    expect(t.x).toBeGreaterThanOrEqual(0);
    expect(t.x + t.width).toBeLessThanOrEqual(612);
  });
});

describe("infinite canvas pictures", () => {
  const blank = (elements: Page["elements"]) => ({ size: { width: 612, height: 792 }, template: { kind: "grid" as const, spacing: 24 }, elements });
  const note: Page["elements"][number] = { type: "text", id: "t", z: 1, x: 100, y: 100, width: 120, height: 20, text: "hi", fontSize: 16, color: "#111111", align: "left" };

  it("previews a small note on a whole page, from its corner", () => {
    const [x0, y0, x1, y1] = canvasSheetRect(blank([note]), true);
    expect([x1 - x0, y1 - y0]).toEqual([612, 792]);
    expect(x0).toBeLessThanOrEqual(100 - 36);
    expect(y0).toBeLessThanOrEqual(100 - 36);
    // the tight cut, for PNG export, is just the note and its margin
    const t = canvasSheetRect(blank([note]));
    expect(t[2] - t[0]).toBeLessThan(612);
  });

  it("grows a preview to keep wide or long content whole, in the page's shape", () => {
    const wide = { ...note, width: 2000 };
    const [x0, y0, x1, y1] = canvasSheetRect(blank([wide]), true);
    expect(x1 - x0).toBeGreaterThanOrEqual(2000);
    expect((y1 - y0) / (x1 - x0)).toBeCloseTo(792 / 612);
    const long = { ...note, height: 5000 };
    const r = canvasSheetRect(blank([long]), true);
    expect(r[3] - r[1]).toBeGreaterThanOrEqual(5000);
    expect((r[3] - r[1]) / (r[2] - r[0])).toBeCloseTo(792 / 612);
  });

  it("cuts a ruled canvas as its column: the page's width from the top", () => {
    const ruled = (elements: Page["elements"]) => ({ ...blank(elements), template: { kind: "ruled" as const, spacing: 24 } });
    expect(canvasSheetRect(ruled([note]))).toEqual([0, 0, 612, 792]);
    const long = { ...note, height: 5000 };
    expect(canvasSheetRect(ruled([long]))).toEqual([0, 0, 612, 100 + 5000 + 36]);
    // a preview is its top page
    expect(canvasSheetRect(ruled([long]), true)).toEqual([0, 0, 612, 792]);
  });
});

describe("ruler", () => {
  const touch = { pointerType: "touch", pointerId: 21, pressure: 0.5 };
  const touch2 = { pointerType: "touch", pointerId: 22, pressure: 0.5 };
  const mouse = { pointerType: "mouse", pointerId: 31 };
  const inks = () => session.page.elements.filter((el) => el.type === "ink") as any[];
  const states: any[] = [];
  const ruler = () => ed.rulerPlacement!;

  beforeEach(() => {
    states.length = 0;
    (ed as any).cb.onState = (s: any) => states.push(s);
  });

  it("toggles, starts across the middle of the view at 0 degrees and reports its state", () => {
    expect(ed.rulerPlacement).toBeNull();
    ed.toggleRuler();
    expect(ruler()).toEqual({ cx: 400, cy: 300, angle: 0 });
    expect(states).toContainEqual(expect.objectContaining({ rulerShown: true, rulerAngle: 0 }));
    ed.toggleRuler();
    expect(ed.rulerPlacement).toBeNull();
    expect(states.at(-1)).toMatchObject({ rulerShown: false });
  });

  it("locks a pen stroke that starts near an edge onto that edge, keeping pressure", () => {
    ed.setRuler(true);
    ed.setTransform({ scale: 2, ox: 10, oy: 20 });
    ptr("pointerdown", 100, 300 + 28 + 10, { pressure: 0.3 });
    move(200, 300 + 28 + 25, { pressure: 0.5 });
    move(300, 300 + 28 - 5, { pressure: 0.7 });
    ptr("pointerup", 400, 300 + 28 + 40, { pressure: 0.4 });
    const [s] = inks();
    expect(s.input.ruler).toEqual({ angle: 0 });
    // every sample lies on the edge line y = 328 px, i.e. page y = (328 - 20) / 2
    for (const p of s.points) expect(p.y).toBeCloseTo(154, 3);
    expect(s.points.map((p: any) => p.x)).toEqual([45, 95, 145, 195]);
    expect(s.points.slice(0, 3).map((p: any) => p.pressure)).toEqual([0.3, 0.5, 0.7]);
  });

  it("locks a stroke at an angle and keeps it on the line however far it wanders", () => {
    ed.setRuler(true);
    for (let i = 0; i < 3; i++) ed.rotateRulerStep(1); // 45 degrees
    expect(ruler().angle).toBe(45);
    // side -1 of a 45 degree ruler through (400, 300) is up and to the right
    const n = Math.SQRT1_2;
    const sx = 400 - 200 * n + 30 * n, sy = 300 - 200 * n - 30 * n;
    drawLine(sx, sy, sx + 150, sy + 60);
    const [s] = inks();
    expect(s.input.ruler).toEqual({ angle: 45 });
    for (const p of s.points) expect(p.y - p.x - (300 - 400) + 28 / n).toBeCloseTo(0, 2);
  });

  it("does not lock a stroke that starts far from the ruler, even if it crosses it", () => {
    ed.setRuler(true);
    drawLine(100, 200, 100, 400);
    const [s] = inks();
    expect(s.input.ruler).toBeUndefined();
    expect(s.points[0]).toMatchObject({ x: 100, y: 200 });
    expect(s.points.at(-1)).toMatchObject({ x: 100, y: 400 });
  });

  it("never locks the eraser or the select tool", () => {
    drawLine(50, 340, 300, 340);
    ed.setRuler(true);
    ed.setTool("eraser");
    drawLine(100, 335, 100, 345);
    expect(inks()).toHaveLength(0);
  });

  it("moves under the mouse on the band without touching the history", () => {
    ed.setRuler(true);
    ptr("pointerdown", 600, 310, mouse);
    ptr("pointermove", 650, 360, mouse);
    ptr("pointerup", 650, 360, mouse);
    expect(ruler()).toEqual({ cx: 450, cy: 350, angle: 0 });
    expect(inks()).toHaveLength(0);
    expect(session.canUndo).toBe(false);
    expect(ed.transform).toMatchObject({ ox: 0, oy: 0 });
  });

  it("stays put under the pen, which draws instead", () => {
    ed.setRuler(true);
    drawLine(600, 310, 700, 310);
    expect(ruler()).toEqual({ cx: 400, cy: 300, angle: 0 });
    const [s] = inks();
    expect(s.input.ruler).toEqual({ angle: 0 });
    for (const p of s.points) expect(p.y).toBe(328);
  });

  it("turns on the dial with detents, for the pen too", () => {
    ed.setRuler(true);
    const a = (deg: number) => [400 + 15 * Math.cos((deg * Math.PI) / 180), 300 + 15 * Math.sin((deg * Math.PI) / 180)] as const;
    ptr("pointerdown", 415, 300);
    move(...a(32));
    expect(ruler().angle).toBe(30);
    move(...a(37));
    expect(ruler().angle).toBeCloseTo(37);
    ptr("pointerup", ...a(37));
    expect(inks()).toHaveLength(0);
    expect(states).toContainEqual({ rulerAngle: 30 });
  });

  it("measures a dial turn from where a press near the centre leaves the dead zone", () => {
    ed.setRuler(true);
    ptr("pointerdown", 401, 301, mouse);
    ptr("pointermove", 401, 305, mouse); // jitter by the centre: no swing
    expect(ruler().angle).toBe(0);
    ptr("pointermove", 400, 312, mouse); // leaves the dead zone straight down: the reference
    expect(ruler().angle).toBe(0);
    ptr("pointermove", 388, 300, mouse); // a quarter turn on from there
    expect(ruler().angle).toBe(0 + 90);
    ptr("pointerup", 388, 300, mouse);
  });

  it("turns with two fingers past 180 degrees without the centre jumping", () => {
    ed.setRuler(true);
    const c = { x: 400, y: 300 };
    const at = (deg: number, sign: 1 | -1) => [c.x + sign * 100 * Math.cos((deg * Math.PI) / 180), c.y + sign * 100 * Math.sin((deg * Math.PI) / 180)] as const;
    ptr("pointerdown", ...at(0, -1), touch);
    ptr("pointerdown", ...at(0, 1), touch2);
    for (let deg = 10; deg <= 200; deg += 10) {
      ptr("pointermove", ...at(deg, -1), touch);
      ptr("pointermove", ...at(deg, 1), touch2);
      // the centre sits between the fingers the whole way round
      expect(ruler().cx).toBeCloseTo(400);
      expect(ruler().cy).toBeCloseTo(300);
    }
    expect(ruler().angle).toBeCloseTo(20);
    ptr("pointerup", ...at(200, -1), touch);
    ptr("pointerup", ...at(200, 1), touch2);
  });

  it("is moved by a finger in pen-only mode and turned by two", () => {
    ed.setSettings({ penOnly: "on" });
    ed.setRuler(true);
    ptr("pointerdown", 300, 300, touch);
    ptr("pointerdown", 500, 300, touch2);
    ptr("pointermove", 500, 300 + 200 * Math.tan(Math.PI / 12), touch2);
    expect(ruler().angle).toBe(15);
    ptr("pointerup", 500, 300, touch2);
    ptr("pointermove", 320, 300, touch);
    ptr("pointerup", 320, 300, touch);
    // it went with the fingers' midpoint, then with the finger left on it
    expect(ruler().angle).toBe(15);
    expect(ruler().cx).toBeCloseTo(420);
    expect(ruler().cy).toBeCloseTo(300 + 100 * Math.tan(Math.PI / 12));
    expect(ed.transform).toMatchObject({ scale: 1, ox: 0, oy: 0 });
    expect(inks()).toHaveLength(0);
  });

  it("stays in the view and does not move with the page", () => {
    ed.setRuler(true);
    ptr("pointerdown", 600, 310, mouse);
    ptr("pointermove", 5000, -900, mouse);
    ptr("pointerup", 5000, -900, mouse);
    expect(ruler()).toMatchObject({ cx: 800, cy: 0 });
    ed.setTransform({ scale: 3, ox: -200, oy: -50 });
    expect(ruler()).toMatchObject({ cx: 800, cy: 0 });
  });

  it("is kept inside the view when the view shrinks or turns", () => {
    ed.setRuler(true);
    ptr("pointerdown", 600, 310, mouse);
    ptr("pointermove", 900, 600, mouse);
    ptr("pointerup", 900, 600, mouse);
    expect(ruler()).toMatchObject({ cx: 700, cy: 590 });
    vp.getBoundingClientRect = () => ({ left: 0, top: 0, width: 600, height: 400, right: 600, bottom: 400, x: 0, y: 0, toJSON() {} }) as DOMRect;
    (ed as any).resize();
    expect(ruler()).toEqual({ cx: 600, cy: 400, angle: 0 });
  });

  it("keeps its place across hiding and pages, and drops a drag on cancel", () => {
    ed.setRuler(true);
    ed.rotateRulerStep(-1);
    ed.setRuler(false);
    ed.rotateRulerStep(1); // hidden: nothing to turn
    ed.setRuler(true);
    expect(ruler().angle).toBe(165);
    ptr("pointerdown", 400, 300, mouse);
    ed.cancelGestures();
    ptr("pointermove", 500, 300, mouse);
    expect(ruler()).toMatchObject({ cx: 400, cy: 300, angle: 165 });
  });

  /** A sample event carrying coalesced and predicted samples, as Chromium delivers them. */
  function richMove(x: number, y: number, coalesced: [number, number][], predicted: [number, number][]) {
    const mk = (type: string, px: number, py: number) => new PointerEvent(type, { clientX: px, clientY: py, pointerId: 1, pointerType: "pen", pressure: 0.6, buttons: 1, bubbles: true, cancelable: true });
    const e = mk((ed as any).sampleEvent, x, y);
    (e as any).getCoalescedEvents = () => coalesced.map(([cx, cy]) => mk("pointermove", cx, cy));
    (e as any).getPredictedEvents = () => predicted.map(([px, py]) => mk("pointermove", px, py));
    vp.dispatchEvent(e);
  }

  it("locks coalesced and predicted samples to the edge too", () => {
    (ed as any).hasCoalesced = true;
    (ed as any).hasPredicted = true;
    ed.setRuler(true);
    ptr("pointerdown", 100, 335);
    richMove(160, 350, [[120, 340], [140, 320], [160, 350]], [[180, 360], [200, 300]]);
    const active = (ed as any).active;
    expect(active.predicted.map((p: any) => p.y)).toEqual([328, 328]);
    expect(active.predicted.map((p: any) => p.x)).toEqual([180, 200]);
    ptr("pointerup", 170, 345);
    const [s] = inks();
    expect(s.points.map((p: any) => [p.x, p.y])).toEqual([[100, 328], [120, 328], [140, 328], [160, 328], [170, 328]]);
    expect(s.points.filter((p: any) => p.flags === 2)).toHaveLength(3);
    expect(s.input.coalescedSamples).toBe(3);
  });

  it("lets go of the ruler on pointercancel and on lost capture", () => {
    ed.setRuler(true);
    ptr("pointerdown", 415, 300, mouse);
    ptr("pointercancel", 415, 300, mouse);
    ptr("pointermove", 400, 315, mouse);
    expect(ruler().angle).toBe(0);
    ptr("pointerdown", 600, 310, mouse);
    ptr("pointermove", 610, 320, mouse);
    vp.dispatchEvent(new PointerEvent("lostpointercapture", { pointerId: mouse.pointerId, pointerType: "mouse", bubbles: true }));
    ptr("pointermove", 700, 400, mouse);
    expect(ruler()).toEqual({ cx: 410, cy: 310, angle: 0 });
    // and the next press is an ordinary one again
    ptr("pointerup", 700, 400, mouse);
    drawLine(100, 100, 200, 100, mouse);
    expect(inks()).toHaveLength(1);
  });

  it("ignores a third finger while two hold the ruler", () => {
    const touch3 = { pointerType: "touch", pointerId: 23, pressure: 0.5 };
    ed.setRuler(true);
    ptr("pointerdown", 300, 300, touch);
    ptr("pointerdown", 500, 300, touch2);
    ptr("pointerdown", 600, 100, touch3);
    ptr("pointermove", 650, 180, touch3);
    ptr("pointerup", 650, 180, touch3);
    expect(ed.transform).toMatchObject({ scale: 1, ox: 0, oy: 0 });
    expect(ruler()).toEqual({ cx: 400, cy: 300, angle: 0 });
    expect(inks()).toHaveLength(0);
  });

  it("lets a native Pencil stroke in flight finish before hiding the native layer", async () => {
    let handlers: NativeHandlers | null = null;
    const noop = async () => {};
    const surface: NativeInkSurface = {
      kind: "ios-pencil",
      ownsCommittedInk: false,
      capabilities: {},
      mount: async (_r, _t, _tool, _s, h) => void (handlers = h),
      unmount: noop,
      setBounds: noop,
      setViewportTransform: noop,
      setTool: noop,
      setVisible: noop,
      loadCommittedInk: noop,
      addStrokes: noop,
      removeStrokes: noop,
      commitAck: noop,
      metrics: async () => ({}),
    };
    ed.setNativeSurface(surface);
    await vi.waitFor(() => expect(ed.nativeIsVisible).toBe(true));
    handlers!.penState(true);
    ed.setRuler(true);
    expect(ed.nativeIsVisible).toBe(true);
    handlers!.penState(false);
    expect(ed.nativeIsVisible).toBe(false);
    // with the pen up, it switches at once
    ed.setRuler(false);
    expect(ed.nativeIsVisible).toBe(true);
    ed.setRuler(true);
    expect(ed.nativeIsVisible).toBe(false);
  });

  it("hides the native pen layer while it shows", async () => {
    const noop = async () => {};
    const surface: NativeInkSurface = {
      kind: "boox",
      ownsCommittedInk: true,
      capabilities: {},
      mount: noop,
      unmount: noop,
      setBounds: noop,
      setViewportTransform: noop,
      setTool: noop,
      setVisible: noop,
      loadCommittedInk: noop,
      addStrokes: noop,
      removeStrokes: noop,
      commitAck: noop,
      metrics: async () => ({}),
    };
    ed.setNativeSurface(surface);
    await vi.waitFor(() => expect(ed.nativeIsVisible).toBe(true));
    ed.setRuler(true);
    expect(ed.nativeIsVisible).toBe(false);
    expect(states).toContainEqual(expect.objectContaining({ nativeActive: false }));
    ed.setRuler(false);
    expect(ed.nativeIsVisible).toBe(true);
  });
});
