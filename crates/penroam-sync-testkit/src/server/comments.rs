//! Comment streams and notifications in the development test server (`comments.v1`, protocol
//! 1.9).
//!
//! Every sharing space has a [`CommentStream`] next to its operation log: the same envelope checks,
//! idempotency and gap-free sequence numbers as the document stream ([`InMemoryServer::push`]), but
//! its own `server_seq`, cursors (bound to a different stream id, so a document cursor never opens
//! a comment page) and per-device acks. Writing needs a role that may comment (owner, editor,
//! commenter); every active member reads.
//!
//! Notifications are rows per recipient, created after a push commits (so a lost response still
//! notifies), from the push's side-car:
//!
//! * a `mention` for each mentioned account (validated as an active member when pushed; at most
//!   20; the sender is skipped);
//! * a `reply` for each earlier participant of the thread (members who pushed a comment of kind
//!   `comment` with that thread id), unless they are mentioned by the same comment; only pushes of
//!   kind `comment` create replies and participation, while `update`s (edits, deletions,
//!   resolving, reopening) notify only the accounts they mention.
//!
//! A row is created only for active members whose prefs don't mute the space or thread, and at most
//! once per (recipient, comment op id, kind). Listing shows only notifications from spaces the caller
//! is still an active member of, so removal hides them immediately. Live sessions get
//! `comment_committed` and `notification` hints. See `docs/protocol/sync-protocol-v1.md` §12h.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use penroam_sync_protocol::comments::{
    CommentPushItem, CommentPushKind, NotificationKind, NotificationPage, NotificationPrefs,
    NotificationRecord, PullCommentsResponse, PulledComment, UpdateNotificationRequest,
    NOTIFICATION_PAGE_MAX,
};
use penroam_sync_protocol::framing::{decode_json_batch, encode_json_batch};
use penroam_sync_protocol::{
    verify_ciphertext, AccountId, AckRequest, AckResult, Cursor, DeviceId, EnvelopeHeader,
    ErrorBody, ErrorCode, NotificationId, OpAck, OpId, PullOpsRequest, PushOpsResponse, ThreadId,
};

use super::sharing::now_ms;
use super::{b64, Account, Caller, InMemoryServer, RequestError, State};
use crate::cursor::{self, CursorError, CursorPayload, STREAM_COMMENTS, STREAM_NOTIFICATIONS};
use penroam_sync_protocol::version::PROTOCOL_MAJOR;

#[derive(Serialize, Deserialize)]
struct StoredComment {
    header: EnvelopeHeader,
    #[serde(with = "b64")]
    ciphertext: Vec<u8>,
    thread_id: ThreadId,
    member: AccountId,
    /// When it was accepted (test clock, Unix ms); 0 when not recorded.
    #[serde(default)]
    accepted_at_ms: u64,
}

/// What a committed comment push leaves for the notification step.
pub(super) struct CommentEvent {
    op_id: OpId,
    thread_id: ThreadId,
    kind: CommentPushKind,
    mentions: Vec<AccountId>,
    sender: AccountId,
}

/// A space's comment stream.
#[derive(Default, Serialize, Deserialize)]
pub(super) struct CommentStream {
    /// `log[i]` has comment `server_seq = i + 1`.
    log: Vec<StoredComment>,
    #[serde(skip)]
    by_op: HashMap<OpId, u64>,
    #[serde(skip)]
    by_device_seq: HashMap<(DeviceId, u64), OpId>,
    pub(super) bytes: u64,
    /// Members who have pushed into each thread.
    participants: BTreeMap<ThreadId, BTreeSet<AccountId>>,
    /// Applied comment positions per device.
    applied: BTreeMap<DeviceId, u64>,
    /// Committed pushes not yet turned into notifications (never persisted: the step runs before
    /// the push call returns).
    #[serde(skip)]
    pub(super) pending: Vec<CommentEvent>,
}

