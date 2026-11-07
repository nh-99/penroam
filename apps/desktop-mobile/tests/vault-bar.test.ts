import { describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";

const api = vi.hoisted(() => ({
  vaults: vi.fn(),
  vaultCreate: vi.fn(async () => "PRK1-AAAA-BBBB-CCCC"),
  vaultLock: vi.fn(),
  vaultForget: vi.fn(async () => {}),
  vaultRestore: vi.fn(async () => {}),
}));
vi.mock("../src/platform/sync", async (orig) => ({
  ...(await orig<typeof import("../src/platform/sync")>()),
  syncApi: api,
}));

const button = (label: string) => Array.from(document.body.querySelectorAll("button")).find((b) => b.textContent?.includes(label));

describe("vault strip on the library screen", () => {
  it("hides itself when the server has no vaults", async () => {
    setActivePinia(createPinia());
    api.vaults.mockResolvedValue({ supported: false, vaults: [], notebooks: {} });
    const VaultBar = (await import("../src/components/VaultBar.vue")).default;
    const w = mount(VaultBar, { attachTo: document.body });
    await flushPromises();
    expect(w.text()).not.toContain("Vaults");
    w.unmount();
  });

  it("lists vaults, creates one and keeps the recovery key until it's confirmed", async () => {
    setActivePinia(createPinia());
    api.vaults.mockResolvedValue({
      supported: true,
      vaults: [
        { vaultId: "v1", name: "Journal", unlocked: true, locked: false, local: false, problem: null },
        { vaultId: "v2", name: null, unlocked: false, locked: false, local: false, problem: null },
      ],
      notebooks: { a: "v1", b: "v1" },
    });
    const VaultBar = (await import("../src/components/VaultBar.vue")).default;
    const w = mount(VaultBar, { attachTo: document.body });
    await flushPromises();
    expect(w.text()).toContain("Journal");
    expect(w.text()).toContain("2");
    expect(w.find('[aria-label="Unlock locked vault"]').exists()).toBe(true);

    button("New vault")!.click();
    await flushPromises();
    const inputs = document.body.querySelectorAll<HTMLInputElement>('[role="dialog"] input');
    for (const [i, v] of ["Taxes", "correct horse battery", "correct horse battery"].entries()) {
      inputs[i].value = v;
      inputs[i].dispatchEvent(new Event("input"));
    }
    await flushPromises();
    button("Create vault")!.click();
    await flushPromises();
    expect(api.vaultCreate).toHaveBeenCalledWith("Taxes", "correct horse battery");
    expect(document.body.textContent).toContain("Save your recovery key");

    // putting the dialog away leaves a way back to it
    document.body.querySelector<HTMLElement>('[role="dialog"]')!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await flushPromises();
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
    button("Save the recovery key")!.click();
    await flushPromises();
    const confirm = document.body.querySelector<HTMLInputElement>('[role="dialog"] input')!;
    confirm.value = "cccc";
    confirm.dispatchEvent(new Event("input"));
    await flushPromises();
    button("I saved the key")!.click();
    await flushPromises();
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
    expect(button("Save the recovery key")).toBeUndefined();
    w.unmount();
  });
});

describe("locking a vault from the library", () => {
  it("locks straight away, or asks for the passphrase once when the device needs it", async () => {
    setActivePinia(createPinia());
    api.vaults.mockResolvedValue({
      supported: true,
      vaults: [{ vaultId: "v1", name: "Journal", unlocked: true, locked: false, local: false, problem: null }],
      notebooks: {},
    });
    api.vaultLock.mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    const VaultBar = (await import("../src/components/VaultBar.vue")).default;
    const w = mount(VaultBar, { attachTo: document.body });
    await flushPromises();
    button("Journal")!.click();
    await flushPromises();
    button("Lock")!.click();
    await flushPromises();
    expect(api.vaultLock).toHaveBeenLastCalledWith("v1", undefined);
    expect(document.body.querySelector('[role="dialog"]')?.textContent).toContain("needs the passphrase once");
    const input = document.body.querySelector<HTMLInputElement>('[role="dialog"] input')!;
    input.value = "correct horse battery";
    input.dispatchEvent(new Event("input"));
    await flushPromises();
    Array.from(document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')).find((b) => b.textContent?.trim() === "Lock")!.click();
    await flushPromises();
    expect(api.vaultLock).toHaveBeenLastCalledWith("v1", { passphrase: "correct horse battery" });
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
    w.unmount();
  });
});

describe("a vault that isn't synced with the cloud", () => {
  const view = {
    supported: true,
    vaults: [{ vaultId: "v9", name: "Old", unlocked: true, locked: false, local: true, problem: "not synced" }],
    notebooks: { n1: "v9" },
  };

  it("can be removed from this device after a warning", async () => {
    setActivePinia(createPinia());
    api.vaults.mockResolvedValue(view);
    const VaultBar = (await import("../src/components/VaultBar.vue")).default;
    const w = mount(VaultBar, { attachTo: document.body });
    await flushPromises();
    expect(w.text()).toContain("Not synced");
    button("Old")!.click();
    await flushPromises();
    button("Remove from this device")!.click();
    await flushPromises();
    expect(document.body.querySelector('[role="dialog"]')?.textContent).toContain("only copy");
    Array.from(document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')).find((b) => b.textContent?.trim() === "Remove")!.click();
    await flushPromises();
    expect(api.vaultForget).toHaveBeenCalledWith("v9");
    w.unmount();
  });

  it("can restore cloud sync with its passphrase", async () => {
    setActivePinia(createPinia());
    api.vaults.mockResolvedValue(view);
    const VaultBar = (await import("../src/components/VaultBar.vue")).default;
    const w = mount(VaultBar, { attachTo: document.body });
    await flushPromises();
    button("Old")!.click();
    await flushPromises();
    button("Restore cloud sync")!.click();
    await flushPromises();
    const input = document.body.querySelector<HTMLInputElement>('[role="dialog"] input')!;
    input.value = "correct horse battery";
    input.dispatchEvent(new Event("input"));
    await flushPromises();
    Array.from(document.body.querySelectorAll<HTMLButtonElement>('[role="dialog"] button')).find((b) => b.textContent?.trim() === "Restore cloud sync")!.click();
    await flushPromises();
    expect(api.vaultRestore).toHaveBeenCalledWith("v9", { passphrase: "correct horse battery" });
    expect(document.body.querySelector('[role="dialog"]')).toBeNull();
    w.unmount();
  });
});
