import { describe, expect, it } from "vitest";
import { canBuy, cloudNotice, describePlan, describeReadOnly, describeRestore, describeStorePurchase, formatBytes, storeManaged, usageFraction, type AccountView } from "../src/platform/sync";

function view(p: Partial<AccountView> = {}): AccountView {
  return {
    planName: "Free",
    planStatus: "free",
    renewsAtMs: null,
    endsAtMs: null,
    usedBytes: 0,
    quotaBytes: 100 * 1024 * 1024,
    freeQuotaBytes: 100 * 1024 * 1024,
    writable: true,
    readOnlyReason: null,
    offers: [],
    canManageBilling: false,
    canRequestDeletion: true,
    canCancelDeletion: false,
    deletionEraseAfterMs: null,
    purchasesAllowed: true,
    storePurchases: false,
    storeOffers: [],
    managedIn: null,
    ...p,
  };
}

const offer = { id: "m", name: "Penroam Cloud", quotaBytes: 5 * 1024 ** 3, price: "$3.99", period: "month" };

describe("store purchases", () => {
  const paused = { writable: false, readOnlyReason: "over_quota", usedBytes: 200 * 1024 * 1024, purchasesAllowed: false, storePurchases: true };

  it("offers a plan when the store can sell one", () => {
    expect(canBuy(view({ purchasesAllowed: false, storeOffers: [offer] }))).toBe(true);
    expect(cloudNotice(view({ ...paused, storeOffers: [offer] }))?.canUpgrade).toBe(true);
  });

  it("offers nothing when the store can't sell here", () => {
    expect(canBuy(view({ purchasesAllowed: false, storeOffers: [] }))).toBe(false);
    expect(cloudNotice(view({ ...paused }))?.canUpgrade).toBe(false);
    expect(cloudNotice(view({ ...paused }))?.text).toContain("Delete notebooks");
  });

  it("knows who manages a plan", () => {
    expect(storeManaged(view({ managedIn: "app_store" }))).toBe(true);
    expect(storeManaged(view({ managedIn: "google_play" }))).toBe(true);
    expect(storeManaged(view({ managedIn: "web" }))).toBe(false);
    expect(storeManaged(view())).toBe(false);
  });

  it("tells the user when a paid purchase hasn't arrived, without asking them to pay again", () => {
    const text = describeStorePurchase({ status: "waiting", message: "try later" });
    expect(text).toContain("won't be charged again");
    expect(describeStorePurchase({ status: "conflict", message: "another account" })).toBe("another account");
    expect(describeStorePurchase({ status: "active", message: null })).toBeNull();
    expect(describeStorePurchase({ status: "cancelled", message: null })).toBeNull();
  });

  it("reports what a restore found", () => {
    const r = { applied: 0, conflicts: 0, waiting: 0, message: null };
    expect(describeRestore(r)).toBe("There are no purchases to restore.");
    expect(describeRestore({ ...r, applied: 1 })).toBe("Your purchases are restored.");
    expect(describeRestore({ ...r, waiting: 1 })).toContain("Try again");
    expect(describeRestore({ ...r, conflicts: 1, message: "another account" })).toBe("another account");
  });
});

describe("cloud account display", () => {
  it("formats bytes", () => {
    expect(formatBytes(1)).toBe("1 byte");
    expect(formatBytes(512)).toBe("512 bytes");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(100 * 1024 * 1024)).toBe("100 MB");
    expect(formatBytes(5 * 1024 ** 3)).toBe("5 GB");
  });

  it("measures usage, capped, unlimited as null", () => {
    expect(usageFraction(view({ usedBytes: 50 * 1024 * 1024 }))).toBe(0.5);
    expect(usageFraction(view({ usedBytes: 900 * 1024 * 1024 }))).toBe(1);
    expect(usageFraction(view({ quotaBytes: null }))).toBeNull();
  });

  it("describes plans without promising more than the server said", () => {
    expect(describePlan(view())).toBe("Free storage");
    const t = Date.UTC(2026, 11, 3);
    expect(describePlan(view({ planName: "Penroam Cloud", planStatus: "active", renewsAtMs: t }))).toMatch(/^Penroam Cloud · renews /);
    expect(describePlan(view({ planName: "Penroam Cloud", planStatus: "canceling", endsAtMs: t }))).toMatch(/ends/);
    expect(describePlan(view({ planName: "Penroam Cloud", planStatus: "past_due", endsAtMs: t }))).toMatch(/payment failed/);
    expect(describePlan(view({ planName: "Something new", planStatus: "teleporting" }))).toBe("Something new");
  });

  it("explains read-only states and keeps local work explicit", () => {
    expect(describeReadOnly(view())).toBeNull();
    const full = describeReadOnly(view({ writable: false, readOnlyReason: "over_quota", offers: [{ id: "m", name: "P", priceLabel: "$1", quotaBytes: 1 }] }))!;
    expect(full).toMatch(/uploads are paused/);
    expect(full).toMatch(/stay on this device/);
    expect(full).toMatch(/plan with more storage/);
    // mobile builds never point at a plan
    const mobile = describeReadOnly(view({ writable: false, readOnlyReason: "over_quota", purchasesAllowed: false }))!;
    expect(mobile).not.toMatch(/plan/);
    expect(describeReadOnly(view({ writable: false, readOnlyReason: "deletion_pending" }))).toMatch(/scheduled for deletion/);
    expect(describeReadOnly(view({ writable: false, readOnlyReason: "future_reason" }))).toMatch(/Uploads are paused/);
  });
});

