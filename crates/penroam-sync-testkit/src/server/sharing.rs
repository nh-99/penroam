//! Shared notebooks in the development test server (`sharing.v1`, protocol 1.7).
//!
//! A sharing space is an [`Account`] with [`Account::space`] set. It has the ordinary operation
//! log, key log, blobs and checkpoints, plus a member list. The routes here (`/v1/shared-notebooks*`
//! and `/v1/invitations*`) run as the caller's **own** account; vault and space sessions are refused
//! with `forbidden`. Every one of them runs under the server's single lock, so accept, decline,
//! cancel, role changes and removals are atomic with respect to each other and to sync requests on
//! the space.
//!
//! Rules modelled (see `docs/protocol/sync-protocol-v1.md` §12f):
//!
//! * A caller who is not a member of a space gets `not_found` for it, never `forbidden`, so space
//!   ids cannot be probed. A removed member gets `access_revoked` on every space route.
//! * Invitations match the invitee's verified email (set by the test sign-in). Creating one
//!   answers the same whether or not the email belongs to an account; the server never looks.
//! * Invitations expire [`INVITATION_TTL_MS`] after creation, measured on the injectable test clock
//!   ([`InMemoryServer::set_clock_ms`]).
//! * Removing a member revokes their devices in the space too; the owner's client then appends
//!   `removeDevice` to the space's key log.
//! * Space storage counts towards the owner's quota ([`InMemoryServer::quota_group`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use penroam_sync_protocol::sharing::{
    ChangeRoleRequest, CreateInvitationRequest, CreateSpaceRequest, IncomingInvitation,
    IncomingInvitationList, InvitationRecord, InvitationState, MemberList, MemberRecord,
    MembershipState, Role, SharedNotebookList, SharedNotebookRecord, SharingError,
    UpdateSpaceMetaRequest, INVITATION_TTL_MS,
};
use penroam_sync_protocol::{
    base64url, AccountId, AccountKeyDocument, DeviceId, ErrorBody, ErrorCode, InvitationId,
};

use super::{Account, InMemoryServer, RequestError, State};

/// Most pending invitations one space may have at a time (the test server's cap).
pub const MAX_PENDING_INVITATIONS: usize = 50;
/// Default for [`InMemoryServer::set_member_device_cap`].
pub const DEFAULT_MEMBER_DEVICE_CAP: usize = 10;

/// One member of a space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Member {
    account: AccountId,
    role: Role,
    state: MembershipState,
    version: u64,
    since_ms: u64,
}

/// The sharing part of a space's account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Space {
    owner: AccountId,
    /// The owner first, then in joining order. Removed members stay, as `revoked`.
    members: Vec<Member>,
    sealed_meta: String,
    version: u64,
    created_at_ms: u64,
}

impl Space {
    fn member(&self, account: &AccountId) -> Option<&Member> {
        self.members.iter().find(|m| &m.account == account)
    }

    fn member_mut(&mut self, account: &AccountId) -> Option<&mut Member> {
        self.members.iter_mut().find(|m| &m.account == account)
    }

    /// Works out which member makes a sync request on this space and with what role.
    ///
    /// The member comes from the session (`member`) or from the device's registrant. Non-members
    /// get `not_found`, removed members `access_revoked`, and only then is a device registered by
    /// another member `forbidden`.
    pub(super) fn resolve(
        &self,
        acct: &Account,
        member: Option<&AccountId>,
        device: Option<&DeviceId>,
    ) -> Result<(Role, AccountId), (ErrorCode, &'static str)> {
        let from_device = device
            .and_then(|d| acct.devices.get(d))
            .and_then(|d| d.registered_by.as_ref());
        let Some(who) = member.or(from_device) else {
            return Err((
                ErrorCode::Forbidden,
                "shared-notebook requests need a member session",
            ));
        };
        // Membership first, so a non-member learns nothing about the space's devices.
        let role = match self.member(who) {
            None => return Err((ErrorCode::NotFound, "unknown shared notebook")),
            Some(m) if m.state == MembershipState::Revoked => {
                return Err((
                    ErrorCode::AccessRevoked,
                    "access to this shared notebook was removed",
                ))
            }
            Some(m) => m.role,
        };
        if from_device.is_some_and(|d| d != who) {
            return Err((ErrorCode::Forbidden, "device registered by another member"));
        }
        Ok((role, who.clone()))
    }

    fn record(&self, id: &AccountId, m: &Member) -> SharedNotebookRecord {
        SharedNotebookRecord {
            space_id: id.clone(),
            role: m.role,
            state: m.state,
            owner: m.role == Role::Owner,
            sealed_meta: self.sealed_meta.clone(),
            version: self.version,
            created_at_ms: self.created_at_ms,
        }
    }
}

