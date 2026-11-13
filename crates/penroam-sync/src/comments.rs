//! The comment stream of a shared notebook: a durable local log of comment operations, sealing and
//! signing them, pushing and pulling them, verifying what arrives, and merging it with
//! [`penroam_core::comment::CommentReplica`]. Also the notification routes.
//!
//! Each sharing space has a comment stream next to its operation log (protocol §12h). Comments
//! never enter the document log. A [`CommentSync`] holds one space's stream on this device; the
//! sync worker calls [`CommentSync::sync`] with the space's verified keys ([`SpaceKeys`]) and
//! its current member list ([`SpaceRoster`]) after the document engine reconciled, so the key log
//! is fresh.
//!
//! # Storage and durability
//!
//! ```text
//! <dir>/comments.log    append-only records: u32 len ‖ u32 crc32 ‖ JSON, after an 8-byte header
//! <dir>/comments.json   the pull cursor (atomic writes)
//! ```
//!
//! The log is the only source of truth; everything in memory is rebuilt from it on open. A local
//! comment is acknowledged to the caller only after its record is fsynced, so a queued comment
//! survives a crash. Pulled envelopes are appended and fsynced *before* the cursor that skips them
//! is saved. A torn last record (a crash mid-append) is cut off on open; a damaged record anywhere
//! else stops the open with an error and nothing is deleted.
//!
//! # Sending
//!
//! A local operation gets this device's next sequence on the stream and the replica's frontier as
//! its causal dependencies. Its envelope is sealed under [`comment_key`] (`HKDF(epoch key, salt =
//! the notebook's routing alias, info = "penroam/comment/v1")`) with a nonce derived from the
//! operation id and plaintext, and signed by the device key, as document envelopes are. The epoch
//! is recorded before the first upload, so a retry after a lost response produces the same bytes
//! and the server answers `duplicate`. The side-car carries the thread id and the mentions parsed
//! from the body, limited to active members other than the author. Its `kind` is
//! `comment` for thread starts and replies and `update` for everything else, so only new comments
//! notify a thread's participants.
//!
//! Every replica needs each device's sequences without gaps, so a sequence once given out is
//! always filled on the server:
//!
//! * **Waiting**: queued, uploaded on the next sync.
//! * **Sent**: the server stored it.
//! * **Draft**: this member may not comment (the role says so, or the server answered
//!   `forbidden`/`access_revoked`). Drafts are never uploaded. A draft written while commenting
//!   wasn't allowed takes no sequence at all. A queued operation that becomes a draft keeps its
//!   sequence, and once commenting is allowed again a **filler** takes its place on the server: an
//!   operation with a derived id that reopens a thread that doesn't exist, which every replica
//!   rejects identically. So the gap closes without posting the draft.
//! * **Failed**: the server refused it for a reason a retry can't fix (quota, validation). A
//!   filler takes its place too. A refusal because of the mention side-car is retried once
//!   without it first, as the side-car only affects notifications.
//!
//! If a filler is refused because its sequence is already taken, the original reached the server
//! after all (a lost response); it is then marked sent and the filler dropped.
//!
//! # Receiving
//!
//! Each pulled envelope is checked like a document envelope: header limits, ciphertext hash,
//! signer certified by the verified key log, signature, and the routing alias of its epoch. (The
//! key log's removed-device cutoff counts document sequences, so it isn't applied here.) It is
//! then opened with the comment key and must decode as a [`CommentOperation`]
//! ([`is_comment_payload`]); anything else (a document operation sealed
//! under the comment key, or a payload sealed under another key) is **quarantined** and reported
//! as a security event, which is what keeps a commenter from smuggling document edits. The
//! decoded operation must match its signed header, and its author must be the member the server
//! attributes the device to. That attribution is pinned on first sight: a device later attributed
//! to someone else is quarantined. Only the owner may set the moderation flag.
//! Once an account is erased the server attributes its comments to an `erased-…` tombstone while
//! the signed payloads keep the original id; those are accepted on their signature (unless the
//! payload names someone still in the space) and shown under the tombstone.
//!
//! Envelopes that can't be checked yet (an unknown device, a missing epoch key, a newer schema)
//! are parked in the log and retried on every sync.
//!
//! Notifications are built by the server from the push side-car, which isn't signed.
//! [`CommentSync::check_notification`] shows one only when the verified comment it names agrees
//! with it (author, thread, mention or participation), and defers it until that comment is pulled. Verified operations wait in memory until their
//! dependencies are applied.
//!
//! The payload format, anchors, merge rules and this log are described in
//! `docs/formats/comments.md`; the routes in `docs/protocol/sync-protocol-v1.md` §12h.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use penroam_core::comment::{
    is_comment_payload, parse_mentions, sanitize_body, CommentAnchor, CommentApplied, CommentError,
    CommentMergeError, CommentOp, CommentOperation, CommentReplica, CommentThread, Rejection,
    COMMENT_SCHEMA_VERSION,
};
use penroam_core::operation::OperationRef;
use penroam_storage::fsutil::write_atomic;
use penroam_sync_protocol::comments::{
    CommentPushItem, CommentPushKind, NotificationKind, NotificationPage, NotificationPrefs,
    NotificationRecord, PullCommentsResponse, PulledComment, COMMENT_PAYLOAD_INFO, MAX_MENTIONS,
};
use penroam_sync_protocol::history::is_erased_account;
use penroam_sync_protocol::version::ENVELOPE_VERSION;
use penroam_sync_protocol::{
    base64url, canonical_aad_bytes, canonical_signing_bytes, verify_ciphertext, AccountId,
    AckRequest, AckResult, Cursor, DeviceId, EnvelopeHeader, ErrorCode, Limits, MemberList,
    MembershipState, NotebookRoutingId, NotificationId, OpId, OpRef, PullOpsRequest,
    PushOpsResponse, Role, ThreadId,
};

use crate::crypto::{self, DeviceKeys, EpochKey};
use crate::keylog::Membership;
use crate::transport::{TResult, TransportError};

/// Comment envelopes per pull page.
const PULL_PAGE: u32 = 200;
/// Comment envelopes per push request (also bounded by the server's limits).
const PUSH_BATCH: usize = 100;
/// Header bytes budgeted per pushed item when sizing a batch.
const ITEM_OVERHEAD: u64 = 4096;

// ---------------------------------------------------------------------------
// Keys and context

/// The comment payload key of an epoch: `HKDF-SHA256(ikm = epoch key, salt = alias, info =
/// "penroam/comment/v1")`, where `alias` is the notebook's routing alias in that epoch
/// ([`EpochKey::routing_id`]). Distinct from the notebook payload key, so a comment payload never
/// opens as a document payload or the reverse.
pub fn comment_key(ek: &EpochKey, alias: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(alias.as_bytes()), &ek.0)
        .expand(COMMENT_PAYLOAD_INFO.as_bytes(), &mut out)
        .expect("32 bytes is a valid HKDF length");
    out
}

/// What sealing and verifying need from the space's verified key log, borrowed from the space's
/// engine (or from [`SpaceCredentials`]).
#[derive(Clone, Copy)]
pub struct SpaceKeys<'a> {
    /// This device's keys in the space.
    pub device: &'a DeviceKeys,
    /// The verified key log.
    pub membership: &'a Membership,
    /// Every epoch key this device holds.
    pub epoch_keys: &'a BTreeMap<u32, EpochKey>,
    /// The space's notebook (its routing alias is derived per epoch).
    pub notebook: Uuid,
}

/// [`SpaceKeys`] owned: loaded from the secret store and the server's key log. The engine has the
/// same values already; this is for callers that don't hold one (tools, tests).
pub struct SpaceCredentials {
    /// This device's keys in the space.
    pub device: DeviceKeys,
    /// The verified key log.
    pub membership: Membership,
    /// Every epoch key, opened with the space root's recovery key.
    pub epoch_keys: BTreeMap<u32, EpochKey>,
    /// The space's notebook.
    pub notebook: Uuid,
}

impl SpaceCredentials {
    /// Reads this device's keys for `space` from `secrets`, fetches the space's key log with
    /// `transport` (a session on the space) and verifies it against the space root kept in
    /// `secrets` (so the account key is not trusted on first use).
    pub fn load(
        transport: &mut dyn crate::SyncTransport,
        secrets: &dyn crate::SecretStore,
        space: &str,
        notebook: Uuid,
    ) -> Result<Self> {
        let scope = crate::vault::Scope::shared(space)
            .map_err(|e| CommentSyncError::Invalid(e.to_string()))?;
        let raw = secrets
            .get(&scope.secret_name(crate::engine::DEVICE_SECRET))?
            .ok_or_else(|| CommentSyncError::Invalid("no device keys for this space".into()))?;
        let device = DeviceKeys::from_secret_string(&raw)
            .map_err(|e| CommentSyncError::Invalid(e.to_string()))?;
        let root = crate::shared::load_root(secrets, space)
            .map_err(|e| CommentSyncError::Invalid(e.to_string()))?
            .ok_or_else(|| CommentSyncError::Invalid("no root for this space".into()))?;
        let mut entries = Vec::new();
        loop {
            let page = transport.keylog_get(entries.len() as u64)?;
            if page.entries.is_empty() {
                break;
            }
            for e in page.entries {
                entries.push(
                    base64url::decode(&e.data)
                        .map_err(|_| CommentSyncError::Integrity("key log entry".into()))?,
                );
            }
            if entries.len() as u64 >= page.next_index {
                break;
            }
        }
        let account_pk = root.account_signing_key().verifying_key().to_bytes();
        let membership = crate::keylog::verify_log_with(
            &entries,
            Some(&account_pk),
            None,
            crate::keylog::GrantRule::Recovery,
        )
        .map_err(|e| CommentSyncError::Integrity(e.to_string()))?;
        let epoch_keys = membership.epoch_keys_by_recovery(&root.recovery_dh());
        Ok(Self {
            device,
            membership,
            epoch_keys,
            notebook,
        })
    }

