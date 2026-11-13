//! Shared notebooks in the app: the `shared_*` commands, the views the library and the share
//! dialog show, and the one check every command that changes a notebook makes first.
//!
//! The work itself is in `penroam_sync::shared`. The sync worker (`crate::sync`) owns a
//! [`SpaceSet`] next to the account engine: after each account reconcile it rereads the server's
//! lists of shared notebooks and invitations, joins, reconciles each space (a failing space never
//! stops the others), and emits `sync://shared` when what [`shared_view`] shows changed. Commands
//! that need the network or an engine are sent to the worker ([`SharedCmd`]); the rest
//! ([`notebook_access`], [`require_writable`]) read the notebook owners file under the library
//! lock.
//!
//! **Read-only rule.** A viewer's notebook, and one whose access was removed, can't be changed
//! through any command: every mutating command in `crate::commands` calls [`require_writable`]
//! with the library lock held, so the check and the change see the same assignment. The server
//! refuses a viewer's uploads on its own as well; the local check is what keeps a viewer's copy
//! unchanged. Exporting and duplicating stay allowed (the owner's decision).
//!
//! The view types are exported to TypeScript (`src/generated`) by ts-rs, as `cargo test` runs.

use crate::commands::CmdError;
use crate::AppState;
use penroam_core::Notebook;
use penroam_storage::{ErrorCode, Library};
use penroam_sync::shared::{self, Access, NotebookSyncState, SharedState, SpaceAccess, SpaceSet};
use penroam_sync::transport::HttpTransport;
use penroam_sync::vault::NotebookOwners;
use penroam_sync::{SyncEngine, SyncTransport as _};
use penroam_sync_protocol::{
    AccountId, ChangeRoleRequest, CreateInvitationRequest, InvitationId, InvitationState,
    MembershipState, Role,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use ts_rs::TS;
use uuid::Uuid;

type Engine = SyncEngine<HttpTransport>;
type Reply<T> = Sender<Result<T, String>>;

/// What a person may do in a shared notebook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum SharedRole {
    /// Shared it; manages people.
    Owner,
    /// Can change it.
    Editor,
    /// Can look and export.
    Viewer,
}

impl From<Role> for SharedRole {
    fn from(r: Role) -> Self {
        match r {
            Role::Owner => Self::Owner,
            Role::Editor => Self::Editor,
            Role::Viewer => Self::Viewer,
        }
    }
}

/// Where this device is with a shared notebook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SharedStateView {
    /// Being downloaded and checked; not shown yet.
    Joining,
    /// Synced.
    Active,
    /// Access was removed; the copy here is kept, read-only.
    AccessRemoved,
}

impl From<SharedState> for SharedStateView {
    fn from(s: SharedState) -> Self {
        match s {
            SharedState::Joining => Self::Joining,
            SharedState::Active => Self::Active,
            SharedState::AccessRemoved => Self::AccessRemoved,
        }
    }
}

/// How far this device's copy has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SharedSyncState {
    /// Saved here, not uploaded.
    LocalSaved,
    /// Changes waiting to upload.
    WaitingToUpload,
    /// Up to date.
    Synced,
    /// Access was removed.
    AccessRemoved,
    /// A conflict to resolve.
    NeedsResolution,
}

impl From<NotebookSyncState> for SharedSyncState {
    fn from(s: NotebookSyncState) -> Self {
        match s {
            NotebookSyncState::LocalSaved => Self::LocalSaved,
            NotebookSyncState::WaitingToUpload => Self::WaitingToUpload,
            NotebookSyncState::Synced => Self::Synced,
            NotebookSyncState::AccessRemoved => Self::AccessRemoved,
            NotebookSyncState::NeedsResolution => Self::NeedsResolution,
        }
    }
}

/// One shared notebook, from the owner's side or a member's.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SharedNotebookView {
    /// The space's id.
    pub space_id: String,
    /// Unknown while joining hasn't started on this device.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub notebook_id: Option<Uuid>,
    /// Its title, as far as this device can read it.
    pub title: String,
    /// This account's role.
    pub role: SharedRole,
    /// Where this device is with it.
    pub state: SharedStateView,
    /// Whether this account owns it.
    pub owner: bool,
    /// The owner's verified email, when known (from the invitation).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub owner_display: Option<String>,
    /// How far this device's copy has got.
    pub sync_state: SharedSyncState,
}

