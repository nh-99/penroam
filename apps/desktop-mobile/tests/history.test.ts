import { beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import ActivityDialog from "../src/components/ActivityDialog.vue";
import AuthorBadge from "../src/components/AuthorBadge.vue";
import ConflictsDialog from "../src/components/ConflictsDialog.vue";
import ExportDialog from "../src/components/ExportDialog.vue";
import HistoryDialog from "../src/components/HistoryDialog.vue";
import RestoreDialog from "../src/components/RestoreDialog.vue";
import {
  activityPeople,
  activityText,
  authorLabel,
  authorsLine,
  changedSincePreview,
  filterActivity,
  groupSessions,
  mergeAuthors,
  personKey,
  type ActivityEntry,
  type AuthorView,
  type RestorePlan,
} from "../src/platform/history";
import { syncStateLabel } from "../src/platform/sharing";
import { describeStatus } from "../src/platform/sync";
import type { Page } from "../src/generated/Page";

const h = vi.hoisted(() => ({
  historyApi: { activity: vi.fn(), compare: vi.fn(), restorePreview: vi.fn(), restoreCommit: vi.fn(), notebookRestorePreview: vi.fn(), notebookRestoreCommit: vi.fn(), keepBoth: vi.fn(async () => {}) },
  ipc: { pageHistory: vi.fn(), pageVersion: vi.fn(), loadPage: vi.fn(), historyStats: vi.fn(async () => ({ bytes: 0 })), restorePageVersion: vi.fn(), restoreNotebookVersion: vi.fn(), clearHistory: vi.fn() },
  conflicts: vi.fn(),
}));
vi.mock("../src/platform/history", async (orig) => ({ ...(await orig<typeof import("../src/platform/history")>()), historyApi: h.historyApi }));
vi.mock("../src/platform/ipc", async (orig) => {
  const o = await orig<typeof import("../src/platform/ipc")>();
  return { ...o, hasHost: () => true, api: { ...o.api, ...h.ipc } };
});
vi.mock("../src/platform/sync", async (orig) => {
  const o = await orig<typeof import("../src/platform/sync")>();
  return { ...o, syncApi: { ...o.syncApi, conflicts: h.conflicts, resolveConflict: vi.fn(async () => {}) } };
});

const member = (display: string, accountId = display): AuthorView => ({ kind: "member", display, accountId });
const before: AuthorView = { kind: "beforeSharing", display: "" };
const unknown: AuthorView = { kind: "unknown", display: "" };
const former: AuthorView = { kind: "former", display: "", accountId: "gone" };

const entry = (o: Partial<ActivityEntry> = {}): ActivityEntry => ({ entryId: "e1", author: member("ann@example.com"), at: 1000, acceptedAt: 1000, editedOffline: false, pageId: "p1", kind: "strokesAdded", count: 3, ...o });
const page = (els: any[] = []): Page => ({ schemaVersion: 1, id: "p1", paper: "letter", size: { width: 600, height: 800 }, template: { kind: "blank", spacing: 24 }, revision: 0, createdAt: 0, updatedAt: 0, elements: els });
const textEl = (id: string, text: string) => ({ type: "text", id, z: 1, x: 60, y: 80, width: 300, height: 40, text, fontSize: 16, color: "#000000", align: "left" });

beforeEach(() => {
  setActivePinia(createPinia());
  document.body.innerHTML = "";
  for (const f of [...Object.values(h.historyApi), ...Object.values(h.ipc), h.conflicts]) f.mockReset();
  h.ipc.historyStats.mockResolvedValue({ bytes: 0 });
  (URL as any).createObjectURL = () => "blob:test";
  (URL as any).revokeObjectURL = () => {};
});

describe("authors are named honestly", () => {
  it("never invents a name", () => {
    expect(authorLabel(member("ann@example.com"))).toBe("ann@example.com");
    expect(authorLabel({ kind: "member", display: " " })).toBe("Collaborator");
    expect(authorLabel(former)).toBe("Former member");
    expect(authorLabel({ kind: "erased", display: "" })).toBe("Former collaborator");
    expect(authorLabel(before)).toBe("Before sharing");
    expect(authorLabel(unknown)).toBe("Unknown");
    expect(authorLabel({ kind: "unverified", display: "x@y.z" })).toBe("Unverified author");
  });

  it("badges carry text and a shape, not just colour", () => {
    const w = mount(AuthorBadge, { props: { author: member("ann.lee@example.com", "a1") } });
    expect(w.text()).toContain("AL");
    expect(w.text()).toContain("ann.lee@example.com");
    expect(w.text()).toMatch(/[●■▲◆★✚⬟⬢]/u);
    expect(mount(AuthorBadge, { props: { author: unknown } }).text()).toContain("Unknown");
  });

  it("merges authors without repeats and groups them into sessions", () => {
    expect(mergeAuthors([member("a")], [member("a"), before]).length).toBe(2);
    expect(authorsLine([member("ann@x.y"), former])).toBe("ann@x.y, Former member");
    expect(authorsLine(undefined)).toBe("");
    const row = (position: number, at: number, authors?: AuthorView[]) => ({ pageId: "p1", revision: null, position, at, added: 1, removed: 0, authors });
    const s = groupSessions([row(0, 0, [before]), row(1, 1000, [member("ann")]), row(2, 2000, [member("ann"), member("bob")])]);
    expect(s.length).toBe(2);
    expect(s[1].authors!.map(authorLabel)).toEqual(["ann", "bob"]);
    expect(groupSessions([{ pageId: "p1", revision: 1, position: null, at: 0, added: 0, removed: 0 }])[0]).not.toHaveProperty("authors");
  });
});

describe("activity", () => {
  it("words entries from kind and count only", () => {
    expect(activityText({ kind: "strokesAdded", count: 1 })).toBe("Added 1 stroke");
    expect(activityText({ kind: "strokesErased", count: 4 })).toBe("Erased 4 strokes");
    expect(activityText({ kind: "pageMoved", count: 2 })).toBe("Moved 2 pages");
    expect(activityText({ kind: "imagesAdded", count: 1 })).toBe("Added 1 image");
    expect(activityText({ kind: "elementsChanged", count: 5 })).toBe("Changed 5 elements");
    expect(activityText({ kind: "notebook", count: 1 })).toBe("Changed the notebook");
  });

  it("filters by page, person and kind, newest first", () => {
    const ann = member("ann", "a1");
    const es = [entry({ entryId: "1", acceptedAt: 10, author: ann }), entry({ entryId: "2", acceptedAt: 30, pageId: "p2", author: before }), entry({ entryId: "3", acceptedAt: 20, kind: "textEdited", author: ann })];
    expect(filterActivity(es, { pageId: "", person: "", kind: "" }).map((e) => e.entryId)).toEqual(["2", "3", "1"]);
    expect(filterActivity(es, { pageId: "p1", person: "", kind: "" }).map((e) => e.entryId)).toEqual(["3", "1"]);
    expect(filterActivity(es, { pageId: "", person: personKey(ann), kind: "" }).map((e) => e.entryId)).toEqual(["3", "1"]);
    expect(filterActivity(es, { pageId: "", person: "", kind: "textEdited" }).map((e) => e.entryId)).toEqual(["3"]);
    expect(activityPeople(es).length).toBe(2);
  });

  async function dialog(entries: ActivityEntry[], next?: string) {
    h.historyApi.activity.mockResolvedValueOnce({ entries, nextCursor: next });
    const w = mount(ActivityDialog, { props: { notebookId: "nb", pageNumbers: { p1: 1, p2: 2 } }, attachTo: document.body });
    await flushPromises();
    return w;
  }

  it("shows before-sharing, unknown, former and edited-offline labels as they are", async () => {
    const w = await dialog([
      entry({ entryId: "a", author: before, acceptedAt: 5 }),
      entry({ entryId: "b", author: unknown, acceptedAt: 4 }),
      entry({ entryId: "c", author: former, acceptedAt: 3 }),
      entry({ entryId: "d", author: member("ann@example.com"), acceptedAt: 9_000_000, at: 100, editedOffline: true }),
    ]);
    const text = document.body.textContent!;
    for (const t of ["Before sharing", "Unknown", "Former member", "ann@example.com", "Edited offline", "Added 3 strokes"]) expect(text).toContain(t);
    expect(document.body.querySelectorAll("[data-testid=offline]").length).toBe(1);
    w.unmount();
  });

  it("filters, moves with the arrow keys, goes to the page and pages in older entries", async () => {
    const w = await dialog([entry({ entryId: "1", acceptedAt: 3 }), entry({ entryId: "2", acceptedAt: 2, pageId: "p2", kind: "textEdited" })], "next");
    const items = () => Array.from(document.body.querySelectorAll<HTMLElement>("[data-entry]"));
    expect(items().length).toBe(2);
    items()[0].focus();
    items()[0].dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await flushPromises();
    expect(document.activeElement).toBe(items()[1]);
    items()[1].dispatchEvent(new KeyboardEvent("keydown", { key: "Home", bubbles: true }));
    await flushPromises();
    expect(document.activeElement).toBe(items()[0]);
    const selects = document.body.querySelectorAll<HTMLSelectElement>("select");
    selects[2].value = "textEdited";
    selects[2].dispatchEvent(new Event("change"));
    await flushPromises();
    expect(items().length).toBe(1);
    items()[0].click();
    expect(w.emitted("goto")![0]).toEqual(["p2"]);
    h.historyApi.activity.mockResolvedValueOnce({ entries: [entry({ entryId: "3", acceptedAt: 1 })] });
    Array.from(document.body.querySelectorAll("button")).find((b) => b.textContent?.includes("Show older"))!.click();
    await flushPromises();
    expect(h.historyApi.activity).toHaveBeenLastCalledWith("nb", "next");
    w.unmount();
  });

  it("says when nothing matches and when loading fails", async () => {
    let w = await dialog([]);
    expect(document.body.querySelector("[data-testid=activity-empty]")).not.toBeNull();
    w.unmount();
    h.historyApi.activity.mockRejectedValueOnce(new Error("rate_limited: slow"));
    w = mount(ActivityDialog, { props: { notebookId: "nb", pageNumbers: {} }, attachTo: document.body });
    await flushPromises();
    expect(document.body.textContent).toContain("Too many tries");
    w.unmount();
  });
});

describe("restore", () => {
  const plan = (items: RestorePlan["items"], keptAdditions = 0): RestorePlan => ({ planId: "plan1", items, keptAdditions });
  const item = (o: Partial<RestorePlan["items"][number]>) => ({ itemId: "x", kind: "revert" as const, authors: [], selected: false, editable: true, ...o });
  const mine = { itemId: "i1", kind: "readd" as const, elementId: "e1", authors: [], selected: true, editable: true };
  const theirs = { itemId: "i2", kind: "revert" as const, elementId: "e2", authors: [member("bob@example.com")], selected: false, editable: true };

  async function open(props: Record<string, unknown> = {}) {
    const w = mount(RestoreDialog, { props: { notebookId: "nb", pageId: "p1", position: 7, when: "Nov 1", ...props }, attachTo: document.body });
    await flushPromises();
    return w;
  }
  const boxes = () => Array.from(document.body.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'));
  const restoreBtn = () => Array.from(document.body.querySelectorAll("button")).find((b) => b.textContent?.startsWith("Restore"))!;

  it("ticks what the plan selects, names others' authors, warns, and keeps additions", async () => {
    h.historyApi.restorePreview.mockResolvedValue(plan([mine, theirs], 3)); // a page preview sends the count, not the items
    const w = await open();
    expect(h.historyApi.restorePreview).toHaveBeenCalledWith("nb", "p1", { position: 7, revision: undefined }, undefined);
    expect(boxes().map((b) => b.checked)).toEqual([true, false]);
    expect(document.body.textContent).toContain("bob@example.com");
    expect(document.body.querySelector("[data-testid=collaborator-warning]")!.textContent).toContain("Collaborators changed one thing");
    expect(document.body.querySelector("[data-testid=kept]")!.textContent).toBe("3 additions since then stay.");
    expect(restoreBtn().textContent).toBe("Restore 1 item");
    boxes()[1].click();
    await flushPromises();
    expect(restoreBtn().textContent).toBe("Restore 2 items");
    restoreBtn().click();
    await flushPromises();
    expect(h.historyApi.restoreCommit).toHaveBeenCalledWith("nb", "plan1", ["i1", "i2"]);
    expect(w.emitted("restored")).toBeTruthy();
    w.unmount();
  });

  it("makes a new plan and explains when the page changed since the preview", async () => {
    h.historyApi.restorePreview.mockResolvedValueOnce(plan([mine])).mockResolvedValueOnce({ planId: "plan2", items: [mine, { ...theirs, selected: false }], keptAdditions: 0 });
    h.historyApi.restoreCommit.mockRejectedValueOnce(new Error("changed_since_preview: head moved"));
    const w = await open();
    restoreBtn().click();
    await flushPromises();
    expect(h.historyApi.restorePreview).toHaveBeenCalledTimes(2);
    expect(document.body.querySelector("[data-testid=changed-since]")!.textContent).toContain("changed while you were looking");
    expect(boxes().length).toBe(2);
    expect(w.emitted("restored")).toBeUndefined();
    restoreBtn().click();
    await flushPromises();
    expect(h.historyApi.restoreCommit).toHaveBeenLastCalledWith("nb", "plan2", ["i1"]);
    expect(changedSincePreview(new Error("changed_since_preview: x"))).toBe(true);
    expect(changedSincePreview(new Error("other"))).toBe(false);
    w.unmount();
  });

  it("never ticks a block's revert and recovered copy together", async () => {
    const revert = item({ itemId: "revert:b", elementId: "b", authors: [member("bob@example.com")], selected: true });
    const copy = item({ itemId: "copy:b", kind: "recoveredCopy", elementId: "b", authors: [member("bob@example.com")], selected: true });
    h.historyApi.restorePreview.mockResolvedValue(plan([revert, copy]));
    const w = await open();
    expect(boxes().map((b) => b.checked)).toEqual([true, false]); // both preselected: only one stays
    expect(document.body.querySelectorAll("[data-testid=exclusive]").length).toBe(2);
    boxes()[1].click();
    await flushPromises();
    expect(boxes().map((b) => b.checked)).toEqual([false, true]);
    restoreBtn().click();
    await flushPromises();
    expect(h.historyApi.restoreCommit).toHaveBeenCalledWith("nb", "plan1", ["copy:b"]);
    w.unmount();
  });

  it("shows a notebook plan: deleted pages, the title, newer pages kept and what can't come back", async () => {
    h.historyApi.notebookRestorePreview.mockResolvedValue(
      plan([
        item({ itemId: "page:1", kind: "readd", pageId: "1", selected: true }),
        item({ itemId: "title", kind: "title", selected: true }),
        item({ itemId: "page:2", kind: "keptAddition", pageId: "2", editable: false }),
        item({ itemId: "unavailable", kind: "unavailable", editable: false }),
      ], 1),
    );
    const w = await open({ pageId: undefined, position: undefined, at: 5, shared: true });
    const text = document.body.textContent!;
    expect(text).toContain("Bring back a deleted page");
    expect(text).toContain("Put the notebook's title back");
    expect(document.body.querySelector("[data-testid=kept]")!.textContent).toBe("1 page added since then stays.");
    expect(document.body.querySelector("[data-testid=unavailable]")!.textContent).toContain("can't be brought back");
    expect(boxes().length).toBe(2);
    restoreBtn().click();
    await flushPromises();
    expect(h.historyApi.notebookRestoreCommit).toHaveBeenCalledWith("nb", "plan1", ["page:1", "title"]);
    w.unmount();
  });

  it("a personal page plan offers removing what was added since, ticked, with a count and a warning", async () => {
    h.historyApi.restorePreview.mockResolvedValue(plan([
      item({ itemId: "revert:a", elementId: "a", selected: true }),
      item({ itemId: "removeAdditions", kind: "removeAdditions", count: 3, selected: true }),
      item({ itemId: "template", kind: "template", selected: true }),
    ]));
    const w = await open();
    const text = document.body.textContent!;
    expect(text).toContain("Remove 3 things added since then");
    expect(text).toContain("Put the paper back the way it was");
    expect(boxes().map((b) => b.checked)).toEqual([true, true, true]);
    expect(document.body.querySelector("[data-testid=personal-warning]")!.textContent).toContain("later changes go to the trash or are removed, and you can undo it by restoring again".replace("later", "Later"));
    expect(document.body.textContent).toContain("Put this page back the way it was");
    w.unmount();
  });

  it("a personal notebook plan names its items by page number and sends only what is ticked", async () => {
    h.historyApi.notebookRestorePreview.mockResolvedValue(plan([
      item({ itemId: "pageContent:p1", kind: "pageContent", pageId: "p1", count: 4, selected: true }),
      item({ itemId: "removeAdditions:p1", kind: "removeAdditions", pageId: "p1", count: 1, selected: true }),
      item({ itemId: "trashNewer:p2", kind: "trashNewer", pageId: "p2", selected: true }),
      item({ itemId: "pageOrder", kind: "pageOrder", selected: false }),
      item({ itemId: "title", kind: "title", selected: true }),
    ]));
    const w = await open({ pageId: undefined, position: undefined, at: 9, pageOrder: ["p1", "p2"] });
    const text = document.body.textContent!;
    expect(text).toContain("Bring back page 1's content (4 changes)");
    expect(text).toContain("Remove 1 thing added to page 1 since then");
    expect(text).toContain("Move page 2, added since then, to the trash");
    expect(text).toContain("Put the pages back in their old order");
    expect(text).toContain("Put the notebook back the way it was");
    expect(boxes().map((b) => b.checked)).toEqual([true, true, true, false, true]);
    expect(document.body.querySelector("[data-testid=personal-warning]")).not.toBeNull();
    boxes()[3].click();
    await flushPromises();
    restoreBtn().click();
    await flushPromises();
    expect(h.historyApi.notebookRestoreCommit).toHaveBeenCalledWith("nb", "plan1", ["pageContent:p1", "removeAdditions:p1", "trashNewer:p2", "pageOrder", "title"]);
    w.unmount();
  });

  it("a shared plan says newer work is kept and shows no personal warning", async () => {
    h.historyApi.restorePreview.mockResolvedValue(plan([item({ itemId: "revert:a", elementId: "a", selected: true })], 2));
    const w = await open({ shared: true });
    expect(document.body.querySelector("[data-testid=personal-warning]")).toBeNull();
    expect(document.body.textContent).toContain("Anything added since stays");
    w.unmount();
  });

  it("restores only chosen elements, and says when there is nothing to restore", async () => {
    h.historyApi.restorePreview.mockResolvedValue(plan([], 1));
    const w = await open({ elementIds: ["e1"] });
    expect(h.historyApi.restorePreview).toHaveBeenCalledWith("nb", "p1", { position: 7, revision: undefined }, ["e1"]);
    expect(document.body.querySelector("[data-testid=nothing-to-restore]")).not.toBeNull();
    expect(restoreBtn().hasAttribute("disabled")).toBe(true);
    w.unmount();
  });

  it("a notebook restore keeps newer pages and uses the notebook commands", async () => {
    h.historyApi.notebookRestorePreview.mockResolvedValue(plan([mine]));
    const w = await open({ pageId: undefined, position: undefined, at: 12345, shared: true });
    expect(h.historyApi.notebookRestorePreview).toHaveBeenCalledWith("nb", 12345);
    expect(document.body.textContent).toContain("Pages added since then are kept");
    restoreBtn().click();
    await flushPromises();
    expect(h.historyApi.notebookRestoreCommit).toHaveBeenCalledWith("nb", "plan1", ["i1"]);
    w.unmount();
  });
});

describe("version history", () => {
  const rows = [
    { pageId: "p1", revision: null, position: 3, at: 1000, added: 2, removed: 0, authors: [before] },
    { pageId: "p1", revision: null, position: 9, at: 9_000_000, added: 1, removed: 1, authors: [member("ann@example.com")] },
  ];
  async function open(shared: boolean) {
    setActivePinia(createPinia());
    h.ipc.pageHistory.mockResolvedValue(rows);
    h.ipc.pageVersion.mockResolvedValue(page([textEl("t1", "old words")]));
    h.ipc.loadPage.mockResolvedValue(page([textEl("t1", "new words"), textEl("t2", "added")]));
    const w = mount(HistoryDialog, { props: { notebook: { id: "nb", pageOrder: ["p1"] } as any, pageId: "p1", render: async () => new Uint8Array([1]), shared }, global: { plugins: [createPinia()] }, attachTo: document.body });
    await flushPromises();
    return w;
  }
  const click = async (label: string) => {
    Array.from(document.body.querySelectorAll("button")).find((b) => b.textContent?.includes(label))!.click();
    await flushPromises();
  };

  it("shows who made each version", async () => {
    const w = await open(true);
    const authors = Array.from(document.body.querySelectorAll("[data-testid=version-authors]")).map((a) => a.textContent);
    expect(authors).toEqual(["By ann@example.com", "By Before sharing"]);
    w.unmount();
  });

  it("previews as a picture with no editor, and the dialog hides native ink while open", async () => {
    const { useUi } = await import("../src/stores/ui");
    const w = await open(true);
    expect(useUi().overlays).toBeGreaterThan(0); // overlays hide the native ink layers
    document.body.querySelectorAll("[aria-label=Versions] button")[0].dispatchEvent(new Event("click"));
    (document.body.querySelector("[aria-label=Versions] button") as HTMLElement).click();
    await flushPromises();
    const preview = document.body.querySelector("[data-testid=version-preview]")!;
    expect(preview.querySelector("img")).not.toBeNull();
    expect(preview.querySelector("canvas, textarea, [contenteditable]")).toBeNull();
    w.unmount();
  });

  it("compares two versions: counts, highlights with marks, and the text diff", async () => {
    const w = await open(true);
    (document.body.querySelector("[aria-label=Versions] button") as HTMLElement).click();
    await flushPromises();
    h.historyApi.compare.mockResolvedValue({ added: ["t2"], removed: [], changed: ["t1"], text: [{ elementId: "t1", diff: [{ op: "equal", text: "the " }, { op: "delete", text: "old" }, { op: "insert", text: "new" }, { op: "equal", text: " words" }] }], templateChanged: false });
    const sel = document.body.querySelector<HTMLSelectElement>("[data-testid=version-preview] ~ div select")!;
    sel.value = "head";
    sel.dispatchEvent(new Event("change"));
    await flushPromises();
    expect(h.historyApi.compare).toHaveBeenCalledWith("nb", "p1", { position: 9 }, "head");
    expect(document.body.querySelector("[data-testid=count-added]")!.textContent).toContain("1 added");
    expect(document.body.querySelector("[data-testid=count-changed]")!.textContent).toContain("1 changed");
    const marks = Array.from(document.body.querySelectorAll("[data-testid=version-preview] [role=img]")).map((b) => b.textContent);
    expect(marks.sort()).toEqual(["+", "~"]);
    const t = document.body.querySelector("[data-testid=text-diff]")!;
    expect(t.querySelector("ins")!.textContent).toBe("new");
    expect(t.querySelector("del")!.textContent).toBe("old");
    w.unmount();
  });

  it("restores a shared notebook through the preview, never the direct command", async () => {
    h.historyApi.restorePreview.mockResolvedValue({ planId: "p", items: [], keptAdditions: 0 });
    const w = await open(true);
    (document.body.querySelector("[aria-label=Versions] button") as HTMLElement).click();
    await flushPromises();
    await click("Restore this page…");
    expect(h.historyApi.restorePreview).toHaveBeenCalledWith("nb", "p1", { position: 9, revision: undefined }, undefined);
    expect(h.ipc.restorePageVersion).not.toHaveBeenCalled();
    w.unmount();
  });

  it("restores element by element from the preview", async () => {
    h.historyApi.restorePreview.mockResolvedValue({ planId: "p", items: [], keptAdditions: 0 });
    const w = await open(true);
    (document.body.querySelector("[aria-label=Versions] button") as HTMLElement).click();
    await flushPromises();
    await click("Pick elements…");
    const box = document.body.querySelector<HTMLElement>('[data-testid=version-preview] button[aria-label^="Text"]')!;
    box.click();
    await flushPromises();
    expect(box.getAttribute("aria-pressed")).toBe("true");
    await click("Restore 1 chosen…");
    expect(h.historyApi.restorePreview).toHaveBeenCalledWith("nb", "p1", { position: 9, revision: undefined }, ["t1"]);
    w.unmount();
  });

  it("restores a personal notebook's local version through the preview too, by revision", async () => {
    h.historyApi.restorePreview.mockResolvedValue({ planId: "p", items: [], keptAdditions: 0 });
    setActivePinia(createPinia());
    h.ipc.pageHistory.mockResolvedValue([{ pageId: "p1", revision: 4, position: null, at: 1000, added: 1, removed: 0 }, { pageId: "p1", revision: 5, position: null, at: 9_000_000, added: 1, removed: 0 }]);
    h.ipc.pageVersion.mockResolvedValue(page([]));
    const w = mount(HistoryDialog, { props: { notebook: { id: "nb", pageOrder: ["p1"] } as any, pageId: "p1", render: async () => new Uint8Array([1]) }, global: { plugins: [createPinia()] }, attachTo: document.body });
    await flushPromises();
    (document.body.querySelector("[aria-label=Versions] button") as HTMLElement).click();
    await flushPromises();
    await click("Restore this page…");
    expect(h.historyApi.restorePreview).toHaveBeenCalledWith("nb", "p1", { position: undefined, revision: 5 }, undefined);
    expect(h.ipc.restorePageVersion).not.toHaveBeenCalled();
    w.unmount();
  });
});

describe("conflicts", () => {
  const conflict = { target: { kind: "element", pageId: "p1", elementId: "e" }, winner: "w", winnerValue: { text: "mine" }, winnerAuthor: member("ann@example.com"), alternates: [{ opId: "o1", value: { text: "theirs" }, author: undefined }] };

  it("shows who wrote each side and offers Keep both as copies", async () => {
    h.conflicts.mockResolvedValue([conflict]);
    setActivePinia(createPinia());
    const w = mount(ConflictsDialog, { props: { notebookId: "nb", currentTitle: "T" }, global: { plugins: [createPinia()] }, attachTo: document.body });
    await flushPromises();
    expect(document.body.querySelector("[data-testid=winner-author]")!.textContent).toContain("ann@example.com");
    expect(document.body.querySelector("[data-testid=alternate-author]")!.textContent).toContain("Unknown"); // not made up
    document.body.querySelector<HTMLElement>("[data-testid=keep-both]")!.click();
    await flushPromises();
    expect(h.historyApi.keepBoth).toHaveBeenCalledWith("nb", conflict.target, "o1");
    w.unmount();
  });
});

describe("export and status", () => {
  it("leaves authors out unless asked for, with a warning", async () => {
    const w = mount(ExportDialog, { props: { title: "Plans" }, attachTo: document.body });
    const boxes = document.body.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect(Array.from(boxes).map((b) => b.checked)).toEqual([false, false]);
    expect(document.body.textContent).toContain("Include authors");
    expect(document.body.querySelector("[data-testid=authors-warning]")).toBeNull();
    boxes[1].click();
    await flushPromises();
    expect(document.body.querySelector("[data-testid=authors-warning]")).not.toBeNull();
    Array.from(document.body.querySelectorAll("button")).find((b) => b.textContent?.includes("Export…"))!.click();
    expect(w.emitted("confirm")![0]).toEqual([{ includeComments: false, includeAuthors: true }]);
    w.unmount();
  });

  it("labels the catch-up state", () => {
    expect(syncStateLabel("needsCatchUp")).toBe("Needs catch-up");
    expect(describeStatus({ phase: "needsCatchUp", queued: 2, conflicts: 0 } as any).text).toContain("Needs catch-up");
  });
});