/// A stored invitation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Invitation {
    id: InvitationId,
    space: AccountId,
    /// Normalized email.
    email: String,
    role: Role,
    state: InvitationState,
    created_at_ms: u64,
    expires_at_ms: u64,
    version: u64,
    idempotency_key: String,
    accepted_by: Option<AccountId>,
}

impl Invitation {
    fn record(&self) -> InvitationRecord {
        InvitationRecord {
            invitation_id: self.id.clone(),
            email: self.email.clone(),
            role: self.role,
            state: self.state,
            expires_at_ms: self.expires_at_ms,
            version: self.version,
        }
    }
}

/// The `entry` object of a stored or submitted key-log entry (protocol §14): base64url of
/// `{"body": base64url(body_json), …}`, whose body holds `entry`. `None` if unreadable.
fn keylog_entry_kind(data: &str) -> Option<serde_json::Value> {
    let entry: serde_json::Value = serde_json::from_slice(&base64url::decode(data).ok()?).ok()?;
    let body = base64url::decode(entry.get("body")?.as_str()?).ok()?;
    let mut body: serde_json::Value = serde_json::from_slice(&body).ok()?;
    Some(body.get_mut("entry")?.take())
}

/// Device ids the stored key log currently treats as active: the genesis device and every
/// `addDevice`, minus every `removeDevice`, in log order. Entries the server can't read are
/// skipped (clients verify the log; this only decides which grants a removal must carry).
fn keylog_active_devices(acct: &Account) -> std::collections::BTreeSet<String> {
    let mut active = std::collections::BTreeSet::new();
    for data in &acct.keylog {
        let Some(e) = keylog_entry_kind(data) else {
            continue;
        };
        match e["kind"].as_str() {
            Some("genesis") | Some("addDevice") => {
                if let Some(d) = e["device"]["deviceId"].as_str() {
                    active.insert(d.to_string());
                }
            }
            Some("removeDevice") => {
                if let Some(d) = e["deviceId"].as_str() {
                    active.remove(d);
                }
            }
            _ => {}
        }
    }
    active
}

/// Checks a key-log entry appended in a sharing space.
///
/// * A non-owner member may only append `addDevice`, `renameDevice` or `removeDevice` for a
///   device they registered in this space; anything else, including an unreadable entry, is
///   `forbidden`.
/// * Any `removeDevice` (the owner's too) must carry a grant of its `newEpoch` to every device
///   that stays active and to `"recovery"`; otherwise it is `validation_failed`. Without this a
///   member could start an epoch that only they (or nobody) can read, cutting everyone else off.
pub(super) fn check_space_keylog_entry(
    acct: &Account,
    data: &str,
) -> Result<(), (ErrorCode, &'static str)> {
    const REFUSED: &str =
        "members may only add, rename or remove their own devices in a shared notebook's key log";
    const INCOMPLETE: &str =
        "removeDevice must grant the new epoch to every remaining device and to recovery";
    let owner = acct.role().is_none_or(|r| r == Role::Owner);
    let kind = keylog_entry_kind(data);
    if !owner {
        let e = kind.as_ref().ok_or((ErrorCode::Forbidden, REFUSED))?;
        let device = match e["kind"].as_str() {
            Some("addDevice") => e["device"]["deviceId"].as_str(),
            Some("renameDevice") | Some("removeDevice") => e["deviceId"].as_str(),
            _ => None,
        }
        .ok_or((ErrorCode::Forbidden, REFUSED))?;
        let device: DeviceId = device
            .parse()
            .map_err(|_| (ErrorCode::Forbidden, REFUSED))?;
        let mine = acct
            .devices
            .get(&device)
            .is_some_and(|d| d.registered_by.is_some() && d.registered_by == acct.ctx.member);
        if !mine {
            return Err((ErrorCode::Forbidden, REFUSED));
        }
    }
    let Some(e) = kind.filter(|e| e["kind"] == "removeDevice") else {
        return Ok(());
    };
    let (Some(removed), Some(epoch)) = (e["deviceId"].as_str(), e["newEpoch"].as_u64()) else {
        return Err((ErrorCode::ValidationFailed, INCOMPLETE));
    };
    let granted: std::collections::BTreeSet<&str> = e["grants"]
        .as_array()
        .map(|g| {
            g.iter()
                .filter(|g| g["epoch"].as_u64() == Some(epoch))
                .filter_map(|g| g["to"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let mut needed = keylog_active_devices(acct);
    needed.remove(removed);
    needed.insert("recovery".to_string());
    if needed.iter().all(|d| granted.contains(d.as_str())) {
        Ok(())
    } else {
        Err((ErrorCode::ValidationFailed, INCOMPLETE))
    }
}

type ErrFn<'a> = &'a mut dyn FnMut(ErrorCode, &str) -> ErrorBody;

/// The caller's active membership of `space`: `not_found` for non-members (and unknown spaces),
/// `access_revoked` for removed members.
fn active<'s>(
    st: &'s State,
    caller: &AccountId,
    space: &AccountId,
    err: ErrFn<'_>,
) -> Result<(&'s Space, &'s Member), ErrorBody> {
    let s = st
        .accounts
        .get(space)
        .and_then(|a| a.space.as_ref())
        .ok_or_else(|| err(ErrorCode::NotFound, "unknown shared notebook"))?;
    match s.member(caller) {
        None => Err(err(ErrorCode::NotFound, "unknown shared notebook")),
        Some(m) if m.state == MembershipState::Revoked => Err(err(
            ErrorCode::AccessRevoked,
            "access to this shared notebook was removed",
        )),
        Some(m) => Ok((s, m)),
    }
}

/// Like [`active`], and the caller must own the space (`forbidden` otherwise).
fn owned<'s>(
    st: &'s State,
    caller: &AccountId,
    space: &AccountId,
    err: ErrFn<'_>,
) -> Result<&'s Space, ErrorBody> {
    let (s, m) = active(st, caller, space, err)?;
    if m.role != Role::Owner {
        return Err(err(ErrorCode::Forbidden, "only the owner may do this"));
    }
    Ok(s)
}

fn space_mut<'s>(st: &'s mut State, space: &AccountId) -> &'s mut Space {
    st.accounts
        .get_mut(space)
        .and_then(|a| a.space.as_mut())
        .expect("checked by the caller")
}

