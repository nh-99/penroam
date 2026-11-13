import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { codeGroups, describeStatus, FIRST_SYNC_TEXT, formatAgo, isFirstSync, statusIcon, tapCounter, type SyncStatus } from "../src/platform/sync";
import { conflictTitle, conflictValuePreview, resolvable } from "../src/platform/conflicts";

const api = vi.hoisted(() => ({
  storeReconcile: vi.fn(async () => ({ applied: 0, conflicts: 0, waiting: 0, message: null })),
  info: vi.fn(),
  connect: vi.fn(),
  erasedNotebooks: vi.fn(async () => []),
  account: vi.fn(async () => null),
  vaults: vi.fn(),
  vaultCreate: vi.fn(),
  vaultUnlock: vi.fn(),
  disconnect: vi.fn(),
  devices: vi.fn(),
  serverInfo: vi.fn(),
  signIn: vi.fn(),
  signOut: vi.fn(),
}));
vi.mock("../src/platform/sync", async (orig) => ({
  ...(await orig<typeof import("../src/platform/sync")>()),
  syncApi: api,
  onSyncStatus: vi.fn(async () => () => {}),
  onSyncChanged: vi.fn(async () => () => {}),
  onVaultsChanged: vi.fn(async () => () => {}),
}));
vi.mock("../src/platform/ipc", async (orig) => ({
  ...(await orig<typeof import("../src/platform/ipc")>()),
  hasHost: () => true,
}));

const base: SyncStatus = {
  phase: "idle",
  queued: 0,
  pendingRemote: 0,
  quarantined: 0,
  conflicts: 0,
  upToDate: true,
  lastSuccessMs: 1_000,
  lastError: null,
  deviceId: "d",
  accountId: "a",
  serverUrl: "http://127.0.0.1:8787",
  secretStore: "file",
  securityReviewPending: true,
};

describe("sync status wording", () => {
  it("is green only when really up to date", () => {
    expect(statusIcon(base)).toBe("check");
    expect(describeStatus(base)).toEqual({ tone: "ok", text: "Up to date" });
    expect(statusIcon({ ...base, upToDate: false })).toBe("busy");
    expect(statusIcon({ ...base, queued: 3, upToDate: false })).toBe("upload");
    expect(describeStatus({ ...base, queued: 3, upToDate: false }).text).toBe("3 change(s) queued for upload");
    expect(statusIcon({ ...base, conflicts: 1 })).toBe("alert");
    expect(statusIcon({ ...base, phase: "offline", upToDate: false })).toBe("off");
    expect(statusIcon(null)).toBe("off");
  });

  it("shows a running sync, and says so when it is the first one", () => {
    expect(statusIcon({ ...base, phase: "syncing", upToDate: false })).toBe("syncing");
    expect(describeStatus({ ...base, phase: "syncing", upToDate: false }).text).toBe("Syncing…");
    // never synced on this device: the first sync can take minutes
    const first = { ...base, lastSuccessMs: null, upToDate: false };
    expect(isFirstSync({ ...first, phase: "syncing" })).toBe(true);
    expect(isFirstSync(first)).toBe(true); // set up, about to start
    expect(describeStatus({ ...first, phase: "syncing" }).text).toBe(FIRST_SYNC_TEXT);
    expect(statusIcon({ ...first, queued: 4 })).toBe("syncing");
    // a problem is not a first sync: its own status says what is wrong
    expect(isFirstSync({ ...first, phase: "offline" })).toBe(false);
    expect(isFirstSync({ ...first, phase: "error" })).toBe(false);
    expect(isFirstSync(base)).toBe(false);
  });

  it("gives actionable text for every problem state", () => {
    expect(describeStatus({ ...base, phase: "revoked" }).text).toMatch(/removed.*local notes are kept/);
    expect(describeStatus({ ...base, phase: "integrityAlert" }).tone).toBe("danger");
    expect(describeStatus({ ...base, phase: "needsUpgrade" }).text).toMatch(/update/);
    expect(describeStatus({ ...base, phase: "offline", queued: 2 }).text).toMatch(/2 change\(s\) queued/);
    expect(describeStatus({ ...base, phase: "error", lastError: "boom" }).text).toBe("Sync error: boom");
    expect(describeStatus({ ...base, pendingRemote: 1, upToDate: false }).tone).toBe("warn");
  });

  it("formats times and codes", () => {
    expect(formatAgo(null)).toBe("never");
    expect(formatAgo(1000, 3000)).toBe("just now");
    expect(formatAgo(0, 125_000)).toBe("2 min ago");
    expect(codeGroups("PRK1-ABCDE-FGHIJ")).toEqual(["PRK1", "ABCDE", "FGHIJ"]);
  });

  it("describes conflicts", () => {
    const title = { target: { kind: "notebookTitle" as const }, winner: "w", winnerValue: "Shown title", alternates: [{ opId: "o", value: "Other title" }] };
    expect(conflictTitle(title)).toBe("Notebook title");
    expect(conflictValuePreview(title, "Other title")).toBe("Other title");
    expect(resolvable(title)).toBe(true);
    expect(conflictValuePreview(title, title.winnerValue)).toBe("Shown title");
    const el = { target: { kind: "element" as const, pageId: "p", elementId: "e", group: 2 }, winner: "w", winnerValue: { type: "text", text: "shown" } as unknown, alternates: [] };
    expect(conflictTitle(el)).toBe("Text changed on two devices");
    expect(conflictValuePreview(el, el.winnerValue)).toBe("shown");
    expect(conflictTitle({ ...el, winnerValue: { type: "ink", points: [] } })).toBe("Handwriting content changed on two devices");
    expect(conflictValuePreview(el, { type: "text", text: "hello" })).toBe("hello");
    expect(resolvable({ ...el, target: { kind: "pageOrder" } })).toBe(false);
  });
});