/// An invitation waiting for this account. No title: that is sealed until accepted.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct IncomingInvitationView {
    /// Pass back to accept or decline.
    pub invitation_id: String,
    /// The space it is for.
    pub space_id: String,
    /// The role on accepting.
    pub role: SharedRole,
    /// The owner's verified email.
    pub invited_by: String,
    /// When it expires (Unix ms).
    #[ts(type = "number")]
    pub expires_at_ms: u64,
}

/// Everything the library needs to show shared notebooks.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SharedView {
    /// Shared notebooks this account owns or belongs to.
    pub notebooks: Vec<SharedNotebookView>,
    /// Invitations waiting for it.
    pub invitations: Vec<IncomingInvitationView>,
    /// Sync is set up and the server supports sharing (`sharing.v1`).
    pub available: bool,
    /// The server wants a fresh sign-in before it shows invitations (`invitations` is empty
    /// then); shared notebooks keep syncing.
    pub invitations_need_sign_in: bool,
}

/// A person with access to a shared notebook.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MemberView {
    /// Their account id.
    pub account_id: String,
    /// Their role.
    pub role: SharedRole,
    /// `active` or `revoked`.
    pub state: String,
    /// Their verified email (only members of this notebook see it).
    pub display: String,
    /// Whether this is the caller.
    pub is_you: bool,
    /// Passed back when changing the role, so a stale change is refused.
    #[ts(type = "number")]
    pub version: u64,
    /// When they joined (Unix ms).
    #[ts(type = "number")]
    pub since_ms: u64,
}

/// An invitation's state.
#[derive(Debug, Clone, Copy, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum InvitationStateView {
    /// Waiting.
    Pending,
    /// Joined.
    Accepted,
    /// Said no.
    Declined,
    /// Withdrawn.
    Cancelled,
    /// Not answered in time.
    Expired,
}

/// An invitation the owner sent.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InvitationView {
    /// Its id.
    pub invitation_id: String,
    /// Who it was sent to.
    pub email: String,
    /// The role on accepting.
    pub role: SharedRole,
    /// Where it stands.
    pub state: InvitationStateView,
    /// When it expires (Unix ms).
    #[ts(type = "number")]
    pub expires_at_ms: u64,
    /// Increases on every change.
    #[ts(type = "number")]
    pub version: u64,
}

/// The people on a notebook and, for its owner, the invitations.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MembersView {
    /// Members, the owner first.
    pub members: Vec<MemberView>,
    /// Invitations (owner only).
    pub invitations: Vec<InvitationView>,
    /// The caller's own role.
    pub you: SharedRole,
}

/// Which kind of notebook it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum NotebookKind {
    /// An ordinary notebook.
    Personal,
    /// In a vault.
    Vault,
    /// Shared.
    Shared,
}

/// Why a notebook can't be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ReadOnlyReason {
    /// The role is viewer.
    Viewer,
    /// Access was removed.
    AccessRemoved,
}

/// What the editor may do with a notebook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NotebookAccess {
    /// Personal, vault or shared.
    pub kind: NotebookKind,
    /// This account's role, for a shared notebook.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<SharedRole>,
    /// Whether changes are refused.
    pub read_only: bool,
    /// Why, when they are.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<ReadOnlyReason>,
}

fn access_view(a: &Access) -> NotebookAccess {
    let reason = match a.read_only_reason() {
        Some("viewer") => Some(ReadOnlyReason::Viewer),
        Some(_) => Some(ReadOnlyReason::AccessRemoved),
        None => None,
    };
    match a {
        Access::Personal => NotebookAccess {
            kind: NotebookKind::Personal,
            role: None,
            read_only: false,
            reason: None,
        },
        Access::Vault => NotebookAccess {
            kind: NotebookKind::Vault,
            role: None,
            read_only: false,
            reason: None,
        },
        Access::Shared { role, .. } => NotebookAccess {
            kind: NotebookKind::Shared,
            role: Some((*role).into()),
            read_only: reason.is_some(),
            reason,
        },
    }
}

fn cmd_err(m: impl Into<String>) -> CmdError {
    CmdError {
        code: ErrorCode::Invalid,
        message: m.into(),
    }
}

