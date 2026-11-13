import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import { InkEditor } from "../src/editor/engine";
import { PageSession } from "../src/editor/session";
import { CommentMarkerLayer } from "../src/editor/commentMarkers";
import CommentBody from "../src/components/CommentBody.vue";
import CommentComposer from "../src/components/CommentComposer.vue";
import CommentThread from "../src/components/CommentThread.vue";
import CommentsDrawer from "../src/components/CommentsDrawer.vue";
import ExportDialog from "../src/components/ExportDialog.vue";
import NotificationCenter from "../src/components/NotificationCenter.vue";
import {
  bodySegments,
  deliveryLabel,
  displayToTokens,
  matchesFilter,
  mentionQuery,
  mentionName,
  mentionToken,
  safeHttpUrl,
  splitThreads,
  stillMentioned,
  suggestMentions,
  tokensToDisplay,
  type CommentView,
  type Mentionable,
  type ThreadView,
} from "../src/platform/comments";
import { accessBanner, roleLabel } from "../src/platform/sharing";
import type { Element } from "../src/generated/Element";
import type { Page } from "../src/generated/Page";

const api = vi.hoisted(() => ({
  commentsApi: { list: vi.fn(), mentionable: vi.fn(), createThread: vi.fn(), reply: vi.fn(), edit: vi.fn(), remove: vi.fn(), setResolved: vi.fn(), reattach: vi.fn(), textAnchor: vi.fn() },
  notificationsApi: { list: vi.fn(), markRead: vi.fn(async () => {}), prefs: vi.fn(), setPrefs: vi.fn() },
}));
vi.mock("../src/platform/comments", async (orig) => ({ ...(await orig<typeof import("../src/platform/comments")>()), ...api }));
vi.mock("../src/platform/ipc", async (orig) => ({ ...(await orig<typeof import("../src/platform/ipc")>()), hasHost: () => true }));

const members: Mentionable[] = [
  { accountId: "a1", display: "Ann Lee" },
  { accountId: "a2", display: "Bob Stone" },
];
const comment = (o: Partial<CommentView> = {}): CommentView => ({ commentId: "c1", authorAccountId: "a1", authorDisplay: "Ann Lee", body: "hello", mentions: [], at: 1_700_000_000_000, edited: false, deleted: false, removedByOwner: false, delivery: "sent", ...o });
const thread = (o: Partial<ThreadView> = {}): ThreadView => ({
  threadId: "t1",
  anchor: { kind: "point", pageId: "p1", x: 10, y: 20 },
  placement: { kind: "placed", pageId: "p1", rect: { x: 10, y: 20, w: 0, h: 0 } },
  status: "open",
  comments: [comment()],
  unread: false,
  mine: false,
  mentionsMe: false,
  ...o,
});