describe("developer mode taps", () => {
  it("toggles only on five taps within three seconds", () => {
    const tap = tapCounter();
    expect([0, 100, 200, 300].map(tap)).toEqual([false, false, false, false]);
    expect(tap(400)).toBe(true);
    // counting starts again
    expect([500, 600, 700, 800].map(tap)).toEqual([false, false, false, false]);
    expect(tap(900)).toBe(true);
    // too slow: taps older than the window drop out
    const slow = tapCounter();
    expect([0, 1000, 2000, 3000, 4000].map(slow)).toEqual([false, false, false, false, false]);
  });
});

describe("sync store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.useFakeTimers();
    Object.values(api).forEach((f) => f.mockReset());
    api.devices.mockResolvedValue([]);
  });
  afterEach(() => vi.useRealTimers());

  it("asks for a server address only in a debug build with developer mode on", async () => {
    const { useSync } = await import("../src/stores/sync");
    const { useSettings } = await import("../src/stores/settings");
    api.info.mockResolvedValue({
      available: true,
      configured: false,
      status: null,
      defaultServerUrl: "https://sync.penroam.ink",
      devBuild: false,
    });
    const settings = useSettings();
    settings.s.developerMode = true;
    const s = useSync();
    await s.init();
    expect(s.defaultServerUrl).toBe("https://sync.penroam.ink");
    expect(s.customServers).toBe(false); // release build: developer mode has no effect
    s.dispose();
    setActivePinia(createPinia());
    api.info.mockResolvedValue({
      available: true,
      configured: false,
      status: null,
      defaultServerUrl: "https://sync.penroam.ink",
      devBuild: true,
    });
    const dev = useSync();
    const devSettings = useSettings();
    devSettings.s.developerMode = false;
    await dev.init();
    expect(dev.customServers).toBe(false);
    devSettings.s.developerMode = true;
    expect(dev.customServers).toBe(true);
  });

  it("does nothing more when sync is not available", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: false, configured: false, status: null });
    const s = useSync();
    await s.init();
    expect(s.available).toBe(false);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(api.connect).not.toHaveBeenCalled();
  });

  it("shows a vault's recovery key once and clears it after confirmation", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: true, status: null });
    api.vaults.mockResolvedValue({ supported: true, vaults: [], notebooks: {} });
    api.vaultCreate.mockResolvedValue("PRK1-AAAAA-BBBBB");
    const s = useSync();
    await s.init();
    expect(await s.createVault("Journal", "a long enough passphrase")).toBe(true);
    expect(api.vaultCreate).toHaveBeenCalledWith("Journal", "a long enough passphrase");
    expect(s.vaultRecoveryCode).toBe("PRK1-AAAAA-BBBBB");
    s.finishVaultRecovery();
    expect(s.vaultRecoveryCode).toBeNull();
  });

  it("reports a wrong passphrase and offers only unlocked vaults for notebooks", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: true, status: null });
    api.vaults.mockResolvedValue({
      supported: true,
      vaults: [
        { vaultId: "v1", name: "Journal", unlocked: true, problem: null },
        { vaultId: "v2", name: null, unlocked: false, problem: null },
      ],
      notebooks: {},
    });
    api.vaultUnlock.mockRejectedValue(new Error("that passphrase doesn't open this vault"));
    const s = useSync();
    await s.init();
    await s.refreshVaults();
    expect(s.openVaults.map((v) => v.vaultId)).toEqual(["v1"]);
    expect(await s.unlockVault("v2", { passphrase: "nope nope nope" })).toBe(false);
    expect(s.error).toMatch(/passphrase/);
  });

  it("asks for a recovery code only for an account that needs one", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: false, status: null });
    api.connect.mockResolvedValueOnce({ needsRecoveryCode: true }).mockResolvedValueOnce({ needsRecoveryCode: false });
    const s = useSync();
    await s.init();
    await s.connect("http://127.0.0.1:8787", "acct");
    expect(s.needsRecoveryCode).toBe(true);
    expect(s.configured).toBe(false);
    api.info.mockResolvedValue({ available: true, configured: true, status: null });
    api.vaults.mockResolvedValue({ supported: true, vaults: [], notebooks: {} });
    await s.connect("http://127.0.0.1:8787", "acct", "PRK1-AAAAA");
    expect(api.connect).toHaveBeenLastCalledWith("http://127.0.0.1:8787", "acct", "PRK1-AAAAA");
    expect(s.needsRecoveryCode).toBe(false);
    expect(s.configured).toBe(true);
  });

  it("checks the server, and signing in is all it takes to connect", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: false, status: null });
    api.serverInfo.mockResolvedValue({ signIn: true, signedInAccount: null });
    api.signIn.mockResolvedValue("a1b2c3");
    api.connect.mockResolvedValue({ needsRecoveryCode: false });
    const s = useSync();
    await s.init();
    await s.checkServer("https://sync.penroam.ink");
    expect(s.server).toEqual({ url: "https://sync.penroam.ink", signIn: true, signedInAccount: null });
    api.info.mockResolvedValue({ available: true, configured: true, status: null });
    api.vaults.mockResolvedValue({ supported: true, vaults: [], notebooks: {} });
    const pending = s.signIn("https://sync.penroam.ink");
    expect(s.signingIn).toBe(true);
    expect(await pending).toBe("a1b2c3");
    expect(s.signingIn).toBe(false);
    expect(s.server?.signedInAccount).toBe("a1b2c3");
    expect(api.connect).toHaveBeenCalledWith("https://sync.penroam.ink", "", undefined);
    expect(s.configured).toBe(true);
  });

  it("keeps the form usable when sign-in fails or the server is unreachable", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: false, status: null });
    api.serverInfo.mockRejectedValueOnce(new Error("could not reach the server: offline"));
    const s = useSync();
    await s.init();
    await s.checkServer("https://sync.penroam.ink");
    expect(s.server).toBeNull();
    expect(s.error).toMatch(/could not reach/);
    api.serverInfo.mockResolvedValue({ signIn: true, signedInAccount: "a1" });
    await s.checkServer("https://sync.penroam.ink");
    api.signOut.mockResolvedValue(undefined);
    await s.signOut("https://sync.penroam.ink");
    expect(s.server?.signedInAccount).toBeNull();
    api.signIn.mockRejectedValue(new Error("sign-in was refused: access_denied"));
    expect(await s.signIn("https://sync.penroam.ink")).toBeNull();
    expect(s.signingIn).toBe(false);
    expect(s.error).toMatch(/refused/);
    s.changeServer();
    expect(s.server).toBeNull();
    expect(s.error).toBeNull();
  });

  it("offers sign-in again after disconnecting, even when the disconnect reported an error", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: false, status: null });
    api.serverInfo.mockResolvedValue({ signIn: true, signedInAccount: null });
    api.signIn.mockResolvedValue("a1");
    api.connect.mockResolvedValue({ needsRecoveryCode: false });
    api.vaults.mockResolvedValue({ supported: true, vaults: [], notebooks: {} });
    const s = useSync();
    await s.init();
    await s.checkServer("https://sync.penroam.ink");
    api.info.mockResolvedValue({ available: true, configured: true, status: null });
    await s.signIn("https://sync.penroam.ink");
    expect(s.server?.signedInAccount).toBe("a1");
    // sync ended and signed out, but a locked file made the call fail
    api.disconnect.mockRejectedValue(new Error("The process cannot access the file"));
    api.info.mockResolvedValue({ available: true, configured: false, status: null });
    await s.disconnect();
    expect(s.configured).toBe(false);
    expect(s.server).toBeNull();
    // a sign-in that ended after the server was checked: Connect fails, the button comes back
    api.serverInfo.mockResolvedValue({ signIn: true, signedInAccount: "a1" });
    await s.checkServer("https://sync.penroam.ink");
    api.serverInfo.mockResolvedValue({ signIn: true, signedInAccount: null });
    api.connect.mockRejectedValue(new Error("sign in to this server first"));
    await s.connect("https://sync.penroam.ink", "");
    expect(s.server?.signedInAccount).toBeNull();
    expect(s.error).toMatch(/sign in/);
  });

  it("says how to recover from an ended sign-in", () => {
    expect(describeStatus({ ...base, phase: "needsReauth" }).text).toMatch(/sign in again.*queued/);
    expect(statusIcon({ ...base, phase: "needsReauth" })).toBe("alert");
  });
});
