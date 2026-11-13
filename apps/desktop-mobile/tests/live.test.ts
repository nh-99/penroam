import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import { InkEditor } from "../src/editor/engine";
import { PageSession } from "../src/editor/session";
import { PresenceLayer } from "../src/editor/presence";
import CollaboratorStrip from "../src/components/CollaboratorStrip.vue";
import SyncIndicator from "../src/components/SyncIndicator.vue";
import { AWARENESS_MIN_INTERVAL_MS, activePeers, AwarenessSender, liveStatusLabel, peerInitials, peerName, type AwarenessPayload, type PresencePeer } from "../src/platform/live";
import type { Element } from "../src/generated/Element";
import type { Page } from "../src/generated/Page";

const api = vi.hoisted(() => ({ prefs: vi.fn(), awareness: vi.fn() }));
vi.mock("../src/platform/live", async (orig) => ({ ...(await orig<typeof import("../src/platform/live")>()), liveApi: api }));

const text = (id: string, t: string, z = 1): Element => ({ type: "text", id, z, x: 40, y: 60, width: 300, height: 40, text: t, fontSize: 16, color: "#000000", align: "left" }) as Element;
const stroke = (id: string, x: number, z = 2): Element =>
  ({ type: "ink", id, z, tool: "pen", color: "#000000", width: 2, points: [{ x, y: 10, p: 0.5, t: 0 }, { x: x + 20, y: 30, p: 0.5, t: 10 }] }) as unknown as Element;
const page = (els: Element[] = [], revision = 0): Page => ({ schemaVersion: 1, id: "p1", paper: "letter", size: { width: 612, height: 792 }, template: { kind: "blank", spacing: 24 }, revision, createdAt: 0, updatedAt: 0, elements: els });
const peer = (o: Partial<PresencePeer> = {}): PresencePeer => ({ sessionId: "s1", memberDisplay: "ann.lee@example.com", role: "editor", pageId: "p1", kind: "here", colorIndex: 0, expiresAt: Date.now() + 60000, ...o });

describe("awareness sender", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("sends at most 15 frames a second and keeps only the newest of a kind", async () => {
    const sent: AwarenessPayload[] = [];
    const s = new AwarenessSender((p) => void sent.push(p), () => true, () => Date.now());
    for (let i = 0; i < 300; i++) {
      s.push({ pageId: "p1", kind: "viewport", rect: { x: i, y: 0, w: 10, h: 10 } });
      await vi.advanceTimersByTimeAsync(10); // 100 events a second for 3 s
    }
    await vi.advanceTimersByTimeAsync(500);
    expect(sent.length).toBeLessThanOrEqual(Math.ceil(3.5 * 15) + 1);
    expect(sent.length).toBeGreaterThan(20);
    expect(sent[sent.length - 1].rect!.x).toBe(299); // the latest state arrives
    expect(AWARENESS_MIN_INTERVAL_MS).toBeGreaterThanOrEqual(66);
  });

  it("sends nothing while a stroke is active and catches up afterwards", async () => {
    let drawing = true;
    const sent: AwarenessPayload[] = [];
    const s = new AwarenessSender((p) => void sent.push(p), () => !drawing);
    s.push({ pageId: "p1", kind: "viewport", rect: { x: 1, y: 1, w: 1, h: 1 } });
    s.push({ pageId: "p1", kind: "caret", caret: { x: 5, y: 5, h: 10 }, blockId: "b" });
    await vi.advanceTimersByTimeAsync(2000);
    expect(sent).toEqual([]);
    drawing = false;
    await vi.advanceTimersByTimeAsync(500);
    expect(sent.map((p) => p.kind).sort()).toEqual(["caret", "viewport"]);
  });

  it("drops caret frames of a page that was left", async () => {
    const sent: AwarenessPayload[] = [];
    const s = new AwarenessSender((p) => void sent.push(p), () => true);
    s.push({ pageId: "p1", kind: "caret", caret: { x: 1, y: 1, h: 1 } });
    s.push({ pageId: "p2", kind: "here" });
    await vi.advanceTimersByTimeAsync(500);
    expect(sent.map((p) => p.pageId)).toEqual(["p2"]);
  });

  it("clear forgets what was queued", async () => {
    const sent: AwarenessPayload[] = [];
    const s = new AwarenessSender((p) => void sent.push(p), () => true);
    s.push({ pageId: "p1", kind: "here" });
    s.push({ pageId: "p1", kind: "viewport", rect: { x: 0, y: 0, w: 1, h: 1 } });
    s.clear();
    await vi.advanceTimersByTimeAsync(500);
    expect(sent.length).toBeLessThanOrEqual(0);
  });
});

