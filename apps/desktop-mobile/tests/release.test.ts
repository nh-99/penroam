import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a), isTauri: () => true }));
vi.mock("../src/platform/ipc", () => ({ hasHost: () => true }));

const { checkAtStartup, updatesApi } = await import("../src/platform/updates");
const { recordUiError } = await import("../src/platform/diagnostics");
const { registerFlusher } = await import("../src/platform/lifecycle");

beforeEach(() => {
  invoke.mockReset();
});

describe("checkAtStartup", () => {
  it("makes no request while the setting is off", async () => {
    expect(await checkAtStartup(false, "stable")).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });

  it("makes no request from a build that can't update", async () => {
    invoke.mockResolvedValueOnce({ enabled: false });
    expect(await checkAtStartup(true, "stable")).toBeNull();
    expect(invoke.mock.calls.map((c) => c[0])).toEqual(["update_support"]);
  });

  it("passes on the channel's offer", async () => {
    const offer = { version: "9.0.0", notes: null };
    invoke.mockResolvedValueOnce({ enabled: true }).mockResolvedValueOnce(offer);
    expect(await checkAtStartup(true, "beta")).toEqual(offer);
    expect(invoke).toHaveBeenLastCalledWith("update_check", { channel: "beta" });
  });

  it("stays quiet when the request fails, because being offline is normal", async () => {
    invoke.mockResolvedValueOnce({ enabled: true }).mockRejectedValueOnce("offline");
    expect(await checkAtStartup(true, "stable")).toBeNull();
  });
});

describe("updatesApi.install", () => {
  it("saves the editor's work before anything is installed", async () => {
    const order: string[] = [];
    const remove = registerFlusher(async () => void order.push("flush"));
    invoke.mockImplementation(async (cmd: string) => void order.push(cmd));
    await updatesApi.install("stable");
    remove();
    expect(order).toEqual(["flush", "update_install"]);
  });

  it("doesn't install when saving failed", async () => {
    const remove = registerFlusher(async () => {
      throw new Error("disk full");
    });
    await expect(updatesApi.install("stable")).rejects.toThrow("disk full");
    remove();
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("recordUiError", () => {
  it("passes the error to the backend, which redacts it, and never throws", () => {
    invoke.mockRejectedValue(new Error("no host"));
    expect(() => recordUiError("window", new TypeError("x is null"))).not.toThrow();
    expect(invoke).toHaveBeenCalledWith("diagnostics_ui_error", { location: "window", message: "TypeError: x is null" });
  });
});
