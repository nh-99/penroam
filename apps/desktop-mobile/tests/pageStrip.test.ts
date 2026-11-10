import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InkEditor } from "../src/editor/engine";
import { layoutStrip, PAGE_GAP, pageAtY } from "../src/editor/pageStrip";
import { PageSession } from "../src/editor/session";
import type { Page } from "../src/generated/Page";

function page(id: string, width = 612, height = 792): Page {
  return { schemaVersion: 1, id, paper: "letter", size: { width, height }, template: { kind: "blank", spacing: 24 }, revision: 0, createdAt: 0, updatedAt: 0, elements: [] };
}

describe("page column layout", () => {
  it("puts the open page at the origin and the others above and below", () => {
    const placed = layoutStrip([{ id: "a" }, { id: "b" }, { id: "c", width: 400, height: 500 }], "b", 612, 792);
    expect(placed.map((p) => p.id)).toEqual(["a", "b", "c"]);
    expect(placed[0]).toMatchObject({ x: 0, y: -792 - PAGE_GAP, width: 612, height: 792 });
    expect(placed[1]).toMatchObject({ x: 0, y: 0 });
    // a smaller page is centred under the open one
    expect(placed[2]).toMatchObject({ x: 106, y: 792 + PAGE_GAP, width: 400, height: 500 });
  });

  it("splits the gaps between pages halfway", () => {
    const placed = layoutStrip([{ id: "a" }, { id: "b" }, { id: "c" }], "a", 100, 100, 20);
    expect(pageAtY(placed, -500)).toBe(0);
    expect(pageAtY(placed, 109)).toBe(0);
    expect(pageAtY(placed, 111)).toBe(1);
    expect(pageAtY(placed, 1000)).toBe(2);
  });

  it("holds only the open page when it is not in the list", () => {
    expect(layoutStrip([{ id: "a" }], "x", 10, 20)).toEqual([{ id: "x", x: 0, y: 0, width: 10, height: 20 }]);
  });
});

describe("continuous scrolling in the editor", () => {
  let vp: HTMLDivElement;
  let ed: InkEditor;
  const scrolledTo: string[] = [];
  const wanted: string[][] = [];
  const session = (id: string) => new PageSession("nb", page(id), async () => ({ revision: 1, updatedAt: 0, durable: true }));

  beforeEach(async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    scrolledTo.length = 0;
    wanted.length = 0;
    vp = document.createElement("div");
    document.body.appendChild(vp);
    vp.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600, x: 0, y: 0, toJSON() {} }) as DOMRect;
    (vp as any).setPointerCapture = () => {};
    ed = new InkEditor(vp, {
      onState: () => {},
      onError: (m) => {
        throw new Error(m);
      },
      onPageEdited: () => {},
      onScrollToPage: (id) => scrolledTo.push(id),
      wantPageImages: (w) => wanted.push(w.map((x) => x.id)),
    });
    ed.setContinuous(true);
    ed.setPageList([{ id: "p1" }, { id: "p2" }, { id: "p3" }]);
    await ed.setSession(session("p1"), null);
    ed.setTransform({ scale: 1, ox: 0, oy: 0 });
  });
  afterEach(() => {
    ed.dispose();
    vp.remove();
    vi.useRealTimers();
  });

  const wheel = (deltaY: number) => vp.dispatchEvent(new WheelEvent("wheel", { deltaY, clientX: 400, clientY: 300, bubbles: true, cancelable: true }));

  it("scrolls past the open page onto the next and asks to open it", () => {
    wheel(600);
    expect(ed.transform.oy).toBe(-600);
    expect(scrolledTo).toEqual(["p2"]);
    // asked once, not on every scroll step
    wheel(10);
    expect(scrolledTo).toEqual(["p2"]);
  });

  it("can't scroll past the first or last page", () => {
    wheel(-5000);
    expect(ed.transform.oy).toBe(600 - 80);
    wheel(1e6);
    expect(ed.transform.oy).toBe(80 - (3 * 792 + 2 * PAGE_GAP));
  });

  it("keeps the view still when the page scrolled onto opens", async () => {
    wheel(600);
    const screenY = ed.transform.oy + 792 + PAGE_GAP; // where p2's top is on screen
    await ed.setSession(session("p2"), null, { keepView: true });
    expect(ed.transform.oy).toBeCloseTo(screenY);
    expect(scrolledTo).toEqual(["p2"]);
  });

  it("fits the page when jumping to it", async () => {
    wheel(600);
    await ed.setSession(session("p3"), null);
    expect(ed.transform.oy).toBe(24);
  });

  it("asks for pictures of the pages near the view", () => {
    vi.advanceTimersByTime(200);
    expect(wanted.at(-1)).toEqual(["p2"]);
  });

  it("opens another page when it is pressed instead of drawing on this one", async () => {
    wheel(600);
    const s2 = session("p2");
    await ed.setSession(s2, null, { keepView: true }); // p1's bottom is still at the top of the view
    scrolledTo.length = 0;
    vp.dispatchEvent(new PointerEvent("pointerdown", { clientX: 300, clientY: 100, pointerId: 1, pointerType: "pen", pressure: 0.5, button: 0, buttons: 1, bubbles: true, cancelable: true }));
    vp.dispatchEvent(new PointerEvent("pointerup", { clientX: 300, clientY: 100, pointerId: 1, pointerType: "pen", pressure: 0, button: 0, buttons: 0, bubbles: true, cancelable: true }));
    expect(scrolledTo).toEqual(["p1"]);
    expect(s2.page.elements).toEqual([]);
  });

  it("shows only the open page when turned off", () => {
    ed.setContinuous(false);
    wheel(5000);
    expect(ed.transform.oy).toBe(80 - 792);
    expect(scrolledTo).toEqual([]);
  });
});
