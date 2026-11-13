import { beforeEach, describe, expect, it, vi } from "vitest";
import { mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import AuthorBadge from "../src/components/AuthorBadge.vue";
import CommentBody from "../src/components/CommentBody.vue";
import CollaboratorStrip from "../src/components/CollaboratorStrip.vue";
import InvitationCard from "../src/components/InvitationCard.vue";
import { PresenceLayer } from "../src/editor/presence";
import { cleanName, cleanNames, cleanText, linkHost, revealedHost } from "../src/platform/display";
import { authorLabel } from "../src/platform/history";
import { peerInitials, peerName } from "../src/platform/live";
import { authorName, bodySegments, mentionName } from "../src/platform/comments";
import { memberName } from "../src/platform/sharing";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", async (orig) => ({ ...(await orig<typeof import("@tauri-apps/api/core")>()), invoke }));
vi.mock("../src/platform/ipc", async (orig) => ({ ...(await orig<typeof import("../src/platform/ipc")>()), hasHost: () => true }));

const BIDI = ["‪", "‫", "‬", "‭", "‮", "⁦", "⁧", "⁨", "⁩", "‎", "‏", "؜"];
const hasBidi = (s: string) => BIDI.some((c) => s.includes(c));
const evil = `ann${BIDI.join("")}@example.com`;

beforeEach(() => setActivePinia(createPinia()));

describe("bidi controls in names", () => {
  it("removes every one of them", () => {
    expect(cleanName(evil)).toBe("ann@example.com");
    for (const c of BIDI) expect(cleanName(`a${c}b`)).toBe("ab");
    expect(cleanName(undefined)).toBe("");
    expect(cleanName("שלום עולם")).toBe("שלום עולם"); // real right-to-left text stays
  });

  it("keeps running text's direction marks but drops forcing controls", () => {
    expect(cleanText("a‮b⁦c‏d")).toBe("abc‏d");
  });

  it("cleans name fields anywhere in a result, and nothing else", () => {
    const r = cleanNames({ members: [{ display: evil, role: "x‮y" }], invitations: [{ email: evil }], invitedBy: evil, nested: { actorDisplay: evil, other: evil } });
    expect(r.members[0].display).toBe("ann@example.com");
    expect(r.members[0].role).toBe("x‮y"); // not a name field
    expect(r.invitations[0].email).toBe("ann@example.com");
    expect(r.invitedBy).toBe("ann@example.com");
    expect(r.nested.actorDisplay).toBe("ann@example.com");
  });

  it("every way of naming a person comes out clean", () => {
    expect(hasBidi(peerName({ memberDisplay: evil }))).toBe(false);
    expect(hasBidi(peerInitials({ memberDisplay: evil }))).toBe(false);
    expect(hasBidi(authorLabel({ kind: "member", display: evil }))).toBe(false);
    expect(hasBidi(authorName({ authorDisplay: evil }))).toBe(false);
    expect(hasBidi(mentionName("a", [{ accountId: "a", display: evil }]))).toBe(false);
    expect(hasBidi(memberName({ display: evil, state: "active" }))).toBe(false);
  });

  it("command results are cleaned on the way in (sharing, comments, history, live)", async () => {
    const { sharingApi } = await import("../src/platform/sharing");
    const { commentsApi, notificationsApi } = await import("../src/platform/comments");
    const { historyApi } = await import("../src/platform/history");
    invoke.mockResolvedValueOnce({ you: "owner", members: [{ display: evil }], invitations: [{ email: evil }] });
    const m = await sharingApi.members("s");
    expect(m.members[0].display).toBe("ann@example.com");
    expect(m.invitations[0].email).toBe("ann@example.com");
    invoke.mockResolvedValueOnce({ canComment: true, isModerator: false, threads: [{ resolvedByDisplay: evil, comments: [{ authorDisplay: evil }] }] });
    const c = await commentsApi.list("nb");
    expect(c.threads[0].resolvedByDisplay).toBe("ann@example.com");
    expect(c.threads[0].comments[0].authorDisplay).toBe("ann@example.com");
    invoke.mockResolvedValueOnce({ items: [{ actorDisplay: evil }], unread: 1 });
    expect((await notificationsApi.list()).items[0].actorDisplay).toBe("ann@example.com");
    invoke.mockResolvedValueOnce({ entries: [{ author: { kind: "member", display: evil } }] });
    expect((await historyApi.activity("nb")).entries[0].author.display).toBe("ann@example.com");
  });
});

describe("names are isolated where they are shown", () => {
  it("wraps names in bdi and shows no controls", () => {
    const badge = mount(AuthorBadge, { props: { author: { kind: "member", display: evil, accountId: "a1" } } });
    expect(badge.find("bdi").exists()).toBe(true);
    expect(hasBidi(badge.text())).toBe(false);
    expect(badge.find("bdi").text()).toBe("ann@example.com");
    const strip = mount(CollaboratorStrip, { props: { peers: [{ sessionId: "s", memberDisplay: evil, role: "editor", pageId: "p", kind: "here", colorIndex: 0, expiresAt: Date.now() + 1000 }], pageNumbers: { p: 1 }, currentPageId: "p" } });
    expect(hasBidi(strip.get("[role=img]").attributes("aria-label")!)).toBe(false);
    const card = mount(InvitationCard, { props: { invitation: { invitationId: "i", spaceId: "s", role: "editor", invitedBy: evil, expiresAtMs: 0 } } });
    expect(card.find("bdi").text()).toBe("ann@example.com");
    expect(hasBidi(card.text())).toBe(false);
  });

  it("presence labels are isolated from the page around them", () => {
    const layer = new PresenceLayer(document.createElement("div"));
    layer.setPeers([{ sessionId: "s", memberDisplay: evil, role: "editor", pageId: "p", kind: "caret", caret: { x: 1, y: 1, h: 10 }, colorIndex: 0, expiresAt: 1e15 }], "p");
    const label = layer.el.querySelector("span")!;
    expect(label.style.cssText).toContain("unicode-bidi: isolate");
    expect(hasBidi(label.textContent!)).toBe(false);
  });

  it("comment text loses forcing controls and mentions are isolated", () => {
    const w = mount(CommentBody, { props: { body: "pay ‮evil‬ now @[a1]", members: [{ accountId: "a1", display: evil }] } });
    expect(hasBidi(w.text())).toBe(false);
    expect(w.get("p").attributes("dir")).toBe("auto");
    expect(w.get("[data-testid=mention-chip] bdi").text()).toBe("ann@example.com");
  });
});

describe("link hosts", () => {
  it("gives the host as the browser parses it (punycode)", () => {
    expect(linkHost("https://аpple.com/login")).toBe("xn--pple-43d.com"); // Cyrillic а
    expect(linkHost("https://Example.com:8080/x")).toBe("example.com:8080");
    expect(linkHost("not a url")).toBeNull();
  });

  it("reveals a look-alike host, and says nothing when the text already shows it", () => {
    expect(revealedHost("https://аpple.com/login", "https://аpple.com/login")).toBe("xn--pple-43d.com");
    expect(revealedHost("https://example.com/a", "https://example.com/a")).toBeNull();
    expect(revealedHost("https://EXAMPLE.com:8080/a", "https://example.com:8080/a")).toBeNull();
    // a user@ in front makes the text read like a different site
    expect(revealedHost("https://apple.com@evil.example/", "https://apple.com@evil.example/")).toBe("evil.example");
    expect(revealedHost("https://münchen.de", "https://münchen.de/")).toBe("xn--mnchen-3ya.de");
  });

  it("shows the host next to a look-alike link, and not next to an ordinary one", () => {
    const parts = bodySegments("see https://аpple.com/login and https://example.com/ok");
    const links = parts.filter((p) => p.t === "link") as { s: string; host?: string }[];
    expect(links[0].host).toBe("xn--pple-43d.com");
    expect(links[1].host).toBeUndefined();
    const w = mount(CommentBody, { props: { body: "see https://аpple.com/login and https://example.com/ok" } });
    expect(w.findAll("[data-testid=link-host]").map((n) => n.text())).toEqual(["(xn--pple-43d.com)"]);
    expect(w.findAll("a").length).toBe(2);
    expect(w.findAll("a")[0].attributes("href")).toContain("xn--pple-43d.com"); // what is opened is the parsed address
  });
});