impl CommentStream {
    pub(super) fn rebuild_indexes(&mut self) {
        self.by_op.clear();
        self.by_device_seq.clear();
        for (i, c) in self.log.iter().enumerate() {
            self.by_op.insert(c.header.op_id.clone(), i as u64 + 1);
            self.by_device_seq.insert(
                (c.header.device_id.clone(), c.header.device_seq),
                c.header.op_id.clone(),
            );
        }
    }

    pub(super) fn len(&self) -> u64 {
        self.log.len() as u64
    }

    /// Attribution of comments from `from_seq` (1-based, inclusive): `(seq, op, member, device,
    /// accepted_at_ms)`.
    pub(super) fn attribution(
        &self,
        from_seq: u64,
        limit: usize,
    ) -> Vec<(u64, OpId, AccountId, DeviceId, u64)> {
        self.log
            .iter()
            .enumerate()
            .skip(from_seq.saturating_sub(1) as usize)
            .take(limit)
            .map(|(i, c)| {
                (
                    i as u64 + 1,
                    c.header.op_id.clone(),
                    c.member.clone(),
                    c.header.device_id.clone(),
                    c.accepted_at_ms,
                )
            })
            .collect()
    }

    /// Account erasure: replaces `old` by the tombstone `new` in authors and participants.
    pub(super) fn erase(&mut self, old: &AccountId, new: &AccountId) {
        for c in &mut self.log {
            if &c.member == old {
                c.member = new.clone();
            }
        }
        for p in self.participants.values_mut() {
            if p.remove(old) {
                p.insert(new.clone());
            }
        }
    }
}

/// A stored notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Notification {
    id: NotificationId,
    recipient: AccountId,
    space: AccountId,
    thread: ThreadId,
    op: OpId,
    kind: NotificationKind,
    actor: AccountId,
    created_at_ms: u64,
    read: bool,
}

/// Account erasure: drops `old`'s own notifications and those about `gone` spaces, and replaces
/// `old` as an actor by the tombstone `new`.
pub(super) fn erase_notifications(
    st: &mut State,
    old: &AccountId,
    new: &AccountId,
    gone: &[AccountId],
) {
    st.notifications
        .retain(|n| &n.recipient != old && !gone.contains(&n.space));
    for n in &mut st.notifications {
        if &n.actor == old {
            n.actor = new.clone();
        }
    }
    st.notification_prefs.remove(old);
}

impl Notification {
    fn record(&self) -> NotificationRecord {
        NotificationRecord {
            notification_id: self.id.clone(),
            space_id: self.space.clone(),
            thread_id: self.thread.clone(),
            comment_op_id: self.op.clone(),
            kind: self.kind,
            actor: self.actor.clone(),
            created_at_ms: self.created_at_ms,
            read: self.read,
        }
    }
}

/// Whether `who` is an active member of `space`.
fn active_member(st: &State, space: &AccountId, who: &AccountId) -> bool {
    st.accounts
        .get(space)
        .and_then(|a| a.space.as_ref())
        .is_some_and(|s| s.is_active(who))
}