describe("storage and payment banner text", () => {
  const MB = 1024 * 1024;
  const offer = { id: "cloud-monthly", name: "Penroam Cloud", priceLabel: "$3.99 / month", quotaBytes: 5 * 1024 ** 3 };

  it("stays quiet below the warning level and for unlimited storage", () => {
    expect(cloudNotice(null)).toBeNull();
    expect(cloudNotice(view({ usedBytes: 89 * MB, offers: [offer] }))).toBeNull();
    expect(cloudNotice(view({ quotaBytes: null, usedBytes: 10 ** 12 }))).toBeNull();
  });

  it("warns when nearly full, and offers plans on desktop only", () => {
    const n = cloudNotice(view({ usedBytes: 95 * MB, offers: [offer] }))!;
    expect(n.kind).toBe("nearlyFull");
    expect(n.text).toContain("95% full");
    expect(n.canUpgrade).toBe(true);
    expect(cloudNotice(view({ usedBytes: 95 * MB, offers: [offer], purchasesAllowed: false }))!.canUpgrade).toBe(false);
  });

  it("explains paused uploads, pointing at a plan only where one can be bought", () => {
    const full = { usedBytes: 120 * MB, writable: false, readOnlyReason: "over_quota" };
    const d = cloudNotice(view({ ...full, offers: [offer] }))!;
    expect(d.kind).toBe("full");
    expect(d.text).toMatch(/uploads are paused.*safe on this device.*more storage/);
    const m = cloudNotice(view({ ...full, offers: [offer], purchasesAllowed: false }))!;
    expect(m.canUpgrade).toBe(false);
    expect(m.text).not.toMatch(/more storage|plan/);
    // subscribed and still full: no offers, so the text suggests making room
    expect(cloudNotice(view({ ...full, planStatus: "active" }))!.text).toMatch(/Delete notebooks/);
  });

  it("asks for a new payment method after a failed payment, desktop only", () => {
    const d = cloudNotice(view({ planStatus: "past_due", planName: "Penroam Cloud", canManageBilling: true }))!;
    expect(d.kind).toBe("pastDue");
    expect(d.canManage).toBe(true);
    expect(cloudNotice(view({ planStatus: "past_due", canManageBilling: true, purchasesAllowed: false }))!.canManage).toBe(false);
  });

  it("leaves deletion and suspension to the settings panel", () => {
    expect(cloudNotice(view({ writable: false, readOnlyReason: "deletion_pending" }))).toBeNull();
    expect(cloudNotice(view({ writable: false, readOnlyReason: "suspended" }))).toBeNull();
  });
});

// ---- the settings panel, against a stubbed command layer ----
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import { vi } from "vitest";

const api = vi.hoisted(() => ({
  account: vi.fn(),
  openBilling: vi.fn(async () => {}),
  requestDeletion: vi.fn(async () => ({ eraseAfterMs: Date.UTC(2026, 11, 3) })),
  cancelDeletion: vi.fn(async () => {}),
  syncNow: vi.fn(async () => {}),
  storeReconcile: vi.fn(async () => ({ applied: 0, conflicts: 0, waiting: 0, message: null })),
  info: vi.fn(async () => ({ available: true, configured: true, status: null, defaultServerUrl: "", devBuild: false })),
}));
vi.mock("../src/platform/sync", async (orig) => ({
  ...(await orig<typeof import("../src/platform/sync")>()),
  syncApi: api,
}));

async function panel(v: AccountView) {
  setActivePinia(createPinia());
  api.account.mockResolvedValue(v);
  const { useSync } = await import("../src/stores/sync");
  const s = useSync();
  s.available = true;
  s.configured = true;
  const C = (await import("../src/components/CloudAccountSection.vue")).default;
  const w = mount(C, { attachTo: document.body });
  await flushPromises();
  return w;
}

