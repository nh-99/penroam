import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { codeGroups, describeStatus, formatAgo, statusIcon, tapCounter, type SyncStatus } from "../src/platform/sync";
import { conflictTitle, conflictValuePreview, resolvable } from "../src/platform/conflicts";

const api = vi.hoisted(() => ({
  storeReconcile: vi.fn(async () => ({ applied: 0, conflicts: 0, waiting: 0, message: null })),
  info: vi.fn(),
  setupPair: vi.fn(),
  setupFirst: vi.fn(),
  pollPairing: vi.fn(),
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
  lastSuccessMs: null,
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
    expect(api.pollPairing).not.toHaveBeenCalled();
  });

  it("polls only while waiting for pairing approval, then asks to compare codes", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: false, status: null });
    api.setupPair.mockResolvedValue({ recoveryCode: null, pairingCode: "ABCD1234-0000-1111-2222-3333" });
    api.pollPairing.mockResolvedValueOnce(null).mockResolvedValueOnce("123 456");
    const s = useSync();
    await s.init();
    await s.setupPair("http://127.0.0.1:8787", "acct", "tablet");
    expect(s.step).toBe("pairing");
    await vi.advanceTimersByTimeAsync(3000);
    expect(s.step).toBe("pairing");
    await vi.advanceTimersByTimeAsync(3000);
    expect(s.step).toBe("compare");
    expect(s.comparison).toBe("123 456");
    await vi.advanceTimersByTimeAsync(9000);
    expect(api.pollPairing).toHaveBeenCalledTimes(2);
    // codes differ: disconnect without keys and explain
    api.disconnect.mockResolvedValue(null);
    await s.confirmComparison(false);
    expect(api.disconnect).toHaveBeenCalled();
    expect(s.configured).toBe(false);
    expect(s.error).toMatch(/did not match/);
  });

  it("shows the recovery code once and clears it after confirmation", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: false, status: null });
    api.setupFirst.mockResolvedValue({ recoveryCode: "PRK1-AAAAA-BBBBB", pairingCode: null });
    const s = useSync();
    await s.init();
    await s.setupFirst("http://127.0.0.1:8787", "acct", "laptop");
    expect(s.step).toBe("recovery");
    expect(s.recoveryCode).toBe("PRK1-AAAAA-BBBBB");
    s.finishRecovery();
    expect(s.recoveryCode).toBeNull();
    expect(s.step).toBe("none");
  });

  it("checks the server, signs in through the browser, and sets up with the server's account", async () => {
    const { useSync } = await import("../src/stores/sync");
    api.info.mockResolvedValue({ available: true, configured: false, status: null });
    api.serverInfo.mockResolvedValue({ signIn: true, signedInAccount: null });
    api.signIn.mockResolvedValue("a1b2c3");
    api.setupFirst.mockResolvedValue({ recoveryCode: "PRK1-AAAAA", pairingCode: null });
    const s = useSync();
    await s.init();
    await s.checkServer("https://sync.penroam.ink");
    expect(s.server).toEqual({ url: "https://sync.penroam.ink", signIn: true, signedInAccount: null });
    const pending = s.signIn("https://sync.penroam.ink");
    expect(s.signingIn).toBe(true);
    expect(await pending).toBe("a1b2c3");
    expect(s.signingIn).toBe(false);
    expect(s.server?.signedInAccount).toBe("a1b2c3");
    await s.setupFirst("https://sync.penroam.ink", "", "laptop");
    expect(api.setupFirst).toHaveBeenCalledWith("https://sync.penroam.ink", "", "laptop");
    expect(s.step).toBe("recovery");
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

  it("says how to recover from an ended sign-in", () => {
    expect(describeStatus({ ...base, phase: "needsReauth" }).text).toMatch(/sign in again.*queued/);
    expect(statusIcon({ ...base, phase: "needsReauth" })).toBe("alert");
  });
});