/// Stores one comment envelope: the document-stream checks of [`InMemoryServer::push`] (header,
/// device, ciphertext, idempotency, sequence reuse, quota) plus the side-car's mentions.
fn accept_comment(
    acct: &mut Account,
    device: &DeviceId,
    item: CommentPushItem,
    ciphertext: Vec<u8>,
    limits: &penroam_sync_protocol::Limits,
) -> Result<AckResult, (ErrorCode, String)> {
    item.validate(limits).map_err(|e| match e {
        penroam_sync_protocol::comments::CommentError::Envelope(e) => {
            (e.error_code(), e.to_string())
        }
        other => (ErrorCode::ValidationFailed, other.to_string()),
    })?;
    let space = acct.space.as_ref().expect("checked by the caller");
    if let Some(m) = item.mentions.iter().find(|m| !space.is_active(m)) {
        return Err((
            ErrorCode::ValidationFailed,
            format!("mentioned account {m} is not an active member"),
        ));
    }
    let header = item.envelope;
    if &header.device_id != device {
        return Err((
            ErrorCode::Forbidden,
            "envelope device_id is not the authenticated device".into(),
        ));
    }
    verify_ciphertext(&header, &ciphertext).map_err(|e| (e.error_code(), e.to_string()))?;
    let stream = &acct.comments;
    if let Some(&seq) = stream.by_op.get(&header.op_id) {
        let existing = &stream.log[(seq - 1) as usize];
        return if existing.header == header && existing.thread_id == item.thread_id {
            Ok(AckResult::Duplicate { server_seq: seq })
        } else {
            Err((
                ErrorCode::OpIdConflict,
                "op_id already used with different content".into(),
            ))
        };
    }
    let key = (header.device_id.clone(), header.device_seq);
    if stream.by_device_seq.contains_key(&key) {
        return Err((
            ErrorCode::OpIdConflict,
            "device_seq already used by a different op_id".into(),
        ));
    }
    let len = ciphertext.len() as u64;
    if acct.over_quota_with(len) {
        return Err((ErrorCode::QuotaExceeded, "account quota exceeded".into()));
    }
    let member = acct
        .ctx
        .member
        .clone()
        .expect("space requests have a member");
    let stream = &mut acct.comments;
    let seq = stream.len() + 1;
    stream.by_op.insert(header.op_id.clone(), seq);
    stream.by_device_seq.insert(key, header.op_id.clone());
    stream.bytes += len;
    stream.pending.push(CommentEvent {
        op_id: header.op_id.clone(),
        thread_id: item.thread_id.clone(),
        kind: item.kind,
        mentions: item.mentions,
        sender: member.clone(),
    });
    let accepted_at_ms = acct.ctx.now_ms;
    let stream = &mut acct.comments;
    stream.log.push(StoredComment {
        header,
        ciphertext,
        thread_id: item.thread_id,
        member,
        accepted_at_ms,
    });
    Ok(AckResult::Accepted { server_seq: seq })
}

/// A comment page: the JSON control document and the PRSB frame of [`PulledComment`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentPage {
    /// JSON control document.
    pub response: PullCommentsResponse,
    /// `PRSB` frame carrying the ciphertexts.
    pub frame: Vec<u8>,
}

fn not_a_space(err: &mut dyn FnMut(ErrorCode, String) -> ErrorBody) -> ErrorBody {
    err(
        ErrorCode::Forbidden,
        "comments are only for shared notebooks".into(),
    )
}

impl InMemoryServer {
    fn comment_cursor(&self, space: &AccountId, position: u64) -> Cursor {
        cursor::mint(
            &self.secret,
            &CursorPayload {
                account: space.to_string(),
                stream: STREAM_COMMENTS,
                protocol_major: PROTOCOL_MAJOR,
                position,
            },
        )
    }

    fn open_comment_cursor(
        &self,
        space: &AccountId,
        acct: &Account,
        c: &Cursor,
    ) -> Result<u64, (ErrorCode, &'static str)> {
        let p = cursor::open(&self.secret, c)
            .map_err(|CursorError::Invalid| (ErrorCode::InvalidCursor, "invalid cursor"))?;
        if p.account != space.as_str()
            || p.stream != STREAM_COMMENTS
            || p.protocol_major != PROTOCOL_MAJOR
        {
            return Err((
                ErrorCode::InvalidCursor,
                "cursor is bound to another account/stream/version",
            ));
        }
        if p.position > acct.comments.len() {
            return Err((ErrorCode::InvalidCursor, "cursor beyond end of stream"));
        }
        Ok(p.position)
    }

