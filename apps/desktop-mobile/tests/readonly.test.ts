import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InkEditor } from "../src/editor/engine";
import { PageSession } from "../src/editor/session";
import type { Page } from "../src/generated/Page";
import { mount } from "@vue/test-utils";
import { createPinia } from "pinia";
import Toolbar from "../src/components/Toolbar.vue";
import PageSidebar from "../src/components/PageSidebar.vue";
import { accessBanner } from "../src/platform/sharing";

function page(): Page {
  return { schemaVersion: 1, id: "p1", paper: "letter", size: { width: 612, height: 792 }, template: { kind: "blank", spacing: 24 }, revision: 0, createdAt: 0, updatedAt: 0, elements: [] };
}

let vp: HTMLDivElement;
let ed: InkEditor;
let session: PageSession;
const persisted: unknown[] = [];

function ptr(type: string, x: number, y: number, pointerType = "mouse") {
  vp.dispatchEvent(new PointerEvent(type, { clientX: x, clientY: y, pointerId: 1, pointerType, pressure: 0.5, button: 0, buttons: type === "pointerup" ? 0 : 1, bubbles: true, cancelable: true }));
}
function drag(x0: number, y0: number, x1: number, y1: number) {
  ptr("pointerdown", x0, y0);
  for (let i = 1; i <= 6; i++) {
    ptr("pointermove", x0 + ((x1 - x0) * i) / 6, y0 + ((y1 - y0) * i) / 6);
  }
  ptr("pointerup", x1, y1);
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

const add = (id: string) => ({ label: "add", forward: [{ op: "addElements" as const, elements: [{ type: "text" as const, id, z: 1, x: 0, y: 0, width: 50, height: 20, text: id, fontSize: 16, color: "#000000", align: "left" as const }] }], inverse: [{ op: "removeElements" as const, ids: [id] }] });

describe("view-only editor", () => {
  it("draws normally before view-only is switched on", async () => {
    ed.setTool("pen");
    drag(100, 100, 200, 160);
    await vi.advanceTimersByTimeAsync(10);
    expect(session.page.elements.length).toBe(1);
  });

  it("refuses tool changes and turns drawing into panning, enqueueing nothing", async () => {
    ed.setTool("pen");
    ed.setReadOnly(true);
    expect(ed.tool).toBe("hand");
    for (const t of ["pen", "highlighter", "eraser", "select", "lasso", "text"] as const) {
      ed.setTool(t);
      expect(ed.tool).toBe("hand");
    }
    drag(100, 100, 160, 140);
    await vi.advanceTimersByTimeAsync(10);
    expect(session.page.elements).toEqual([]);
    expect(persisted).toEqual([]);
    expect(ed.transform.ox).toBeCloseTo(60); // the hand still pans
    expect(ed.transform.oy).toBeCloseTo(40);
  });

  it("returns to editing when access comes back", () => {
    ed.setReadOnly(true);
    ed.setReadOnly(false);
    ed.setTool("pen");
    expect(ed.tool).toBe("pen");
  });

  it("refuses template, delete and select-all commands", async () => {
    session.commit(add("a"));
    ed.setReadOnly(true);
    ed.selectAll();
    ed.deleteSelection();
    ed.setTemplate({ kind: "ruled", spacing: 24 });
    await vi.advanceTimersByTimeAsync(10);
    expect(session.page.elements.map((e) => e.id)).toEqual(["a"]);
    expect(session.page.template.kind).toBe("blank");
    expect(persisted.length).toBe(1); // only the edit made before view-only
  });

  it("does not attach native ink for writing", () => {
    ed.setReadOnly(true);
    const ctx = { native: {}, settings: { useNativeInk: true }, session, tool: "pen" };
    const want = (viewOnly: boolean) => (ed as any).wantNative.call({ ...ctx, viewOnly });
    expect(want(false)).toBe(true); // same setup writes natively when editable
    expect(want(true)).toBe(false);
  });
});

describe("session guard", () => {
  it("refuses commit, undo and redo and reports no history", async () => {
    session.commit(add("a"));
    session.undo();
    expect(session.canRedo).toBe(true);
    session.readOnly = true;
    expect(session.canUndo).toBe(false);
    expect(session.canRedo).toBe(false);
    await session.flush();
    const before = persisted.length;
    session.commit(add("b"));
    expect(session.redo()).toBeNull();
    expect(session.undo()).toBeNull();
    await session.flush();
    expect(session.page.elements).toEqual([]);
    expect(persisted.length).toBe(before);
    session.readOnly = false;
    expect(session.canRedo).toBe(true);
  });

  it("applies to a session opened after view-only was set", async () => {
    ed.setReadOnly(true);
    const next = new PageSession("nb", { ...page(), id: "p2" }, async () => ({ revision: 1, updatedAt: 0, durable: true }));
    await ed.setSession(next, null);
    expect(next.readOnly).toBe(true);
    next.commit(add("x"));
    expect(next.page.elements).toEqual([]);
  });
});

describe("access banner", () => {
  it("says what the person can do", () => {
    expect(accessBanner({ kind: "shared", role: "viewer", readOnly: true, reason: "viewer" })).toBe("View only — you can look and export, not edit.");
    expect(accessBanner({ kind: "shared", readOnly: true, reason: "accessRemoved" })).toMatch(/^Access removed — /);
    expect(accessBanner({ kind: "personal", readOnly: false })).toBeNull();
    expect(accessBanner(null)).toBeNull();
  });
});

describe("view-only chrome", () => {
  const state: any = { tool: "hand", scale: 1, canUndo: true, canRedo: true, selectionCount: 0, selectionHasText: false, selectionTextFree: true, selectionTextFont: null, pageHasRows: false, editingText: false, textFormats: [], rulerShown: false, rulerAngle: 0, saveState: "saved", saveError: null, nativeActive: false, penSeen: false };

  it("toolbar keeps only pan and zoom", () => {
    const w = mount(Toolbar, { props: { state, readOnly: true }, global: { plugins: [createPinia()] } });
    const labels = w.findAll("button").map((b) => b.attributes("aria-label"));
    expect(labels).toContain("Pan");
    expect(labels).toContain("Zoom in");
    for (const l of ["Pen", "Highlighter", "Stroke eraser", "Select and move", "Text", "Undo", "Redo", "Ruler"]) expect(labels).not.toContain(l);
  });

  it("page list has no add, reorder, delete or restore", () => {
    const notebook: any = { pageOrder: ["p1", "p2"], trashedPages: [{ id: "t", formerIndex: 0, deletedAt: 0 }] };
    const w = mount(PageSidebar, { props: { notebook, current: "p1", thumbs: {}, readOnly: true } });
    const labels = w.findAll("button").map((b) => b.attributes("aria-label") ?? "");
    expect(labels.some((l) => l.startsWith("Add page") || l.startsWith("Actions for page"))).toBe(false);
    expect(w.text()).not.toContain("Deleted pages");
    expect(w.find('button[aria-label="Page 2"]').attributes("draggable")).toBe("false");
  });
});
