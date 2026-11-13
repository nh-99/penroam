// Typed facade over the shared-notebook commands (apps/desktop-mobile/src-tauri/src/sync.rs) and
// the `sync://shared` event, plus the plain-language labels the UI shows for their states.
//
// Types come from the Rust schema via ts-rs (src/generated).
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { Notebook } from "../generated/Notebook";
import type { SharedRole } from "../generated/SharedRole";
import type { SharedStateView } from "../generated/SharedStateView";
import type { SharedSyncState } from "../generated/SharedSyncState";
import type { SharedView } from "../generated/SharedView";
import type { SharedNotebookView } from "../generated/SharedNotebookView";
import type { IncomingInvitationView } from "../generated/IncomingInvitationView";
import type { MemberView } from "../generated/MemberView";
import type { MembersView } from "../generated/MembersView";
import type { InvitationView } from "../generated/InvitationView";
import type { NotebookAccess } from "../generated/NotebookAccess";
import { CommandError, hasHost } from "./ipc";

export type { SharedRole, SharedSyncState, SharedView, SharedNotebookView, IncomingInvitationView, MemberView, MembersView, InvitationView, NotebookAccess };
/** Roles an owner can hand out. */
export type InviteRole = Exclude<SharedRole, "owner">;
/** Where this device is with a shared notebook. */
export type SharedState = SharedStateView;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasHost()) throw new CommandError("noHost", "Sharing needs the Penroam app.");
  try {
    return await invoke<T>(cmd, args);
  } catch (e: any) {
    if (e && typeof e === "object" && "code" in e) throw new CommandError(e.code, e.message ?? String(e));
    throw new CommandError("io", String(e));
  }
}

export const sharingApi = {
  list: () => call<SharedView>("shared_list"),
  /** Shares a notebook; safe to repeat. Earlier versions of the notebook become visible to its members. */
  share: (notebookId: string) => call<SharedNotebookView>("shared_share", { notebookId }),
  members: (spaceId: string) => call<MembersView>("shared_members", { spaceId }),
  invite: (spaceId: string, email: string, role: InviteRole) => call<InvitationView>("shared_invite", { spaceId, email, role }),
  cancelInvitation: (spaceId: string, invitationId: string) => call<void>("shared_cancel_invitation", { spaceId, invitationId }),
  changeRole: (spaceId: string, accountId: string, role: InviteRole, version: number) => call<MemberView>("shared_change_role", { spaceId, accountId, role, version }),
  removeMember: (spaceId: string, accountId: string) => call<void>("shared_remove_member", { spaceId, accountId }),
  leave: (spaceId: string) => call<void>("shared_leave", { spaceId }),
  accept: (invitationId: string) => call<SharedNotebookView>("shared_accept", { invitationId }),
  decline: (invitationId: string) => call<void>("shared_decline", { invitationId }),
  keepPrivateCopy: (spaceId: string) => call<Notebook>("shared_keep_private_copy", { spaceId }),
  removeLocal: (spaceId: string) => call<void>("shared_remove_local", { spaceId }),
  access: (notebookId: string) => call<NotebookAccess>("notebook_access", { notebookId }),
};

/** The list of shared notebooks or invitations changed. */
export async function onSharedChanged(cb: () => void): Promise<UnlistenFn> {
  return listen("sync://shared", () => cb());
}

/** An owner's own notebook that has been shared. */
export const isSharedByMe = (v: SharedNotebookView) => v.owner && v.state !== "accessRemoved";

/** Words for a role. */
export function roleLabel(r: SharedRole): string {
  return r === "owner" ? "Owner" : r === "editor" ? "Can edit" : "View only";
}

/** Short sync label for a shared notebook, as the card and the editor show it. */
export function syncStateLabel(s: SharedSyncState): string {
  switch (s) {
    case "localSaved":
      return "Local saved";
    case "waitingToUpload":
      return "Waiting to upload";
    case "synced":
      return "Synced";
    case "accessRemoved":
      return "Access removed";
    case "needsResolution":
      return "Needs resolution";
  }
}

/** The state label for a card: joining and access loss come before the sync state. */
export function sharedStatusLabel(v: SharedNotebookView): string {
  if (v.state === "joining") return "Joining…";
  if (v.state === "accessRemoved") return "Access removed";
  return syncStateLabel(v.syncState);
}

/** The banner above a read-only notebook; null when it can be edited. */
export function accessBanner(a: NotebookAccess | null): string | null {
  if (!a?.readOnly) return null;
  return a.reason === "accessRemoved"
    ? "Access removed — you can still look and export what is on this device, but changes are no longer saved or shared."
    : "View only — you can look and export, not edit.";
}

/** Why a notebook can't be shared right now, in words; null when it can. */
export function shareBlockedReason(opts: { inVault: boolean; configured: boolean; available: boolean }): string | null {
  if (opts.inVault) return "Notebooks in a vault can't be shared.";
  if (!opts.configured) return "Sign in to Penroam Cloud in Settings to share notebooks.";
  if (!opts.available) return "This server doesn't support shared notebooks.";
  return null;
}

/**
 * Plain-language errors. Rust reports wire failures as code "invalid" with a message that starts
 * with the server's code ("access_revoked: …"); other refusals (a read-only notebook, say) carry
 * their own clear message, which is shown as is.
 */
export function describeSharingError(e: unknown): string {
  const raw = (e as Error)?.message ?? String(e);
  const code = /^([a-z_]+):/.exec(raw)?.[1] ?? (e instanceof CommandError ? String(e.code) : "");
  switch (code) {
    case "reauth_required":
      return "Sign in again to confirm your email address, then accept.";
    case "access_revoked":
      return "Your access to this notebook was removed.";
    case "invitation_expired":
      return "That invitation has expired. Ask for a new one.";
    case "invalid_role":
      return "That role isn't available.";
    case "rate_limited":
      return "Too many tries. Wait a moment and try again.";
    case "not_found":
      return "That notebook or invitation isn't available any more.";
    case "conflict":
      return "This changed elsewhere. Reopen the list and try again.";
    case "forbidden":
      return "You don't have permission to do that.";
    default:
      return raw;
  }
}

/** Why accepting an invitation failed, when the card can offer a way forward; otherwise null. */
export function acceptProblem(e: unknown): "reauth" | "oldCopy" | null {
  const raw = (e as Error)?.message ?? String(e);
  if (/^reauth_required:/.test(raw)) return "reauth";
  if (/^this device still has your earlier copy of this notebook/i.test(raw)) return "oldCopy";
  return null;
}

/** What a person is called in the list; former members have no email any more. */
export const memberName = (m: { display: string; state: string }) => m.display || (m.state === "revoked" ? "Former member" : "Unknown member");

/** Text of the privacy explanation; the dialog shows these and the tests check they stay. */
export const SHARING_PRIVACY_POINTS: readonly string[] = [
  "Penroam Cloud can read ordinary and shared notebooks. They aren't end-to-end encrypted; only notebooks in a vault are private from the server.",
  "People you remove keep any copy they already downloaded. Removing someone stops future updates reaching them.",
  "Sharing a notebook that already exists shares its earlier versions too, not only new changes.",
];