    /// Borrowed view.
    pub fn keys(&self) -> SpaceKeys<'_> {
        SpaceKeys {
            device: &self.device,
            membership: &self.membership,
            epoch_keys: &self.epoch_keys,
            notebook: self.notebook,
        }
    }
}

/// Who is in the space, from its member list (`GET /v1/shared-notebooks/{id}/members`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceRoster {
    /// This member's account id.
    pub me: AccountId,
    /// This member's role.
    pub role: Role,
    /// The space's owner (the only member who may moderate).
    pub owner: AccountId,
    /// Active members (who may be mentioned).
    pub active: BTreeSet<AccountId>,
    /// Optional device → member map from a source the caller trusts. When a device is listed, the
    /// server's attribution of its comments must agree.
    pub devices: BTreeMap<String, AccountId>,
}

impl SpaceRoster {
    /// The roster in a member list (the caller is the record with `is_you`). `None` if the list
    /// has no caller or no owner.
    pub fn from_member_list(list: &MemberList) -> Option<Self> {
        let me = list.members.iter().find(|m| m.is_you)?;
        let owner = list.members.iter().find(|m| m.role == Role::Owner)?;
        Some(Self {
            me: me.account_id.clone(),
            role: if me.state == MembershipState::Active {
                me.role
            } else {
                Role::Unknown
            },
            owner: owner.account_id.clone(),
            active: list
                .members
                .iter()
                .filter(|m| m.state == MembershipState::Active)
                .map(|m| m.account_id.clone())
                .collect(),
            devices: BTreeMap::new(),
        })
    }

    /// Whether this member may write comments now.
    pub fn may_comment(&self) -> bool {
        self.role.may_comment() && self.active.contains(&self.me)
    }

    /// Whether this member moderates (owns the space).
    pub fn is_owner(&self) -> bool {
        self.me == self.owner
    }
}

// ---------------------------------------------------------------------------
// Errors

/// Why a comment call failed. The local log is unchanged unless said otherwise.
#[derive(Debug, thiserror::Error)]
pub enum CommentSyncError {
    /// Reading or writing the local log failed.
    #[error("comment log: {0}")]
    Io(#[from] io::Error),
    /// A server call failed; [`TransportError::is_retryable`] says whether to try again.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The comment isn't valid (body, anchor, ids).
    #[error(transparent)]
    Comment(#[from] CommentError),
    /// The comment isn't allowed (editing someone else's comment, an unknown thread, …).
    #[error("not allowed: {0}")]
    NotPermitted(Rejection),
    /// The local log or the keys are inconsistent (never repaired silently).
    #[error("integrity: {0}")]
    Integrity(String),
    /// A bad argument or missing setup.
    #[error("{0}")]
    Invalid(String),
}

/// Result of comment calls.
pub type Result<T> = std::result::Result<T, CommentSyncError>;

// ---------------------------------------------------------------------------
// Transport

/// One pulled page of the comment stream with its ciphertexts.
#[derive(Debug, Clone)]
pub struct PulledComments {
    /// The control document.
    pub response: PullCommentsResponse,
    /// One ciphertext per item, in the same order.
    pub ciphertexts: Vec<Vec<u8>>,
}

/// The comment and notification routes (protocol §12h). The comment routes need a device session
/// on the space; the notification routes need the member's own account.
pub trait CommentTransport {
    /// `POST /v1/comments/batch` with a PRSB frame of [`CommentPushItem`]s.
    fn comment_push(&mut self, frame: Vec<u8>) -> TResult<PushOpsResponse>;
    /// `GET /v1/comments?cursor=&limit=`.
    fn comment_pull(&mut self, req: &PullOpsRequest, limits: &Limits) -> TResult<PulledComments>;
    /// `POST /v1/comments/acks`.
    fn comment_ack(&mut self, req: &AckRequest) -> TResult<()>;
    /// `GET /v1/notifications?cursor=&limit=`.
    fn notifications_list(
        &mut self,
        cursor: Option<&Cursor>,
        limit: u32,
    ) -> TResult<NotificationPage>;
    /// `PATCH /v1/notifications/{id}`.
    fn notification_update(
        &mut self,
        id: &NotificationId,
        read: bool,
    ) -> TResult<NotificationRecord>;
    /// `GET /v1/notification-prefs`.
    fn notification_prefs_get(&mut self) -> TResult<NotificationPrefs>;
    /// `PUT /v1/notification-prefs`.
    fn notification_prefs_put(&mut self, prefs: &NotificationPrefs) -> TResult<()>;
}

/// Pairs a pulled control document with its frame; a frame whose headers differ is a protocol
/// error (a server bug or tampering).
#[cfg(any(feature = "http", feature = "testkit"))]
fn pulled_comments(
    response: PullCommentsResponse,
    items: Vec<(PulledComment, Vec<u8>)>,
) -> TResult<PulledComments> {
    if items.len() != response.items.len()
        || items.iter().zip(&response.items).any(|((h, _), e)| h != e)
    {
        return Err(TransportError::Protocol(
            "comment frame does not match its control document".into(),
        ));
    }
    Ok(PulledComments {
        response,
        ciphertexts: items.into_iter().map(|(_, ct)| ct).collect(),
    })
}

#[cfg(feature = "testkit")]
mod in_process {
    use super::*;
    use crate::transport::InProcessTransport;
    use penroam_sync_protocol::comments::UpdateNotificationRequest;
    use penroam_sync_protocol::framing::decode_json_batch;
    use penroam_sync_testkit::{Caller, RequestError};

    fn map(e: RequestError) -> TransportError {
        match e {
            RequestError::Api(b) => TransportError::Api(b),
            RequestError::ResponseLost => TransportError::ResponseLost,
        }
    }

    fn up(t: &InProcessTransport) -> TResult<()> {
        if t.offline {
            Err(TransportError::Network("offline".into()))
        } else {
            Ok(())
        }
    }

    fn caller(t: &InProcessTransport) -> Caller<'_> {
        Caller {
            account: &t.account,
            member: t.member.as_ref(),
        }
    }

    /// The member's own account (notification routes).
    fn own(t: &InProcessTransport) -> AccountId {
        t.member.clone().unwrap_or_else(|| t.account.clone())
    }

    impl CommentTransport for InProcessTransport {
        fn comment_push(&mut self, frame: Vec<u8>) -> TResult<PushOpsResponse> {
            up(self)?;
            self.server
                .comment_push(caller(self), &self.device, &frame)
                .map_err(map)
        }
        fn comment_pull(
            &mut self,
            req: &PullOpsRequest,
            limits: &Limits,
        ) -> TResult<PulledComments> {
            up(self)?;
            let page = self
                .server
                .comment_pull(caller(self), &self.device, req)
                .map_err(map)?;
            let items = decode_json_batch(&page.frame, limits)
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
            pulled_comments(page.response, items)
        }
        fn comment_ack(&mut self, req: &AckRequest) -> TResult<()> {
            up(self)?;
            self.server
                .comment_ack(caller(self), &self.device, req)
                .map_err(map)
        }
        fn notifications_list(
            &mut self,
            cursor: Option<&Cursor>,
            limit: u32,
        ) -> TResult<NotificationPage> {
            up(self)?;
            self.server
                .notifications_list(&own(self), cursor, limit)
                .map_err(map)
        }
        fn notification_update(
            &mut self,
            id: &NotificationId,
            read: bool,
        ) -> TResult<NotificationRecord> {
            up(self)?;
            self.server
                .notification_update(&own(self), id, &UpdateNotificationRequest { read })
                .map_err(map)
        }
        fn notification_prefs_get(&mut self) -> TResult<NotificationPrefs> {
            up(self)?;
            self.server.notification_prefs_get(&own(self)).map_err(map)
        }
        fn notification_prefs_put(&mut self, prefs: &NotificationPrefs) -> TResult<()> {
            up(self)?;
            self.server
                .notification_prefs_put(&own(self), prefs)
                .map_err(map)
        }
    }
}

#[cfg(feature = "http")]
pub use http::HttpCommentTransport;

#[cfg(feature = "http")]
mod http {
    use super::*;
    use penroam_sync_protocol::comments::UpdateNotificationRequest;
    use penroam_sync_protocol::framing::decode_json_batch;
    use penroam_sync_protocol::{routes, ErrorBody};
    use std::time::Duration;

    const JSON_LIMIT: u64 = 1 << 20;

    /// The comment and notification routes over HTTP(S), with a bearer token held in memory.
    /// Build one per session: a device session on the space for the comment routes, the member's
    /// account session for the notification routes (the same token as the engine's transport,
    /// [`crate::transport::HttpTransport::token`]).
    pub struct HttpCommentTransport {
        base: String,
        token: String,
        agent: ureq::Agent,
    }

    impl std::fmt::Debug for HttpCommentTransport {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("HttpCommentTransport")
                .field("base", &self.base)
                .finish_non_exhaustive()
        }
    }

    /// A dropped connection after sending may have been processed: a lost response, not offline.
    fn net(e: ureq::Error) -> TransportError {
        match e {
            ureq::Error::Io(ref io)
                if matches!(
                    io.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                ) =>
            {
                TransportError::ResponseLost
            }
            ureq::Error::Protocol(_) => TransportError::ResponseLost,
            other => TransportError::Network(other.to_string()),
        }
    }

    type Answer = (u16, ureq::http::HeaderMap, Vec<u8>);