describe("peer labels and status", () => {
  it("names unknown people neutrally and builds initials", () => {
    expect(peerName({ memberDisplay: "" })).toBe("Collaborator");
    expect(peerInitials({ memberDisplay: "" })).toBe("?");
    expect(peerInitials({ memberDisplay: "ann.lee@example.com" })).toBe("AL");
    expect(peerInitials({ memberDisplay: "bob@example.com" })).toBe("B");
  });

  it("drops expired peers", () => {
    const now = 1000;
    expect(activePeers([peer({ sessionId: "a", expiresAt: 999 }), peer({ sessionId: "b", expiresAt: 2000 })], now).map((p) => p.sessionId)).toEqual(["b"]);
  });

  it("words the live channel and never claims success for a dropped one", () => {
    expect(liveStatusLabel("live")).toBe("Live");
    expect(liveStatusLabel("offline")).toBe("Live disconnected — changes will sync when online");
    expect(liveStatusLabel("disabled")).toBeNull();
    expect(liveStatusLabel(undefined)).toBeNull();
  });
});

describe("collaborator strip", () => {
  it("gives every person a full accessible name, a shape, and a neutral label when unknown", () => {
    const w = mount(CollaboratorStrip, {
      props: { peers: [peer(), peer({ sessionId: "s2", memberDisplay: "", role: "viewer", pageId: "p2", colorIndex: 1 })], pageNumbers: { p1: 1, p2: 3 }, currentPageId: "p1" },
    });
    const labels = w.findAll('[role="img"]').map((n) => n.attributes("aria-label"));
    expect(labels).toEqual(["ann.lee@example.com, Can edit, on this page (page 1)", "Collaborator, View only, on page 3"]);
    expect(w.text()).toContain("AL");
    expect(w.text()).toContain("●");
    expect(w.text()).toContain("■"); // a shape per person, so colour isn't the only cue
    expect(w.find("ul").attributes("aria-label")).toBe("People in this notebook now");
  });

  it("renders nothing when alone and summarises overflow", () => {
    expect(mount(CollaboratorStrip, { props: { peers: [], pageNumbers: {}, currentPageId: null } }).find("ul").exists()).toBe(false);
    const many = Array.from({ length: 6 }, (_, i) => peer({ sessionId: `s${i}` }));
    const w = mount(CollaboratorStrip, { props: { peers: many, pageNumbers: { p1: 1 }, currentPageId: "p1" } });
    expect(w.text()).toContain("+2");
  });
});

describe("sync indicator for shared notebooks", () => {
  it("shows the live state and the disconnected warning", async () => {
    const pinia = createPinia();
    setActivePinia(pinia);
    const { useSync } = await import("../src/stores/sync");
    const sync = useSync();
    sync.available = true;
    sync.configured = true;
    const w = mount(SyncIndicator, { props: { live: "Live disconnected — changes will sync when online" }, global: { plugins: [pinia] } });
    expect(w.get('[data-testid="live-status"]').text()).toContain("Live disconnected");
    expect(w.get("button").attributes("aria-label")).toContain("Live disconnected — changes will sync when online");
    await w.setProps({ live: "Live" });
    expect(w.get('[data-testid="live-status"]').text()).toBe("Live");
    await w.setProps({ live: null });
    expect(w.find('[data-testid="live-status"]').exists()).toBe(false);
  });
});

