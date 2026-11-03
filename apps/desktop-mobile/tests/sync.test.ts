import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { codeGroups, describeStatus, formatAgo, statusIcon, type SyncStatus } from "../src/platform/sync";
import { conflictTitle, conflictValuePreview, resolvable } from "../src/platform/conflicts";

const api = vi.hoisted(() => ({
  info: vi.fn(),
  setupPair: vi.fn(),
  setupFirst: vi.fn(),
  pollPairing: vi.fn(),
  disconnect: vi.fn(),
  devices: vi.fn(),
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
    const title = { target: { kind: "notebookTitle" as const }, winner: "w", alternates: [{ opId: "o", value: "Other title" }] };
    expect(conflictTitle(title)).toBe("Notebook title");
    expect(conflictValuePreview(title, "Other title")).toBe("Other title");
    expect(resolvable(title)).toBe(true);
    const el = { target: { kind: "element" as const, pageId: "p", elementId: "e", group: 2 }, winner: "w", alternates: [] };
    expect(conflictTitle(el)).toMatch(/content/);
    expect(conflictValuePreview(el, { type: "text", text: "hello" })).toBe("hello");
    expect(resolvable({ ...el, target: { kind: "pageOrder" } })).toBe(false);
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
});