describe("Penroam Cloud panel", () => {
  it("desktop: offers open the checkout with the offer id, never a price", async () => {
    const w = await panel(view({ offers: [{ id: "cloud-monthly", name: "Penroam Cloud", priceLabel: "$3.99 / month", quotaBytes: 5 * 1024 ** 3 }] }));
    expect(w.text()).toContain("Free storage");
    expect(w.text()).toContain("0 bytes of 100 MB used");
    const buy = w.findAll("button").find((b) => b.text().includes("$3.99 / month"))!;
    await buy.trigger("click");
    expect(api.openBilling).toHaveBeenCalledWith("subscribe", "cloud-monthly");
    w.unmount();
  });

  it("mobile: no prices, no billing buttons, read-only explained without an upsell", async () => {
    const w = await panel(view({ purchasesAllowed: false, writable: false, readOnlyReason: "over_quota", usedBytes: 200 * 1024 * 1024 }));
    expect(w.text()).not.toMatch(/\$|plan with more storage|Manage plan/);
    expect(w.text()).toContain("Cloud storage is full");
    expect(w.find('[role="progressbar"]').attributes("aria-valuenow")).toBe("100");
    w.unmount();
  });

  it("deletion asks first and warns about unsynced changes", async () => {
    const w = await panel(view());
    await w.findAll("button").find((b) => b.text().includes("Delete account"))!.trigger("click");
    await flushPromises();
    expect(document.body.textContent).toContain("Notes on this device and your other devices are not deleted");
    const go = [...document.body.querySelectorAll("button")].find((b) => b.textContent?.includes("Continue in browser"))!;
    go.click();
    await flushPromises();
    expect(api.requestDeletion).toHaveBeenCalledOnce();
    w.unmount();
  });

  it("pending deletion can be withdrawn", async () => {
    const w = await panel(view({ writable: false, readOnlyReason: "deletion_pending", canRequestDeletion: false, canCancelDeletion: true, deletionEraseAfterMs: Date.UTC(2026, 11, 3) }));
    expect(w.text()).toContain("scheduled for deletion");
    await w.findAll("button").find((b) => b.text().includes("Keep my account"))!.trigger("click");
    await flushPromises();
    expect(api.cancelDeletion).toHaveBeenCalledOnce();
    w.unmount();
  });
});

async function shell(v: AccountView) {
  setActivePinia(createPinia());
  api.account.mockResolvedValue(v);
  const { useSync } = await import("../src/stores/sync");
  const s = useSync();
  s.available = true;
  s.configured = true;
  await s.refreshAccount();
  const Banner = (await import("../src/components/CloudBanner.vue")).default;
  const Dialog = (await import("../src/components/UpgradeDialog.vue")).default;
  const { defineComponent, h } = await import("vue");
  const Shell = defineComponent(() => () => [h(Banner), s.upgradeOpen ? h(Dialog) : null]);
  const w = mount(Shell, { attachTo: document.body });
  await flushPromises();
  return { w, s };
}

describe("upgrade prompts", () => {
  const offer = { id: "cloud-monthly", name: "Penroam Cloud", priceLabel: "$3.99 / month", quotaBytes: 5 * 1024 ** 3 };
  const full = { usedBytes: 120 * 1024 * 1024, writable: false, readOnlyReason: "over_quota" };
  const button = (label: string) => [...document.body.querySelectorAll("button")].find((b) => b.textContent?.includes(label));

  it("desktop: banner opens the plan picker, choosing opens checkout", async () => {
    api.openBilling.mockClear();
    const { w } = await shell(view({ ...full, offers: [offer] }));
    expect(w.text()).toContain("Cloud storage is full");
    button("Get more storage")!.click();
    await flushPromises();
    expect(document.body.textContent).toContain("More cloud storage");
    expect(document.body.textContent).toContain("5 GB of cloud storage · $3.99 / month");
    button("Choose")!.click();
    await flushPromises();
    expect(api.openBilling).toHaveBeenCalledWith("subscribe", "cloud-monthly");
    w.unmount();
  });

  it("after paying, the banner goes, the picker closes and queued uploads resume", async () => {
    api.syncNow.mockClear();
    const { w, s } = await shell(view({ ...full, offers: [offer] }));
    s.status = { phase: "quotaExceeded" } as never;
    s.showUpgrade();
    await flushPromises();
    api.account.mockResolvedValue(view({ planStatus: "active", planName: "Penroam Cloud", usedBytes: full.usedBytes, quotaBytes: offer.quotaBytes, canManageBilling: true }));
    await s.refreshAccount();
    await flushPromises();
    expect(s.upgradeOpen).toBe(false);
    expect(s.notice).toBeNull();
    expect(api.syncNow).toHaveBeenCalledOnce();
    w.unmount();
  });

  it("mobile: the banner explains but never leads to a purchase", async () => {
    const { w, s } = await shell(view({ ...full, offers: [offer], purchasesAllowed: false }));
    expect(w.text()).toContain("Cloud storage is full");
    expect(button("Get more storage")).toBeUndefined();
    s.showUpgrade();
    await flushPromises();
    expect(s.upgradeOpen).toBe(false);
    w.unmount();
  });

  it("closing the banner hides it for this run", async () => {
    const { w, s } = await shell(view({ usedBytes: 95 * 1024 * 1024, offers: [offer] }));
    expect(w.text()).toContain("95% full");
    (document.body.querySelector('[aria-label="Hide this message"]') as HTMLButtonElement).click();
    await flushPromises();
    expect(s.notice).toBeNull();
    expect(w.text()).not.toContain("95% full");
    w.unmount();
  });
});
