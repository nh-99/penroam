import { beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import type { SharedNotebookView, MembersView } from "../src/platform/sharing";

const api = vi.hoisted(() => ({
  list: vi.fn(),
  share: vi.fn(),
  members: vi.fn(),
  invite: vi.fn(),
  cancelInvitation: vi.fn(async () => {}),
  changeRole: vi.fn(),
  removeMember: vi.fn(async () => {}),
  leave: vi.fn(async () => {}),
  accept: vi.fn(),
  decline: vi.fn(async () => {}),
  keepPrivateCopy: vi.fn(),
  removeLocal: vi.fn(async () => {}),
  access: vi.fn(),
}));
vi.mock("../src/platform/sharing", async (orig) => ({
  ...(await orig<typeof import("../src/platform/sharing")>()),
  sharingApi: api,
  onSharedChanged: async () => () => {},
}));
vi.mock("../src/platform/ipc", async (orig) => ({
  ...(await orig<typeof import("../src/platform/ipc")>()),
  hasHost: () => true,
}));

const buttons = () => Array.from(document.body.querySelectorAll("button"));
const button = (label: string) => buttons().find((b) => b.textContent?.includes(label) || b.getAttribute("aria-label")?.includes(label));

/** A button of the topmost dialog, by its exact text. */
const confirmButton = (text: string) => {
  const dlgs = document.body.querySelectorAll('[role="dialog"]');
  return Array.from(dlgs[dlgs.length - 1].querySelectorAll("button")).find((b) => b.textContent?.trim() === text)!;
};

const owned: SharedNotebookView = { spaceId: "s1", notebookId: "nb1", title: "Plans", role: "owner", state: "active", owner: true, syncState: "synced" };
const members = (you: "owner" | "editor" | "viewer" = "owner"): MembersView => ({
  you,
  members: [
    { accountId: "a1", role: "owner", state: "active", display: "owner@example.com", isYou: you === "owner", version: 1, sinceMs: 0 },
    { accountId: "a2", role: "editor", state: "active", display: "ann@example.com", isYou: you === "editor", version: 3, sinceMs: 0 },
  ],
  invitations: you === "owner" ? [{ invitationId: "i1", email: "bob@example.com", role: "viewer", state: "pending", expiresAtMs: 0, version: 1 }] : [],
});

async function setup(opts: { shared?: SharedNotebookView; inVault?: boolean; configured?: boolean; available?: boolean; you?: "owner" | "editor" | "viewer" } = {}) {
  const pinia = createPinia();
  setActivePinia(pinia);
  const { useSync } = await import("../src/stores/sync");
  const { useSharing } = await import("../src/stores/sharing");
  const sync = useSync();
  sync.configured = opts.configured ?? true;
  if (opts.inVault) sync.vaults = { supported: true, vaults: [], notebooks: { nb1: "v1" } };
  const sharing = useSharing();
  sharing.available = opts.available ?? true;
  sharing.notebooks = opts.shared ? [opts.shared] : [];
  api.members.mockResolvedValue(members(opts.you ?? "owner"));
  api.list.mockResolvedValue({ notebooks: sharing.notebooks, invitations: [], available: sharing.available });
  const ShareDialog = (await import("../src/components/ShareDialog.vue")).default;
  const w = mount(ShareDialog, { props: { notebookId: "nb1", title: "Plans" }, global: { plugins: [pinia] }, attachTo: document.body });
  await flushPromises();
  return { w, sharing };
}

beforeEach(() => {
  document.body.innerHTML = "";
  for (const f of Object.values(api)) f.mockClear();
});

describe("share dialog", () => {
  it("explains privacy before the first invite and starts sharing only on request", async () => {
    api.share.mockResolvedValue(owned);
    const { w } = await setup();
    const text = document.body.textContent!;
    expect(text).toContain("Penroam Cloud can read ordinary and shared notebooks");
    expect(text).toContain("People you remove keep any copy they already downloaded");
    expect(text).toContain("earlier versions");
    expect(document.body.querySelector('input[type="email"]')).toBeNull();
    expect(api.share).not.toHaveBeenCalled();
    button("Start sharing")!.click();
    await flushPromises();
    expect(api.share).toHaveBeenCalledWith("nb1");
    w.unmount();
  });

  it("is disabled with an explanation for vault notebooks", async () => {
    const { w } = await setup({ inVault: true });
    expect(document.body.textContent).toContain("Notebooks in a vault can't be shared");
    expect(button("Start sharing")).toBeUndefined();
    w.unmount();
  });

  it("is disabled when sync isn't set up or the server lacks sharing", async () => {
    let t = await setup({ configured: false });
    expect(document.body.textContent).toContain("Sign in to Penroam Cloud");
    t.w.unmount();
    t = await setup({ available: false });
    expect(document.body.textContent).toContain("doesn't support shared notebooks");
    t.w.unmount();
  });

  it("lets the owner invite, cancel an invitation, change a role and remove a person", async () => {
    api.invite.mockResolvedValue({});
    api.changeRole.mockResolvedValue({});
    const { w } = await setup({ shared: owned });
    expect(document.body.textContent).toContain("ann@example.com");
    expect(document.body.textContent).toContain("bob@example.com");

    const email = document.body.querySelector<HTMLInputElement>('input[type="email"]')!;
    email.value = "cy@example.com";
    email.dispatchEvent(new Event("input"));
    await flushPromises();
    button("Send invitation")!.click();
    await flushPromises();
    expect(api.invite).toHaveBeenCalledWith("s1", "cy@example.com", "editor");

    button("Cancel invitation to bob@example.com")!.click();
    await flushPromises();
    expect(api.cancelInvitation).toHaveBeenCalledWith("s1", "i1");

    const role = document.body.querySelector<HTMLSelectElement>("#role-a2")!;
    role.value = "viewer";
    role.dispatchEvent(new Event("change"));
    await flushPromises();
    expect(api.changeRole).toHaveBeenCalledWith("s1", "a2", "viewer", 3);

    button("Remove ann@example.com")!.click();
    await flushPromises();
    expect(api.removeMember).not.toHaveBeenCalled(); // asks first
    confirmButton("Remove").click();
    await flushPromises();
    expect(api.removeMember).toHaveBeenCalledWith("s1", "a2");
    w.unmount();
  });

  it("shows non-owners a read-only list with Leave", async () => {
    const { w } = await setup({ shared: { ...owned, owner: false, role: "editor" }, you: "editor" });
    expect(document.body.querySelector('input[type="email"]')).toBeNull();
    expect(document.body.querySelector("select")).toBeNull();
    expect(button("Remove ann")).toBeUndefined();
    button("Leave this notebook")!.click();
    await flushPromises();
    confirmButton("Leave").click();
    await flushPromises();
    expect(api.leave).toHaveBeenCalledWith("s1");
    w.unmount();
  });

  it("is a labelled dialog that closes on Escape", async () => {
    const { w } = await setup({ shared: owned });
    const dlg = document.body.querySelector('[role="dialog"]')!;
    expect(dlg.getAttribute("aria-modal")).toBe("true");
    expect(document.getElementById(dlg.getAttribute("aria-labelledby")!)!.textContent).toContain("Share");
    expect(document.body.querySelector('label[for="role-a2"]')).not.toBeNull();
    dlg.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(w.emitted("close")).toBeTruthy();
    w.unmount();
  });
});

describe("shared with me", () => {
  async function library() {
    const pinia = createPinia();
    setActivePinia(pinia);
    const { useSharing } = await import("../src/stores/sharing");
    const { useLibrary } = await import("../src/stores/library");
    const sharing = useSharing();
    const lib = useLibrary();
    lib.loaded = true;
    lib.refresh = vi.fn(async () => {}) as any;
    lib.notebooks = [
      { id: "mine", title: "My notes", pageCount: 1, cover: "#123456", updatedAt: 0, createdAt: 0 } as any,
      { id: "theirs", title: "Team plan", pageCount: 4, cover: "#654321", updatedAt: 0, createdAt: 0 } as any,
      { id: "nb1", title: "Plans", pageCount: 2, cover: "#111111", updatedAt: 0, createdAt: 0 } as any,
    ];
    sharing.notebooks = [
      { spaceId: "s2", notebookId: "theirs", title: "Team plan", role: "viewer", state: "active", owner: false, ownerDisplay: "dee@example.com", syncState: "waitingToUpload" },
      { spaceId: "s3", title: "", role: "editor", state: "joining", owner: false, syncState: "localSaved" },
      { spaceId: "s4", notebookId: "gone", title: "Old plan", role: "editor", state: "accessRemoved", owner: false, syncState: "accessRemoved" },
      owned,
    ];
    sharing.invitations = [{ invitationId: "inv1", spaceId: "s9", role: "editor", invitedBy: "eve@example.com", expiresAtMs: 0 }];
    api.list.mockImplementation(async () => ({ notebooks: sharing.notebooks, invitations: sharing.invitations, available: true }));
    const LibraryView = (await import("../src/views/LibraryView.vue")).default;
    const w = mount(LibraryView, { props: { nativeKind: null }, global: { plugins: [pinia] }, attachTo: document.body });
    await flushPromises();
    return { w, sharing, lib };
  }

  it("asks to sign in again when invitations can't be matched, and when declining needs it", async () => {
    const { w, sharing } = await library();
    expect(w.find('[data-testid="invites-signin"]').exists()).toBe(false);
    sharing.invitationsNeedSignIn = true;
    await flushPromises();
    const note = w.find('[data-testid="invites-signin"]');
    expect(note.text()).toContain("Sign in again to see invitations sent to your email address");
    expect(note.find("button").text()).toBe("Sign in again");

    api.decline.mockRejectedValueOnce(new Error("reauth_required: stale"));
    await w.find('[aria-labelledby="invites-h"]').find('button[aria-label^="Decline"]').trigger("click");
    await flushPromises();
    expect(sharing.problems.inv1).toBe("reauth");
    w.unmount();
  });

  it("lists notebooks others own with role chips and state labels, apart from the shelf", async () => {
    const { w } = await library();
    const section = w.find('[aria-labelledby="shared-h"]');
    expect(section.exists()).toBe(true);
    const text = section.text();
    expect(text).toContain("Team plan");
    expect(text).toContain("View only");
    expect(text).toContain("Waiting to upload");
    expect(text).toContain("Joining…");
    expect(text).toContain("Access removed");
    // not repeated on the shelf; the owner's notebook there carries the shared badge
    const shelfTitles = w.findAll("h3").map((h) => h.text());
    expect(shelfTitles.filter((t) => t.includes("Team plan")).length).toBe(1);
    expect(w.find('[data-testid="shared-badge"]').text()).toContain("Synced");
    expect(w.find('button[disabled][aria-label^="Joining"]').exists()).toBe(true);
    w.unmount();
  });

  it("shows invitations without a title and accepts or declines them", async () => {
    api.accept.mockResolvedValue({});
    const { w } = await library();
    const inv = w.find('[aria-labelledby="invites-h"]');
    expect(inv.text()).toContain("eve@example.com invited you");
    expect(inv.text()).toContain("Can edit");
    await inv.find('button[aria-label^="Accept"]').trigger("click");
    await flushPromises();
    expect(api.accept).toHaveBeenCalledWith("inv1");
    await inv.find('button[aria-label^="Decline"]').trigger("click");
    await flushPromises();
    expect(api.decline).toHaveBeenCalledWith("inv1");
    w.unmount();
  });

  it("offers a private copy or removing this device's copy after access is removed", async () => {
    api.keepPrivateCopy.mockResolvedValue({ id: "new" });
    const { w } = await library();
    const keep = w.findAll("button").find((b) => b.text() === "Keep a private copy")!;
    await keep.trigger("click");
    await flushPromises();
    expect(api.keepPrivateCopy).toHaveBeenCalledWith("s4");

    await w.findAll("button").find((b) => b.text() === "Remove from this device")!.trigger("click");
    await flushPromises();
    expect(document.body.textContent).toContain("Nothing is deleted for the owner or anyone else");
    expect(api.removeLocal).not.toHaveBeenCalled();
    confirmButton("Remove from this device").click();
    await flushPromises();
    expect(api.removeLocal).toHaveBeenCalledWith("s4");
    w.unmount();
  });
});

describe("sync state labels", () => {
  it("names every state", async () => {
    const { syncStateLabel, sharedStatusLabel } = await import("../src/platform/sharing");
    expect(["localSaved", "waitingToUpload", "synced", "accessRemoved", "needsResolution"].map((s) => syncStateLabel(s as any))).toEqual([
      "Local saved",
      "Waiting to upload",
      "Synced",
      "Access removed",
      "Needs resolution",
    ]);
    expect(sharedStatusLabel({ ...owned, state: "joining" })).toBe("Joining…");
    expect(sharedStatusLabel({ ...owned, syncState: "needsResolution" })).toBe("Needs resolution");
  });
});

describe("error messages", () => {
  it("maps wire-code prefixes and shows other messages as they are", async () => {
    const { describeSharingError } = await import("../src/platform/sharing");
    const { CommandError } = await import("../src/platform/ipc");
    const err = (m: string) => new CommandError("invalid" as any, m);
    expect(describeSharingError(err("access_revoked: gone"))).toBe("Your access to this notebook was removed.");
    expect(describeSharingError(err("invitation_expired: old"))).toMatch(/expired/);
    expect(describeSharingError(err("invalid_role: x"))).toMatch(/role/);
    expect(describeSharingError(err("rate_limited: slow"))).toMatch(/Too many/);
    expect(describeSharingError(err("not_found: x"))).toMatch(/isn't available/);
    expect(describeSharingError(err("conflict: x"))).toMatch(/changed elsewhere/);
    expect(describeSharingError(err("This notebook is view only."))).toBe("This notebook is view only.");
  });

  it("describes the access-removed sync phase", async () => {
    const { describeStatus, statusIcon } = await import("../src/platform/sync");
    const s: any = { phase: "accessRemoved", queued: 0, conflicts: 0 };
    expect(describeStatus(s).text).toMatch(/access/i);
    expect(statusIcon(s)).toBe("alert");
  });
});

describe("accepting problems and former members", () => {
  it("classifies accept failures and names former members", async () => {
    const { acceptProblem, memberName, describeSharingError } = await import("../src/platform/sharing");
    expect(acceptProblem(new Error("reauth_required: stale"))).toBe("reauth");
    expect(acceptProblem(new Error("this device still has your earlier copy of this notebook. Handle it"))).toBe("oldCopy");
    expect(acceptProblem(new Error("not_found: x"))).toBeNull();
    expect(describeSharingError(new Error("reauth_required: x"))).toMatch(/Sign in again/);
    expect(memberName({ display: "", state: "revoked" })).toBe("Former member");
    expect(memberName({ display: "a@b.c", state: "active" })).toBe("a@b.c");
  });

  it("shows the inline actions on the invitation card and signs in again", async () => {
    const pinia = createPinia();
    setActivePinia(pinia);
    const { useSharing } = await import("../src/stores/sharing");
    const sharing = useSharing();
    const inv = { invitationId: "inv1", spaceId: "s4", role: "editor", invitedBy: "eve@example.com", expiresAtMs: 0 } as any;
    api.accept.mockRejectedValueOnce(new Error("this device still has your earlier copy of this notebook. Keep it"));
    await sharing.accept(inv);
    expect(sharing.problems.inv1).toBe("oldCopy");
    api.accept.mockRejectedValueOnce(new Error("reauth_required: stale"));
    await sharing.accept(inv);
    expect(sharing.problems.inv1).toBe("reauth");

    const InvitationCard = (await import("../src/components/InvitationCard.vue")).default;
    const w = mount(InvitationCard, { props: { invitation: inv, problem: "reauth" } });
    await w.findAll("button").find((b) => b.text() === "Sign in again")!.trigger("click");
    expect(w.emitted("signIn")).toBeTruthy();
    await w.setProps({ problem: "oldCopy" });
    const labels = w.findAll("button").map((b) => b.text());
    expect(labels).toContain("Keep a private copy");
    expect(labels).toContain("Remove from this device");
  });
});

describe("when the server offers no sharing", () => {
  it("shows no notification bell and no people, comment or settings controls", async () => {
    const pinia = createPinia();
    setActivePinia(pinia);
    const { useSharing } = await import("../src/stores/sharing");
    const { useComments } = await import("../src/stores/comments");
    const sharing = useSharing();
    sharing.available = false;
    sharing.notebooks = [];
    const Bell = (await import("../src/components/NotificationBell.vue")).default;
    const w = mount(Bell, { global: { plugins: [pinia] } });
    expect(w.find("button").exists()).toBe(false);
    sharing.available = true;
    await flushPromises();
    expect(w.find("button").exists()).toBe(true);
    sharing.available = false;
    useComments().notifications = [{ notificationId: "n", notebookId: "nb", threadId: "t", commentId: "c", kind: "mention", actorDisplay: "x", at: 1, read: false }];
    await flushPromises();
    expect(w.find("button").exists()).toBe(true); // old notifications stay reachable
  });

  it("explains in the share dialog instead of offering to share", async () => {
    const { w } = await setup({ available: false });
    expect(document.body.textContent).toContain("doesn't support shared notebooks");
    expect(button("Start sharing")).toBeUndefined();
    w.unmount();
  });
});
