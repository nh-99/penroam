//! Comments and mentions on shared notebooks (capability `comments.v1`, protocol 1.9).
//!
//! Each sharing space has a second durable stream next to its operation log: the **comment
//! stream**. It uses the same envelope, PRSB framing, signatures, key log, epochs and cursor rules
//! (protocol §4 to §15a) with its own `server_seq`, cursors and acks, at `/v1/comments*`. Comment
//! payloads are sealed under a different key (`HKDF(epoch, salt = alias, info =
//! "penroam/comment/v1")`, [`COMMENT_PAYLOAD_INFO`]) and decode only as comment operations; a
//! client rejects a comment-stream envelope that isn't one and a document-stream envelope that is
//! one. Because the server lets commenters write only this stream, that domain separation is what
//! keeps a commenter from smuggling document edits.
//!
//! The server never sees comment text. Per pushed envelope it sees the op id, the sender, the
//! thread's routing id and an optional plaintext side-car of mentioned account ids
//! ([`CommentPushItem`]), which it uses only to create **notifications** ([`NotificationRecord`]).
//! Notifications carry ids only, never text or titles.
//!
//! The normative description is `docs/protocol/sync-protocol-v1.md` §12h.

use serde::{Deserialize, Serialize};

use crate::dto::Cursor;
use crate::envelope::{EnvelopeError, EnvelopeHeader};
use crate::ids::{AccountId, NotificationId, OpId, ThreadId};
use crate::limits::Limits;
use crate::sharing::SpaceId;

/// HKDF-SHA256 `info` of the comment payload key (with `salt` = the notebook's routing alias).
pub const COMMENT_PAYLOAD_INFO: &str = "penroam/comment/v1";
/// Most accounts one comment may mention.
pub const MAX_MENTIONS: usize = 20;
/// Largest comment body a client writes, in UTF-8 bytes (inside the ciphertext; a client rule).
pub const MAX_COMMENT_BODY_BYTES: usize = 8192;
/// Most entries in each list of [`NotificationPrefs`].
pub const MAX_MUTES: usize = 500;
/// Most notifications in one [`NotificationPage`].
pub const NOTIFICATION_PAGE_MAX: u32 = 200;

/// Why a comment push item or a prefs document is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommentError {
    /// The envelope header failed validation.
    #[error(transparent)]
    Envelope(#[from] EnvelopeError),
    /// More than [`MAX_MENTIONS`] mentions, or one account mentioned twice.
    #[error("too many or repeated mentions")]
    Mentions,
    /// More than [`MAX_MUTES`] entries, or a repeated entry.
    #[error("too many or repeated mute entries")]
    Mutes,
}

/// What a pushed comment operation is, for notifications. Clients must set it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentPushKind {
    /// A new comment: a thread's first comment or a reply. Notifies the thread's earlier
    /// participants (`reply`) and makes the sender a participant. The default when absent.
    #[default]
    Comment,
    /// Anything else on an existing thread: an edit, a deletion, resolving, reopening or moving
    /// the anchor. Notifies only the accounts in `mentions`, and doesn't make the sender a
    /// participant.
    Update,
}

/// One item of a comment push: the header JSON of each PRSB item of `POST /v1/comments/batch`
/// (the item's body is the ciphertext, as for document pushes).
///
/// `thread_id` and `mentions` are the server-visible side-car. They are **not** signed and clients
/// never rely on them: the authoritative thread and mentions are inside the sealed comment. The
/// server uses them only to track thread participants and create notifications.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentPushItem {
    /// The signed envelope header, exactly as for the document stream.
    pub envelope: EnvelopeHeader,
    /// The thread this comment belongs to (a random id chosen when the thread was created).
    pub thread_id: ThreadId,
    /// A new comment or an update (see [`CommentPushKind`]). Clients must send it; a server reads
    /// an item without it as `comment`.
    #[serde(default)]
    pub kind: CommentPushKind,
    /// Accounts the comment mentions: at most [`MAX_MENTIONS`], no repeats. Each must be an
    /// active member of the space, or the server rejects this item with `validation_failed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<AccountId>,
}