describe("comment text is inert", () => {
  const payloads = [
    "<script>alert(1)</script>",
    '<img src=x onerror="alert(1)">',
    "<a href=\"javascript:alert(1)\">x</a>",
    "javascript:alert(1)",
    "[click](javascript:alert(1))",
    "https://example.com/\"><script>alert(1)</script>",
    "<svg onload=alert(1)>",
    "&lt;b&gt; {{ constructor.constructor('alert(1)')() }}",
  ];

  it("shows every payload as text, with no elements created from it", () => {
    for (const p of payloads) {
      const w = mount(CommentBody, { props: { body: p } });
      const html = w.get("[data-testid=comment-body]");
      expect(html.element.querySelector("script, img, svg, iframe, style")).toBeNull();
      expect(html.text().replace(/\s+/g, " ")).toContain(p.replace(/\s+/g, " ").split("https://")[0].trim().slice(0, 12));
      for (const a of html.element.querySelectorAll("a")) expect(a.getAttribute("href")).toMatch(/^https?:\/\//);
    }
    const w = mount(CommentBody, { props: { body: payloads[1] } });
    expect(w.text()).toContain("onerror");
  });

  it("links only http and https, with safe attributes, and never javascript: or data:", () => {
    expect(safeHttpUrl("javascript:alert(1)")).toBeNull();
    expect(safeHttpUrl("data:text/html,<b>")).toBeNull();
    expect(safeHttpUrl("ftp://x")).toBeNull();
    expect(safeHttpUrl("https://example.com/a?b=1")).toBe("https://example.com/a?b=1");
    const w = mount(CommentBody, { props: { body: "see https://example.com/page, and javascript:alert(1)" } });
    const links = w.findAll("a");
    expect(links.length).toBe(1);
    expect(links[0].attributes("href")).toBe("https://example.com/page");
    expect(links[0].attributes("rel")).toContain("noopener");
    expect(w.text()).toContain("javascript:alert(1)");
  });

  it("renders mentions as chips by looking the id up, and unknown ones as Former member", () => {
    const body = `hi ${mentionToken(members[0])}, ${mentionToken({ accountId: "au" })} and <b>${mentionToken({ accountId: "zz" })}</b>`;
    const w = mount(CommentBody, { props: { body, members, authors: [{ accountId: "au", display: "Old Author" }] } });
    expect(w.findAll("[data-testid=mention-chip]").map((c) => c.text())).toEqual(["@Ann Lee", "@Old Author", "@Former member"]);
    expect(w.element.querySelector("b")).toBeNull();
    expect(w.text()).not.toContain("a1]"); // the body holds ids, which are never shown
    expect(mentionName("a2", members)).toBe("Bob Stone");
    expect(mentionName("a2")).toBe("Former member");
    expect(mentionName("a2", [{ accountId: "a2", display: "  " }])).toBe("Former member");
  });

  it("accepts account ids of 1 to 128 characters and no more", () => {
    const id128 = "a".repeat(64) + "B_-" + "9".repeat(61);
    expect(id128.length).toBe(128);
    expect(bodySegments(`hi ${mentionToken({ accountId: id128 })}!`).filter((p) => p.t === "mention")).toEqual([{ t: "mention", id: id128 }]);
    expect(bodySegments("@[x]").filter((p) => p.t === "mention").length).toBe(1);
    // 129 characters is not an id: it stays plain text
    expect(bodySegments(`@[${"a".repeat(129)}]`).some((p) => p.t === "mention")).toBe(false);
    expect(tokensToDisplay(`@[${id128}]`).chosen[0].accountId).toBe(id128);
    const w = mount(CommentBody, { props: { body: `@[${id128}]` } });
    expect(w.get("[data-testid=mention-chip]").text()).toBe("@Former member");
  });

  it("stores only the account id in the token", () => {
    expect(mentionToken(members[1])).toBe("@[a2]");
    expect(bodySegments("x @[a2] y @[not valid] @[]").filter((p) => p.t === "mention")).toEqual([{ t: "mention", id: "a2" }]);
  });

  it("splits bodies without losing text", () => {
    const body = `a ${mentionToken(members[1])} b https://x.org/y. c`;
    expect(bodySegments(body).map((p) => (p.t === "text" ? p.s : p.t === "link" ? p.s : `@${p.id}`)).join("")).toBe("a @a2 b https://x.org/y. c");
  });
});

describe("mentions", () => {
  it("suggests only the listed members and matches names", () => {
    expect(suggestMentions(members, "a").map((m) => m.accountId)).toEqual(["a1"]);
    expect(suggestMentions(members, "e").map((m) => m.accountId)).toEqual(["a1", "a2"]);
    expect(suggestMentions(members, "").map((m) => m.accountId)).toEqual(["a1", "a2"]);
    expect(suggestMentions(members, "bo").map((m) => m.display)).toEqual(["Bob Stone"]);
    expect(suggestMentions(members, "zzz")).toEqual([]);
    expect(suggestMentions([], "a")).toEqual([]);
  });

  it("finds the @query before the caret", () => {
    expect(mentionQuery("hi @an", 6)).toEqual({ start: 3, query: "an" });
    expect(mentionQuery("mail me at a@b.c", 16)).toBeNull();
    expect(mentionQuery("@", 1)).toEqual({ start: 0, query: "" });
    expect(mentionQuery("done @an and", 12)).toBeNull();
  });

  it("round-trips between what is typed and what is sent", () => {
    const text = "@Ann Lee please look, cc @Bob Stone! also @Carl";
    const sent = displayToTokens(text, members);
    expect(sent).toBe(`${mentionToken(members[0])} please look, cc ${mentionToken(members[1])}! also @Carl`);
    expect(sent).toBe("@[a1] please look, cc @[a2]! also @Carl");
    const back = tokensToDisplay(sent, members);
    expect(back.text).toBe(text);
    // two people who are no longer listed stay apart on the way back
    const two = tokensToDisplay("@[x1] and @[x2]");
    expect(two.text).toBe("@Former member and @Former member (2)");
    expect(displayToTokens(two.text, two.chosen)).toBe("@[x1] and @[x2]");
    expect(back.chosen.map((m) => m.accountId).sort()).toEqual(["a1", "a2"]);
    expect(stillMentioned("only @Ann Lee left", members).map((m) => m.accountId)).toEqual(["a1"]);
  });

  it("the composer offers only the listed members and sends tokens for chosen ones", async () => {
    const w = mount(CommentComposer, { props: { members, label: "Reply", submit: "Send reply" }, attachTo: document.body });
    const ta = w.get("textarea");
    const type = async (v: string) => {
      (ta.element as HTMLTextAreaElement).value = v;
      (ta.element as HTMLTextAreaElement).setSelectionRange(v.length, v.length);
      await ta.trigger("input");
    };
    await type("hi @b");
    expect(w.findAll("[role=option]").map((o) => o.text())).toEqual(["Bob Stone"]);
    await type("hi @carl");
    expect(w.find("[role=listbox]").exists()).toBe(false); // not a member: nothing to pick
    await type("hi @b");
    await ta.trigger("keydown", { key: "Enter" });
    await flushPromises();
    expect((ta.element as HTMLTextAreaElement).value).toBe("hi @Bob Stone ");
    expect(w.findAll("[data-testid=composer-chip]").map((c) => c.text())).toEqual(["@Bob Stone"]);
    await w.get("form").trigger("submit");
    expect(w.emitted("submit")![0]).toEqual(["hi @[a2]"]);
    w.unmount();
  });

  it("Escape closes the suggestions only", async () => {
    const w = mount(CommentComposer, { props: { members, label: "Reply", submit: "Send" }, attachTo: document.body });
    const ta = w.get("textarea");
    (ta.element as HTMLTextAreaElement).value = "@a";
    (ta.element as HTMLTextAreaElement).setSelectionRange(2, 2);
    await ta.trigger("input");
    expect(w.find("[role=listbox]").exists()).toBe(true);
    await ta.trigger("keydown", { key: "Escape" });
    expect(w.find("[role=listbox]").exists()).toBe(false);
    expect(w.emitted("cancel")).toBeUndefined();
    w.unmount();
  });
});

describe("delivery, editing and moderation", () => {
  const mountThread = (t: ThreadView, o: Record<string, unknown> = {}) =>
    mount(CommentThread, { props: { thread: t, members, canComment: true, isModerator: false, selfId: "a1", muted: false, expanded: true, ...o } });

  it("words every delivery state and never calls a queued comment sent", () => {
    expect(deliveryLabel("waiting")).toBe("Waiting to send");
    expect(deliveryLabel("failed")).toBe("Failed — retry");
    expect(deliveryLabel("draft")).toBe("Draft (you can no longer comment)");
    expect(deliveryLabel("sent")).toBeNull();
    const w = mountThread(thread({ comments: [comment({ commentId: "s" }), comment({ commentId: "w", delivery: "waiting" }), comment({ commentId: "f", delivery: "failed", body: "again" }), comment({ commentId: "d", delivery: "draft" })] }));
    expect(w.findAll("[data-testid=delivery]").map((d) => d.text())).toEqual(["Waiting to send", "Failed — retry", "Draft (you can no longer comment)"]);
    expect(w.text()).not.toMatch(/delivered/i);
  });

  it("offers retry only on a failed comment, and edit and delete only on delivered own ones", async () => {
    const w = mountThread(thread({ comments: [comment({ commentId: "w", delivery: "waiting" }), comment({ commentId: "f", delivery: "failed", body: "again" }), comment({ commentId: "o", authorAccountId: "a2", authorDisplay: "Bob" }), comment({ commentId: "m" })] }));
    const labels = w.findAll("button").map((b) => b.text());
    expect(labels.filter((l) => l === "Retry").length).toBe(1);
    expect(labels.filter((l) => l === "Edit").length).toBe(1); // only my delivered one
    await w.findAll("button").find((b) => b.text() === "Retry")!.trigger("click");
    expect(w.emitted("retry")![0]).toEqual(["again"]);
  });

  it("marks an edited comment and shows a removed one as a tombstone", () => {
    const w = mountThread(thread({ comments: [comment({ edited: true }), comment({ commentId: "x", deleted: true, removedByOwner: true, body: "" }), comment({ commentId: "y", deleted: true, body: "" })] }));
    expect(w.get("[data-testid=edited]").text()).toBe("(edited)");
    expect(w.findAll("[data-testid=tombstone]").map((t) => t.text())).toEqual(["Removed by the owner", "Deleted"]);
  });

  it("lets the owner remove others' comments but only author edit their own", () => {
    const t = thread({ comments: [comment({ commentId: "b", authorAccountId: "a2", authorDisplay: "Bob" })] });
    const mod = mountThread(t, { isModerator: true });
    expect(mod.findAll("button").some((b) => b.text() === "Remove")).toBe(true);
    expect(mod.findAll("button").some((b) => b.text() === "Edit")).toBe(false);
    const other = mountThread(t);
    expect(other.findAll("button").some((b) => b.text() === "Remove")).toBe(false);
  });

  it("resolves and reopens, and a reader cannot reply", async () => {
    const w = mountThread(thread());
    await w.findAll("button").find((b) => b.text() === "Resolve")!.trigger("click");
    expect(w.emitted("resolve")![0]).toEqual([true]);
    const r = mountThread(thread({ status: "resolved", resolvedByDisplay: "Ann" }), { canComment: false });
    expect(r.text()).toContain("Resolved by Ann");
    expect(r.findAll("button").some((b) => b.text() === "Reply")).toBe(false);
    expect(r.text()).toContain("read this discussion but not write");
  });

  it("shows an unknown author neutrally and unread as text", () => {
    const w = mountThread(thread({ unread: true, comments: [comment({ authorDisplay: "" })] }));
    expect(w.text()).toContain("Collaborator");
    expect(w.text()).toContain("New activity");
  });
});

describe("filters and orphans", () => {
  const threads = [
    thread({ threadId: "open1" }),
    thread({ threadId: "res1", status: "resolved" }),
    thread({ threadId: "mine1", mine: true }),
    thread({ threadId: "men1", mentionsMe: true, unread: true }),
    thread({ threadId: "orph1", placement: { kind: "orphaned", reason: "textChanged", lastPageId: "p1", quote: "the old words" } }),
  ];

  it("filters Open, Resolved, Mine and Mentions", () => {
    expect(threads.filter((t) => matchesFilter(t, "open")).map((t) => t.threadId)).toEqual(["open1", "mine1", "men1", "orph1"]);
    expect(threads.filter((t) => matchesFilter(t, "resolved")).map((t) => t.threadId)).toEqual(["res1"]);
    expect(threads.filter((t) => matchesFilter(t, "mine")).map((t) => t.threadId)).toEqual(["mine1"]);
    expect(threads.filter((t) => matchesFilter(t, "mentions")).map((t) => t.threadId)).toEqual(["men1"]);
    const s = splitThreads(threads, "open");
    expect(s.orphaned.map((t) => t.threadId)).toEqual(["orph1"]);
    expect(s.placed.length).toBe(3);
  });

  async function drawer(canComment = true) {
    const pinia = createPinia();
    setActivePinia(pinia);
    const { useComments } = await import("../src/stores/comments");
    const store = useComments();
    store.notebookId = "nb";
    store.view = { canComment, isModerator: false, threads };
    store.members = members;
    const w = mount(CommentsDrawer, { props: { can: { selection: false, element: true, text: false, point: true, rect: true, pdf: false }, pending: null, placing: null, selfId: "a1" }, global: { plugins: [pinia] }, attachTo: document.body });
    await flushPromises();
    return { w, store };
  }

  it("shows the filters, switches between them, and marks unread", async () => {
    const { w, store } = await drawer();
    expect(w.findAll("[role=radio]").map((r) => r.text())).toEqual(["Open", "Resolved", "Mine", "Mentions"]);
    expect(w.findAll("[role=radio]")[0].attributes("aria-checked")).toBe("true");
    expect(w.findAll("[data-thread]").length).toBe(4);
    await w.findAll("[role=radio]")[3].trigger("click");
    expect(store.filter).toBe("mentions");
    expect(w.findAll("[data-thread]").map((t) => t.attributes("data-thread"))).toEqual(["men1"]);
    expect(w.get("[data-thread=men1]").text()).toContain("New activity");
    await w.findAll("[role=radio]")[1].trigger("click");
    expect(w.findAll("[data-thread]").map((t) => t.attributes("data-thread"))).toEqual(["res1"]);
    w.unmount();
  });

  it("lists orphaned threads in their own section, with the quote and locate / re-attach", async () => {
    const { w, store } = await drawer();
    store.focusThread = "orph1";
    await flushPromises();
    const sec = w.get("[data-testid=orphaned-section]");
    expect(sec.text()).toContain("Not on the page");
    expect(sec.text()).toContain("The text this was attached to changed.");
    expect(sec.text()).toContain("“the old words”");
    const labels = sec.findAll("button").map((b) => b.text());
    expect(labels).toContain("Locate");
    expect(labels).toContain("Re-attach…");
    await sec.findAll("button").find((b) => b.text() === "Locate")!.trigger("click");
    expect(w.emitted("locate")![0]).toEqual(["orph1"]);
    w.unmount();
  });

  it("explains when you can read but not write, and hides the ways to start a comment", async () => {
    const { w } = await drawer(false);
    expect(w.get("[data-testid=no-comment-note]").text()).toContain("read comments but not write");
    expect(w.text()).not.toContain("Add a comment on");
    w.unmount();
  });

  it("offers large buttons for each way of attaching, disabled when not possible", async () => {
    const { w } = await drawer();
    const group = w.get('[role=group][aria-label="Add a comment on"]');
    const byLabel = Object.fromEntries(group.findAll("button").map((b) => [b.text(), b.attributes("disabled") !== undefined]));
    expect(byLabel).toEqual({ "Selected object": true, "Pick an object": false, "Selected text": true, "A point": false, "An area": false, "PDF area": true });
    await group.findAll("button").find((b) => b.text() === "A point")!.trigger("click");
    expect(w.emitted("start")![0]).toEqual(["point"]);
    w.unmount();
  });
});

describe("role labels", () => {
  it("names the commenter role and its banner", () => {
    expect(roleLabel("commenter")).toBe("Can comment");
    expect(roleLabel("viewer")).toBe("View only");
    expect(accessBanner({ kind: "shared", role: "commenter", readOnly: true, reason: "viewer" })).toBe("Comment only — you can read, comment and export, not edit.");
    expect(accessBanner({ kind: "shared", role: "viewer", readOnly: true, reason: "viewer" })).toBe("View only — you can look and export, not edit.");
    expect(accessBanner({ kind: "shared", role: "commenter", readOnly: true, reason: "accessRemoved" })).toMatch(/^Access removed/);
  });
});

describe("export dialog", () => {
  it("leaves comments out unless asked, with a warning", async () => {
    const w = mount(ExportDialog, { props: { title: "Plans" }, attachTo: document.body });
    const box = document.body.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    expect(box.checked).toBe(false);
    expect(document.body.textContent).toContain("Include comments");
    expect(document.body.querySelector("[data-testid=comments-warning]")).toBeNull();
    const export1 = Array.from(document.body.querySelectorAll("button")).find((b) => b.textContent?.includes("Export…"))!;
    export1.click();
    expect(w.emitted("confirm")![0]).toEqual([{ includeComments: false, includeAuthors: false }]);
    box.click();
    await flushPromises();
    const warn = document.body.querySelector("[data-testid=comments-warning]")!;
    expect(warn.textContent).toContain("comments and their account identifiers");
    expect(document.body.textContent).toContain("comment text and their account identifiers, readable by anyone you give it to");
    export1.click();
    expect(w.emitted("confirm")![1]).toEqual([{ includeComments: true, includeAuthors: false }]);
    w.unmount();
  });
});

describe("notification center", () => {
  it("lists notifications, opens a notebook this device has, and explains one it doesn't", async () => {
    const pinia = createPinia();
    setActivePinia(pinia);
    const { useComments } = await import("../src/stores/comments");
    const { useLibrary } = await import("../src/stores/library");
    const { useUi } = await import("../src/stores/ui");
    const store = useComments();
    const lib = useLibrary();
    lib.notebooks = [{ id: "nb1", title: "Plans", pageCount: 1, cover: "#111111", updatedAt: 0, createdAt: 0 } as any];
    lib.open = vi.fn(async () => {}) as any;
    store.notifications = [
      { notificationId: "n1", notebookId: "nb1", threadId: "t1", commentId: "c1", kind: "mention", actorDisplay: "Ann", at: 2, read: false },
      { notificationId: "n2", notebookId: "other", threadId: "t9", commentId: "c9", kind: "reply", actorDisplay: "", at: 1, read: true },
    ];
    store.unreadNotifications = 1;
    api.notificationsApi.setPrefs.mockImplementation(async (p) => p);
    const w = mount(NotificationCenter, { global: { plugins: [pinia] }, attachTo: document.body });
    const text = document.body.textContent!;
    expect(text).toContain("Ann mentioned you");
    expect(text).toContain("Someone replied in a thread you're in");
    expect(text).toContain("Plans");
    expect(text).not.toContain("t9"); // ids and unknown notebooks reveal nothing
    expect(text).toContain("A notebook that isn't on this device");
    document.body.querySelector<HTMLElement>('[data-notification="n1"] button')!.click();
    await flushPromises();
    expect(api.notificationsApi.markRead).toHaveBeenCalledWith("n1");
    expect(lib.open).toHaveBeenCalledWith("nb1");
    expect(store.focusThread).toBe("t1");
    expect(store.drawerOpen).toBe(true);
    document.body.querySelector<HTMLElement>('[data-notification="n2"] button')!.click();
    await flushPromises();
    expect(lib.open).toHaveBeenCalledTimes(1);
    expect(useUi().toasts.some((t) => /isn't available on this device/.test(t.text))).toBe(true);
    // muting
    const mute = Array.from(document.body.querySelectorAll<HTMLElement>('[data-notification="n1"] button')).find((b) => b.textContent?.includes("Mute notebook"))!;
    mute.click();
    await flushPromises();
    expect(api.notificationsApi.setPrefs).toHaveBeenCalledWith({ mutedNotebooks: ["nb1"], mutedThreads: [] });
    w.unmount();
  });
});

describe("anchor markers", () => {
  it("are labelled buttons of touch size that don't let a press reach the editor, and never animate", () => {
    const world = document.createElement("div");
    const layer = new CommentMarkerLayer(world);
    const pressed = vi.fn();
    world.addEventListener("pointerdown", pressed);
    const opened = vi.fn();
    layer.setThreads(
      [thread({ unread: true }), thread({ threadId: "t2", status: "resolved", anchor: { kind: "rect", pageId: "p1", rect: { x: 50, y: 50, w: 100, h: 40 } }, placement: { kind: "placed", pageId: "p1", rect: { x: 50, y: 50, w: 100, h: 40 } } }), thread({ threadId: "t3", comments: [comment({ delivery: "waiting" })], placement: { kind: "placed", pageId: "other", rect: { x: 0, y: 0, w: 1, h: 1 } } }), thread({ threadId: "t4", placement: { kind: "orphaned", reason: "missing" } })],
      "p1",
      "t2",
      opened,
    );
    const markers = layer.el.querySelectorAll<HTMLButtonElement>("button.penroam-comment-marker");
    expect(markers.length).toBe(2); // other pages and orphaned threads draw nothing
    expect(markers[0].getAttribute("aria-label")).toContain("1 comment, open, new activity");
    expect(markers[1].getAttribute("aria-label")).toContain("resolved");
    expect(markers[1].getAttribute("aria-current")).toBe("true");
    expect(markers[0].style.width).toBe("44px");
    expect(markers[1].textContent).toBe("✓"); // a state mark besides the colour
    expect(layer.el.querySelector(".penroam-comment-outline")).not.toBeNull(); // the open thread's anchor
    markers[0].dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(pressed).not.toHaveBeenCalled();
    markers[0].click();
    expect(opened).toHaveBeenCalledWith("t1");
    for (const el of layer.el.querySelectorAll<HTMLElement>("*")) expect(el.style.cssText).not.toMatch(/animation|transition/);
    expect(layer.el.style.pointerEvents).toBe("none");
  });
});

describe("choosing what to comment on", () => {
  let vp: HTMLDivElement;
  let ed: InkEditor;
  let session: PageSession;
  const anchors: unknown[] = [];
  const errors: string[] = [];
  const persisted: unknown[] = [];
  const stroke = (id: string, x: number): Element => ({ type: "ink", id, z: 2, tool: "pen", color: "#000000", width: 2, points: [{ x, y: 100, p: 0.5, t: 0 }, { x: x + 40, y: 100, p: 0.5, t: 10 }, { x: x + 80, y: 100, p: 0.5, t: 20 }] }) as unknown as Element;
  const page = (els: Element[], background?: Page["background"]): Page => ({ schemaVersion: 1, id: "p1", paper: "letter", size: { width: 600, height: 800 }, template: { kind: "blank", spacing: 24 }, revision: 0, createdAt: 0, updatedAt: 0, elements: els, background });
  const ptr = (type: string, x: number, y: number, target: EventTarget = vp) => {
    const e = new PointerEvent(type, { clientX: x, clientY: y, pointerId: 1, pointerType: "touch", pressure: 0.5, button: 0, buttons: type === "pointerup" ? 0 : 1, bubbles: true, cancelable: true });
    target.dispatchEvent(e);
  };

  async function open(els: Element[], background?: Page["background"]) {
    session = new PageSession("nb", page(els, background), async (b) => (persisted.push(b), { revision: 1, updatedAt: 0, durable: true }));
    await ed.setSession(session, null);
    ed.setTransform({ scale: 1, ox: 0, oy: 0 });
  }

  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    vp = document.createElement("div");
    document.body.appendChild(vp);
    vp.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600, x: 0, y: 0, toJSON() {} }) as DOMRect;
    (vp as any).setPointerCapture = () => {};
    anchors.length = 0;
    errors.length = 0;
    persisted.length = 0;
    ed = new InkEditor(vp, { onState: () => {}, onError: (m) => void errors.push(m), onPageEdited: () => {}, onCommentAnchor: (a) => void anchors.push(a) });
  });
  afterEach(() => {
    ed.dispose();
    vp.remove();
    vi.useRealTimers();
  });

  it("a tap places a point without changing the tool or drawing anything", async () => {
    await open([]);
    ed.setTool("pen");
    ed.beginCommentPlacement("point");
    ptr("pointerdown", 120, 240);
    ptr("pointerup", 120, 240, window);
    expect(anchors).toEqual([{ kind: "point", pageId: "p1", x: 120, y: 240 }]);
    expect(ed.tool).toBe("pen");
    expect(session.page.elements).toEqual([]);
    expect(persisted).toEqual([]);
    // the next press draws again: the placement ended by itself
    expect(vp.dataset.placing).toBeUndefined();
  });

  it("a drag places an area, a small tap a box around it", async () => {
    await open([]);
    ed.beginCommentPlacement("rect");
    ptr("pointerdown", 100, 100);
    ptr("pointermove", 260, 180, window);
    ptr("pointerup", 260, 180, window);
    expect(anchors[0]).toEqual({ kind: "rect", pageId: "p1", rect: { x: 100, y: 100, w: 160, h: 80 } });
    ed.beginCommentPlacement("rect");
    ptr("pointerdown", 300, 300);
    ptr("pointerup", 301, 301, window);
    expect((anchors[1] as any).rect).toMatchObject({ w: 40, h: 40 });
  });

  it("picks the object under a tap, and says so when there is none", async () => {
    await open([stroke("s1", 100)]);
    ed.beginCommentPlacement("element");
    ptr("pointerdown", 400, 400);
    ptr("pointerup", 400, 400, window);
    expect(anchors).toEqual([]);
    expect(errors[0]).toContain("Tap a drawing");
    ptr("pointerdown", 140, 100);
    ptr("pointerup", 140, 100, window);
    expect(anchors[0]).toMatchObject({ kind: "element", pageId: "p1", elementId: "s1" });
  });

  it("a PDF area is normalized to the page", async () => {
    await open([], { assetHash: "h", pageIndex: 3, mediaBox: [0, 0, 600, 800], cropBox: [0, 0, 600, 800], rotation: 90 });
    ed.beginCommentPlacement("pdf");
    ptr("pointerdown", 60, 80);
    ptr("pointermove", 300, 400, window);
    ptr("pointerup", 300, 400, window);
    expect(anchors[0]).toEqual({ kind: "pdf", pageId: "p1", pdfPageIndex: 3, normalizedRect: { x: 0.1, y: 0.1, w: 0.4, h: 0.4 }, rotation: 90 });
  });

  it("can be cancelled, and a selection becomes an element anchor", async () => {
    await open([stroke("s1", 100)]);
    ed.setTool("hand"); // so the tap below can't draw
    ed.beginCommentPlacement("point");
    ed.cancelCommentPlacement();
    ptr("pointerdown", 10, 10);
    ptr("pointerup", 10, 10, window);
    expect(anchors).toEqual([]);
    expect(ed.selectionAnchor()).toBeNull();
    ed.setTool("select");
    ed.selectAll();
    expect(ed.selectionAnchor()).toMatchObject({ kind: "element", elementId: "s1" });
  });
});