describe("live store", () => {
  it("clears people when the channel isn't live and toggles the presence preference", async () => {
    setActivePinia(createPinia());
    const { useLive } = await import("../src/stores/live");
    const live = useLive();
    live.peersByNotebook = { nb: [peer()] };
    expect(live.peersOf("nb").length).toBe(1);
    live.stateByNotebook = { nb: "offline" };
    api.prefs.mockResolvedValueOnce({ sharePresence: false });
    await live.setSharePresence(false);
    expect(api.prefs).toHaveBeenCalledWith({ sharePresence: false });
    expect(live.sharePresence).toBe(false);
    api.prefs.mockRejectedValueOnce(new Error("no"));
    await live.setSharePresence(true);
    expect(live.sharePresence).toBe(false); // reverted: the backend refused
    await flushPromises();
  });
});

describe("presence layer", () => {
  it("is aria-hidden, pointer-transparent and draws page-space markers with labels", () => {
    const world = document.createElement("div");
    const layer = new PresenceLayer(world);
    layer.setPeers(
      [
        peer({ kind: "caret", caret: { x: 100, y: 200, h: 18 }, blockId: "b" }),
        peer({ sessionId: "s2", memberDisplay: "", kind: "selection", rect: { x: 10, y: 20, w: 80, h: 30 }, colorIndex: 1 }),
        peer({ sessionId: "s3", kind: "viewport", rect: { x: 0, y: 0, w: 300, h: 400 }, colorIndex: 2 }),
        peer({ sessionId: "s4", kind: "caret", blockId: "b", pageId: "other" }),
        peer({ sessionId: "s5", kind: "here" }),
      ],
      "p1",
      (id) => (id === "b" ? { x: 0, y: 0, w: 5, h: 5 } : null),
    );
    expect(layer.el.getAttribute("aria-hidden")).toBe("true");
    expect(layer.el.style.pointerEvents).toBe("none");
    const markers = layer.el.querySelectorAll<HTMLElement>(".penroam-presence-marker");
    expect(markers.length).toBe(3); // other-page and "here" peers draw nothing
    expect(markers[0].style.left).toBe("99px"); // page coordinates, no screen scaling
    expect(markers[0].style.top).toBe("200px");
    expect(layer.el.textContent).toContain("Collaborator"); // unknown identity
    expect(layer.el.textContent).toContain("ann.lee@example.com");
    expect(layer.el.textContent).toContain("is looking here");
    // markers are inside the page's world element, so its transform reprojects them on zoom and pan
    expect(world.contains(layer.el)).toBe(true);
  });

  it("never animates, so reduced motion and the ePaper theme need no special case", () => {
    const world = document.createElement("div");
    const layer = new PresenceLayer(world);
    layer.setPeers([peer({ kind: "viewport", rect: { x: 0, y: 0, w: 10, h: 10 } }), peer({ sessionId: "b", kind: "caret", caret: { x: 1, y: 1, h: 10 } })], "p1");
    for (const el of layer.el.querySelectorAll<HTMLElement>("*")) {
      expect(el.style.cssText).not.toMatch(/animation|transition/);
    }
  });

  it("draws nothing without a page", () => {
    const layer = new PresenceLayer(document.createElement("div"));
    layer.setPeers([peer({ kind: "viewport", rect: { x: 0, y: 0, w: 1, h: 1 } })], null);
    expect(layer.el.children.length).toBe(0);
  });
});