/// Invitations are addressed by email, so answering them needs an email verified by a sign-in
/// within the re-authentication window (when one is set): `reauth_required` otherwise. Listing
/// uses the same test but answers an empty list with the `reauth_required` flag instead.
/// Callers run this only after matching the caller's own email, so it reveals nothing to others.
fn fresh_email(st: &State, caller: &AccountId, now: u64, err: ErrFn<'_>) -> Result<(), ErrorBody> {
    let Some(window) = st.reauth_window_ms else {
        return Ok(());
    };
    let at = st.email_verified_at.get(caller).copied().unwrap_or(0);
    if now.saturating_sub(at) > window {
        return Err(err(
            ErrorCode::ReauthRequired,
            "sign in again to confirm your email",
        ));
    }
    Ok(())
}

fn sharing_err(e: SharingError, err: ErrFn<'_>) -> ErrorBody {
    err(e.error_code(), &e.to_string())
}

fn now_ms(st: &State) -> u64 {
    st.clock_ms.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    })
}

impl InMemoryServer {
    /// `(quota, bytes stored by the rest of the group)` for `account`. The group is the owner
    /// plus every sharing space it owns, and the owner's quota applies to all of it.
    pub(super) fn quota_group(
        accounts: &BTreeMap<AccountId, Account>,
        account: &AccountId,
    ) -> (Option<u64>, u64) {
        let Some(acct) = accounts.get(account) else {
            return (None, 0);
        };
        let root_id = acct.space.as_ref().map_or(account, |s| &s.owner);
        let Some(root) = accounts.get(root_id) else {
            return (acct.quota_bytes, 0);
        };
        let elsewhere = std::iter::once(root_id)
            .chain(root.owned_spaces.iter())
            .filter(|id| *id != account)
            .filter_map(|id| accounts.get(id))
            .map(|a| a.op_bytes + a.blob_bytes)
            .sum();
        (root.quota_bytes, elsewhere)
    }

    // ---- administration ----

    /// Records `email` as `account`'s verified sign-in email (normalized with
    /// [`penroam_sync_protocol::sharing::normalize_email`]); invitations to it then reach this
    /// account. Creates the account if needed. `None` forgets the email.
    pub fn set_verified_email(
        &self,
        account: &AccountId,
        email: Option<&str>,
    ) -> Result<(), SharingError> {
        let email = email
            .map(penroam_sync_protocol::sharing::normalize_email)
            .transpose()?;
        let mut s = self.lock();
        s.accounts.entry(account.clone()).or_default();
        let now = now_ms(&s);
        match email {
            Some(e) => {
                s.emails.insert(account.clone(), e);
                s.email_verified_at.insert(account.clone(), now);
            }
            None => {
                s.emails.remove(account);
                s.email_verified_at.remove(account);
            }
        };
        self.save_admin(&s);
        Ok(())
    }