    /// `POST /v1/comments/batch`: a PRSB frame of [`CommentPushItem`] headers. Needs a role that
    /// may comment (`forbidden` for viewers); each item is checked like a document push plus its
    /// mentions (`validation_failed` per item for more than 20, repeats, or anyone who isn't an
    /// active member). Accepted comments then create notifications and live hints.
    pub fn comment_push<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        body: &[u8],
    ) -> Result<PushOpsResponse, RequestError> {
        let caller = caller.into();
        let limits = self.limits;
        let result = self.request(caller, Some(device), true, |acct, err| {
            if acct.space.is_none() {
                return Err(not_a_space(err));
            }
            if !acct.role().is_some_and(|r| r.may_comment()) {
                return Err(err(ErrorCode::Forbidden, "this role can't comment".into()));
            }
            let items: Vec<(CommentPushItem, Vec<u8>)> =
                decode_json_batch(body, &limits).map_err(|e| err(e.error_code(), e.to_string()))?;
            let acks = items
                .into_iter()
                .map(|(item, ciphertext)| {
                    let op_id = item.envelope.op_id.clone();
                    let result = match accept_comment(acct, device, item, ciphertext, &limits) {
                        Ok(r) => r,
                        Err((code, message)) => AckResult::Rejected {
                            error: err(code, message),
                        },
                    };
                    OpAck { op_id, result }
                })
                .collect();
            Ok(PushOpsResponse { acks })
        });
        // After the commit, even if the response was lost.
        self.comments_committed(caller.account);
        result
    }

    /// `GET /v1/comments`: every active member.
    pub fn comment_pull<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        req: &PullOpsRequest,
    ) -> Result<CommentPage, RequestError> {
        let caller = caller.into();
        let space = caller.account;
        let limits = self.limits;
        self.request(caller, Some(device), false, |acct, err| {
            if acct.space.is_none() {
                return Err(not_a_space(err));
            }
            if req.limit == 0 {
                return Err(err(
                    ErrorCode::ValidationFailed,
                    "limit must be >= 1".into(),
                ));
            }
            let start = match &req.cursor {
                Some(c) => self
                    .open_comment_cursor(space, acct, c)
                    .map_err(|(code, m)| err(code, m.into()))?,
                None => 0,
            };
            let limit = req.limit.min(limits.max_pull_page_size) as usize;
            let mut items = Vec::new();
            let mut frame_items = Vec::new();
            let mut bytes = penroam_sync_protocol::framing::PREAMBLE_LEN as u64;
            for (i, c) in acct
                .comments
                .log
                .iter()
                .enumerate()
                .skip(start as usize)
                .take(limit)
            {
                let pulled = PulledComment {
                    server_seq: i as u64 + 1,
                    header: c.header.clone(),
                    thread_id: c.thread_id.clone(),
                    member: c.member.clone(),
                };
                let hlen = serde_json::to_vec(&pulled).map(|v| v.len()).unwrap_or(0) as u64;
                let item = 8 + hlen + c.ciphertext.len() as u64;
                if !items.is_empty() && bytes + item > limits.max_batch_bytes {
                    break;
                }
                bytes += item;
                frame_items.push((pulled.clone(), c.ciphertext.clone()));
                items.push(pulled);
            }
            let end = start + items.len() as u64;
            let frame = encode_json_batch(&frame_items, &limits)
                .map_err(|e| err(ErrorCode::InternalError, e.to_string()))?;
            Ok(CommentPage {
                response: PullCommentsResponse {
                    items,
                    next_cursor: self.comment_cursor(space, end),
                    has_more: end < acct.comments.len(),
                },
                frame,
            })
        })
    }

    /// `POST /v1/comments/acks`: the device's applied comment position (only moves forward).
    pub fn comment_ack<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        req: &AckRequest,
    ) -> Result<(), RequestError> {
        let caller = caller.into();
        let space = caller.account;
        self.request(caller, Some(device), true, |acct, err| {
            if acct.space.is_none() {
                return Err(not_a_space(err));
            }
            let pos = self
                .open_comment_cursor(space, acct, &req.applied_cursor)
                .map_err(|(code, m)| err(code, m.into()))?;
            let a = acct.comments.applied.entry(device.clone()).or_insert(0);
            *a = (*a).max(pos);
            Ok(())
        })
    }

    /// The comment position `device` acknowledged in `space` (tests and diagnostics).
    pub fn comment_applied_position(&self, space: &AccountId, device: &DeviceId) -> Option<u64> {
        self.lock()
            .accounts
            .get(space)
            .and_then(|a| a.comments.applied.get(device).copied())
    }

    /// Number of comments stored in a space's comment stream.
    pub fn comment_count(&self, space: &AccountId) -> u64 {
        self.lock()
            .accounts
            .get(space)
            .map_or(0, |a| a.comments.len())
    }

    /// Turns committed comment pushes into participants, notifications and live hints.
    fn comments_committed(&self, space: &AccountId) {
        let mut guard = self.lock();
        let st: &mut State = &mut guard;
        let Some(acct) = st.accounts.get_mut(space) else {
            return;
        };
        let events = std::mem::take(&mut acct.comments.pending);
        let seq = acct.comments.len();
        if events.is_empty() {
            return;
        }
        let now = now_ms(st);
        let mut created = Vec::new();
        for e in events {
            let participants: Vec<AccountId> = st.accounts[space]
                .comments
                .participants
                .get(&e.thread_id)
                .map(|p| p.iter().cloned().collect())
                .unwrap_or_default();
            let mut wanted: Vec<(AccountId, NotificationKind)> = e
                .mentions
                .iter()
                .map(|m| (m.clone(), NotificationKind::Mention))
                .collect();
            // Only a new comment is a reply; edits, deletions, resolving and the like notify only
            // whoever they mention.
            if e.kind == CommentPushKind::Comment {
                for p in participants {
                    if !e.mentions.contains(&p) {
                        wanted.push((p, NotificationKind::Reply));
                    }
                }
            }
            for (recipient, kind) in wanted {
                if recipient == e.sender
                    || !active_member(st, space, &recipient)
                    || st
                        .notification_prefs
                        .get(&recipient)
                        .is_some_and(|p| p.mutes(space, &e.thread_id))
                    || st
                        .notifications
                        .iter()
                        .any(|n| n.recipient == recipient && n.op == e.op_id && n.kind == kind)
                {
                    continue;
                }
                let n = Notification {
                    id: format!("n-{}", uuid::Uuid::new_v4().simple())
                        .parse()
                        .expect("hex digits with a prefix are a valid id"),
                    recipient,
                    space: space.clone(),
                    thread: e.thread_id.clone(),
                    op: e.op_id.clone(),
                    kind,
                    actor: e.sender.clone(),
                    created_at_ms: now,
                    read: false,
                };
                created.push((n.recipient.clone(), n.id.clone()));
                st.notifications.push(n);
            }
            if e.kind == CommentPushKind::Comment {
                st.accounts
                    .get_mut(space)
                    .expect("checked")
                    .comments
                    .participants
                    .entry(e.thread_id)
                    .or_default()
                    .insert(e.sender);
            }
        }
        super::live::comments_committed(st, space, seq);
        for (recipient, id) in created {
            super::live::notification(st, &recipient, &id, space);
        }
        if let Err(e) = self.save(st) {
            panic!("test server could not persist its state: {e}");
        }
    }

    // ---- notifications (the caller's own account) ----

    /// Runs `f` as the caller's own account, refusing vault and space sessions like the sharing
    /// routes do.
    fn own_account<T>(
        &self,
        caller: &AccountId,
        mutates: bool,
        f: impl FnOnce(&mut State, &mut dyn FnMut(ErrorCode, &str) -> ErrorBody) -> Result<T, ErrorBody>,
    ) -> Result<T, RequestError> {
        let mut guard = self.lock();
        let st: &mut State = &mut guard;
        st.request_counter += 1;
        let rid = format!("req-{}", st.request_counter);
        let mut err = |code: ErrorCode, m: &str| ErrorBody::new(code, m, rid.clone());
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
        let result = f(st, &mut err).map_err(RequestError::Api);
        if mutates && result.is_ok() {
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

    fn notification_cursor(&self, account: &AccountId, position: u64) -> Cursor {
        cursor::mint(
            &self.secret,
            &CursorPayload {
                account: account.to_string(),
                stream: STREAM_NOTIFICATIONS,
                protocol_major: PROTOCOL_MAJOR,
                position,
            },
        )
    }

    /// `GET /v1/notifications?cursor=&limit=`: the caller's notifications oldest first, only from
    /// spaces they are still an active member of.
    pub fn notifications_list(
        &self,
        caller: &AccountId,
        cursor: Option<&Cursor>,
        limit: u32,
    ) -> Result<NotificationPage, RequestError> {
        self.own_account(caller, false, |st, err| {
            if limit == 0 {
                return Err(err(ErrorCode::ValidationFailed, "limit must be >= 1"));
            }
            let start = match cursor {
                None => 0,
                Some(c) => {
                    let p = cursor::open(&self.secret, c).map_err(|CursorError::Invalid| {
                        err(ErrorCode::InvalidCursor, "invalid cursor")
                    })?;
                    if p.account != caller.as_str()
                        || p.stream != STREAM_NOTIFICATIONS
                        || p.protocol_major != PROTOCOL_MAJOR
                        || p.position > st.notifications.len() as u64
                    {
                        return Err(err(
                            ErrorCode::InvalidCursor,
                            "cursor is for something else",
                        ));
                    }
                    p.position as usize
                }
            };
            let visible =
                |n: &Notification| &n.recipient == caller && active_member(st, &n.space, caller);
            let unread = st
                .notifications
                .iter()
                .filter(|n| visible(n) && !n.read)
                .count() as u64;
            let limit = limit.min(NOTIFICATION_PAGE_MAX) as usize;
            let mut page = Vec::new();
            let mut end = start;
            for (i, n) in st.notifications.iter().enumerate().skip(start) {
                if page.len() == limit {
                    break;
                }
                end = i + 1;
                if visible(n) {
                    page.push(n.record());
                }
            }
            let has_more = st.notifications[end..].iter().any(visible);
            Ok(NotificationPage {
                notifications: page,
                next_cursor: self.notification_cursor(caller, end as u64),
                has_more,
                unread,
            })
        })
    }

    /// `PATCH /v1/notifications/{id}`: mark read or unread. Someone else's notification, or one
    /// from a space the caller lost access to, is `not_found`.
    pub fn notification_update(
        &self,
        caller: &AccountId,
        id: &NotificationId,
        req: &UpdateNotificationRequest,
    ) -> Result<NotificationRecord, RequestError> {
        self.own_account(caller, true, |st, err| {
            let i = st
                .notifications
                .iter()
                .position(|n| {
                    &n.id == id && &n.recipient == caller && active_member(st, &n.space, caller)
                })
                .ok_or_else(|| err(ErrorCode::NotFound, "unknown notification"))?;
            st.notifications[i].read = req.read;
            Ok(st.notifications[i].record())
        })
    }

    /// `GET /v1/notification-prefs`.
    pub fn notification_prefs_get(
        &self,
        caller: &AccountId,
    ) -> Result<NotificationPrefs, RequestError> {
        self.own_account(caller, false, |st, _| {
            Ok(st
                .notification_prefs
                .get(caller)
                .cloned()
                .unwrap_or_default())
        })
    }

    /// `PUT /v1/notification-prefs`: replaces the caller's mutes (at most 500 of each kind, no
    /// repeats: `validation_failed`). Muting a space the caller isn't in is allowed and harmless.
    pub fn notification_prefs_put(
        &self,
        caller: &AccountId,
        prefs: &NotificationPrefs,
    ) -> Result<(), RequestError> {
        self.own_account(caller, true, |st, err| {
            prefs
                .validate()
                .map_err(|e| err(ErrorCode::ValidationFailed, &e.to_string()))?;
            let mut p = prefs.clone();
            p.muted_spaces.sort();
            p.muted_threads.sort();
            st.notification_prefs.insert(caller.clone(), p);
            Ok(())
        })
    }
}