    impl HttpCommentTransport {
        /// A client for `base_url` (trailing slash ignored) with `token`.
        pub fn new(base_url: &str, token: String) -> Self {
            Self {
                base: base_url.trim_end_matches('/').to_string(),
                token,
                agent: ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(60)))
                    .timeout_connect(Some(Duration::from_secs(10)))
                    .http_status_as_error(false)
                    .build()
                    .into(),
            }
        }

        /// Replaces the bearer token (after re-authentication).
        pub fn set_token(&mut self, token: String) {
            self.token = token;
        }

        fn url(&self, path: &str) -> String {
            format!("{}{}", self.base, path)
        }

        fn auth(&self) -> String {
            format!("Bearer {}", self.token)
        }

        fn finish(mut r: ureq::http::Response<ureq::Body>, limit: u64) -> TResult<Answer> {
            let status = r.status().as_u16();
            let headers = r.headers().clone();
            let body = r
                .body_mut()
                .with_config()
                .limit(limit)
                .read_to_vec()
                .map_err(net)?;
            if status >= 400 {
                return Err(match serde_json::from_slice::<ErrorBody>(&body) {
                    Ok(b) => TransportError::Api(b),
                    Err(_) if status >= 500 => TransportError::Api(ErrorBody::new(
                        ErrorCode::ServiceUnavailable,
                        format!("HTTP {status}"),
                        "",
                    )),
                    Err(_) => {
                        TransportError::Protocol(format!("HTTP {status} without an error body"))
                    }
                });
            }
            Ok((status, headers, body))
        }

        fn json<T: serde::de::DeserializeOwned>(b: &[u8]) -> TResult<T> {
            serde_json::from_slice(b).map_err(|e| TransportError::Protocol(e.to_string()))
        }
    }

    impl CommentTransport for HttpCommentTransport {
        fn comment_push(&mut self, frame: Vec<u8>) -> TResult<PushOpsResponse> {
            let r = self
                .agent
                .post(self.url(routes::COMMENTS_BATCH))
                .header("Authorization", self.auth())
                .header("Content-Type", routes::MEDIA_BATCH)
                .send(&frame[..])
                .map_err(net)?;
            Self::json(&Self::finish(r, JSON_LIMIT)?.2)
        }
        fn comment_pull(
            &mut self,
            req: &PullOpsRequest,
            limits: &Limits,
        ) -> TResult<PulledComments> {
            let r = self
                .agent
                .get(format!("{}?{}", self.url(routes::COMMENTS), req.to_query()))
                .header("Authorization", self.auth())
                .header("Accept", routes::MEDIA_BATCH)
                .call()
                .map_err(net)?;
            let (_, headers, body) = Self::finish(r, limits.max_batch_bytes + 1024)?;
            let h = |n: &str| {
                headers
                    .get(n)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let next = h(routes::HEADER_NEXT_CURSOR)
                .ok_or_else(|| TransportError::Protocol("pull without next cursor".into()))?;
            let next_cursor =
                Cursor::new(next).map_err(|e| TransportError::Protocol(e.to_string()))?;
            let has_more = h(routes::HEADER_HAS_MORE).as_deref() == Some("true");
            let items: Vec<(PulledComment, Vec<u8>)> = decode_json_batch(&body, limits)
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
            let response = PullCommentsResponse {
                items: items.iter().map(|(p, _)| p.clone()).collect(),
                next_cursor,
                has_more,
            };
            pulled_comments(response, items)
        }
        fn comment_ack(&mut self, req: &AckRequest) -> TResult<()> {
            let r = self
                .agent
                .post(self.url(routes::COMMENT_ACKS))
                .header("Authorization", self.auth())
                .send_json(req)
                .map_err(net)?;
            Self::finish(r, JSON_LIMIT).map(|_| ())
        }
        fn notifications_list(
            &mut self,
            cursor: Option<&Cursor>,
            limit: u32,
        ) -> TResult<NotificationPage> {
            let q = PullOpsRequest {
                cursor: cursor.cloned(),
                limit,
            }
            .to_query();
            let r = self
                .agent
                .get(format!("{}?{q}", self.url(routes::NOTIFICATIONS)))
                .header("Authorization", self.auth())
                .call()
                .map_err(net)?;
            Self::json(&Self::finish(r, JSON_LIMIT)?.2)
        }
        fn notification_update(
            &mut self,
            id: &NotificationId,
            read: bool,
        ) -> TResult<NotificationRecord> {
            let path = routes::NOTIFICATION.replace("{id}", id.as_str());
            let r = self
                .agent
                .patch(self.url(&path))
                .header("Authorization", self.auth())
                .send_json(UpdateNotificationRequest { read })
                .map_err(net)?;
            Self::json(&Self::finish(r, JSON_LIMIT)?.2)
        }
        fn notification_prefs_get(&mut self) -> TResult<NotificationPrefs> {
            let r = self
                .agent
                .get(self.url(routes::NOTIFICATION_PREFS))
                .header("Authorization", self.auth())
                .call()
                .map_err(net)?;
            Self::json(&Self::finish(r, JSON_LIMIT)?.2)
        }
        fn notification_prefs_put(&mut self, prefs: &NotificationPrefs) -> TResult<()> {
            let r = self
                .agent
                .put(self.url(routes::NOTIFICATION_PREFS))
                .header("Authorization", self.auth())
                .send_json(prefs)
                .map_err(net)?;
            Self::finish(r, JSON_LIMIT).map(|_| ())
        }
    }
}

// ---------------------------------------------------------------------------
// Notifications

/// Every notification after `cursor` (all pages), and the unread count.
pub fn notifications(
    t: &mut dyn CommentTransport,
    cursor: Option<&Cursor>,
) -> Result<(Vec<NotificationRecord>, u64, Cursor)> {
    let mut out = Vec::new();
    let mut cursor = cursor.cloned();
    loop {
        let page = t.notifications_list(cursor.as_ref(), 200)?;
        out.extend(page.notifications);
        let more = page.has_more;
        let unread = page.unread;
        cursor = Some(page.next_cursor);
        if !more {
            return Ok((out, unread, cursor.expect("set above")));
        }
    }
}

/// Marks a notification read (or unread).
pub fn mark_notification(
    t: &mut dyn CommentTransport,
    id: &NotificationId,
    read: bool,
) -> Result<NotificationRecord> {
    Ok(t.notification_update(id, read)?)
}

/// The caller's notification mutes.
pub fn notification_prefs(t: &mut dyn CommentTransport) -> Result<NotificationPrefs> {
    Ok(t.notification_prefs_get()?)
}

/// Replaces the caller's mutes (checked first: at most 500 of each kind, no repeats) and returns
/// what the server saved.
pub fn set_notification_prefs(
    t: &mut dyn CommentTransport,
    prefs: &NotificationPrefs,
) -> Result<NotificationPrefs> {
    prefs
        .validate()
        .map_err(|e| CommentSyncError::Invalid(e.to_string()))?;
    t.notification_prefs_put(prefs)?;
    Ok(t.notification_prefs_get()?)
}

// ---------------------------------------------------------------------------
// The durable log

const LOG_MAGIC: &[u8; 4] = b"PRCL";
const LOG_VERSION: u16 = 1;
const LOG_HEADER: usize = 8;
/// Upper bound on one record (an envelope plus its JSON), so a bad length can't cause a huge read.
const MAX_RECORD: usize = 4 << 20;

/// Append-only records `u32 len ‖ u32 crc32 ‖ payload`.
struct RecordLog {
    file: File,
    /// Appended but not yet fsynced.
    dirty: bool,
}

impl RecordLog {
    /// Opens or creates the log, returning every intact record. A torn tail is cut off.
    fn open(path: &Path) -> io::Result<(Self, Vec<Vec<u8>>)> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes.is_empty() {
            let mut h = LOG_MAGIC.to_vec();
            h.extend_from_slice(&LOG_VERSION.to_le_bytes());
            h.extend_from_slice(&[0, 0]);
            file.write_all(&h)?;
            file.sync_all()?;
            if let Some(dir) = path.parent() {
                // the new file's directory entry must be durable too
                if let Ok(d) = File::open(dir) {
                    let _ = d.sync_all();
                }
            }
            return Ok((Self { file, dirty: false }, Vec::new()));
        }
        if bytes.len() < LOG_HEADER || &bytes[..4] != LOG_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a comment log",
            ));
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != LOG_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("comment log version {version} is not supported"),
            ));
        }
        let mut records = Vec::new();
        let mut at = LOG_HEADER;
        while at < bytes.len() {
            let torn = || io::Error::new(io::ErrorKind::InvalidData, "damaged comment log record");
            if bytes.len() - at < 8 {
                break; // torn header
            }
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4")) as usize;
            let crc = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4"));
            if len > MAX_RECORD {
                return Err(torn());
            }
            let end = at + 8 + len;
            if end > bytes.len() {
                break; // torn payload
            }
            let payload = &bytes[at + 8..end];
            if crc32fast::hash(payload) != crc {
                if end == bytes.len() {
                    break; // the last record, half written
                }
                return Err(torn());
            }
            records.push(payload.to_vec());
            at = end;
        }
        if at < bytes.len() {
            file.set_len(at as u64)?;
            file.sync_all()?;
        }
        file.seek(SeekFrom::End(0))?;
        Ok((Self { file, dirty: false }, records))
    }

    fn append_buffered(&mut self, payload: &[u8]) -> io::Result<()> {
        let mut rec = Vec::with_capacity(payload.len() + 8);
        rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        rec.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
        rec.extend_from_slice(payload);
        self.file.write_all(&rec)?;
        self.dirty = true;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.dirty {
            self.file.sync_data()?;
            self.dirty = false;
        }
        Ok(())
    }
}

/// How far a local operation got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "s", rename_all = "camelCase")]
enum LocalStatus {
    Waiting,
    Sent,
    Draft,
    Failed {
        code: String,
    },
    /// Dropped by the user (a draft or failed comment), or a filler made unnecessary.
    Discarded,
}