impl CommentPushItem {
    /// Checks the envelope header against `limits` and the mention list's size and uniqueness.
    pub fn validate(&self, limits: &Limits) -> Result<(), CommentError> {
        self.envelope.validate(limits)?;
        if self.mentions.len() > MAX_MENTIONS {
            return Err(CommentError::Mentions);
        }
        let mut seen = std::collections::BTreeSet::new();
        if !self.mentions.iter().all(|m| seen.insert(m)) {
            return Err(CommentError::Mentions);
        }
        Ok(())
    }
}

/// One pulled comment-stream item: the header JSON of each PRSB item of `GET /v1/comments`, and
/// an entry of [`PullCommentsResponse::items`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PulledComment {
    /// Position in the space's comment stream.
    pub server_seq: u64,
    /// The signed envelope header.
    pub header: EnvelopeHeader,
    /// The thread routing id the sender gave.
    pub thread_id: ThreadId,
    /// The member whose session pushed it (server attribution). Clients still verify the
    /// signing device against the key log and check this matches that device's member.
    pub member: AccountId,
}

/// `GET /v1/comments?cursor=&limit=` JSON control document (same paging rules as
/// [`crate::PullOpsResponse`]; cursors of the two streams are not interchangeable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullCommentsResponse {
    /// Items in `server_seq` order.
    pub items: Vec<PulledComment>,
    /// Cursor after the last item (persist only after applying the items durably).
    pub next_cursor: Cursor,
    /// Whether more items existed when the query ran.
    pub has_more: bool,
}

/// What a notification is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// The recipient was mentioned.
    Mention,
    /// Someone commented in a thread the recipient has commented in.
    Reply,
    /// A kind this client doesn't know (from a newer server); show it generically or ignore it.
    #[serde(other)]
    Unknown,
}

/// One notification for the caller. Ids only: the client fetches and decrypts the comment itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationRecord {
    /// The notification's id.
    pub notification_id: NotificationId,
    /// The space the comment is in.
    pub space_id: SpaceId,
    /// The comment's thread.
    pub thread_id: ThreadId,
    /// The comment operation that caused it.
    pub comment_op_id: OpId,
    /// Why the caller is notified.
    pub kind: NotificationKind,
    /// The member who wrote the comment.
    pub actor: AccountId,
    /// When the server created it, Unix milliseconds.
    pub created_at_ms: u64,
    /// Whether the caller marked it read.
    pub read: bool,
}

/// `GET /v1/notifications?cursor=&limit=`: the caller's notifications, oldest first, from spaces
/// they are still an active member of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPage {
    /// At most `limit` (and [`NOTIFICATION_PAGE_MAX`]) notifications after the cursor.
    pub notifications: Vec<NotificationRecord>,
    /// Cursor after the last one returned; pass it to fetch newer ones.
    pub next_cursor: Cursor,
    /// Whether more existed when the query ran.
    pub has_more: bool,
    /// Unread notifications in all of the caller's visible notifications (not just this page).
    pub unread: u64,
}

/// `PATCH /v1/notifications/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateNotificationRequest {
    /// Mark read (`true`) or unread (`false`).
    pub read: bool,
}

/// A muted thread.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MutedThread {
    /// Its space.
    pub space_id: SpaceId,
    /// The thread.
    pub thread_id: ThreadId,
}

/// `GET` / `PUT /v1/notification-prefs`: which notebooks and threads create no notifications for
/// the caller. `PUT` replaces the whole document. Muting affects new notifications only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPrefs {
    /// Spaces with no notifications at all.
    #[serde(default)]
    pub muted_spaces: Vec<SpaceId>,
    /// Single threads with no notifications.
    #[serde(default)]
    pub muted_threads: Vec<MutedThread>,
}