/// The notebook owners file, read again only when it was saved here or its size or modification
/// time changed, so
/// the check before every save (each pen-up) doesn't parse it each time.
fn owners_cached(root: &Path) -> std::io::Result<NotebookOwners> {
    type Stamp = Option<(u64, std::time::SystemTime, u64)>;
    type Cache = Option<(PathBuf, Stamp, NotebookOwners)>;
    static CACHE: Mutex<Cache> = Mutex::new(None);
    // saves from this process, then size and time for changes from elsewhere
    let stamp = std::fs::metadata(root.join("vault-notebooks.json"))
        .ok()
        .map(|m| {
            (
                NotebookOwners::save_count(),
                m.modified().unwrap_or(std::time::UNIX_EPOCH),
                m.len(),
            )
        });
    let mut c = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((r, st, o)) = c.as_ref() {
        // an unknown time (no metadata) is never trusted
        if r == root && *st == stamp && stamp.is_some() {
            return Ok(o.clone());
        }
    }
    let o = NotebookOwners::load(root)?;
    *c = Some((root.to_path_buf(), stamp, o.clone()));
    Ok(o)
}

/// What the read-only rule says about notebook `nb`: `Err` only when the owners file positively
/// records it as one this account may not change. A damaged owners file doesn't block local
/// saves: every sync engine refuses to run with it (so nothing is uploaded), and the warning goes
/// to the log and the library's recovery report.
fn read_only(lib: &Library, nb: Uuid) -> Option<Access> {
    match owners_cached(lib.root()) {
        Ok(o) => o.shared_entry(&nb).map(|e| Access::Shared {
            role: e.role,
            state: e.state,
        }),
        Err(e) => {
            eprintln!("penroam: the notebook owners file can't be read ({e}); sync is stopped, local edits continue");
            None
        }
    }
}

/// The check every command that changes notebook `nb` makes first, with the library lock held:
/// refuses a notebook shared with this account to view only, and one whose access was removed.
pub fn require_writable(lib: &Library, nb: Uuid) -> Result<(), CmdError> {
    match read_only(lib, nb)
        .as_ref()
        .and_then(Access::read_only_reason)
    {
        None => Ok(()),
        Some("viewer") => Err(cmd_err(VIEWER_REFUSAL)),
        Some(_) => Err(cmd_err(
            "your access to this shared notebook was removed; keep a private copy to go on editing",
        )),
    }
}

/// [`require_writable`] for page edits (`apply_page_operations`): refuses a viewer's notebook,
/// but lets edits reach a notebook whose access was removed. Its space's engine is stopped, so
/// they stay on this device (and in a private copy); refusing them would lose batches the editor
/// had in flight when access was removed. The UI accepts no new input there.
pub fn require_page_writable(lib: &Library, nb: Uuid) -> Result<(), CmdError> {
    match read_only(lib, nb)
        .as_ref()
        .and_then(Access::read_only_reason)
    {
        Some("viewer") => Err(cmd_err(VIEWER_REFUSAL)),
        _ => Ok(()),
    }
}

const VIEWER_REFUSAL: &str =
    "this notebook is shared with you to view only; you can look and export, not edit";

/// Notebook ids imports must not reuse ([`NotebookOwners::reserved_import_ids`]). Call with the
/// library lock held, right before an import or a restore.
pub fn reserve_import_ids(lib: &mut Library) {
    if let Ok(o) = owners_cached(lib.root()) {
        lib.set_reserved_ids(o.reserved_import_ids());
    }
}

/// Refuses a shared notebook (for moving into a vault: vault notebooks can't be shared, and a
/// shared one can't become private this way; keep a private copy instead).
pub fn require_not_shared(lib: &Library, nb: Uuid) -> Result<(), CmdError> {
    match shared::notebook_access(lib.root(), nb).map_err(|e| cmd_err(e.to_string()))? {
        Access::Shared { .. } => Err(cmd_err(
            "a shared notebook can't go into a vault; make a private copy and move that",
        )),
        _ => Ok(()),
    }
}