/// One record of the log. JSON, tagged by `t`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
enum Record {
    /// A local operation with a sequence.
    Local { op: String },
    /// A local draft without a sequence (written while commenting wasn't allowed).
    Draft { op: String },
    /// A verified remote operation.
    Remote {
        seq: u64,
        op: String,
        member: String,
    },
    /// An envelope that can't be verified yet.
    Parked {
        seq: u64,
        item: PulledComment,
        ct: String,
        reason: String,
    },
    /// A parked envelope was verified or quarantined since.
    Unparked { op_id: String },
    /// An envelope that failed verification (kept, never applied).
    Quarantined {
        seq: u64,
        item: PulledComment,
        ct: String,
        reason: String,
    },
    /// A local operation's (or filler's) status changed.
    Status { op: Uuid, status: LocalStatus },
    /// The filler that takes the place of a local operation on the server.
    Filler { op: String, replaces: Uuid },
    /// The epoch a local operation is sealed with (fixed before the first upload).
    Sealed { op: Uuid, epoch: u32 },
    /// The member the server attributes a device to (first seen).
    Pin { device: String, member: String },
}

fn enc(op: &CommentOperation) -> String {
    base64url::encode(&op.encode())
}

fn dec(s: &str) -> Result<CommentOperation> {
    let b = base64url::decode(s).map_err(|_| CommentSyncError::Integrity("log record".into()))?;
    CommentOperation::decode(&b).map_err(|e| CommentSyncError::Integrity(e.to_string()))
}

/// `comments.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredState {
    format: String,
    version: u32,
    /// Pull cursor; everything before it is in the log.
    cursor: Option<String>,
    /// Last cursor acknowledged to the server.
    acked: Option<String>,
}

// ---------------------------------------------------------------------------
// The comment stream

/// Whether a comment reached the server, as the UI shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Delivery {
    /// Stored by the server.
    Sent,
    /// Queued; uploaded on the next sync.
    Waiting,
    /// The server refused it; it won't be retried (the UI may send the text again).
    Failed,
    /// Kept on this device only: this member may not comment.
    Draft,
}

impl Delivery {
    /// Rank for "the worst state of several operations".
    fn rank(self) -> u8 {
        match self {
            Delivery::Sent => 0,
            Delivery::Waiting => 1,
            Delivery::Draft => 2,
            Delivery::Failed => 3,
        }
    }
}

/// A change the user makes. Bodies are cleaned with [`sanitize_body`] before use.
#[derive(Debug, Clone, PartialEq)]
pub enum CommentIntent {
    /// Start a thread.
    CreateThread {
        /// What it is attached to.
        anchor: CommentAnchor,
        /// The first comment.
        body: String,
    },
    /// Reply in a thread.
    Reply {
        /// The thread.
        thread_id: Uuid,
        /// The text.
        body: String,
        /// The comment answered, if any.
        reply_to: Option<Uuid>,
    },
    /// Edit one's own comment.
    Edit {
        /// The comment.
        comment_id: Uuid,
        /// The new text.
        body: String,
    },
    /// Delete one's own comment, or (owner) remove anyone's.
    Delete {
        /// The comment.
        comment_id: Uuid,
    },
    /// Resolve or reopen a thread.
    SetResolved {
        /// The thread.
        thread_id: Uuid,
        /// Resolve (`true`) or reopen.
        resolved: bool,
    },
    /// Attach a thread to something else.
    Reattach {
        /// The thread.
        thread_id: Uuid,
        /// The new anchor.
        anchor: CommentAnchor,
    },
}

/// A local operation that isn't part of the merged threads: a draft, or a failed comment whose
/// place on the server a filler took. The UI shows these next to [`CommentSync::threads`].
#[derive(Debug, Clone, PartialEq)]
pub struct UnsentComment {
    /// The operation's id (a new thread's or comment's id too).
    pub op_id: Uuid,
    /// What it does.
    pub op: CommentOp,
    /// When it was written.
    pub at: i64,
    /// [`Delivery::Draft`] or [`Delivery::Failed`].
    pub delivery: Delivery,
}

/// An envelope that failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedComment {
    /// Its position in the stream (0 for one refused by the local merge).
    pub server_seq: u64,
    /// Its op id.
    pub op_id: String,
    /// The device that signed it (as the header claims).
    pub device_id: String,
    /// Why it was refused.
    pub reason: String,
}

/// What one [`CommentSync::sync`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommentReport {
    /// Envelopes uploaded (including retries the server answered `duplicate`).
    pub pushed: usize,
    /// Envelopes pulled.
    pub pulled: usize,
    /// Remote operations merged.
    pub applied: usize,
    /// Envelopes quarantined (security events).
    pub quarantined: usize,
    /// Envelopes parked (unknown device, missing key, newer schema).
    pub parked: usize,
    /// Local operations that became drafts.
    pub drafted: usize,
    /// Local operations the server refused.
    pub failed: usize,
    /// This member may not comment (role, or the server said `forbidden`/`access_revoked`).
    pub permission_lost: bool,
    /// An envelope came from a device the key log doesn't know yet: refresh it and sync again.
    pub needs_keylog: bool,
}

#[derive(Debug, Clone)]
struct LocalEntry {
    op: CommentOperation,
    status: LocalStatus,
    filler: Option<(CommentOperation, LocalStatus)>,
}

impl LocalEntry {
    /// The operation that stands for this sequence on the server and in the replica.
    fn standing(&self) -> (&CommentOperation, &LocalStatus) {
        match &self.filler {
            Some((f, s)) if *s != LocalStatus::Discarded => (f, s),
            _ => (&self.op, &self.status),
        }
    }
}

#[derive(Debug, Clone)]
struct Parked {
    seq: u64,
    item: PulledComment,
    ct: Vec<u8>,
}

/// One space's comment stream on this device. See the module docs.
pub struct CommentSync {
    dir: PathBuf,
    log: RecordLog,
    state: StoredState,
    device: Uuid,
    me: AccountId,
    // rebuilt from the log
    locals: BTreeMap<u64, LocalEntry>,
    local_ids: HashMap<Uuid, u64>,
    drafts: Vec<(CommentOperation, LocalStatus)>,
    remote: Vec<CommentOperation>,
    remote_ids: HashSet<Uuid>,
    parked: BTreeMap<String, Parked>,
    quarantined: Vec<QuarantinedComment>,
    pins: BTreeMap<String, String>,
    /// Original author id → the tombstone the server names since the account was erased.
    erased: BTreeMap<String, String>,
    sealed: HashMap<Uuid, u32>,
    replica: CommentReplica,
    waiting: Vec<CommentOperation>,
    /// Notifications found forged this session (security events), by notification id.
    forged: BTreeMap<String, &'static str>,
}

/// A derived id: stable for the same inputs (fillers must be identical on every retry).
fn derived(id: Uuid, label: &str) -> Uuid {
    let h = crypto::sha256(&[label.as_bytes(), id.as_bytes()].concat());
    uuid::Builder::from_random_bytes(h[..16].try_into().expect("16")).into_uuid()
}