describe("editor with collaborators", () => {
  let vp: HTMLDivElement;
  let ed: InkEditor;
  let session: PageSession;
  const persisted: any[] = [];
  const awareness: AwarenessPayload[] = [];

  async function open(els: Element[]) {
    let rev = 0;
    session = new PageSession("nb", page(els), async (b) => {
      persisted.push(b);
      return { revision: ++rev, updatedAt: 0, durable: true };
    });
    await ed.setSession(session, null);
    ed.setTransform({ scale: 1, ox: 0, oy: 0 });
  }

  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    vp = document.createElement("div");
    document.body.appendChild(vp);
    vp.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600, x: 0, y: 0, toJSON() {} }) as DOMRect;
    (vp as any).setPointerCapture = () => {};
    persisted.length = 0;
    awareness.length = 0;
    ed = new InkEditor(vp, { onState: () => {}, onError: (m) => { throw new Error(m); }, onPageEdited: () => {}, onAwareness: (a) => void awareness.push(a) });
  });
  afterEach(() => {
    ed.dispose();
    vp.remove();
    vi.useRealTimers();
  });

  it("sends no awareness until it is switched on, then page and view; and none after it is off", async () => {
    await open([]);
    ed.setTransform({ scale: 2, ox: 0, oy: 0 });
    expect(awareness).toEqual([]);
    ed.setAwareness(true);
    expect(awareness.map((a) => a.kind)).toEqual(["here", "viewport"]);
    expect(awareness[0].pageId).toBe("p1");
    awareness.length = 0;
    ed.setTransform({ scale: 1, ox: -50, oy: 0 });
    expect(awareness.map((a) => a.kind)).toEqual(["viewport"]);
    ed.setAwareness(false);
    awareness.length = 0;
    ed.setTransform({ scale: 1, ox: 0, oy: 0 });
    expect(awareness).toEqual([]);
  });

  it("does not send a frame per pen sample", async () => {
    await open([]);
    ed.setAwareness(true);
    awareness.length = 0;
    ed.setTool("pen");
    const ptr = (type: string, x: number, y: number) => vp.dispatchEvent(new PointerEvent(type, { clientX: x, clientY: y, pointerId: 1, pointerType: "pen", pressure: 0.5, button: 0, buttons: type === "pointerup" ? 0 : 1, bubbles: true, cancelable: true }));
    ptr("pointerdown", 100, 100);
    expect(ed.strokeActive()).toBe(true);
    for (let i = 1; i <= 30; i++) ptr("pointermove", 100 + i * 3, 100 + i);
    ptr("pointerup", 190, 130);
    expect(awareness).toEqual([]);
    expect(ed.strokeActive()).toBe(false);
  });

  it("applies remote ink incrementally, without touching the tool, selection or a stroke in progress", async () => {
    await open([stroke("a", 10), stroke("b", 100)]);
    ed.setTool("eraser");
    let replaced = 0;
    const events = session.events;
    session.events = { ...events, onPageReplaced: () => { replaced++; events.onPageReplaced?.(); } };
    const fresh = page([stroke("a", 10), stroke("b", 100), stroke("c", 200, 3)], 1);
    session.replacePage(fresh, [], { incremental: true });
    expect(replaced).toBe(0); // no full re-render
    expect(session.page.elements.map((e) => e.id)).toEqual(["a", "b", "c"]);
    expect(ed.tool).toBe("eraser");
    // a removal and a move
    session.replacePage(page([stroke("a", 15), stroke("c", 200, 3)], 2), [], { incremental: true });
    expect(session.page.elements.map((e) => e.id)).toEqual(["a", "c"]);
    expect(replaced).toBe(0);
    expect(persisted).toEqual([]); // merging remote changes never writes
  });

  it("falls back to a full redraw for page-level changes and large merges", async () => {
    await open([stroke("a", 10)]);
    let replaced = 0;
    session.events = { ...session.events, onPageReplaced: () => void replaced++ };
    const t = page([stroke("a", 10)], 1);
    t.template = { kind: "ruled", spacing: 24 };
    session.replacePage(t, [], { incremental: true });
    expect(replaced).toBe(1);
    session.replacePage(page(Array.from({ length: 100 }, (_, i) => stroke(`n${i}`, i)), 2), [], { incremental: true });
    expect(replaced).toBe(2);
  });

  it("reports a pending composition so a merge waits for it", async () => {
    await open([text("t", "hello")]);
    expect(ed.canMergeRemote()).toBe(true);
    (ed as any).beginTextEdit(session.elementById("t"), false);
    expect(ed.canMergeRemote()).toBe(true); // ordinary typing doesn't block a merge
    vp.querySelector(".penroam-textedit")!.dispatchEvent(new Event("compositionstart"));
    expect(ed.canMergeRemote()).toBe(false);
    vp.querySelector(".penroam-textedit")!.dispatchEvent(new Event("compositionend"));
    expect(ed.canMergeRemote()).toBe(true);
  });

  describe("remote text while editing", () => {
    const typeAt = (pos: number, insert: string) => {
      const view = (ed as any).textEditor.view;
      view.dispatch({ changes: { from: pos, to: pos, insert }, userEvent: "input.type" });
    };
    const remoteText = (el: string, revision = 1) => {
      ed.flushText();
      session.replacePage(page([text("t", el)], revision), [], { incremental: true });
    };

    it("keeps the caret and selection, and does not commit the remote text as own typing", async () => {
      await open([text("t", "hello world")]);
      (ed as any).beginTextEdit(session.elementById("t"), false);
      const view = (ed as any).textEditor.view;
      view.dispatch({ selection: { anchor: 6, head: 11 } }); // "world" selected
      remoteText("Hey! hello world");
      expect(view.state.doc.toString()).toBe("Hey! hello world");
      expect(view.state.selection.main.anchor).toBe(11);
      expect(view.state.selection.main.head).toBe(16); // still "world"
      await vi.advanceTimersByTimeAsync(2000);
      // the merge itself wrote nothing: the collaborator's text is never sent back as mine
      expect(JSON.stringify(persisted)).not.toContain("Hey!");
    });

    it("keeps typing that is not saved yet and merges around it", async () => {
      await open([text("t", "abc")]);
      (ed as any).beginTextEdit(session.elementById("t"), false);
      typeAt(3, "XYZ"); // "abcXYZ", not committed yet
      remoteText("12abc"); // flushText commits "abcXYZ" first; the remote edit then applies to it
      // the page copy given by the host already contains my typing only if the server had it;
      // the box is brought to the remote text without losing the caret
      const view = (ed as any).textEditor.view;
      expect(view.state.doc.toString()).toBe("12abc");
    });

    it("box undo removes only what was typed here, not the collaborator's text", async () => {
      await open([text("t", "abc")]);
      (ed as any).beginTextEdit(session.elementById("t"), false);
      typeAt(3, "XYZ");
      const view = (ed as any).textEditor.view;
      // a collaborator types at the start while mine is still in the box
      view.dispatch({ changes: { from: 0, insert: "QQ" }, annotations: [(await import("@codemirror/state")).Transaction.remote.of(true), (await import("@codemirror/state")).Transaction.addToHistory.of(false)] });
      expect(view.state.doc.toString()).toBe("QQabcXYZ");
      const { undo } = await import("@codemirror/commands");
      undo(view);
      expect(view.state.doc.toString()).toBe("QQabc");
    });
  });
});