/// What the editor may do with a notebook: its kind, role and whether it's read-only.
#[tauri::command(async)]
pub fn notebook_access(
    state: State<'_, AppState>,
    notebook_id: Uuid,
) -> Result<NotebookAccess, CmdError> {
    let lib = state
        .lib
        .lock()
        .map_err(|_| cmd_err("library lock poisoned"))?;
    let a = shared::notebook_access(lib.root(), notebook_id).map_err(|e| cmd_err(e.to_string()))?;
    Ok(access_view(&a))
}

/// The library's view of shared notebooks: what the server listed last, joined with what this
/// device has. Shared notebooks this device has but the server didn't list (offline, or not
/// refreshed yet) are included from the owners file.
pub fn shared_view(lib: &Library, spaces: &SpaceSet<HttpTransport>) -> SharedView {
    let owners = NotebookOwners::load(lib.root()).unwrap_or_default();
    let mut out = Vec::new();
    let title_of = |nb: Uuid, stored: &Option<String>| {
        lib.notebook(nb)
            .map(|n| n.title.clone())
            .ok()
            .or_else(|| stored.clone())
            .unwrap_or_else(|| "Shared notebook".into())
    };
    for rec in &spaces.records {
        match owners.space(rec.space_id.as_str()) {
            Some((nb, e)) => out.push(SharedNotebookView {
                space_id: e.space_id.clone(),
                notebook_id: Some(nb),
                title: title_of(nb, &e.title),
                role: rec.role.into(),
                state: e.state.into(),
                owner: rec.owner,
                owner_display: e.owner_display.clone(),
                sync_state: spaces.sync_state(lib, &owners, nb).into(),
            }),
            // revoked before this device ever joined: nothing of it here
            None if rec.state == MembershipState::Revoked => {}
            None => out.push(SharedNotebookView {
                space_id: rec.space_id.to_string(),
                notebook_id: None,
                title: "Shared notebook".into(),
                role: rec.role.into(),
                state: SharedStateView::Joining,
                owner: rec.owner,
                owner_display: None,
                sync_state: SharedSyncState::LocalSaved,
            }),
        }
    }
    for (nb, e) in &owners.shared {
        if out.iter().any(|v| v.space_id == e.space_id) {
            continue;
        }
        out.push(SharedNotebookView {
            space_id: e.space_id.clone(),
            notebook_id: Some(*nb),
            title: title_of(*nb, &e.title),
            role: e.role.into(),
            state: e.state.into(),
            owner: e.role == Role::Owner,
            owner_display: e.owner_display.clone(),
            sync_state: spaces.sync_state(lib, &owners, *nb).into(),
        });
    }
    SharedView {
        notebooks: out,
        invitations: spaces
            .invitations
            .iter()
            .map(|i| IncomingInvitationView {
                invitation_id: i.invitation_id.to_string(),
                space_id: i.space_id.to_string(),
                role: i.role.into(),
                invited_by: i.invited_by.clone(),
                expires_at_ms: i.expires_at_ms,
            })
            .collect(),
        available: spaces.available,
        invitations_need_sign_in: spaces.invitations_need_sign_in,
    }
}

/// [`shared_view`] for the worker, with the library lock taken briefly.
pub fn view_now(app: &AppHandle, spaces: &SpaceSet<HttpTransport>) -> SharedView {
    let state = app.state::<AppState>();
    let lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
    shared_view(&lib, spaces)
}

/// Requests for the sync worker. Each is answered on its reply channel.
pub enum SharedCmd {
    /// [`shared_list`].
    List(Reply<SharedView>),
    /// [`shared_share`].
    Share(Uuid, Reply<SharedNotebookView>),
    /// [`shared_members`].
    Members(String, Reply<MembersView>),
    /// [`shared_invite`].
    Invite(String, String, SharedRole, Reply<InvitationView>),
    /// [`shared_cancel_invitation`].
    CancelInvitation(String, String, Reply<()>),
    /// [`shared_change_role`].
    ChangeRole(String, String, SharedRole, u64, Reply<MemberView>),
    /// [`shared_remove_member`].
    RemoveMember(String, String, Reply<()>),
    /// [`shared_leave`].
    Leave(String, Reply<()>),
    /// [`shared_accept`].
    Accept(String, Reply<SharedNotebookView>),
    /// [`shared_decline`].
    Decline(String, Reply<()>),
    /// [`shared_remove_local`].
    RemoveLocal(String, Reply<()>),
}

