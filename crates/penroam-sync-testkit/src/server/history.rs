//! Attribution, the previous checkpoint and account erasure in the development test server
//! (`history.v1`, protocol 1.10).
//!
//! Attribution is recorded when an operation or comment is accepted (`request()` fills in the
//! author and the test-clock time), and read back here per stream. In a space the author is the
//! member behind the session; elsewhere it is the account. Any active member may read a space's
//! attribution; removed members get `access_revoked` and others `not_found`, as on every space
//! route.
//!
//! [`InMemoryServer::erase_account`] models account erasure: the account, its vaults and the spaces
//! it owns are deleted, and everywhere else its id becomes a stable tombstone
//! (`erased-` + 32 hex digits of a keyed hash), so attribution keeps pointing at the same
//! "former collaborator" without revealing who it was. See `docs/protocol/sync-protocol-v1.md`
//! §12i.

use penroam_sync_protocol::envelope::sha256_hex;
use penroam_sync_protocol::history::{
    AttributionEntry, AttributionPage, ATTRIBUTION_PAGE_MAX, ERASED_ACCOUNT_PREFIX,
};
use penroam_sync_protocol::{AccountId, CheckpointRecord, DeviceId, ErrorBody, ErrorCode};

use super::{Caller, InMemoryServer, RequestError, State};

impl InMemoryServer {
    /// The tombstone id that replaces `account` once it is erased: stable for the account, and
    /// unlinkable to it without the server secret.
    pub fn erased_account_id(&self, account: &AccountId) -> AccountId {
        let mut input = self.secret.to_vec();
        input.extend_from_slice(b"penroam/erased-account/v1\0");
        input.extend_from_slice(account.as_str().as_bytes());
        format!("{ERASED_ACCOUNT_PREFIX}{}", &sha256_hex(&input)[..32])
            .parse()
            .expect("a prefix and hex digits are a valid id")
    }

    fn page(items: Vec<AttributionEntry>, from_seq: u64, total: u64) -> AttributionPage {
        let next = items.last().map_or(from_seq.max(1), |e| e.server_seq + 1);
        AttributionPage {
            has_more: next <= total,
            next_from_seq: next,
            items,
        }
    }

    /// `GET /v1/attribution?from_seq=&limit=`: who pushed each operation of the document stream,
    /// from `from_seq` (1-based, inclusive; 0 means 1), at most `limit` (1 to 1000).
    pub fn attribution<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        from_seq: u64,
        limit: u32,
    ) -> Result<AttributionPage, RequestError> {
        let caller = caller.into();
        let account = caller.account.clone();
        self.request(caller, Some(device), false, |acct, err| {
            if limit == 0 {
                return Err(err(
                    ErrorCode::ValidationFailed,
                    "limit must be >= 1".into(),
                ));
            }
            let limit = limit.min(ATTRIBUTION_PAGE_MAX) as usize;
            let items = acct
                .log
                .iter()
                .enumerate()
                .skip(from_seq.saturating_sub(1) as usize)
                .take(limit)
                .map(|(i, op)| AttributionEntry {
                    server_seq: i as u64 + 1,
                    op_id: op.header.op_id.clone(),
                    account_id: op.account.clone().unwrap_or_else(|| account.clone()),
                    device_id: op.header.device_id.clone(),
                    accepted_at_ms: op.accepted_at_ms,
                })
                .collect();
            Ok(Self::page(items, from_seq, acct.log.len() as u64))
        })
    }

    /// `GET /v1/comments/attribution?from_seq=&limit=`: the same for a space's comment stream.
    pub fn comment_attribution<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        from_seq: u64,
        limit: u32,
    ) -> Result<AttributionPage, RequestError> {
        self.request(caller.into(), Some(device), false, |acct, err| {
            if acct.space.is_none() {
                return Err(err(
                    ErrorCode::Forbidden,
                    "comments are only for shared notebooks".into(),
                ));
            }
            if limit == 0 {
                return Err(err(
                    ErrorCode::ValidationFailed,
                    "limit must be >= 1".into(),
                ));
            }
            let limit = limit.min(ATTRIBUTION_PAGE_MAX) as usize;
            let items = acct
                .comments
                .attribution(from_seq, limit)
                .into_iter()
                .map(
                    |(server_seq, op_id, account_id, device_id, accepted_at_ms)| AttributionEntry {
                        server_seq,
                        op_id,
                        account_id,
                        device_id,
                        accepted_at_ms,
                    },
                )
                .collect();
            Ok(Self::page(items, from_seq, acct.comments.len()))
        })
    }

    /// `GET /v1/checkpoints/previous`: the checkpoint before the latest, or `not_found`.
    pub fn checkpoint_previous<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
    ) -> Result<CheckpointRecord, RequestError> {
        self.request(caller.into(), Some(device), false, |acct, err| {
            acct.previous_checkpoint
                .clone()
                .ok_or_else(|| err(ErrorCode::NotFound, "no previous checkpoint".into()))
        })
    }

    /// Erases an account (administration, not a protocol route): the account, its vaults and the
    /// spaces it owns are deleted (with their invitations, notifications and live sessions); in
    /// every other space its membership is revoked and its id is replaced by
    /// [`Self::erased_account_id`] in the member list, device registrations, attribution and
    /// comment authorship. Its email, prefs and notifications go too. Returns the tombstone, or
    /// `not_found` for an unknown account.
    pub fn erase_account(&self, account: &AccountId) -> Result<AccountId, RequestError> {
        let tomb = self.erased_account_id(account);
        let mut guard = self.lock();
        let st: &mut State = &mut guard;
        let Some(acct) = st.accounts.remove(account) else {
            return Err(RequestError::Api(ErrorBody::new(
                ErrorCode::NotFound,
                "unknown account",
                "admin",
            )));
        };
        let mut gone: Vec<AccountId> = acct.owned_spaces.clone();
        gone.extend(
            st.accounts
                .iter()
                .filter(|(_, a)| a.vault_of.as_ref() == Some(account))
                .map(|(id, _)| id.clone()),
        );
        for id in &gone {
            st.accounts.remove(id);
        }
        st.invitations.retain(|i| !gone.contains(i.space()));
        for a in st.accounts.values_mut() {
            let Some(space) = a.space.as_mut() else {
                continue;
            };
            if !space.erase_member(account, &tomb) {
                continue;
            }
            for d in a.devices.values_mut() {
                if d.registered_by.as_ref() == Some(account) {
                    d.registered_by = Some(tomb.clone());
                    d.revoked = true;
                }
            }
            for op in &mut a.log {
                if op.account.as_ref() == Some(account) {
                    op.account = Some(tomb.clone());
                }
            }
            a.comments.erase(account, &tomb);
        }
        super::comments::erase_notifications(st, account, &tomb, &gone);
        super::live::erase(st, account, &gone);
        st.emails.remove(account);
        st.email_verified_at.remove(account);
        if let Err(e) = self.save(st) {
            panic!("test server could not persist its state: {e}");
        }
        Ok(tomb)
    }
}