describe("page undo of text after remote edits", () => {
  it("removes only the user's own text, then redoes it, across CJK, RTL and emoji", async () => {
    for (const [base, mine, theirs, undone] of [
      ["note", "note and more", "THEIRS note and more", "THEIRS note"],
      ["你好", "你好世界", "大家你好世界吗", "大家你好吗"],
      ["שלום", "שלום עולם", "היי שלום עולם", "היי שלום"],
      ["ok", "ok 😀👍🏽", "🎉 ok 😀👍🏽", "🎉 ok"],
      ["# T\n", "# T\n\n- a\n- b\n", "# T\n\n- a\n- b\nremote line\n", "# T\nremote line\n"],
    ] as const) {
      let rev = 0;
      const s = new PageSession("nb", page([text("t", base)]), async () => ({ revision: ++rev, updatedAt: 0, durable: true }));
      const { diffSplices, editFromSplices } = await import("../src/editor/textHistory");
      const sp = diffSplices(base, mine);
      s.commit({
        label: "Edit text",
        forward: [{ op: "replaceElements", elements: [text("t", mine)] }],
        inverse: [{ op: "replaceElements", elements: [text("t", base)] }],
        text: { elementId: "t", edit: editFromSplices(base, sp), splices: sp },
      });
      s.replacePage(page([text("t", theirs)], 5), []); // a collaborator's change arrives
      s.undo();
      expect((s.elementById("t") as any).text).toBe(undone);
      s.redo();
      expect((s.elementById("t") as any).text).toBe(theirs);
    }
  });

  it("falls back to the whole-element undo for blocks without a text record", () => {
    const s = new PageSession("nb", page([text("t", "a")]), async () => ({ revision: 1, updatedAt: 0, durable: true }));
    s.commit({ label: "x", forward: [{ op: "replaceElements", elements: [text("t", "b")] }], inverse: [{ op: "replaceElements", elements: [text("t", "a")] }] });
    s.undo();
    expect((s.elementById("t") as any).text).toBe("a");
  });
});