    /// Sets when `account`'s email was last verified by a sign-in (Unix milliseconds on the test
    /// clock). [`Self::set_verified_email`] sets it to "now".
    pub fn set_email_verified_at_ms(&self, account: &AccountId, at_ms: u64) {
        let mut s = self.lock();
        s.email_verified_at.insert(account.clone(), at_ms);
        self.save_admin(&s);
    }

    /// Most devices one member may have registered (and not revoked) in one space; registering
    /// another is `conflict`. Defaults to 10. Keeps one member from
    /// bloating the space's device list and key log that everyone has to process.
    pub fn set_member_device_cap(&self, cap: usize) {
        self.lock().member_device_cap = cap;
    }

    /// How recent the invitee's email verification must be for accepting or declining invitations
    /// (older ones get `reauth_required`) and for listing them (older ones get an empty list
    /// flagged `reauth_required`). `None` (the default) disables the check.
    pub fn set_reauth_window_ms(&self, window_ms: Option<u64>) {
        self.lock().reauth_window_ms = window_ms;
    }

    /// Pins the test clock to `now_ms` (Unix milliseconds); `None` returns to the system clock.
    /// Only invitation timestamps and expiry use it.
    pub fn set_clock_ms(&self, now_ms: Option<u64>) {
        self.lock().clock_ms = now_ms;
    }

    /// Moves the test clock forward by `ms` (pinning it first if it follows the system clock).
    pub fn advance_clock_ms(&self, ms: u64) {
        let mut s = self.lock();
        let now = now_ms(&s);
        s.clock_ms = Some(now.saturating_add(ms));
    }

    /// The member who registered `device` in a space, if any (tests and diagnostics).
    pub fn device_registered_by(&self, space: &AccountId, device: &DeviceId) -> Option<AccountId> {
        self.lock()
            .accounts
            .get(space)
            .and_then(|a| a.devices.get(device))
            .and_then(|d| d.registered_by.clone())
    }

    /// Whether `device` exists in `account` and is revoked (tests and diagnostics).
    pub fn device_revoked(&self, account: &AccountId, device: &DeviceId) -> bool {
        self.lock()
            .accounts
            .get(account)
            .and_then(|a| a.devices.get(device))
            .is_some_and(|d| d.revoked)
    }

    // ---- request plumbing ----