impl NotificationPrefs {
    /// Checks the list sizes ([`MAX_MUTES`] each) and that no entry repeats.
    pub fn validate(&self) -> Result<(), CommentError> {
        if self.muted_spaces.len() > MAX_MUTES || self.muted_threads.len() > MAX_MUTES {
            return Err(CommentError::Mutes);
        }
        let mut spaces = std::collections::BTreeSet::new();
        let mut threads = std::collections::BTreeSet::new();
        if !self.muted_spaces.iter().all(|s| spaces.insert(s))
            || !self.muted_threads.iter().all(|t| threads.insert(t))
        {
            return Err(CommentError::Mutes);
        }
        Ok(())
    }

    /// Whether a notification about `thread` in `space` is muted.
    pub fn mutes(&self, space: &SpaceId, thread: &ThreadId) -> bool {
        self.muted_spaces.contains(space)
            || self
                .muted_threads
                .iter()
                .any(|t| &t.space_id == space && &t.thread_id == thread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> EnvelopeHeader {
        serde_json::from_str(include_str!("../fixtures/v1/envelope-header.valid.json")).unwrap()
    }

    #[test]
    fn mentions_are_bounded_and_unique() {
        let mut item = CommentPushItem {
            envelope: header(),
            thread_id: "t-1".parse().unwrap(),
            kind: CommentPushKind::Comment,
            mentions: vec![],
        };
        // `kind` is written explicitly and read as `comment` when missing
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["kind"], "comment");
        let mut old = json.clone();
        old.as_object_mut().unwrap().remove("kind");
        assert_eq!(
            serde_json::from_value::<CommentPushItem>(old).unwrap().kind,
            CommentPushKind::Comment
        );
        let mut upd = json;
        upd["kind"] = "update".into();
        assert_eq!(
            serde_json::from_value::<CommentPushItem>(upd).unwrap().kind,
            CommentPushKind::Update
        );
        assert!(item.validate(&Limits::DEFAULT).is_ok());
        assert!(!serde_json::to_string(&item).unwrap().contains("mentions"));
        item.mentions = (0..MAX_MENTIONS)
            .map(|i| format!("acct-{i}").parse().unwrap())
            .collect();
        assert!(item.validate(&Limits::DEFAULT).is_ok());
        item.mentions.push("acct-x".parse().unwrap());
        assert_eq!(item.validate(&Limits::DEFAULT), Err(CommentError::Mentions));
        item.mentions = vec!["a".parse().unwrap(), "a".parse().unwrap()];
        assert_eq!(item.validate(&Limits::DEFAULT), Err(CommentError::Mentions));
        // the side-car can't carry extra fields
        let mut v = serde_json::to_value(&item).unwrap();
        v["author_name"] = "x".into();
        assert!(serde_json::from_value::<CommentPushItem>(v).is_err());
    }

    #[test]
    fn prefs_mute_spaces_and_threads() {
        let s: SpaceId = "s-1".parse().unwrap();
        let t: ThreadId = "t-1".parse().unwrap();
        let mut p = NotificationPrefs::default();
        assert!(!p.mutes(&s, &t));
        p.muted_threads.push(MutedThread {
            space_id: s.clone(),
            thread_id: t.clone(),
        });
        assert!(p.mutes(&s, &t) && !p.mutes(&s, &"t-2".parse().unwrap()));
        p.muted_spaces.push(s.clone());
        assert!(p.mutes(&s, &"t-2".parse().unwrap()));
        assert!(p.validate().is_ok());
        p.muted_spaces.push(s);
        assert_eq!(p.validate(), Err(CommentError::Mutes));
        let empty: NotificationPrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, NotificationPrefs::default());
    }

    #[test]
    fn unknown_notification_kinds_are_tolerated() {
        assert_eq!(
            serde_json::from_str::<NotificationKind>("\"reaction\"").unwrap(),
            NotificationKind::Unknown
        );
        assert_eq!(
            serde_json::to_string(&NotificationKind::Mention).unwrap(),
            "\"mention\""
        );
    }
}