fn grantable(r: SharedRole) -> Result<Role, String> {
    match r {
        SharedRole::Editor => Ok(Role::Editor),
        SharedRole::Viewer => Ok(Role::Viewer),
        SharedRole::Owner => Err("that role isn't available".into()),
    }
}

fn space_id(s: &str) -> Result<AccountId, String> {
    s.parse()
        .map_err(|_| "not a shared notebook id".to_string())
}

fn member_view(m: penroam_sync_protocol::MemberRecord) -> MemberView {
    MemberView {
        account_id: m.account_id.to_string(),
        role: m.role.into(),
        state: match m.state {
            MembershipState::Active => "active".into(),
            MembershipState::Revoked => "revoked".into(),
        },
        display: m.display,
        is_you: m.is_you,
        version: m.version,
        since_ms: m.since_ms,
    }
}

fn invitation_view(i: penroam_sync_protocol::InvitationRecord) -> InvitationView {
    InvitationView {
        invitation_id: i.invitation_id.to_string(),
        email: i.email,
        role: i.role.into(),
        state: match i.state {
            InvitationState::Pending => InvitationStateView::Pending,
            InvitationState::Accepted => InvitationStateView::Accepted,
            InvitationState::Declined => InvitationStateView::Declined,
            InvitationState::Cancelled => InvitationStateView::Cancelled,
            InvitationState::Expired => InvitationStateView::Expired,
        },
        expires_at_ms: i.expires_at_ms,
        version: i.version,
    }
}

/// An error for the UI. A server error's text already starts with its wire code
/// (`access_revoked: …`), which the frontend can match on.
fn sync_msg(e: penroam_sync::SyncError) -> String {
    e.to_string()
}

/// [`sync_msg`], with a plain request to sign in again when the server wants a recent sign-in
/// to match the verified email (`reauth_required`). The message keeps the code as its prefix.
fn reauth_msg(e: penroam_sync::SyncError, what: &str) -> String {
    match &e {
        penroam_sync::SyncError::Transport(t)
            if t.code() == Some(penroam_sync_protocol::ErrorCode::ReauthRequired) =>
        {
            format!("reauth_required: sign in again to {what}")
        }
        _ => sync_msg(e),
    }
}

fn view_for(
    app: &AppHandle,
    spaces: &SpaceSet<HttpTransport>,
    space: &str,
) -> Result<SharedNotebookView, String> {
    view_now(app, spaces)
        .notebooks
        .into_iter()
        .find(|v| v.space_id == space)
        .ok_or_else(|| "that shared notebook isn't listed".to_string())
}

