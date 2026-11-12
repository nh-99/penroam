// What every Penroam Cloud plan includes. Plans differ only in how much storage they give and what
// they cost; the features below are the same on the free allowance and on every paid plan, and
// nothing on a device is gated by plan. Each row is backed by the user manual
// (docs/manual/sync-and-vaults.md, docs/manual/import-export.md) and by `describeReadOnly` in
// ./sync.ts. Keep it to things that exist today.

/** One row of the "Included in every plan" list. */
export interface PlanFeature {
  title: string;
  detail: string;
}

export const SHARED_PLAN_FEATURES: readonly PlanFeature[] = [
  {
    title: "Sync across your devices",
    detail: "Sign in on each device and your notebooks and folders arrive.",
  },
  {
    title: "Encrypted before upload",
    detail: "Notebooks are encrypted on your device before upload; put a notebook in a vault so only you can read it.",
  },
  {
    title: "Vaults",
    detail: "Keep notebooks only you can read, behind a passphrase.",
  },
  {
    title: "Writing and export are never limited",
    detail: "Notes, editing and export work the same on every plan, with or without an account.",
  },
  {
    title: "Your notes stay with you",
    detail: "If storage runs out, uploads pause. Nothing on your devices is removed.",
  },
];