    /// Runs one sharing request as `caller`'s own account: request id, injected faults, a check
    /// that the caller exists and is neither a vault nor a space session, lazy invitation expiry,
    /// persistence of successful mutations, and response dropping.
    fn sharing_request<T>(
        &self,
        caller: &AccountId,
        mutates: bool,
        f: impl FnOnce(&mut State, u64, ErrFn<'_>) -> Result<T, ErrorBody>,
    ) -> Result<T, RequestError> {
        let mut guard = self.lock();
        let st: &mut State = &mut guard;
        st.request_counter += 1;
        let request_id = format!("req-{}", st.request_counter);
        let mut err = |code: ErrorCode, message: &str| {
            let mut b = ErrorBody::new(code, message, request_id.clone());
            if code == ErrorCode::RateLimited || code == ErrorCode::ServiceUnavailable {
                b.retry_after_ms = Some(1000);
            }
            b
        };
        if let Some(code) = st.faults.fail_next.pop_front() {
            return Err(RequestError::Api(err(code, "injected failure")));
        }
        match st.accounts.get(caller) {
            None => {
                return Err(RequestError::Api(err(
                    ErrorCode::Unauthenticated,
                    "unknown account",
                )))
            }
            Some(a) if a.vault_of.is_some() || a.space.is_some() => {
                return Err(RequestError::Api(err(
                    ErrorCode::Forbidden,
                    "use your own account's session, not a vault or shared notebook session",
                )))
            }
            Some(_) => {}
        }
        let now = now_ms(st);
        let mut expired = false;
        for inv in st.invitations.iter_mut() {
            if inv.state == InvitationState::Pending && now >= inv.expires_at_ms {
                inv.state = InvitationState::Expired;
                inv.version += 1;
                expired = true;
            }
        }
        let result = f(st, now, &mut err).map_err(RequestError::Api);
        if (mutates && result.is_ok()) || expired {
            if let Err(e) = self.save(st) {
                return Err(RequestError::Api(err(
                    ErrorCode::InternalError,
                    &format!("could not persist: {e}"),
                )));
            }
        }
        if st.faults.drop_next_responses > 0 {
            st.faults.drop_next_responses -= 1;
            return Err(RequestError::ResponseLost);
        }
        result
    }

    // ---- routes ----

    /// `POST /v1/shared-notebooks`: creates a space owned by `caller`. The same
    /// `idempotency_key` returns the space created the first time.
    pub fn shared_create(
        &self,
        caller: &AccountId,
        req: &CreateSpaceRequest,
    ) -> Result<SharedNotebookRecord, RequestError> {
        self.sharing_request(caller, true, |st, now, err| {
            req.validate().map_err(|e| sharing_err(e, err))?;
            let owner = st.accounts.get(caller).expect("checked");
            if let Some(id) = owner.space_creates.get(&req.idempotency_key) {
                let s = st.accounts[id].space.as_ref().expect("a space");
                let m = s.member(caller).expect("the owner is a member");
                return Ok(s.record(id, m));
            }
            let id: AccountId = format!("s-{}", uuid::Uuid::new_v4().simple())
                .parse()
                .expect("hex digits with a prefix are a valid id");
            let owner_member = Member {
                account: caller.clone(),
                role: Role::Owner,
                state: MembershipState::Active,
                version: 1,
                since_ms: now,
            };
            let space = Space {
                owner: caller.clone(),
                members: vec![owner_member.clone()],
                sealed_meta: req.sealed_meta.clone(),
                version: 1,
                created_at_ms: now,
            };
            let record = space.record(&id, &owner_member);
            st.accounts.insert(
                id.clone(),
                Account {
                    space: Some(space),
                    ..Account::default()
                },
            );
            let owner = st.accounts.get_mut(caller).expect("checked");
            owner.owned_spaces.push(id.clone());
            owner.space_creates.insert(req.idempotency_key.clone(), id);
            Ok(record)
        })
    }

    /// `GET /v1/shared-notebooks`: spaces `caller` owns or belongs to, including revoked
    /// memberships (once each), oldest first.
    pub fn shared_list(&self, caller: &AccountId) -> Result<SharedNotebookList, RequestError> {
        self.sharing_request(caller, false, |st, _, _| {
            let mut spaces: Vec<SharedNotebookRecord> = st
                .accounts
                .iter()
                .filter_map(|(id, a)| {
                    let s = a.space.as_ref()?;
                    Some(s.record(id, s.member(caller)?))
                })
                .collect();
            spaces.sort_by(|a, b| {
                (a.created_at_ms, &a.space_id).cmp(&(b.created_at_ms, &b.space_id))
            });
            Ok(SharedNotebookList { spaces })
        })
    }

    /// `PUT /v1/shared-notebooks/{id}/meta`: owner only (the sealed meta names the notebook id).
    pub fn shared_set_meta(
        &self,
        caller: &AccountId,
        space: &AccountId,
        req: &UpdateSpaceMetaRequest,
    ) -> Result<(), RequestError> {
        self.sharing_request(caller, true, |st, _, err| {
            // Owner only: the sealed meta names the notebook id, and an editor could point it
            // somewhere else.
            owned(st, caller, space, err)?;
            req.validate().map_err(|e| sharing_err(e, err))?;
            let s = space_mut(st, space);
            s.sealed_meta = req.sealed_meta.clone();
            s.version += 1;
            Ok(())
        })
    }

    /// `POST /v1/shared-notebooks/{id}/session`: checks that `caller` is an active member and
    /// returns their role. The HTTP layer then issues a session on the space for that member;
    /// in-process callers use [`super::Caller::member`].
    pub fn shared_session_check(
        &self,
        caller: &AccountId,
        space: &AccountId,
    ) -> Result<Role, RequestError> {
        self.sharing_request(caller, false, |st, _, err| {
            Ok(active(st, caller, space, err)?.1.role)
        })
    }

    /// `GET /v1/shared-notebooks/{id}/key`: active members; `not_found` until the owner stores it.
    pub fn shared_key_get(
        &self,
        caller: &AccountId,
        space: &AccountId,
    ) -> Result<AccountKeyDocument, RequestError> {
        self.sharing_request(caller, false, |st, _, err| {
            active(st, caller, space, err)?;
            st.accounts[space]
                .root_key
                .clone()
                .map(|key| AccountKeyDocument { key })
                .ok_or_else(|| err(ErrorCode::NotFound, "no key stored yet"))
        })
    }

    /// `PUT /v1/shared-notebooks/{id}/key`: the owner stores the 32-byte space root once;
    /// a second store is `conflict`.
    pub fn shared_key_put(
        &self,
        caller: &AccountId,
        space: &AccountId,
        doc: &AccountKeyDocument,
    ) -> Result<(), RequestError> {
        self.sharing_request(caller, true, |st, _, err| {
            owned(st, caller, space, err)?;
            if base64url::decode(&doc.key).map_or(true, |k| k.len() != 32) {
                return Err(err(
                    ErrorCode::ValidationFailed,
                    "the key must be 32 bytes of base64url",
                ));
            }
            let a = st.accounts.get_mut(space).expect("checked");
            if a.root_key.is_some() {
                return Err(err(ErrorCode::Conflict, "a key is already stored"));
            }
            a.root_key = Some(doc.key.clone());
            Ok(())
        })
    }

    /// `GET /v1/shared-notebooks/{id}/members`: active members; invitations only for the owner.
    pub fn shared_members(
        &self,
        caller: &AccountId,
        space: &AccountId,
    ) -> Result<MemberList, RequestError> {
        self.sharing_request(caller, false, |st, _, err| {
            let (s, me) = active(st, caller, space, err)?;
            let members = s
                .members
                .iter()
                .map(|m| MemberRecord {
                    account_id: m.account.clone(),
                    role: m.role,
                    state: m.state,
                    display: st.emails.get(&m.account).cloned().unwrap_or_default(),
                    is_you: &m.account == caller,
                    version: m.version,
                    since_ms: m.since_ms,
                })
                .collect();
            let invitations = if me.role == Role::Owner {
                st.invitations
                    .iter()
                    .filter(|i| &i.space == space)
                    .map(Invitation::record)
                    .collect()
            } else {
                Vec::new()
            };
            Ok(MemberList {
                members,
                invitations,
            })
        })
    }

    /// `POST /v1/shared-notebooks/{id}/invitations`: owner only. Never consults whether the email
    /// belongs to an account. The same `idempotency_key` returns the same invitation (a different
    /// email or role under that key is `conflict`); a second pending invitation for one email, or
    /// inviting a current member, is `conflict`.
    pub fn shared_invite(
        &self,
        caller: &AccountId,
        space: &AccountId,
        req: &CreateInvitationRequest,
    ) -> Result<InvitationRecord, RequestError> {
        self.sharing_request(caller, true, |st, now, err| {
            let s = owned(st, caller, space, err)?;
            let email = req.validate().map_err(|e| sharing_err(e, err))?;
            if let Some(inv) = st
                .invitations
                .iter()
                .find(|i| &i.space == space && i.idempotency_key == req.idempotency_key)
            {
                return if inv.email == email && inv.role == req.role {
                    Ok(inv.record())
                } else {
                    Err(err(
                        ErrorCode::Conflict,
                        "idempotency key reused for a different invitation",
                    ))
                };
            }
            let is_member = s.members.iter().any(|m| {
                m.state == MembershipState::Active
                    && st.emails.get(&m.account).is_some_and(|e| *e == email)
            });
            if is_member {
                return Err(err(ErrorCode::Conflict, "already a member"));
            }
            let pending: Vec<&Invitation> = st
                .invitations
                .iter()
                .filter(|i| &i.space == space && i.state == InvitationState::Pending)
                .collect();
            if pending.iter().any(|i| i.email == email) {
                return Err(err(ErrorCode::Conflict, "already invited"));
            }
            if pending.len() >= MAX_PENDING_INVITATIONS {
                return Err(err(ErrorCode::RateLimited, "too many pending invitations"));
            }
            let inv = Invitation {
                id: format!("inv-{}", uuid::Uuid::new_v4().simple())
                    .parse()
                    .expect("hex digits with a prefix are a valid id"),
                space: space.clone(),
                email,
                role: req.role,
                state: InvitationState::Pending,
                created_at_ms: now,
                expires_at_ms: now.saturating_add(INVITATION_TTL_MS),
                version: 1,
                idempotency_key: req.idempotency_key.clone(),
                accepted_by: None,
            };
            let record = inv.record();
            st.invitations.push(inv);
            Ok(record)
        })
    }

    /// `PATCH /v1/shared-notebooks/{id}/members/{account_id}`: owner only; `version` must be the
    /// member's current version.
    pub fn shared_change_role(
        &self,
        caller: &AccountId,
        space: &AccountId,
        member: &AccountId,
        req: &ChangeRoleRequest,
    ) -> Result<MemberRecord, RequestError> {
        self.sharing_request(caller, true, |st, _, err| {
            let s = owned(st, caller, space, err)?;
            req.validate().map_err(|e| sharing_err(e, err))?;
            let target = s
                .member(member)
                .ok_or_else(|| err(ErrorCode::NotFound, "not a member"))?;
            if target.role == Role::Owner {
                return Err(err(ErrorCode::Forbidden, "the owner's role cannot change"));
            }
            if target.state == MembershipState::Revoked {
                return Err(err(ErrorCode::Conflict, "the member was removed"));
            }
            if target.version != req.version {
                return Err(err(ErrorCode::Conflict, "the member changed; reload"));
            }
            let display = st.emails.get(member).cloned().unwrap_or_default();
            let m = space_mut(st, space).member_mut(member).expect("checked");
            m.role = req.role;
            m.version += 1;
            Ok(MemberRecord {
                account_id: m.account.clone(),
                role: m.role,
                state: m.state,
                display,
                is_you: false,
                version: m.version,
                since_ms: m.since_ms,
            })
        })
    }

    /// `DELETE /v1/shared-notebooks/{id}/members/{account_id}`: the owner removes a member, or a
    /// member removes themselves (leaves). The owner cannot leave. The membership becomes
    /// `revoked` and the member's devices in the space are revoked. Removing an already removed
    /// member succeeds.
    pub fn shared_remove_member(
        &self,
        caller: &AccountId,
        space: &AccountId,
        member: &AccountId,
    ) -> Result<(), RequestError> {
        self.sharing_request(caller, true, |st, _, err| {
            let (s, me) = active(st, caller, space, err)?;
            if member == caller {
                if me.role == Role::Owner {
                    return Err(err(
                        ErrorCode::Forbidden,
                        "the owner cannot leave their own shared notebook",
                    ));
                }
            } else {
                if me.role != Role::Owner {
                    return Err(err(
                        ErrorCode::Forbidden,
                        "only the owner may remove members",
                    ));
                }
                match s.member(member) {
                    None => return Err(err(ErrorCode::NotFound, "not a member")),
                    Some(t) if t.state == MembershipState::Revoked => return Ok(()),
                    Some(_) => {}
                }
            }
            let a = st.accounts.get_mut(space).expect("checked");
            let m = a
                .space
                .as_mut()
                .and_then(|s| s.member_mut(member))
                .expect("checked");
            m.state = MembershipState::Revoked;
            m.version += 1;
            for d in a.devices.values_mut() {
                if d.registered_by.as_ref() == Some(member) {
                    d.revoked = true;
                }
            }
            Ok(())
        })
    }

    /// `GET /v1/invitations`: pending, unexpired invitations for `caller`'s verified email.
    /// When that verification is older than the re-authentication window the answer is an empty
    /// list with `reauth_required: true`, never an error, so periodic refreshes keep working.
    pub fn invitations_incoming(
        &self,
        caller: &AccountId,
    ) -> Result<IncomingInvitationList, RequestError> {
        self.sharing_request(caller, false, |st, now, err| {
            let Some(email) = st.emails.get(caller) else {
                return Ok(IncomingInvitationList {
                    invitations: Vec::new(),
                    reauth_required: false,
                });
            };
            if fresh_email(st, caller, now, err).is_err() {
                return Ok(IncomingInvitationList {
                    invitations: Vec::new(),
                    reauth_required: true,
                });
            }
            let invitations = st
                .invitations
                .iter()
                .filter(|i| i.state == InvitationState::Pending && &i.email == email)
                .filter_map(|i| {
                    let s = st.accounts.get(&i.space)?.space.as_ref()?;
                    Some(IncomingInvitation {
                        invitation_id: i.id.clone(),
                        space_id: i.space.clone(),
                        role: i.role,
                        invited_by: st.emails.get(&s.owner).cloned().unwrap_or_default(),
                        expires_at_ms: i.expires_at_ms,
                    })
                })
                .collect();
            Ok(IncomingInvitationList {
                invitations,
                reauth_required: false,
            })
        })
    }

    /// Index of an invitation addressed to `caller`'s verified email; `not_found` otherwise, so
    /// other accounts cannot learn that an invitation id exists.
    fn invitation_for(st: &State, caller: &AccountId, inv: &InvitationId) -> Option<usize> {
        let email = st.emails.get(caller)?;
        st.invitations
            .iter()
            .position(|i| &i.id == inv && &i.email == email)
    }

    /// `POST /v1/invitations/{inv}/accept`: joins the space with the invitation's role. With a
    /// re-authentication window set ([`Self::set_reauth_window_ms`]), an invitee whose email was
    /// verified longer ago than that gets `reauth_required`.
    /// Accepting again returns the same record; an expired invitation is
    /// `invitation_expired`; a declined one `conflict`; a cancelled one `not_found`. A member
    /// removed after accepting gets `access_revoked`.
    pub fn invitation_accept(
        &self,
        caller: &AccountId,
        inv: &InvitationId,
    ) -> Result<SharedNotebookRecord, RequestError> {
        self.sharing_request(caller, true, |st, now, err| {
            let i = Self::invitation_for(st, caller, inv)
                .ok_or_else(|| err(ErrorCode::NotFound, "unknown invitation"))?;
            fresh_email(st, caller, now, err)?;
            let invitation = st.invitations[i].clone();
            let space = invitation.space.clone();
            match invitation.state {
                InvitationState::Pending => {}
                InvitationState::Accepted if invitation.accepted_by.as_ref() == Some(caller) => {
                    let (s, m) = active(st, caller, &space, err)?;
                    return Ok(s.record(&space, m));
                }
                InvitationState::Expired => {
                    return Err(err(ErrorCode::InvitationExpired, "the invitation expired"))
                }
                InvitationState::Declined => {
                    return Err(err(ErrorCode::Conflict, "the invitation was declined"))
                }
                InvitationState::Accepted | InvitationState::Cancelled => {
                    return Err(err(ErrorCode::NotFound, "unknown invitation"))
                }
            }
            let Some(s) = st.accounts.get_mut(&space).and_then(|a| a.space.as_mut()) else {
                return Err(err(ErrorCode::NotFound, "unknown invitation"));
            };
            match s.member_mut(caller) {
                Some(m) if m.state == MembershipState::Active => {}
                Some(m) => {
                    m.role = invitation.role;
                    m.state = MembershipState::Active;
                    m.version += 1;
                    m.since_ms = now;
                }
                None => s.members.push(Member {
                    account: caller.clone(),
                    role: invitation.role,
                    state: MembershipState::Active,
                    version: 1,
                    since_ms: now,
                }),
            }
            let record = s.record(&space, s.member(caller).expect("just added"));
            let stored = &mut st.invitations[i];
            stored.state = InvitationState::Accepted;
            stored.accepted_by = Some(caller.clone());
            stored.version += 1;
            Ok(record)
        })
    }

    /// `POST /v1/invitations/{inv}/decline`. Declining again succeeds; the owner may then invite
    /// the same email again.
    pub fn invitation_decline(
        &self,
        caller: &AccountId,
        inv: &InvitationId,
    ) -> Result<(), RequestError> {
        self.sharing_request(caller, true, |st, now, err| {
            let i = Self::invitation_for(st, caller, inv)
                .ok_or_else(|| err(ErrorCode::NotFound, "unknown invitation"))?;
            fresh_email(st, caller, now, err)?;
            let stored = &mut st.invitations[i];
            match stored.state {
                InvitationState::Pending => {
                    stored.state = InvitationState::Declined;
                    stored.version += 1;
                    Ok(())
                }
                InvitationState::Declined => Ok(()),
                InvitationState::Expired => {
                    Err(err(ErrorCode::InvitationExpired, "the invitation expired"))
                }
                InvitationState::Accepted => {
                    Err(err(ErrorCode::Conflict, "the invitation was accepted"))
                }
                InvitationState::Cancelled => Err(err(ErrorCode::NotFound, "unknown invitation")),
            }
        })
    }

    /// `DELETE /v1/invitations/{inv}`: the space owner cancels a pending invitation. Cancelling
    /// again succeeds; an answered or expired invitation is `conflict`. Everyone else gets the
    /// same `not_found` as for an unknown id.
    pub fn invitation_cancel(
        &self,
        caller: &AccountId,
        inv: &InvitationId,
    ) -> Result<(), RequestError> {
        self.sharing_request(caller, true, |st, _, err| {
            // Anyone but the space's owner gets exactly the answer an unknown id gets, so
            // invitation ids can't be probed (not even by other members).
            let i = st
                .invitations
                .iter()
                .position(|i| &i.id == inv)
                .filter(|&i| {
                    st.accounts
                        .get(&st.invitations[i].space)
                        .and_then(|a| a.space.as_ref())
                        .and_then(|s| s.member(caller))
                        .is_some_and(|m| {
                            m.role == Role::Owner && m.state == MembershipState::Active
                        })
                })
                .ok_or_else(|| err(ErrorCode::NotFound, "unknown invitation"))?;
            let stored = &mut st.invitations[i];
            match stored.state {
                InvitationState::Pending => {
                    stored.state = InvitationState::Cancelled;
                    stored.version += 1;
                    Ok(())
                }
                InvitationState::Cancelled => Ok(()),
                _ => Err(err(
                    ErrorCode::Conflict,
                    "the invitation was already answered or expired",
                )),
            }
        })
    }
}