/// Runs one [`SharedCmd`] on the worker thread. `access` gives space sessions and secret
/// stores. Returns whether the shared list may have changed (the worker then emits
/// `sync://shared`).
pub fn handle(
    app: &AppHandle,
    account: &mut Engine,
    spaces: &mut SpaceSet<HttpTransport>,
    access: &mut SpaceAccess<'_, HttpTransport>,
    cmd: SharedCmd,
) -> bool {
    let state = app.state::<AppState>();
    let lib = &state.lib;
    match cmd {
        SharedCmd::List(r) => {
            let _ = spaces.refresh(account, lib);
            let _ = r.send(Ok(view_now(app, spaces)));
            false
        }
        SharedCmd::Share(nb, r) => {
            let res = spaces
                .share(account, lib, access, nb)
                .map_err(sync_msg)
                .and_then(|space| {
                    // the space first, so other devices find the notebook there when the marker
                    // reaches them
                    if let Some(e) = spaces.engine(&space) {
                        let _ = e.reconcile(lib);
                    }
                    let _ = account.reconcile(lib);
                    let _ = spaces.refresh(account, lib);
                    view_for(app, spaces, &space)
                });
            let _ = r.send(res);
            true
        }
        SharedCmd::Members(space, r) => {
            let res = space_id(&space).and_then(|id| {
                let l = account
                    .transport_mut()
                    .shared_members(&id)
                    .map_err(|e| sync_msg(e.into()))?;
                let you = l
                    .members
                    .iter()
                    .find(|m| m.is_you)
                    .map(|m| m.role.into())
                    .unwrap_or(SharedRole::Viewer);
                Ok(MembersView {
                    members: l.members.into_iter().map(member_view).collect(),
                    invitations: l.invitations.into_iter().map(invitation_view).collect(),
                    you,
                })
            });
            let _ = r.send(res);
            false
        }
        SharedCmd::Invite(space, email, role, r) => {
            let res = space_id(&space).and_then(|id| {
                let req = CreateInvitationRequest {
                    email,
                    role: grantable(role)?,
                    idempotency_key: Uuid::new_v4().hyphenated().to_string(),
                };
                req.validate().map_err(|e| e.to_string())?;
                account
                    .transport_mut()
                    .shared_invite(&id, &req)
                    .map(invitation_view)
                    .map_err(|e| sync_msg(e.into()))
            });
            let _ = r.send(res);
            false
        }
        SharedCmd::CancelInvitation(_space, inv, r) => {
            let res = inv
                .parse::<InvitationId>()
                .map_err(|_| "not an invitation id".to_string())
                .and_then(|i| {
                    account
                        .transport_mut()
                        .invitation_cancel(&i)
                        .map_err(|e| sync_msg(e.into()))
                });
            let _ = r.send(res);
            false
        }
        SharedCmd::ChangeRole(space, member, role, version, r) => {
            let res = (|| {
                let id = space_id(&space)?;
                let m: AccountId = member
                    .parse()
                    .map_err(|_| "not an account id".to_string())?;
                account
                    .transport_mut()
                    .shared_change_role(
                        &id,
                        &m,
                        &ChangeRoleRequest {
                            role: grantable(role)?,
                            version,
                        },
                    )
                    .map(member_view)
                    .map_err(|e| sync_msg(e.into()))
            })();
            let _ = r.send(res);
            false
        }
        SharedCmd::RemoveMember(space, member, r) => {
            let res = (|| {
                let id = space_id(&space)?;
                let m: AccountId = member
                    .parse()
                    .map_err(|_| "not an account id".to_string())?;
                account
                    .transport_mut()
                    .shared_remove_member(&id, &m)
                    .map_err(|e| sync_msg(e.into()))?;
                // a new key epoch without their devices, now rather than at the next pass
                if let Some(e) = spaces.engine(&space) {
                    let _ = e.remove_revoked_devices();
                }
                Ok(())
            })();
            let _ = r.send(res);
            false
        }
        SharedCmd::Leave(space, r) => {
            let res = (|| {
                let id = space_id(&space)?;
                let me: AccountId = account
                    .account_id()
                    .parse()
                    .map_err(|_| "invalid account id".to_string())?;
                account
                    .transport_mut()
                    .shared_remove_member(&id, &me)
                    .map_err(|e| sync_msg(e.into()))?;
                // the copy here stays, read-only, like any access loss
                spaces.stop(&space, lib).map_err(sync_msg)?;
                let _ = spaces.refresh(account, lib);
                Ok(())
            })();
            let _ = r.send(res);
            true
        }
        SharedCmd::Accept(inv, r) => {
            let res = inv
                .parse::<InvitationId>()
                .map_err(|_| "not an invitation id".to_string())
                .and_then(|i| {
                    spaces
                        .accept(account, lib, access, &i)
                        .map_err(|e| reauth_msg(e, "accept this invitation"))
                })
                .and_then(|(rec, _, report)| {
                    crate::sync::announce(app, &report);
                    view_for(app, spaces, rec.space_id.as_str())
                });
            let _ = r.send(res);
            true
        }
        SharedCmd::Decline(inv, r) => {
            let res = inv
                .parse::<InvitationId>()
                .map_err(|_| "not an invitation id".to_string())
                .and_then(|i| {
                    account
                        .transport_mut()
                        .invitation_decline(&i)
                        .map_err(|e| reauth_msg(e.into(), "decline this invitation"))
                });
            spaces
                .invitations
                .retain(|i| i.invitation_id.as_str() != inv);
            let _ = r.send(res);
            true
        }
        SharedCmd::RemoveLocal(space, r) => {
            spaces.take(&space);
            let res = (|| {
                let store = (access.secrets)().map_err(sync_msg)?;
                let mut l = lib.lock().unwrap_or_else(|p| p.into_inner());
                let nb = NotebookOwners::load(l.root())
                    .ok()
                    .and_then(|o| o.space(&space).map(|(n, _)| n));
                shared::remove_local(&mut l, store.as_ref(), &space).map_err(sync_msg)?;
                drop(l);
                if let Some(nb) = nb {
                    state.search.notebook_changed(nb);
                }
                Ok(())
            })();
            let _ = r.send(res);
            true
        }
    }
}