impl CommentSync {
    /// Opens (or creates) the comment stream kept in `dir` for this device (`device`: its id in
    /// the space's key log) acting as member `me`.
    pub fn open(dir: &Path, device: Uuid, me: &AccountId) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let (log, records) = RecordLog::open(&dir.join("comments.log"))?;
        let state_path = dir.join("comments.json");
        let state = match std::fs::read(&state_path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| CommentSyncError::Integrity(format!("comments.json: {e}")))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StoredState {
                format: "penroam-comment-state".into(),
                version: 1,
                ..Default::default()
            },
            Err(e) => return Err(e.into()),
        };
        let mut me_ = Self {
            dir: dir.to_path_buf(),
            log,
            state,
            device,
            me: me.clone(),
            locals: BTreeMap::new(),
            local_ids: HashMap::new(),
            drafts: Vec::new(),
            remote: Vec::new(),
            remote_ids: HashSet::new(),
            parked: BTreeMap::new(),
            quarantined: Vec::new(),
            pins: BTreeMap::new(),
            erased: BTreeMap::new(),
            sealed: HashMap::new(),
            replica: CommentReplica::new(),
            waiting: Vec::new(),
            forged: BTreeMap::new(),
        };
        for raw in records {
            let r: Record = serde_json::from_slice(&raw)
                .map_err(|e| CommentSyncError::Integrity(format!("comment log: {e}")))?;
            me_.load(r)?;
        }
        me_.rebuild_replica();
        Ok(me_)
    }

    /// Folds one record into the in-memory state.
    fn load(&mut self, r: Record) -> Result<()> {
        match r {
            Record::Local { op } => {
                let op = dec(&op)?;
                self.local_ids.insert(op.id, op.sequence);
                self.locals.insert(
                    op.sequence,
                    LocalEntry {
                        op,
                        status: LocalStatus::Waiting,
                        filler: None,
                    },
                );
            }
            Record::Draft { op } => self.drafts.push((dec(&op)?, LocalStatus::Draft)),
            Record::Remote { op, member, .. } => {
                let op = dec(&op)?;
                self.note_erased(&op, &member);
                self.remote_ids.insert(op.id);
                self.remote.push(op);
            }
            Record::Parked { seq, item, ct, .. } => {
                let ct = base64url::decode(&ct)
                    .map_err(|_| CommentSyncError::Integrity("parked record".into()))?;
                self.parked.insert(
                    item.header.op_id.as_str().to_string(),
                    Parked { seq, item, ct },
                );
            }
            Record::Unparked { op_id } => {
                self.parked.remove(&op_id);
            }
            Record::Quarantined {
                seq, item, reason, ..
            } => self.quarantined.push(QuarantinedComment {
                server_seq: seq,
                op_id: item.header.op_id.as_str().into(),
                device_id: item.header.device_id.as_str().into(),
                reason,
            }),
            Record::Status { op, status } => self.set_status_mem(op, status),
            Record::Filler { op, replaces } => {
                let f = dec(&op)?;
                if let Some(seq) = self.local_ids.get(&replaces).copied() {
                    self.local_ids.insert(f.id, seq);
                    let e = self.locals.get_mut(&seq).expect("indexed");
                    e.filler = Some((f, LocalStatus::Waiting));
                }
            }
            Record::Sealed { op, epoch } => {
                self.sealed.insert(op, epoch);
            }
            Record::Pin { device, member } => {
                self.pins.insert(device, member);
            }
        }
        Ok(())
    }

    fn set_status_mem(&mut self, op: Uuid, status: LocalStatus) {
        if let Some(seq) = self.local_ids.get(&op) {
            let e = self.locals.get_mut(seq).expect("indexed");
            match &mut e.filler {
                Some((f, s)) if f.id == op => *s = status,
                _ => e.status = status,
            }
        } else if let Some(d) = self.drafts.iter_mut().find(|(d, _)| d.id == op) {
            d.1 = status;
        }
    }

    fn append(&mut self, r: &Record) -> Result<()> {
        let b = serde_json::to_vec(r).expect("record serializes");
        self.log.append_buffered(&b)?;
        Ok(())
    }

    /// Appends, fsyncs and folds a record.
    fn commit(&mut self, r: Record) -> Result<()> {
        self.append(&r)?;
        self.log.flush()?;
        self.load(r)
    }

    fn set_status(&mut self, op: Uuid, status: LocalStatus) -> Result<()> {
        self.append(&Record::Status {
            op,
            status: status.clone(),
        })?;
        self.set_status_mem(op, status);
        Ok(())
    }

    /// Rebuilds the merged threads from scratch (after a filler changed what stands for a
    /// sequence).
    fn rebuild_replica(&mut self) {
        self.replica = CommentReplica::new();
        let mut all: Vec<CommentOperation> = self.remote.clone();
        all.extend(self.locals.values().map(|e| e.standing().0.clone()));
        self.waiting = all;
        self.quarantined.retain(|q| q.server_seq != 0);
        self.settle();
    }

    /// Applies waiting operations until none makes progress. Returns how many were applied.
    fn settle(&mut self) -> usize {
        let mut applied = 0;
        loop {
            let mut progressed = false;
            let mut rest = Vec::new();
            for op in std::mem::take(&mut self.waiting) {
                match self.replica.apply(&op) {
                    Ok(_) => {
                        progressed = true;
                        applied += 1;
                    }
                    Err(CommentMergeError::MissingDependency(_)) => rest.push(op),
                    Err(e) => self.quarantined.push(QuarantinedComment {
                        server_seq: 0,
                        op_id: op.id.to_string(),
                        device_id: op.device_id.to_string(),
                        reason: e.to_string(),
                    }),
                }
            }
            self.waiting = rest;
            if !progressed {
                return applied;
            }
        }
    }

    // ------------------------------------------------------------- reading --

    /// The merged threads (every member sees the same ones once synced). Local drafts and
    /// failed comments are in [`Self::unsent`]. An erased member appears under the tombstone id
    /// the server gives them, never under the original id their payloads still carry.
    pub fn threads(&self) -> Vec<CommentThread> {
        let mut threads = self.replica.threads();
        if self.erased.is_empty() {
            return threads;
        }
        let shown = |a: &mut String| {
            if let Some(t) = self.erased.get(a.as_str()) {
                *a = t.clone();
            }
        };
        for t in &mut threads {
            shown(&mut t.created_by);
            if let Some(c) = &mut t.status_change {
                shown(&mut c.by);
            }
            for c in &mut t.comments {
                shown(&mut c.author);
            }
        }
        threads
    }

    /// Remembers that the author of `op` is erased, when the server attributes it to a tombstone.
    fn note_erased(&mut self, op: &CommentOperation, member: &str) {
        let erased = AccountId::new(member.to_string()).is_ok_and(|m| is_erased_account(&m));
        if erased && op.author != member {
            self.erased.insert(op.author.clone(), member.to_string());
        }
    }

    /// The merge model, for anchor resolution and lookups.
    pub fn replica(&self) -> &CommentReplica {
        &self.replica
    }

    /// The delivery state of one operation (`None` if unknown or discarded).
    pub fn delivery(&self, op: Uuid) -> Option<Delivery> {
        if self.remote_ids.contains(&op) {
            return Some(Delivery::Sent);
        }
        let status = if let Some(seq) = self.local_ids.get(&op) {
            let e = &self.locals[seq];
            if e.op.id != op {
                return None; // fillers are not the user's
            }
            &e.status
        } else {
            &self.drafts.iter().find(|(d, _)| d.id == op)?.1
        };
        match status {
            LocalStatus::Waiting => Some(Delivery::Waiting),
            LocalStatus::Sent => Some(Delivery::Sent),
            LocalStatus::Draft => Some(Delivery::Draft),
            LocalStatus::Failed { .. } => Some(Delivery::Failed),
            LocalStatus::Discarded => None,
        }
    }

    /// The worst delivery state of the local operations that wrote, edited or deleted a comment
    /// ([`Delivery::Sent`] for someone else's).
    pub fn comment_delivery(&self, comment: Uuid) -> Delivery {
        self.locals
            .values()
            .map(|e| &e.op)
            .chain(self.drafts.iter().map(|(d, _)| d))
            .filter(|o| match &o.op {
                CommentOp::ThreadCreated { thread_id, .. } => *thread_id == comment,
                CommentOp::Replied { comment_id, .. }
                | CommentOp::CommentEdited { comment_id, .. }
                | CommentOp::CommentDeleted { comment_id } => *comment_id == comment,
                _ => false,
            })
            .filter_map(|o| self.delivery(o.id))
            .max_by_key(|d| d.rank())
            .unwrap_or(Delivery::Sent)
    }

    /// Drafts, and failed operations a filler replaced, oldest first.
    pub fn unsent(&self) -> Vec<UnsentComment> {
        let mut out: Vec<UnsentComment> = self
            .locals
            .values()
            .filter(|e| e.standing().0.id != e.op.id)
            .filter_map(|e| {
                Some(UnsentComment {
                    op_id: e.op.id,
                    op: e.op.op.clone(),
                    at: e.op.at,
                    delivery: self.delivery(e.op.id)?,
                })
            })
            .collect();
        out.extend(self.drafts.iter().filter_map(|(d, _)| {
            Some(UnsentComment {
                op_id: d.id,
                op: d.op.clone(),
                at: d.at,
                delivery: self.delivery(d.id)?,
            })
        }));
        out.sort_by_key(|u| (u.at, u.op_id));
        out
    }

    /// Local operations not yet stored by the server (waiting to upload).
    pub fn waiting_count(&self) -> usize {
        self.locals
            .values()
            .filter(|e| *e.standing().1 == LocalStatus::Waiting)
            .count()
    }

    /// Envelopes refused by verification or the merge (security events), oldest first.
    pub fn quarantined(&self) -> &[QuarantinedComment] {
        &self.quarantined
    }

    /// Envelopes parked until keys, devices or an upgrade arrive.
    pub fn parked_count(&self) -> usize {
        self.parked.len()
    }

    // ------------------------------------------------------------- writing --

    /// Records a change made by this member and returns its operation id (a new thread's or
    /// reply's id). Returns only once the change is durable on disk.
    ///
    /// With `may_comment` false (the caller's view of the role) the change is kept as a
    /// [`Delivery::Draft`] and never uploaded. `is_owner` lets a delete remove someone else's
    /// comment. Errors: an invalid body or anchor ([`CommentSyncError::Comment`]), or a change the
    /// merge rules refuse ([`CommentSyncError::NotPermitted`]: someone else's comment, an unknown
    /// thread or comment).
    pub fn submit(
        &mut self,
        intent: CommentIntent,
        may_comment: bool,
        is_owner: bool,
        at: i64,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let clean = |b: &str| sanitize_body(b);
        let mut moderation = false;
        let op = match intent {
            CommentIntent::CreateThread { anchor, body } => CommentOp::ThreadCreated {
                thread_id: id,
                anchor,
                body: clean(&body),
            },
            CommentIntent::Reply {
                thread_id,
                body,
                reply_to,
            } => CommentOp::Replied {
                thread_id,
                comment_id: id,
                body: clean(&body),
                reply_to,
            },
            CommentIntent::Edit { comment_id, body } => CommentOp::CommentEdited {
                comment_id,
                body: clean(&body),
            },
            CommentIntent::Delete { comment_id } => {
                let author =
                    self.comment_author(comment_id)
                        .ok_or(CommentSyncError::NotPermitted(Rejection::UnknownComment(
                            comment_id,
                        )))?;
                if author != self.me.as_str() {
                    if !is_owner {
                        return Err(CommentSyncError::NotPermitted(Rejection::NotAuthor(
                            comment_id,
                        )));
                    }
                    moderation = true;
                }
                CommentOp::CommentDeleted { comment_id }
            }
            CommentIntent::SetResolved {
                thread_id,
                resolved,
            } => {
                if resolved {
                    CommentOp::ThreadResolved { thread_id }
                } else {
                    CommentOp::ThreadReopened { thread_id }
                }
            }
            CommentIntent::Reattach { thread_id, anchor } => {
                CommentOp::AnchorReattached { thread_id, anchor }
            }
        };
        let next = self.locals.keys().next_back().copied().unwrap_or(0) + 1;
        let op = CommentOperation {
            id,
            device_id: self.device,
            sequence: next,
            schema_version: COMMENT_SCHEMA_VERSION,
            causal_dependencies: self.replica.frontier(),
            author: self.me.as_str().to_string(),
            moderation,
            at,
            op,
        };
        op.validate()?;
        // the merge rules decide exactly as every replica will: refuse here what they'd reject
        if let CommentApplied::Rejected(r) = self
            .replica
            .clone()
            .apply(&op)
            .map_err(|e| CommentSyncError::Integrity(e.to_string()))?
        {
            return Err(CommentSyncError::NotPermitted(r));
        }
        if !may_comment {
            self.commit(Record::Draft { op: enc(&op) })?;
            return Ok(id);
        }
        self.commit(Record::Local { op: enc(&op) })?;
        self.replica
            .apply(&op)
            .map_err(|e| CommentSyncError::Integrity(e.to_string()))?;
        Ok(id)
    }

    /// The author of a comment in the merged threads.
    fn comment_author(&self, comment: Uuid) -> Option<String> {
        self.replica.threads().into_iter().find_map(|t| {
            t.comments
                .into_iter()
                .find(|c| c.comment_id == comment)
                .map(|c| c.author)
        })
    }

    /// Drops a draft or failed operation (the UI sends the text again as a new comment). A
    /// queued draft that holds a sequence keeps its filler, so nothing is left out on the
    /// server. Returns whether something was dropped.
    pub fn discard(&mut self, op: Uuid) -> Result<bool> {
        let droppable = matches!(
            self.delivery(op),
            Some(Delivery::Draft) | Some(Delivery::Failed)
        );
        if !droppable {
            return Ok(false);
        }
        if let Some(&seq) = self.local_ids.get(&op) {
            if self.locals[&seq].filler.is_none() {
                self.add_filler(seq)?;
            }
        }
        self.set_status(op, LocalStatus::Discarded)?;
        self.log.flush()?;
        self.rebuild_replica();
        Ok(true)
    }

    /// Writes the filler for the local operation at `seq` (see the module docs).
    fn add_filler(&mut self, seq: u64) -> Result<()> {
        let orig = self.locals[&seq].op.clone();
        let f = CommentOperation {
            id: derived(orig.id, "penroam/comment-filler/v1"),
            op: CommentOp::ThreadReopened {
                thread_id: derived(orig.id, "penroam/comment-filler-thread/v1"),
            },
            moderation: false,
            ..orig.clone()
        };
        self.append(&Record::Filler {
            op: enc(&f),
            replaces: orig.id,
        })?;
        self.local_ids.insert(f.id, seq);
        self.locals.get_mut(&seq).expect("exists").filler = Some((f, LocalStatus::Waiting));
        Ok(())
    }

    // ---------------------------------------------------------------- sync --

    /// One exchange with the server: upload what is queued (or turn it into drafts when this
    /// member may not comment), then pull, verify and merge everything new, then acknowledge.
    ///
    /// Everything that arrives is durable before the cursor moves. A network failure leaves the
    /// queue as it was ([`TransportError::is_retryable`]); call again later.
    pub fn sync(
        &mut self,
        t: &mut dyn CommentTransport,
        keys: &SpaceKeys<'_>,
        roster: &SpaceRoster,
        limits: &Limits,
    ) -> Result<CommentReport> {
        if keys.device.device_id != self.device.hyphenated().to_string() {
            return Err(CommentSyncError::Invalid(
                "these keys belong to another device".into(),
            ));
        }
        let mut report = CommentReport::default();
        self.retry_parked(keys, roster, &mut report)?;
        let push = self.push(t, keys, roster, limits, &mut report);
        match push {
            Ok(()) => {}
            // revoked members can't pull either; anything else, pull anyway
            Err(CommentSyncError::Transport(TransportError::Api(b)))
                if b.code == ErrorCode::AccessRevoked =>
            {
                return Ok(report);
            }
            Err(e) => return Err(e),
        }
        match self.pull(t, keys, roster, limits, &mut report) {
            Err(CommentSyncError::Transport(TransportError::Api(b)))
                if matches!(b.code, ErrorCode::AccessRevoked | ErrorCode::NotFound) =>
            {
                report.permission_lost = true;
                self.draft_waiting(&mut report)?;
                Ok(report)
            }
            other => other.map(|()| report),
        }
    }

    /// Every queued operation becomes a draft.
    fn draft_waiting(&mut self, report: &mut CommentReport) -> Result<()> {
        let ids: Vec<Uuid> = self
            .locals
            .values()
            .filter(|e| e.filler.is_none() && e.status == LocalStatus::Waiting)
            .map(|e| e.op.id)
            .collect();
        for id in ids {
            self.set_status(id, LocalStatus::Draft)?;
            report.drafted += 1;
        }
        self.log.flush()?;
        Ok(())
    }

    fn push(
        &mut self,
        t: &mut dyn CommentTransport,
        keys: &SpaceKeys<'_>,
        roster: &SpaceRoster,
        limits: &Limits,
        report: &mut CommentReport,
    ) -> Result<()> {
        if !roster.may_comment() {
            report.permission_lost = true;
            return self.draft_waiting(report);
        }
        // drafts and failures that hold a sequence get their filler before anything uploads
        let need: Vec<u64> = self
            .locals
            .iter()
            .filter(|(_, e)| {
                e.filler.is_none()
                    && matches!(e.status, LocalStatus::Draft | LocalStatus::Failed { .. })
            })
            .map(|(s, _)| *s)
            .collect();
        if !need.is_empty() {
            for seq in need {
                self.add_filler(seq)?;
            }
            self.log.flush()?;
            self.rebuild_replica();
        }
        let mut without_sidecar: HashSet<Uuid> = HashSet::new();
        for _round in 0..4 {
            let queue: Vec<CommentOperation> = self
                .locals
                .values()
                .filter(|e| *e.standing().1 == LocalStatus::Waiting)
                .map(|e| e.standing().0.clone())
                .collect();
            if queue.is_empty() {
                return Ok(());
            }
            // fix each operation's epoch before its first upload, so retries are identical
            for op in &queue {
                if !self.sealed.contains_key(&op.id) {
                    let epoch = keys.membership.current_epoch;
                    self.append(&Record::Sealed { op: op.id, epoch })?;
                    self.sealed.insert(op.id, epoch);
                }
            }
            self.log.flush()?;
            let mut again = false;
            let max_items = PUSH_BATCH.min(limits.max_batch_envelopes.max(1) as usize);
            let mut i = 0;
            while i < queue.len() {
                let mut items: Vec<(CommentPushItem, Vec<u8>)> = Vec::new();
                let mut bytes = 0u64;
                while i < queue.len() && items.len() < max_items {
                    let op = &queue[i];
                    let (h, ct) = seal(op, self.sealed[&op.id], keys)?;
                    let size = ct.len() as u64 + ITEM_OVERHEAD;
                    if !items.is_empty() && bytes + size > limits.max_batch_bytes {
                        break;
                    }
                    bytes += size;
                    let mentions = if without_sidecar.contains(&op.id) {
                        Vec::new()
                    } else {
                        self.sidecar_mentions(op, roster)
                    };
                    items.push((
                        CommentPushItem {
                            envelope: h,
                            thread_id: self.routing_thread(op)?,
                            // only new comments notify the thread's participants
                            kind: match op.op {
                                CommentOp::ThreadCreated { .. } | CommentOp::Replied { .. } => {
                                    CommentPushKind::Comment
                                }
                                _ => CommentPushKind::Update,
                            },
                            mentions,
                        },
                        ct,
                    ));
                    i += 1;
                }
                let frame = penroam_sync_protocol::framing::encode_json_batch(&items, limits)
                    .map_err(|e| CommentSyncError::Invalid(e.to_string()))?;
                let resp = match t.comment_push(frame) {
                    Ok(r) => r,
                    Err(TransportError::Api(b))
                        if matches!(b.code, ErrorCode::Forbidden | ErrorCode::AccessRevoked) =>
                    {
                        report.permission_lost = true;
                        self.draft_waiting(report)?;
                        return if b.code == ErrorCode::AccessRevoked {
                            Err(TransportError::Api(b).into())
                        } else {
                            Ok(())
                        };
                    }
                    Err(e) => return Err(e.into()),
                };
                report.pushed += items.len();
                let sent: HashMap<String, (Uuid, bool)> = items
                    .iter()
                    .map(|(it, _)| {
                        let id = Uuid::parse_str(it.envelope.op_id.as_str()).expect("ours");
                        (
                            it.envelope.op_id.as_str().to_string(),
                            (id, !it.mentions.is_empty()),
                        )
                    })
                    .collect();
                let mut rebuild = false;
                for ack in resp.acks {
                    let Some(&(id, had_mentions)) = sent.get(ack.op_id.as_str()) else {
                        continue;
                    };
                    match ack.result {
                        AckResult::Accepted { .. } | AckResult::Duplicate { .. } => {
                            self.set_status(id, LocalStatus::Sent)?;
                        }
                        AckResult::Rejected { error } => {
                            let is_filler = self.is_filler(id);
                            match error.code {
                                ErrorCode::ValidationFailed if had_mentions => {
                                    // only the unsigned side-car can make a valid comment
                                    // fail validation: send it once more without
                                    without_sidecar.insert(id);
                                    again = true;
                                }
                                ErrorCode::OpIdConflict if is_filler => {
                                    // the sequence is taken: the original got there after all
                                    let seq = self.local_ids[&id];
                                    let orig = self.locals[&seq].op.id;
                                    self.set_status(orig, LocalStatus::Sent)?;
                                    self.set_status(id, LocalStatus::Discarded)?;
                                    rebuild = true;
                                }
                                code if code.is_retryable() => {}
                                code => {
                                    let seq = self.local_ids[&id];
                                    let orig = self.locals[&seq].op.id;
                                    if is_filler {
                                        return Err(CommentSyncError::Integrity(format!(
                                            "the server refused a filler: {code}"
                                        )));
                                    }
                                    self.set_status(
                                        orig,
                                        LocalStatus::Failed {
                                            code: code.to_string(),
                                        },
                                    )?;
                                    self.add_filler(seq)?;
                                    report.failed += 1;
                                    rebuild = true;
                                    again = true;
                                }
                            }
                        }
                    }
                }
                self.log.flush()?;
                if rebuild {
                    self.rebuild_replica();
                }
            }
            if !again {
                return Ok(());
            }
        }
        Ok(())
    }

    fn is_filler(&self, id: Uuid) -> bool {
        self.local_ids
            .get(&id)
            .is_some_and(|s| self.locals[s].op.id != id)
    }

    /// The server-visible thread id of an operation: the thread it belongs to (a filler names
    /// the made-up thread it reopens).
    fn routing_thread(&self, op: &CommentOperation) -> Result<ThreadId> {
        let thread = match &op.op {
            CommentOp::CommentEdited { comment_id, .. }
            | CommentOp::CommentDeleted { comment_id } => self
                .replica
                .threads()
                .into_iter()
                .find(|t| t.comments.iter().any(|c| c.comment_id == *comment_id))
                .map(|t| t.thread_id)
                .unwrap_or(*comment_id),
            other => other.thread_id().expect("names a thread"),
        };
        ThreadId::new(thread.hyphenated().to_string())
            .map_err(|e| CommentSyncError::Invalid(e.to_string()))
    }

    /// Whether a notification is backed by a verified comment (see [`NotificationCheck`]). The
    /// server builds notifications from the push side-car, which isn't signed, so a member could
    /// name a thread or mention someone the comment doesn't. Only a [`NotificationCheck::Genuine`]
    /// notification should be shown.
    pub fn check_notification(&self, n: &NotificationRecord) -> NotificationCheck {
        use NotificationCheck::*;
        let Ok(op_id) = Uuid::parse_str(n.comment_op_id.as_str()) else {
            return Forged("not a comment operation id");
        };
        if self
            .quarantined
            .iter()
            .any(|q| q.op_id == op_id.to_string() || q.op_id == n.comment_op_id.as_str())
        {
            return Forged("names a quarantined envelope");
        }
        if self.local_ids.contains_key(&op_id) {
            return Forged("names this member's own comment");
        }
        if self.parked.contains_key(n.comment_op_id.as_str())
            || self.waiting.iter().any(|o| o.id == op_id)
        {
            return Pending;
        }
        let Some(op) = self.remote.iter().find(|o| o.id == op_id) else {
            return Pending; // not pulled yet
        };
        // After an erasure the server names the author's tombstone while the payload keeps the
        // original id. Accept the tombstone this device learned for the author, or any tombstone
        // when it pulled the comment before the erasure and so never learned one; the checks
        // below on the comment itself still apply, and `actor` only decides what is shown.
        let actor_ok = op.author == n.actor.as_str()
            || (is_erased_account(&n.actor)
                && self
                    .erased
                    .get(&op.author)
                    .is_none_or(|t| t == n.actor.as_str()));
        if !actor_ok {
            return Forged("actor isn't the comment's author");
        }
        // the comment the operation wrote or edited, as merged
        let target = match &op.op {
            CommentOp::ThreadCreated { thread_id, .. } => *thread_id,
            CommentOp::Replied { comment_id, .. } | CommentOp::CommentEdited { comment_id, .. } => {
                *comment_id
            }
            _ => return Forged("operation isn't a comment"),
        };
        let Some((thread, comment)) = self.replica.threads().into_iter().find_map(|t| {
            let c = t.comments.iter().find(|c| c.comment_id == target)?.clone();
            Some((t, c))
        }) else {
            return Forged("the merge rejected the operation");
        };
        if thread.thread_id.hyphenated().to_string() != n.thread_id.as_str() {
            return Forged("names another thread");
        }
        if matches!(op.op, CommentOp::CommentEdited { .. }) && comment.author != op.author {
            return Forged("the merge rejected the edit");
        }
        if comment.state != penroam_core::comment::CommentState::Visible {
            return Hidden; // genuine, but deleted since
        }
        let me = self.me.as_str();
        match n.kind {
            NotificationKind::Mention => {
                let body = op.op.body().unwrap_or_default();
                if parse_mentions(body).iter().any(|m| m == me) {
                    Genuine
                } else {
                    Forged("the comment doesn't mention the recipient")
                }
            }
            NotificationKind::Reply => {
                if !matches!(op.op, CommentOp::Replied { .. }) {
                    return Forged("a reply notification for something that isn't a reply");
                }
                if thread
                    .comments
                    .iter()
                    .any(|c| c.author == me && c.comment_id != target)
                {
                    Genuine
                } else {
                    Forged("the recipient hasn't commented in the thread")
                }
            }
            NotificationKind::Unknown => Hidden,
        }
    }

    /// [`Self::check_notification`] as a yes/no for showing it. A forged notification is
    /// recorded as a security event ([`Self::forged_notifications`]); a pending one is just not
    /// shown yet (sync and ask again).
    pub fn verify_notification(&mut self, n: &NotificationRecord) -> bool {
        match self.check_notification(n) {
            NotificationCheck::Genuine => true,
            NotificationCheck::Forged(why) => {
                self.forged.insert(n.notification_id.to_string(), why);
                false
            }
            NotificationCheck::Pending | NotificationCheck::Hidden => false,
        }
    }

    /// Notifications found forged since this stream was opened: (notification id, why).
    pub fn forged_notifications(&self) -> Vec<(String, &'static str)> {
        self.forged.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }

    /// The comment an operation wrote, edited or deleted (for a notification, which names the
    /// operation): a new thread's first comment is the thread id. `None` for operations on a
    /// thread (resolve, reopen, reattach) or ones this device doesn't have.
    pub fn comment_of_op(&self, op: Uuid) -> Option<Uuid> {
        let found = self
            .remote
            .iter()
            .chain(self.locals.values().map(|e| &e.op))
            .find(|o| o.id == op)?;
        match &found.op {
            CommentOp::ThreadCreated { thread_id, .. } => Some(*thread_id),
            CommentOp::Replied { comment_id, .. }
            | CommentOp::CommentEdited { comment_id, .. }
            | CommentOp::CommentDeleted { comment_id } => Some(*comment_id),
            _ => None,
        }
    }

    /// The thread an operation belongs to (for a notification, which names the operation), if
    /// this device has the operation and its thread.
    pub fn thread_of_op(&self, op: Uuid) -> Option<Uuid> {
        let found = self
            .remote
            .iter()
            .chain(self.locals.values().map(|e| &e.op))
            .find(|o| o.id == op)?;
        match &found.op {
            CommentOp::CommentEdited { comment_id, .. }
            | CommentOp::CommentDeleted { comment_id } => self
                .replica
                .threads()
                .into_iter()
                .find(|t| t.comments.iter().any(|c| c.comment_id == *comment_id))
                .map(|t| t.thread_id),
            other => other.thread_id(),
        }
    }

    /// Mentions for the side-car: active members other than the author, at most 20.
    fn sidecar_mentions(&self, op: &CommentOperation, roster: &SpaceRoster) -> Vec<AccountId> {
        let Some(body) = op.op.body() else {
            return Vec::new();
        };
        parse_mentions(body)
            .into_iter()
            .filter_map(|m| AccountId::new(m).ok())
            .filter(|m| roster.active.contains(m) && *m != roster.me)
            .take(MAX_MENTIONS)
            .collect()
    }

    fn retry_parked(
        &mut self,
        keys: &SpaceKeys<'_>,
        roster: &SpaceRoster,
        report: &mut CommentReport,
    ) -> Result<()> {
        let parked: Vec<Parked> = self.parked.values().cloned().collect();
        for p in parked {
            let op_id = p.item.header.op_id.as_str().to_string();
            match self.verify(&p.item, &p.ct, keys, roster) {
                Verdict::Park(_) => continue,
                v => {
                    self.take(p.seq, &p.item, &p.ct, v, report)?;
                    self.append(&Record::Unparked {
                        op_id: op_id.clone(),
                    })?;
                    self.parked.remove(&op_id);
                }
            }
        }
        self.log.flush()?;
        report.applied += self.settle();
        Ok(())
    }

    /// Records the verdict on a pulled envelope (not flushed).
    fn take(
        &mut self,
        seq: u64,
        item: &PulledComment,
        ct: &[u8],
        verdict: Verdict,
        report: &mut CommentReport,
    ) -> Result<()> {
        match verdict {
            Verdict::Accept(op) => {
                let op = *op;
                self.note_erased(&op, item.member.as_str());
                if !self.pins.contains_key(item.header.device_id.as_str())
                    && !is_erased_account(&item.member)
                {
                    let r = Record::Pin {
                        device: item.header.device_id.as_str().into(),
                        member: item.member.as_str().into(),
                    };
                    self.append(&r)?;
                    self.load(r)?;
                }
                let r = Record::Remote {
                    seq,
                    op: enc(&op),
                    member: item.member.as_str().into(),
                };
                self.append(&r)?;
                self.remote_ids.insert(op.id);
                self.remote.push(op.clone());
                self.waiting.push(op);
            }
            Verdict::Park(reason) => {
                if reason == "unknown-device" || reason == "key" {
                    report.needs_keylog = true;
                }
                report.parked += 1;
                let r = Record::Parked {
                    seq,
                    item: item.clone(),
                    ct: base64url::encode(ct),
                    reason: reason.into(),
                };
                self.append(&r)?;
                self.load(r)?;
            }
            Verdict::Quarantine(reason) => {
                report.quarantined += 1;
                let r = Record::Quarantined {
                    seq,
                    item: item.clone(),
                    ct: base64url::encode(ct),
                    reason,
                };
                self.append(&r)?;
                self.load(r)?;
            }
        }
        Ok(())
    }

    fn pull(
        &mut self,
        t: &mut dyn CommentTransport,
        keys: &SpaceKeys<'_>,
        roster: &SpaceRoster,
        limits: &Limits,
        report: &mut CommentReport,
    ) -> Result<()> {
        loop {
            let cursor = match &self.state.cursor {
                Some(c) => Some(
                    Cursor::new(c.clone())
                        .map_err(|e| CommentSyncError::Integrity(e.to_string()))?,
                ),
                None => None,
            };
            let page = t.comment_pull(
                &PullOpsRequest {
                    cursor,
                    limit: PULL_PAGE,
                },
                limits,
            )?;
            let mut rebuild = false;
            for (item, ct) in page.response.items.iter().zip(&page.ciphertexts) {
                report.pulled += 1;
                let id = Uuid::parse_str(item.header.op_id.as_str()).ok();
                // our own operations come back: they are stored, nothing to merge
                if let Some(id) = id.filter(|i| self.local_ids.contains_key(i)) {
                    let seq = self.local_ids[&id];
                    let e = &self.locals[&seq];
                    let is_original = e.op.id == id;
                    let original_sent = e.status == LocalStatus::Sent;
                    let filler = e.filler.as_ref().map(|(f, s)| (f.id, s.clone()));
                    if is_original {
                        if !original_sent {
                            self.set_status(id, LocalStatus::Sent)?;
                        }
                        if let Some((fid, s)) = filler {
                            if s != LocalStatus::Discarded {
                                self.set_status(fid, LocalStatus::Discarded)?;
                                rebuild = true;
                            }
                        }
                    } else if filler.is_some_and(|(_, s)| s != LocalStatus::Sent) {
                        self.set_status(id, LocalStatus::Sent)?;
                    }
                    continue;
                }
                if id.is_some_and(|i| self.remote_ids.contains(&i))
                    || self.parked.contains_key(item.header.op_id.as_str())
                {
                    continue;
                }
                let v = self.verify(item, ct, keys, roster);
                self.take(item.server_seq, item, ct, v, report)?;
            }
            // everything on this page is in the log before the cursor moves past it
            self.log.flush()?;
            if rebuild {
                self.rebuild_replica();
            } else {
                report.applied += self.settle();
            }
            let next = page.response.next_cursor.as_str().to_string();
            if self.state.cursor.as_deref() != Some(next.as_str()) {
                self.state.cursor = Some(next);
                self.save_state()?;
            }
            if !page.response.has_more {
                break;
            }
        }
        if let Some(c) = self.state.cursor.clone() {
            if self.state.acked.as_deref() != Some(c.as_str()) {
                t.comment_ack(&AckRequest {
                    applied_cursor: Cursor::new(c.clone())
                        .map_err(|e| CommentSyncError::Integrity(e.to_string()))?,
                })?;
                self.state.acked = Some(c);
                self.save_state()?;
            }
        }
        Ok(())
    }

    fn save_state(&self) -> Result<()> {
        let b = serde_json::to_vec_pretty(&self.state).expect("state serializes");
        write_atomic(&self.dir.join("comments.json"), &b)?;
        Ok(())
    }

    /// The checks of the module docs, in order.
    fn verify(
        &self,
        item: &PulledComment,
        ct: &[u8],
        keys: &SpaceKeys<'_>,
        roster: &SpaceRoster,
    ) -> Verdict {
        let h = &item.header;
        let q = |r: &str| Verdict::Quarantine(r.to_string());
        if let Err(e) = h.validate(&Limits::HARD_MAX) {
            return q(e.kind());
        }
        if verify_ciphertext(h, ct).is_err() {
            return q("ciphertext does not match its declared hash");
        }
        if h.op_schema_version > COMMENT_SCHEMA_VERSION {
            return Verdict::Park("upgrade");
        }
        let Some(member) = keys.membership.devices.get(h.device_id.as_str()) else {
            return Verdict::Park("unknown-device");
        };
        let Ok(pk) = member.cert.sign_pk() else {
            return q("unreadable device key");
        };
        let sig = base64url::decode(&h.signature).unwrap_or_default();
        if crypto::verify(&pk, b"", &canonical_signing_bytes(h), &sig).is_err() {
            return q("bad signature");
        }
        // No removal cutoff here: the key log's `RemoveDevice` cutoff (`last_seq`) counts the
        // removed device's *document* stream, and comment sequences are a separate counter, so
        // applying it would refuse every comment a removed member ever wrote. The comment stream
        // needs a cutoff of its own in the key log; until then a removed device's comments are
        // accepted on its signature (in a space the server, which keeps the root, could add
        // devices anyway).
        let Some(ek) = keys.epoch_keys.get(&h.key_epoch) else {
            return Verdict::Park("key");
        };
        let alias = ek.routing_id(keys.notebook);
        if h.notebook_id.as_str() != alias {
            return q("routing alias of another notebook");
        }
        let Ok(plain) = crypto::open(&comment_key(ek, &alias), &canonical_aad_bytes(h), ct) else {
            return q("authentication tag mismatch (not sealed as a comment)");
        };
        if !is_comment_payload(&plain) {
            return q("payload is not a comment operation");
        }
        let op = match CommentOperation::decode(&plain) {
            Ok(op) => op,
            Err(CommentError::NewerSchema(_)) => return Verdict::Park("upgrade"),
            Err(e) => return Verdict::Quarantine(format!("payload: {e}")),
        };
        // the server and other devices act on the header: the payload must say the same
        let mut deps: Vec<(String, u64)> = op
            .causal_dependencies
            .iter()
            .map(|d| (d.device_id.hyphenated().to_string(), d.device_seq))
            .collect();
        deps.sort();
        let mut hdeps: Vec<(String, u64)> = h
            .depends_on
            .iter()
            .map(|d| (d.device_id.as_str().to_string(), d.device_seq))
            .collect();
        hdeps.sort();
        if op.id.hyphenated().to_string() != h.op_id.as_str()
            || op.device_id.hyphenated().to_string() != h.device_id.as_str()
            || op.sequence != h.device_seq
            || op.schema_version != h.op_schema_version
            || deps != hdeps
        {
            return q("payload does not match its signed header");
        }
        // attribution: the member the server names, pinned per device on first sight. Once an
        // account is erased the server names its tombstone, while the signed payload keeps the
        // original id: the signature (from a device of the key log) is then what vouches for the
        // author, who must not be anyone still in the space.
        let device = h.device_id.as_str();
        let erased = is_erased_account(&item.member);
        if erased
            && (roster.active.iter().any(|m| m.as_str() == op.author)
                || op.author == item.member.as_str())
        {
            return q("erased member's comment names a current member as its author");
        }
        let names = |m: &str| m == item.member.as_str() || (erased && m == op.author);
        if let Some(pinned) = self.pins.get(device) {
            if !names(pinned) {
                return q("device attributed to another member than before");
            }
        }
        if let Some(listed) = roster.devices.get(device) {
            if !names(listed.as_str()) {
                return q("device attributed to another member than the roster says");
            }
        }
        let attributed = if erased {
            op.author.as_str()
        } else {
            item.member.as_str()
        };
        // an erased member never owned the space (erasure deletes the spaces one owns)
        let owner = !erased && item.member == roster.owner;
        if let Err(e) = op.check_attribution(op.device_id, attributed, owner) {
            return Verdict::Quarantine(e.to_string());
        }
        Verdict::Accept(Box::new(op))
    }
}

