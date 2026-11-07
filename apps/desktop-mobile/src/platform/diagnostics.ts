// Local crash reports and the diagnostic report (src-tauri/src/diagnostics.rs). Nothing here uses
// the network: a report leaves the device only if the user copies it.
import { invoke } from "@tauri-apps/api/core";
import { hasHost } from "./ipc";

export const diagnosticsApi = {
  report: (inkPath: string | null) => invoke<string>("diagnostics_report", { inkPath }),
  clear: () => invoke<void>("diagnostics_clear"),
  thirdPartyNotices: () => invoke<string | null>("third_party_notices"),
  openHelp: (page: "support" | "privacy" | "subscriptions" | "terms") => invoke<void>("open_help", { page }),
};

/** Stores a redacted report of an unhandled UI error. It must never raise an error of its own. */
export function recordUiError(location: string, error: unknown) {
  if (!hasHost()) return;
  const message = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
  void invoke("diagnostics_ui_error", { location, message }).catch(() => {});
}