fn ask<T>(
    state: &State<'_, AppState>,
    make: impl FnOnce(Reply<T>) -> SharedCmd,
) -> Result<T, CmdError> {
    crate::sync::ask_shared(&state.sync, make)
}

/// Shared notebooks and invitations. Without sync, or on a server without `sharing.v1`,
/// `available` is false and the lists are what this device has.
#[tauri::command(async)]
pub fn shared_list(state: State<'_, AppState>) -> Result<SharedView, CmdError> {
    if !state.sync.running() {
        let lib = state
            .lib
            .lock()
            .map_err(|_| cmd_err("library lock poisoned"))?;
        let mut v = shared_view(&lib, &SpaceSet::new());
        v.available = false;
        return Ok(v);
    }
    ask(&state, SharedCmd::List)
}

/// Shares a notebook (owner): creates its space and moves its history there. Safe to repeat.
/// Earlier versions of the notebook become visible to everyone invited.
#[tauri::command(async)]
pub fn shared_share(
    state: State<'_, AppState>,
    notebook_id: Uuid,
) -> Result<SharedNotebookView, CmdError> {
    ask(&state, |r| SharedCmd::Share(notebook_id, r))
}

/// The people on a shared notebook (and, for its owner, the invitations).
#[tauri::command(async)]
pub fn shared_members(
    state: State<'_, AppState>,
    space_id: String,
) -> Result<MembersView, CmdError> {
    ask(&state, |r| SharedCmd::Members(space_id, r))
}

/// Invites someone by their verified email (owner).
#[tauri::command(async)]
pub fn shared_invite(
    state: State<'_, AppState>,
    space_id: String,
    email: String,
    role: SharedRole,
) -> Result<InvitationView, CmdError> {
    ask(&state, |r| SharedCmd::Invite(space_id, email, role, r))
}

/// Withdraws an invitation (owner).
#[tauri::command(async)]
pub fn shared_cancel_invitation(
    state: State<'_, AppState>,
    space_id: String,
    invitation_id: String,
) -> Result<(), CmdError> {
    ask(&state, |r| {
        SharedCmd::CancelInvitation(space_id, invitation_id, r)
    })
}

/// Changes a member's role (owner). `version` is the member's version as listed.
#[tauri::command(async)]
pub fn shared_change_role(
    state: State<'_, AppState>,
    space_id: String,
    account_id: String,
    role: SharedRole,
    version: u64,
) -> Result<MemberView, CmdError> {
    ask(&state, |r| {
        SharedCmd::ChangeRole(space_id, account_id, role, version, r)
    })
}

/// Removes a member (owner). They keep what they already downloaded.
#[tauri::command(async)]
pub fn shared_remove_member(
    state: State<'_, AppState>,
    space_id: String,
    account_id: String,
) -> Result<(), CmdError> {
    ask(&state, |r| SharedCmd::RemoveMember(space_id, account_id, r))
}

/// Leaves a shared notebook. The copy on this device stays, read-only.
#[tauri::command(async)]
pub fn shared_leave(state: State<'_, AppState>, space_id: String) -> Result<(), CmdError> {
    ask(&state, |r| SharedCmd::Leave(space_id, r))
}

/// Accepts an invitation. The notebook appears once it is downloaded and checked
/// (`state: "joining"` until then).
#[tauri::command(async)]
pub fn shared_accept(
    state: State<'_, AppState>,
    invitation_id: String,
) -> Result<SharedNotebookView, CmdError> {
    ask(&state, |r| SharedCmd::Accept(invitation_id, r))
}

/// Declines an invitation.
#[tauri::command(async)]
pub fn shared_decline(state: State<'_, AppState>, invitation_id: String) -> Result<(), CmdError> {
    ask(&state, |r| SharedCmd::Decline(invitation_id, r))
}