/// What [`CommentSync::check_notification`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationCheck {
    /// The named operation is a verified comment in the named thread, by the named actor, that
    /// mentions the recipient (mention) or answers a thread the recipient commented in (reply).
    Genuine,
    /// The operation hasn't been pulled and verified yet: don't show it until it has.
    Pending,
    /// Genuine but not worth showing (the comment was deleted since, or an unknown kind).
    Hidden,
    /// The notification claims something the verified comment doesn't (a security event).
    Forged(&'static str),
}

/// What verification decided about a pulled envelope.
enum Verdict {
    Accept(Box<CommentOperation>),
    Park(&'static str),
    Quarantine(String),
}

/// Seals a comment operation for `epoch` (deterministic: retries give identical bytes).
pub fn seal(
    op: &CommentOperation,
    epoch: u32,
    keys: &SpaceKeys<'_>,
) -> Result<(EnvelopeHeader, Vec<u8>)> {
    let ek = keys
        .epoch_keys
        .get(&epoch)
        .ok_or_else(|| CommentSyncError::Integrity(format!("no key for epoch {epoch}")))?;
    let alias = ek.routing_id(keys.notebook);
    seal_payload(
        op,
        &op.encode(),
        epoch,
        &comment_key(ek, &alias),
        &alias,
        keys,
    )
}

/// Seals arbitrary `plaintext` under `key` with the header of `op`. Used by [`seal`]; public so
/// tests can build envelopes a misbehaving client would send.
pub fn seal_payload(
    op: &CommentOperation,
    plaintext: &[u8],
    epoch: u32,
    key: &[u8; 32],
    alias: &str,
    keys: &SpaceKeys<'_>,
) -> Result<(EnvelopeHeader, Vec<u8>)> {
    let bad = |e: penroam_sync_protocol::IdError| CommentSyncError::Invalid(e.to_string());
    let mut h = EnvelopeHeader {
        envelope_version: ENVELOPE_VERSION,
        op_id: OpId::new(op.id.hyphenated().to_string()).map_err(bad)?,
        device_id: DeviceId::new(op.device_id.hyphenated().to_string()).map_err(bad)?,
        device_seq: op.sequence,
        notebook_id: NotebookRoutingId::new(alias.to_string()).map_err(bad)?,
        depends_on: op
            .causal_dependencies
            .iter()
            .map(|d: &OperationRef| {
                Ok(OpRef {
                    device_id: DeviceId::new(d.device_id.hyphenated().to_string()).map_err(bad)?,
                    device_seq: d.device_seq,
                })
            })
            .collect::<Result<_>>()?,
        key_epoch: epoch,
        op_schema_version: op.schema_version,
        // the AAD covers the length but not the hash and signature, filled in below
        ciphertext_sha256: "0".repeat(64),
        ciphertext_length: crypto::sealed_len(plaintext.len()) as u64,
        signature: String::new(),
    };
    let ct = crypto::seal(key, op.id.as_bytes(), &canonical_aad_bytes(&h), plaintext);
    h.ciphertext_sha256 = crypto::sha256_hex(&ct);
    let sig = crypto::sign(&keys.device.signing, b"", &canonical_signing_bytes(&h));
    h.signature = base64url::encode(&sig);
    Ok((h, ct))
}