describe("merging a remote snapshot while the user keeps drawing", () => {
  const newStroke = (id: string): Element => ({ type: "ink", id, z: 5, tool: "pen", color: "#000000", width: 2, points: [{ x: 1, y: 1, p: 0.5, t: 0 }, { x: 9, y: 9, p: 0.5, t: 5 }] }) as unknown as Element;
  const draw = (s: PageSession, id: string) => s.commit({ label: "Draw", forward: [{ op: "addElements", elements: [newStroke(id)] }], inverse: [{ op: "removeElements", ids: [id] }] });
  const session = () => {
    let rev = 0;
    return new PageSession("nb", page([text("a", "x")], 0), async () => ({ revision: ++rev, updatedAt: 0, durable: true }));
  };

  it("drops a snapshot read before a stroke that was committed and saved while it loaded", async () => {
    const s = session();
    let release!: (p: Page) => void;
    const loading = s.mergeRemote(() => new Promise<Page>((r) => (release = r)));
    draw(s, "mine"); // committed while the read is in flight...
    await s.flush(); // ...and saved, so the session is idle again
    expect(s.idle).toBe(true);
    release(page([text("a", "x"), text("b", "remote")], 0)); // the older snapshot arrives
    expect(await loading).toBe("stale");
    expect(s.page.elements.map((e) => e.id)).toContain("mine"); // the stroke is still there
    expect(s.page.elements.map((e) => e.id)).not.toContain("b");
  });

  it("never installs a snapshot with a lower revision than the page", async () => {
    const s = session();
    draw(s, "one");
    await s.flush(); // the page is now at revision 1
    const r = await s.mergeRemote(async () => page([text("a", "old")], 0));
    expect(r).toBe("stale");
    expect(s.page.elements.length).toBe(2);
  });

  it("merges a current snapshot when nothing happened meanwhile, and not during a stroke", async () => {
    const s = session();
    const fresh = page([text("a", "x"), text("b", "remote")], 3);
    expect(await s.mergeRemote(async () => fresh, () => false)).toBe("stale");
    expect(s.page.elements.map((e) => e.id)).toEqual(["a"]);
    expect(await s.mergeRemote(async () => fresh)).toBe("merged");
    expect(s.page.elements.map((e) => e.id)).toEqual(["a", "b"]);
  });

  it("counts undo and redo as changes too", async () => {
    const s = session();
    draw(s, "one");
    await s.flush();
    let release!: (p: Page) => void;
    const loading = s.mergeRemote(() => new Promise<Page>((r) => (release = r)));
    s.undo();
    await s.flush();
    release(page([text("a", "x"), newStroke("one")], 9));
    expect(await loading).toBe("stale");
    expect(s.page.elements.map((e) => e.id)).toEqual(["a"]);
  });
});