/// Copies a shared notebook as it is on this device, unsent edits included, into the personal
/// library under a new id.
#[tauri::command(async)]
pub fn shared_keep_private_copy(
    state: State<'_, AppState>,
    space_id: String,
) -> Result<Notebook, CmdError> {
    let copy = {
        let mut lib = state
            .lib
            .lock()
            .map_err(|_| cmd_err("library lock poisoned"))?;
        let owners = NotebookOwners::load(lib.root()).map_err(|e| cmd_err(e.to_string()))?;
        let (nb, _) = owners
            .space(&space_id)
            .ok_or_else(|| cmd_err("this shared notebook isn't on this device"))?;
        shared::keep_private_copy(&mut lib, nb).map_err(|e| cmd_err(e.to_string()))?
    };
    state.sync.nudge();
    state.search.notebook_changed(copy.id);
    Ok(copy)
}

/// Removes this device's copy of a shared notebook whose access was removed, with its sync data.
#[tauri::command(async)]
pub fn shared_remove_local(
    app: AppHandle,
    state: State<'_, AppState>,
    space_id: String,
) -> Result<(), CmdError> {
    if state.sync.running() {
        return ask(&state, |r| SharedCmd::RemoveLocal(space_id, r));
    }
    let store = {
        let root = state
            .lib
            .lock()
            .map_err(|_| cmd_err("library lock poisoned"))?
            .root()
            .to_path_buf();
        crate::sync::secrets(&app, &root)?
    };
    let mut lib = state
        .lib
        .lock()
        .map_err(|_| cmd_err("library lock poisoned"))?;
    shared::remove_local(&mut lib, store.as_ref(), &space_id).map_err(|e| cmd_err(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_serialize_as_the_frontend_expects() {
        let v = SharedNotebookView {
            space_id: "sp".into(),
            notebook_id: None,
            title: "T".into(),
            role: SharedRole::Viewer,
            state: SharedStateView::AccessRemoved,
            owner: false,
            owner_display: None,
            sync_state: SharedSyncState::WaitingToUpload,
        };
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(
            j,
            serde_json::json!({"spaceId":"sp","title":"T","role":"viewer","state":"accessRemoved",
                "owner":false,"syncState":"waitingToUpload"})
        );
        let a = access_view(&Access::Shared {
            role: Role::Viewer,
            state: SharedState::Active,
        });
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::json!({"kind":"shared","role":"viewer","readOnly":true,"reason":"viewer"})
        );
        assert_eq!(
            serde_json::to_value(access_view(&Access::Personal)).unwrap(),
            serde_json::json!({"kind":"personal","readOnly":false})
        );
        assert_eq!(
            serde_json::to_value(InvitationStateView::Cancelled).unwrap(),
            "cancelled"
        );
    }

    #[test]
    fn the_guard_refuses_viewers_and_removed_access_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut lib = Library::open(dir.path()).unwrap();
        let (nb, _) = lib
            .create_notebook("N", penroam_core::PageDefaults::default(), None)
            .unwrap();
        assert!(require_writable(&lib, nb.id).is_ok());
        let set = |role, st| {
            let mut o = NotebookOwners::load(dir.path()).unwrap();
            o.shared
                .insert(nb.id, shared::SharedEntry::new("sp", role, st, 0));
            o.save(dir.path()).unwrap();
        };
        set(Role::Editor, SharedState::Active);
        assert!(require_writable(&lib, nb.id).is_ok());
        assert!(require_not_shared(&lib, nb.id).is_err());
        set(Role::Viewer, SharedState::Active);
        assert!(require_writable(&lib, nb.id)
            .unwrap_err()
            .message
            .contains("view only"));
        set(Role::Owner, SharedState::AccessRemoved);
        assert!(require_writable(&lib, nb.id)
            .unwrap_err()
            .message
            .contains("removed"));
        // page edits still reach a notebook whose access was removed, never a viewer's
        assert!(require_page_writable(&lib, nb.id).is_ok());
        set(Role::Viewer, SharedState::Active);
        assert!(require_page_writable(&lib, nb.id).is_err());
        // a damaged owners file doesn't block local saves (sync stops on it instead)
        std::fs::write(dir.path().join("vault-notebooks.json"), b"{not json").unwrap();
        assert!(require_writable(&lib, nb.id).is_ok());
        assert!(require_page_writable(&lib, nb.id).is_ok());
    }
}
