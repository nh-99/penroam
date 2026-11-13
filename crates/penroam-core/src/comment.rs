//! Comments on shared notebooks: the operations, their binary encoding, the
//! order-independent merge of a space's comment stream, and anchor resolution.
//!
//! Comments never go into the document operation log. Each shared space has a
//! separate *comment stream* (`docs/protocol/sync-protocol-v1.md`) whose
//! payloads are [`CommentOperation`]s, encoded with [`CommentOperation::encode`].
//! Every encoding starts with [`COMMENT_MAGIC`] (`PRCM`), which differs from
//! the journal record (`PRJR`), checkpoint (`PRCP`), page (`PRPG`) and frame
//! (`PRSB`) magics, so a comment payload never decodes as a document operation
//! and a document payload never decodes as a comment. A receiver uses
//! [`is_comment_payload`] to quarantine a stream envelope of the wrong kind:
//! that is what stops a commenter smuggling document edits through the
//! comment stream.
//!
//! # Operations
//!
//! A [`CommentOperation`] carries one [`CommentOp`] with the identity and
//! causal context needed to deduplicate and merge it: a random operation id,
//! the writing device and its sequence *on the comment stream*, the causal
//! frontier (the same version vector the document replica uses) and the
//! author. `at` is wall-clock time, shown to people and used to sort lists,
//! never to decide a merge.
//!
//! The author and the device are not trusted because they are in the payload:
//! the caller must check them against the verified envelope (the signing
//! device and the member the key log assigns it to) before applying, and must
//! only accept [`CommentOperation::moderation`] from an owner. This module has
//! no key log; [`CommentOperation::check_attribution`] does the comparison.
//!
//! A thread's id is the id of the operation that created it, and so is a
//! reply's comment id ([`CommentOperation::validate`] enforces it). Ids
//! therefore cannot collide without the operation ids colliding too, which
//! the replica already refuses, and every replica agrees on which operation
//! made which comment. The first comment of a thread has the thread's id.
//!
//! # Merge rules
//!
//! [`CommentReplica`] applies operations under causal delivery and its
//! materialized [`CommentThread`]s are identical for any delivery order:
//!
//! * An operation may only refer to threads and comments its writer had seen
//!   (created by an operation in its causal past). Anything else is
//!   [`Rejection::UnknownThread`]/[`Rejection::UnknownComment`].
//! * Only a comment's author edits it. Its author deletes it, or an owner does
//!   with the moderation flag ("removed by owner"). A refused operation is a
//!   [`CommentApplied::Rejected`] no-op that still advances the frontier, so
//!   one bad operation doesn't block the rest of its device's stream. Whether
//!   an operation is refused depends only on itself and on operations in its
//!   causal past, so every replica refuses the same ones.
//! * A comment's text is a multi-value register: concurrent edits all stay,
//!   the one with the highest operation id is shown and the others are listed
//!   as [`Comment::alternate_bodies`] (at most [`limits::MAX_ALTERNATES`]). So
//!   is a thread's anchor.
//! * Delete is a tombstone and wins over concurrent edits. The merged view hides
//!   the comment's text (and this replica stops holding it) but keeps its place,
//!   so replies stay attached. The deleted text is still in the operations of
//!   the comment stream, on devices and on the server.
//! * Open/resolved is a register written by resolve, reopen *and reply*. If
//!   any concurrent head is a reply the thread is open; otherwise the head
//!   with the highest operation id decides. A reply made concurrently with or
//!   after a resolve therefore reopens the thread, and a resolve made after
//!   seeing the reply closes it.
//! * Retried operations (same id, same content) are no-ops; a reused id with
//!   different content, or a reused (device, sequence), is an error.
//!
//! # Anchors
//!
//! [`resolve`] places a [`CommentAnchor`] on the current notebook: element
//! anchors follow the element; text ranges are located through the text
//! CRDT's sticky indexes and must still hash to the quoted text; PDF regions
//! go through the PDF page's placement and rotation. A thread whose target
//! has gone becomes [`Resolved::Orphaned`]. It never moves to another page or
//! to different text.
//!
//! The format, merge rules and limits are documented in `docs/formats/comments.md`.

use crate::model::{Element, Notebook, Page, Timestamp};
use crate::operation::{DeviceId, OperationId, OperationRef};
use crate::textcrdt::b64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use uuid::Uuid;

/// First bytes of every encoded [`CommentOperation`].
pub const COMMENT_MAGIC: &[u8; 4] = b"PRCM";

/// Current comment operation schema (and binary encoding) version. Newer
/// versions are refused with [`CommentError::NewerSchema`], never dropped.
pub const COMMENT_SCHEMA_VERSION: u16 = 1;

/// Hard limits on comment operations. Decoding checks each length against
/// them (and against the bytes left) before allocating.
pub mod limits {
    /// UTF-8 bytes of a comment body.
    pub const MAX_BODY_BYTES: usize = 8 * 1024;
    /// Distinct mentions in one body (the server's notification side-car allows as many).
    pub const MAX_MENTIONS: usize = 20;
    /// UTF-8 bytes of a text anchor's quote.
    pub const MAX_QUOTE_BYTES: usize = 1024;
    /// Bytes of one sticky index.
    pub const MAX_STICKY_INDEX_BYTES: usize = 256;
    /// Bytes of an author or mentioned account id.
    pub const MAX_ACCOUNT_ID_BYTES: usize = 128;
    /// Entries in a causal frontier.
    pub const MAX_DEPENDENCIES: usize = 1024;
    /// Bytes of one encoded operation.
    pub const MAX_ENCODED_BYTES: usize = 64 * 1024;
    /// Concurrent alternates a materialized comment or thread lists (the highest operation ids).
    /// The register itself holds at most one write per writing device, since one device's writes
    /// are always ordered; it is not truncated, because dropping a write could change which one
    /// wins after a later write supersedes the others.
    pub const MAX_ALTERNATES: usize = 16;
    /// Largest absolute coordinate (the page model's own limit).
    pub const MAX_COORD_PT: f64 = crate::validate::limits::MAX_COORD_PT;
}

/// A rectangle: top-left corner (`x`, `y`) and non-negative size (`w`, `h`).
/// JSON `{"x":…,"y":…,"w":…,"h":…}`, the shape the app uses; an array is
/// refused, so a corner array `[x0, y0, x1, y1]` can't be misread as a size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width (`>= 0`).
    pub w: f64,
    /// Height (`>= 0`).
    pub h: f64,
}

impl<'de> Deserialize<'de> for Rect {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // a map only: serde's derived form would also take a 4-element array
        let m = BTreeMap::<String, f64>::deserialize(d)?;
        let get = |k: &str| {
            m.get(k)
                .copied()
                .ok_or_else(|| serde::de::Error::custom(format!("rect is missing `{k}`")))
        };
        if m.len() != 4 {
            return Err(serde::de::Error::custom(
                "rect must have exactly x, y, w, h",
            ));
        }
        Ok(Rect {
            x: get("x")?,
            y: get("y")?,
            w: get("w")?,
            h: get("h")?,
        })
    }
}

impl Rect {
    /// The rectangle spanning corners `[x0, y0, x1, y1]` (as
    /// [`Element::bounds`] gives them), in either order.
    pub fn from_corners(c: [f64; 4]) -> Self {
        let (x0, x1) = (c[0].min(c[2]), c[0].max(c[2]));
        let (y0, y1) = (c[1].min(c[3]), c[1].max(c[3]));
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    /// Corners `[x0, y0, x1, y1]`.
    pub fn corners(&self) -> [f64; 4] {
        [self.x, self.y, self.x + self.w, self.y + self.h]
    }
}

/// Why a comment operation or body is unusable.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum CommentError {
    /// The bytes are not a well-formed encoded comment operation.
    #[error("malformed comment operation: {0}")]
    Malformed(String),
    /// Written by a newer build; keep it and upgrade rather than drop it.
    #[error("comment schema {0} is newer than this build supports")]
    NewerSchema(u16),
    /// Well-formed but breaks a rule (limits, characters, ids).
    #[error("invalid comment operation: {0}")]
    Invalid(String),
}

fn invalid<T>(msg: impl Into<String>) -> Result<T, CommentError> {
    Err(CommentError::Invalid(msg.into()))
}

// ---------------------------------------------------------------------------
// Bodies and mentions

/// Whether `s` is an account id: 1 to 128 bytes of `[A-Za-z0-9_-]`, the
/// protocol's id charset.
pub fn is_account_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= limits::MAX_ACCOUNT_ID_BYTES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Characters a body or quote may not contain: control characters other than
/// newline and tab, and the bidirectional overrides and isolates that can make
/// text display in a different order from how it reads.
fn is_forbidden_char(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Cleans text typed into the comment composer: `\r\n` and lone `\r` become
/// `\n`, and forbidden characters (controls other than `\n`/`\t`, bidi
/// overrides and isolates) are removed. Bodies in operations must already be
/// in this form; [`validate_body`] refuses anything else rather than cleaning
/// it, so every replica shows the same text.
pub fn sanitize_body(raw: &str) -> String {
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| !is_forbidden_char(*c))
        .collect()
}

/// The account ids mentioned in a body, distinct, in order of first
/// appearance.
///
/// A mention token is `@[account_id]`: `@`, `[`, an id of 1 to 128 characters
/// from `[A-Za-z0-9_-]`, `]`. Anything else (including `@name` or an
/// `@[…]` with other characters) is plain text. Display names are not part
/// of the body: the app shows each token as the member's current name, and
/// as plain text if the account is not a member.
pub fn parse_mentions(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find("@[") {
        let after = &rest[at + 2..];
        match after.find(']') {
            Some(end) if is_account_id(&after[..end]) => {
                let id = &after[..end];
                if !out.iter().any(|m| m == id) {
                    out.push(id.to_owned());
                }
                rest = &after[end + 1..];
            }
            // not a token: carry on after the `@[`, which may start one later
            _ => rest = after,
        }
    }
    out
}

/// Checks a comment body and returns its mentions.
///
/// Errors ([`CommentError::Invalid`]): empty or whitespace only, longer than
/// [`limits::MAX_BODY_BYTES`], not in [`sanitize_body`] form, or more than
/// [`limits::MAX_MENTIONS`] distinct mentions. Bodies are plain text: the app
/// renders them escaped, never as HTML or Markdown.
pub fn validate_body(body: &str) -> Result<Vec<String>, CommentError> {
    if body.trim().is_empty() {
        return invalid("empty comment");
    }
    if body.len() > limits::MAX_BODY_BYTES {
        return invalid(format!(
            "comment is {} bytes, more than {}",
            body.len(),
            limits::MAX_BODY_BYTES
        ));
    }
    if body.contains('\r') || body.chars().any(is_forbidden_char) {
        return invalid("comment contains control or bidi override characters");
    }
    let mentions = parse_mentions(body);
    if mentions.len() > limits::MAX_MENTIONS {
        return invalid(format!("more than {} mentions", limits::MAX_MENTIONS));
    }
    Ok(mentions)
}

/// SHA-256 of the UTF-8 bytes of the selected text: the
/// `quote_hash` of a [`CommentAnchor::TextRange`] for a selection. No normalization,
/// so the TypeScript client gets the same value from WebCrypto.
pub fn quote_hash(selected: &str) -> [u8; 32] {
    Sha256::digest(selected.as_bytes()).into()
}

// ---------------------------------------------------------------------------
// Anchors

/// What a thread is attached to, in document coordinates (points, origin at
/// the top-left of the page as displayed, y down). Coordinates may be
/// negative: infinite canvases extend in every direction.
///
/// JSON is tagged by `kind`: `point`, `rect`, `element`, `text`, `pdf`, with
/// camelCase fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum CommentAnchor {
    /// A point on a page.
    #[serde(rename = "point", rename_all = "camelCase")]
    PagePoint {
        /// The page.
        page_id: Uuid,
        /// X in points.
        x: f64,
        /// Y in points.
        y: f64,
    },
    /// An area of a page.
    #[serde(rename = "rect", rename_all = "camelCase")]
    PageRect {
        /// The page.
        page_id: Uuid,
        /// The area.
        rect: Rect,
    },
    /// An element (stroke, text block, image): follows it when it moves.
    #[serde(rename = "element", rename_all = "camelCase")]
    Element {
        /// The page holding it.
        page_id: Uuid,
        /// The element.
        element_id: Uuid,
        /// Its bounds when the comment was made, used if it has none now
        /// (a stroke without points).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback_rect: Option<Rect>,
    },
    /// A range of a text block's text.
    #[serde(rename = "text", rename_all = "camelCase")]
    TextRange {
        /// The page holding the block.
        page_id: Uuid,
        /// The text block.
        element_id: Uuid,
        /// Sticky index of the range start, from the text CRDT (opaque here).
        #[serde(with = "b64_bytes")]
        start: Vec<u8>,
        /// Sticky index of the range end.
        #[serde(with = "b64_bytes")]
        end: Vec<u8>,
        /// The selected text, or its beginning if longer than
        /// [`limits::MAX_QUOTE_BYTES`]: shown when the range is lost.
        quote: String,
        /// [`quote_hash`] of the whole selected text (JSON: lowercase hex).
        #[serde(with = "hex32")]
        quote_hash: [u8; 32],
        /// The range's bounds when the comment was made.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback_rect: Option<Rect>,
    },
    /// An area of a PDF page, shown as the page background or as an image
    /// element on an infinite canvas.
    #[serde(rename = "pdf", rename_all = "camelCase")]
    PdfRegion {
        /// The notebook page.
        page_id: Uuid,
        /// Zero-based page index in the source PDF.
        pdf_page_index: u32,
        /// The area as fractions (0 to 1) of the PDF page as it was displayed
        /// when the comment was made, origin top-left, y down.
        normalized_rect: Rect,
        /// The page's clockwise rotation (0, 90, 180, 270) when the comment was
        /// made: the frame `normalized_rect` is measured in.
        rotation: u16,
        /// SHA-256 of the PDF asset the page showed (JSON: lowercase hex). When set, only that
        /// PDF places the anchor: if the page's PDF was replaced, it is orphaned rather than put
        /// on a different document.
        #[serde(default, skip_serializing_if = "Option::is_none", with = "opt_hex32")]
        pdf_asset_sha256: Option<[u8; 32]>,
    },
}

impl CommentAnchor {
    /// The page the anchor is on.
    pub fn page_id(&self) -> Uuid {
        match self {
            CommentAnchor::PagePoint { page_id, .. }
            | CommentAnchor::PageRect { page_id, .. }
            | CommentAnchor::Element { page_id, .. }
            | CommentAnchor::TextRange { page_id, .. }
            | CommentAnchor::PdfRegion { page_id, .. } => *page_id,
        }
    }

    /// Checks coordinates (finite, within [`limits::MAX_COORD_PT`], sizes not
    /// negative, normalized rects within the page), rotation and field sizes.
    pub fn validate(&self) -> Result<(), CommentError> {
        fn coord(v: f64) -> Result<(), CommentError> {
            if v.is_finite() && v.abs() <= limits::MAX_COORD_PT {
                Ok(())
            } else {
                invalid("anchor coordinate out of range")
            }
        }
        fn rect(r: &Rect) -> Result<(), CommentError> {
            r.corners().iter().try_for_each(|v| coord(*v))?;
            // `!(>= 0)` also refuses NaN
            if !(r.w >= 0.0 && r.h >= 0.0) {
                return invalid("anchor rectangle has a negative size");
            }
            Ok(())
        }
        match self {
            CommentAnchor::PagePoint { x, y, .. } => {
                coord(*x)?;
                coord(*y)
            }
            CommentAnchor::PageRect { rect: r, .. } => rect(r),
            CommentAnchor::Element { fallback_rect, .. } => {
                fallback_rect.as_ref().map_or(Ok(()), rect)
            }
            CommentAnchor::TextRange {
                start,
                end,
                quote,
                fallback_rect,
                ..
            } => {
                for s in [start, end] {
                    if s.is_empty() || s.len() > limits::MAX_STICKY_INDEX_BYTES {
                        return invalid("sticky index is empty or too long");
                    }
                }
                if quote.len() > limits::MAX_QUOTE_BYTES {
                    return invalid("quote too long");
                }
                if quote.contains('\r') || quote.chars().any(is_forbidden_char) {
                    return invalid("quote contains control or bidi override characters");
                }
                fallback_rect.as_ref().map_or(Ok(()), rect)
            }
            CommentAnchor::PdfRegion {
                normalized_rect,
                rotation,
                ..
            } => {
                if !matches!(rotation, 0 | 90 | 180 | 270) {
                    return invalid("PDF rotation must be 0, 90, 180 or 270");
                }
                rect(normalized_rect)?;
                // a little slack for x + w rounding just past 1
                let inside = |v: f64| (0.0..=1.0 + 1e-9).contains(&v);
                if !normalized_rect.corners().into_iter().all(inside) {
                    return invalid("normalized rectangle outside 0..1");
                }
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Operations

/// The change carried by a [`CommentOperation`]. JSON-tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CommentOp {
    /// Starts a thread with its first comment (whose comment id is `thread_id`).
    #[serde(rename_all = "camelCase")]
    ThreadCreated {
        /// The new thread; must equal the operation id.
        thread_id: Uuid,
        /// What it is attached to.
        anchor: CommentAnchor,
        /// The first comment's text.
        body: String,
    },
    /// Adds a comment to a thread.
    #[serde(rename_all = "camelCase")]
    Replied {
        /// The thread.
        thread_id: Uuid,
        /// The new comment; must equal the operation id.
        comment_id: Uuid,
        /// The text.
        body: String,
        /// The comment in the same thread being answered, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<Uuid>,
    },
    /// Replaces a comment's text (author only).
    #[serde(rename_all = "camelCase")]
    CommentEdited {
        /// The comment (a thread id for a thread's first comment).
        comment_id: Uuid,
        /// The new text.
        body: String,
    },
    /// Tombstones a comment (author, or an owner with the moderation flag).
    #[serde(rename_all = "camelCase")]
    CommentDeleted {
        /// The comment.
        comment_id: Uuid,
    },
    /// Marks a thread resolved.
    #[serde(rename_all = "camelCase")]
    ThreadResolved {
        /// The thread.
        thread_id: Uuid,
    },
    /// Marks a thread open again.
    #[serde(rename_all = "camelCase")]
    ThreadReopened {
        /// The thread.
        thread_id: Uuid,
    },
    /// Attaches a thread to something else (for an orphaned thread).
    #[serde(rename_all = "camelCase")]
    AnchorReattached {
        /// The thread.
        thread_id: Uuid,
        /// The new anchor.
        anchor: CommentAnchor,
    },
}

impl CommentOp {
    /// The `type` name used in JSON.
    pub fn name(&self) -> &'static str {
        match self {
            CommentOp::ThreadCreated { .. } => "threadCreated",
            CommentOp::Replied { .. } => "replied",
            CommentOp::CommentEdited { .. } => "commentEdited",
            CommentOp::CommentDeleted { .. } => "commentDeleted",
            CommentOp::ThreadResolved { .. } => "threadResolved",
            CommentOp::ThreadReopened { .. } => "threadReopened",
            CommentOp::AnchorReattached { .. } => "anchorReattached",
        }
    }

    /// The thread the operation names directly (`None` for edits and deletes,
    /// which name a comment). This is the server-visible routing id.
    pub fn thread_id(&self) -> Option<Uuid> {
        match self {
            CommentOp::ThreadCreated { thread_id, .. }
            | CommentOp::Replied { thread_id, .. }
            | CommentOp::ThreadResolved { thread_id }
            | CommentOp::ThreadReopened { thread_id }
            | CommentOp::AnchorReattached { thread_id, .. } => Some(*thread_id),
            CommentOp::CommentEdited { .. } | CommentOp::CommentDeleted { .. } => None,
        }
    }

    /// The body text, for operations that carry one.
    pub fn body(&self) -> Option<&str> {
        match self {
            CommentOp::ThreadCreated { body, .. }
            | CommentOp::Replied { body, .. }
            | CommentOp::CommentEdited { body, .. } => Some(body),
            _ => None,
        }
    }
}

/// One comment-stream operation: a [`CommentOp`] plus its identity, author
/// and causal context. See the module docs for which fields must be checked
/// against verified data before it is applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentOperation {
    /// Stable random id (retries reuse it).
    pub id: OperationId,
    /// The writing device (must be the envelope's signing device).
    pub device_id: DeviceId,
    /// 1, 2, 3, … per device on this space's comment stream.
    pub sequence: u64,
    /// [`COMMENT_SCHEMA_VERSION`] of the encoding.
    pub schema_version: u16,
    /// Causal frontier on the comment stream when made: for each device whose
    /// comment operations had been applied, the highest sequence. Sorted by
    /// device id, one entry per device.
    pub causal_dependencies: Vec<OperationRef>,
    /// Account id of the member who wrote it (verified by the caller).
    pub author: String,
    /// Set by an owner deleting someone else's comment. Only valid on
    /// [`CommentOp::CommentDeleted`]; the caller must refuse it from non-owners.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub moderation: bool,
    /// Wall-clock milliseconds when made. Display and list order only.
    pub at: Timestamp,
    /// What changed.
    pub op: CommentOp,
}

impl CommentOperation {
    /// Checks every rule that doesn't need other operations: schema, ids,
    /// limits, body and anchor contents, frontier shape. [`Self::decode`] and
    /// [`CommentReplica::apply`] call it.
    pub fn validate(&self) -> Result<(), CommentError> {
        if self.schema_version > COMMENT_SCHEMA_VERSION {
            return Err(CommentError::NewerSchema(self.schema_version));
        }
        if self.schema_version == 0 {
            return invalid("schema version 0");
        }
        if self.sequence == 0 {
            return invalid("sequence 0");
        }
        if !is_account_id(&self.author) {
            return invalid("author is not an account id");
        }
        if self.causal_dependencies.len() > limits::MAX_DEPENDENCIES {
            return invalid("too many causal dependencies");
        }
        for (i, d) in self.causal_dependencies.iter().enumerate() {
            if d.device_seq == 0 {
                return invalid("dependency on sequence 0");
            }
            if i > 0 && self.causal_dependencies[i - 1].device_id >= d.device_id {
                return invalid("causal dependencies must be sorted by device, one each");
            }
            if d.device_id == self.device_id && d.device_seq >= self.sequence {
                return invalid("operation depends on itself or a later operation");
            }
        }
        if self.moderation && !matches!(self.op, CommentOp::CommentDeleted { .. }) {
            return invalid("the moderation flag is only allowed on deletes");
        }
        match &self.op {
            CommentOp::ThreadCreated {
                thread_id,
                anchor,
                body,
            } => {
                if *thread_id != self.id {
                    return invalid("a new thread's id must be its operation id");
                }
                anchor.validate()?;
                validate_body(body)?;
            }
            CommentOp::Replied {
                comment_id,
                body,
                reply_to,
                ..
            } => {
                if *comment_id != self.id {
                    return invalid("a new comment's id must be its operation id");
                }
                if *reply_to == Some(*comment_id) {
                    return invalid("a comment can't reply to itself");
                }
                validate_body(body)?;
            }
            CommentOp::CommentEdited { body, .. } => {
                validate_body(body)?;
            }
            CommentOp::AnchorReattached { anchor, .. } => anchor.validate()?,
            CommentOp::CommentDeleted { .. }
            | CommentOp::ThreadResolved { .. }
            | CommentOp::ThreadReopened { .. } => {}
        }
        Ok(())
    }

    /// Compares the payload's device and author with the verified envelope
    /// attribution, and refuses a moderation flag unless the author is an
    /// owner. Call before [`CommentReplica::apply`] with values from the key
    /// log, never from the payload.
    pub fn check_attribution(
        &self,
        signing_device: DeviceId,
        member: &str,
        member_is_owner: bool,
    ) -> Result<(), CommentError> {
        if self.device_id != signing_device {
            return invalid("payload device differs from the signing device");
        }
        if self.author != member {
            return invalid("payload author differs from the signing member");
        }
        if self.moderation && !member_is_owner {
            return invalid("only an owner may moderate comments");
        }
        Ok(())
    }

    /// The compact binary encoding (layout in the module source, at
    /// [`CommentOperation::decode`]). Deterministic: one operation has
    /// exactly one encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::with_capacity(128);
        w.extend_from_slice(COMMENT_MAGIC);
        w.extend_from_slice(&self.schema_version.to_le_bytes());
        w.extend_from_slice(self.id.as_bytes());
        w.extend_from_slice(self.device_id.as_bytes());
        put_varint(&mut w, self.sequence);
        put_varint(&mut w, zigzag(self.at));
        w.push(u8::from(self.moderation));
        put_bytes(&mut w, self.author.as_bytes());
        put_varint(&mut w, self.causal_dependencies.len() as u64);
        for d in &self.causal_dependencies {
            w.extend_from_slice(d.device_id.as_bytes());
            put_varint(&mut w, d.device_seq);
        }
        match &self.op {
            CommentOp::ThreadCreated {
                thread_id,
                anchor,
                body,
            } => {
                w.push(1);
                w.extend_from_slice(thread_id.as_bytes());
                put_anchor(&mut w, anchor);
                put_bytes(&mut w, body.as_bytes());
            }
            CommentOp::Replied {
                thread_id,
                comment_id,
                body,
                reply_to,
            } => {
                w.push(2);
                w.extend_from_slice(thread_id.as_bytes());
                w.extend_from_slice(comment_id.as_bytes());
                put_bytes(&mut w, body.as_bytes());
                match reply_to {
                    Some(r) => {
                        w.push(1);
                        w.extend_from_slice(r.as_bytes());
                    }
                    None => w.push(0),
                }
            }
            CommentOp::CommentEdited { comment_id, body } => {
                w.push(3);
                w.extend_from_slice(comment_id.as_bytes());
                put_bytes(&mut w, body.as_bytes());
            }
            CommentOp::CommentDeleted { comment_id } => {
                w.push(4);
                w.extend_from_slice(comment_id.as_bytes());
            }
            CommentOp::ThreadResolved { thread_id } => {
                w.push(5);
                w.extend_from_slice(thread_id.as_bytes());
            }
            CommentOp::ThreadReopened { thread_id } => {
                w.push(6);
                w.extend_from_slice(thread_id.as_bytes());
            }
            CommentOp::AnchorReattached { thread_id, anchor } => {
                w.push(7);
                w.extend_from_slice(thread_id.as_bytes());
                put_anchor(&mut w, anchor);
            }
        }
        let crc = crc32fast::hash(&w);
        w.extend_from_slice(&crc.to_le_bytes());
        w
    }

    /// Decodes and validates an encoded operation.
    ///
    /// Layout (integers little-endian, `varint` = unsigned LEB128, `zz` =
    /// zigzag varint, `bytes` = varint length + bytes, `uuid` = 16 bytes,
    /// `f64` = IEEE 754 bits, `rect` = f64 x, y, w, h):
    ///
    /// ```text
    /// "PRCM" | u16 schema | uuid id | uuid device | varint sequence | zz at
    /// | u8 flags (bit 0 moderation) | bytes author
    /// | varint n | n × (uuid device, varint seq) | u8 op | op fields | u32 crc32
    /// op 1 threadCreated:    uuid thread, anchor, bytes body
    /// op 2 replied:          uuid thread, uuid comment, bytes body, u8 has_reply_to [uuid]
    /// op 3 commentEdited:    uuid comment, bytes body
    /// op 4 commentDeleted:   uuid comment
    /// op 5 threadResolved:   uuid thread
    /// op 6 threadReopened:   uuid thread
    /// op 7 anchorReattached: uuid thread, anchor
    /// anchor 1 pagePoint: uuid page, f64 x, f64 y
    /// anchor 2 pageRect:  uuid page, rect
    /// anchor 3 element:   uuid page, uuid element, u8 has_fallback [rect]
    /// anchor 4 textRange: uuid page, uuid element, bytes start, bytes end, bytes quote,
    ///                     32 quote_hash, u8 has_fallback [rect]
    /// anchor 5 pdfRegion: uuid page, varint pdf_page_index, rect, u16 rotation,
    ///                     u8 has_asset [32 sha256]
    /// ```
    ///
    /// The CRC covers everything before it. Only the canonical encoding is
    /// accepted (minimal varints, no trailing bytes), so decode then encode
    /// gives back the same bytes.
    ///
    /// Errors: [`CommentError::Malformed`] for anything that isn't this layout
    /// (wrong magic, bad CRC, truncation, unknown tag, too large),
    /// [`CommentError::NewerSchema`] for a newer version, and
    /// [`CommentError::Invalid`] from [`Self::validate`].
    pub fn decode(bytes: &[u8]) -> Result<Self, CommentError> {
        if bytes.len() > limits::MAX_ENCODED_BYTES {
            return Err(malformed("too large"));
        }
        if !is_comment_payload(bytes) {
            return Err(malformed("not a comment operation"));
        }
        let schema = u16::from_le_bytes([bytes[4], bytes[5]]);
        if schema > COMMENT_SCHEMA_VERSION {
            return Err(CommentError::NewerSchema(schema));
        }
        if schema == 0 {
            return Err(malformed("schema version 0"));
        }
        if bytes.len() < 10 {
            return Err(malformed("truncated"));
        }
        let (body, crc) = bytes.split_at(bytes.len() - 4);
        if crc32fast::hash(body) != u32::from_le_bytes(crc.try_into().expect("4 bytes")) {
            return Err(malformed("checksum mismatch"));
        }
        let mut r = Reader { b: &body[6..] };
        let id = r.uuid()?;
        let device_id = r.uuid()?;
        let sequence = r.varint()?;
        let at = unzigzag(r.varint()?);
        let flags = r.u8()?;
        if flags > 1 {
            return Err(malformed("unknown flags"));
        }
        let author = r.string(limits::MAX_ACCOUNT_ID_BYTES)?;
        let n = r.varint()?;
        if n > limits::MAX_DEPENDENCIES as u64 {
            return Err(malformed("too many dependencies"));
        }
        let mut causal_dependencies = Vec::with_capacity(n as usize);
        for _ in 0..n {
            causal_dependencies.push(OperationRef {
                device_id: r.uuid()?,
                device_seq: r.varint()?,
            });
        }
        let op = match r.u8()? {
            1 => CommentOp::ThreadCreated {
                thread_id: r.uuid()?,
                anchor: r.anchor()?,
                body: r.string(limits::MAX_BODY_BYTES)?,
            },
            2 => CommentOp::Replied {
                thread_id: r.uuid()?,
                comment_id: r.uuid()?,
                body: r.string(limits::MAX_BODY_BYTES)?,
                reply_to: if r.flag()? { Some(r.uuid()?) } else { None },
            },
            3 => CommentOp::CommentEdited {
                comment_id: r.uuid()?,
                body: r.string(limits::MAX_BODY_BYTES)?,
            },
            4 => CommentOp::CommentDeleted {
                comment_id: r.uuid()?,
            },
            5 => CommentOp::ThreadResolved {
                thread_id: r.uuid()?,
            },
            6 => CommentOp::ThreadReopened {
                thread_id: r.uuid()?,
            },
            7 => CommentOp::AnchorReattached {
                thread_id: r.uuid()?,
                anchor: r.anchor()?,
            },
            t => return Err(malformed(format!("unknown operation {t}"))),
        };
        if !r.b.is_empty() {
            return Err(malformed("trailing bytes"));
        }
        let op = CommentOperation {
            id,
            device_id,
            sequence,
            schema_version: schema,
            causal_dependencies,
            author,
            moderation: flags & 1 != 0,
            at,
            op,
        };
        op.validate()?;
        // reject non-minimal varints and the like: one operation, one encoding
        if op.encode() != bytes {
            return Err(malformed("not the canonical encoding"));
        }
        Ok(op)
    }

    /// This operation as a causal reference (device, sequence).
    pub fn op_ref(&self) -> OperationRef {
        OperationRef {
            device_id: self.device_id,
            device_seq: self.sequence,
        }
    }
}

/// Whether `bytes` start like an encoded comment operation (the magic and a
/// version field). Cheap; use it to route or quarantine envelopes, then
/// [`CommentOperation::decode`] to check the rest.
pub fn is_comment_payload(bytes: &[u8]) -> bool {
    bytes.len() >= 6 && &bytes[..4] == COMMENT_MAGIC
}

fn malformed(msg: impl Into<String>) -> CommentError {
    CommentError::Malformed(msg.into())
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn put_varint(w: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        w.push((v as u8) | 0x80);
        v >>= 7;
    }
    w.push(v as u8);
}

fn put_bytes(w: &mut Vec<u8>, b: &[u8]) {
    put_varint(w, b.len() as u64);
    w.extend_from_slice(b);
}

fn put_rect(w: &mut Vec<u8>, r: &Rect) {
    for v in [r.x, r.y, r.w, r.h] {
        w.extend_from_slice(&v.to_le_bytes());
    }
}

fn put_opt_rect(w: &mut Vec<u8>, r: &Option<Rect>) {
    match r {
        Some(r) => {
            w.push(1);
            put_rect(w, r);
        }
        None => w.push(0),
    }
}

fn put_anchor(w: &mut Vec<u8>, a: &CommentAnchor) {
    match a {
        CommentAnchor::PagePoint { page_id, x, y } => {
            w.push(1);
            w.extend_from_slice(page_id.as_bytes());
            w.extend_from_slice(&x.to_le_bytes());
            w.extend_from_slice(&y.to_le_bytes());
        }
        CommentAnchor::PageRect { page_id, rect } => {
            w.push(2);
            w.extend_from_slice(page_id.as_bytes());
            put_rect(w, rect);
        }
        CommentAnchor::Element {
            page_id,
            element_id,
            fallback_rect,
        } => {
            w.push(3);
            w.extend_from_slice(page_id.as_bytes());
            w.extend_from_slice(element_id.as_bytes());
            put_opt_rect(w, fallback_rect);
        }
        CommentAnchor::TextRange {
            page_id,
            element_id,
            start,
            end,
            quote,
            quote_hash,
            fallback_rect,
        } => {
            w.push(4);
            w.extend_from_slice(page_id.as_bytes());
            w.extend_from_slice(element_id.as_bytes());
            put_bytes(w, start);
            put_bytes(w, end);
            put_bytes(w, quote.as_bytes());
            w.extend_from_slice(quote_hash);
            put_opt_rect(w, fallback_rect);
        }
        CommentAnchor::PdfRegion {
            page_id,
            pdf_page_index,
            normalized_rect,
            rotation,
            pdf_asset_sha256,
        } => {
            w.push(5);
            w.extend_from_slice(page_id.as_bytes());
            put_varint(w, u64::from(*pdf_page_index));
            put_rect(w, normalized_rect);
            w.extend_from_slice(&rotation.to_le_bytes());
            match pdf_asset_sha256 {
                Some(h) => {
                    w.push(1);
                    w.extend_from_slice(h);
                }
                None => w.push(0),
            }
        }
    }
}

/// Bounds-checked cursor over an encoded operation.
struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CommentError> {
        if self.b.len() < n {
            return Err(malformed("truncated"));
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }
    fn u8(&mut self) -> Result<u8, CommentError> {
        Ok(self.take(1)?[0])
    }
    fn flag(&mut self) -> Result<bool, CommentError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(malformed("bad presence flag")),
        }
    }
    fn uuid(&mut self) -> Result<Uuid, CommentError> {
        Ok(Uuid::from_bytes(self.take(16)?.try_into().expect("16")))
    }
    fn f64(&mut self) -> Result<f64, CommentError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().expect("8")))
    }
    fn rect(&mut self) -> Result<Rect, CommentError> {
        Ok(Rect {
            x: self.f64()?,
            y: self.f64()?,
            w: self.f64()?,
            h: self.f64()?,
        })
    }
    fn opt_rect(&mut self) -> Result<Option<Rect>, CommentError> {
        Ok(if self.flag()? {
            Some(self.rect()?)
        } else {
            None
        })
    }
    fn varint(&mut self) -> Result<u64, CommentError> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = self.u8()?;
            if i == 9 && b > 1 {
                return Err(malformed("varint overflow"));
            }
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(malformed("varint overflow"))
    }
    /// Length-prefixed bytes, refusing lengths over `max` before allocating.
    fn bytes(&mut self, max: usize) -> Result<Vec<u8>, CommentError> {
        let n = self.varint()?;
        if n > max as u64 {
            return Err(malformed("field too long"));
        }
        Ok(self.take(n as usize)?.to_vec())
    }
    fn string(&mut self, max: usize) -> Result<String, CommentError> {
        String::from_utf8(self.bytes(max)?).map_err(|_| malformed("text is not UTF-8"))
    }
    fn anchor(&mut self) -> Result<CommentAnchor, CommentError> {
        Ok(match self.u8()? {
            1 => CommentAnchor::PagePoint {
                page_id: self.uuid()?,
                x: self.f64()?,
                y: self.f64()?,
            },
            2 => CommentAnchor::PageRect {
                page_id: self.uuid()?,
                rect: self.rect()?,
            },
            3 => CommentAnchor::Element {
                page_id: self.uuid()?,
                element_id: self.uuid()?,
                fallback_rect: self.opt_rect()?,
            },
            4 => CommentAnchor::TextRange {
                page_id: self.uuid()?,
                element_id: self.uuid()?,
                start: self.bytes(limits::MAX_STICKY_INDEX_BYTES)?,
                end: self.bytes(limits::MAX_STICKY_INDEX_BYTES)?,
                quote: self.string(limits::MAX_QUOTE_BYTES)?,
                quote_hash: self.take(32)?.try_into().expect("32"),
                fallback_rect: self.opt_rect()?,
            },
            5 => CommentAnchor::PdfRegion {
                page_id: self.uuid()?,
                pdf_page_index: u32::try_from(self.varint()?)
                    .map_err(|_| malformed("PDF page index too large"))?,
                normalized_rect: self.rect()?,
                rotation: u16::from_le_bytes(self.take(2)?.try_into().expect("2")),
                pdf_asset_sha256: if self.flag()? {
                    Some(self.take(32)?.try_into().expect("32"))
                } else {
                    None
                },
            },
            t => return Err(malformed(format!("unknown anchor {t}"))),
        })
    }
}

mod b64_bytes {
    use super::b64;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&b64::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        b64::decode(&s).ok_or_else(|| serde::de::Error::custom("not unpadded base64url"))
    }
}

mod opt_hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<[u8; 32]>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(h) => super::hex32::serialize(h, s),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 32]>, D::Error> {
        #[derive(Deserialize)]
        struct W(#[serde(with = "super::hex32")] [u8; 32]);
        Ok(Option::<W>::deserialize(d)?.map(|W(h)| h))
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        let bad = || serde::de::Error::custom("expected 64 lowercase hex digits");
        if s.len() != 64 || !s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(bad());
        }
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Merge

type Frontier = BTreeMap<DeviceId, u64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct OpKey {
    device: DeviceId,
    seq: u64,
}

/// `h` happened before the operation (`key`, `frontier`).
fn dominated(h: OpKey, key: OpKey, frontier: &Frontier) -> bool {
    if h.device == key.device {
        h.seq < key.seq
    } else {
        frontier.get(&h.device).is_some_and(|s| *s >= h.seq)
    }
}

#[derive(Debug, Clone)]
struct Write<T> {
    id: OperationId,
    key: OpKey,
    author: Arc<str>,
    at: Timestamp,
    value: T,
}

/// A multi-value register: every write no later write has superseded, sorted
/// by operation id. Same rule as the document replica's registers.
#[derive(Debug, Clone)]
struct Register<T> {
    heads: Vec<Write<T>>,
}

impl<T> Register<T> {
    fn new(w: Write<T>) -> Self {
        Self { heads: vec![w] }
    }
    fn write(&mut self, w: Write<T>, frontier: &Frontier) {
        self.heads.retain(|h| !dominated(h.key, w.key, frontier));
        let at = self.heads.partition_point(|h| h.id < w.id);
        self.heads.insert(at, w);
    }
    fn winner(&self) -> &Write<T> {
        self.heads.last().expect("a register always has a write")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusWrite {
    Created,
    Resolved,
    Reopened,
    Reply,
}

#[derive(Debug, Clone)]
struct ThreadCell {
    key: OpKey,
    created_by: Arc<str>,
    created_at: Timestamp,
    anchor: Register<CommentAnchor>,
    status: Register<StatusWrite>,
    comments: Vec<Uuid>,
}

#[derive(Debug, Clone)]
struct Deletion {
    id: OperationId,
    at: Timestamp,
    by_author: bool,
}

#[derive(Debug, Clone)]
struct CommentCell {
    thread: Uuid,
    key: OpKey,
    author: Arc<str>,
    reply_to: Option<Uuid>,
    created_at: Timestamp,
    /// `(text, is_edit)`; emptied once deleted.
    body: Register<(String, bool)>,
    deletions: Vec<Deletion>,
}

/// Result of [`CommentReplica::apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentApplied {
    /// Merged.
    Applied,
    /// Already applied (a retry); nothing changed.
    Duplicate,
    /// Recorded (the frontier advanced) but without effect, for the reason
    /// given. Every replica refuses the same operations.
    Rejected(Rejection),
}

/// Why an otherwise valid operation had no effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    /// The thread doesn't exist in the operation's causal past.
    #[error("thread {0} unknown to the writer")]
    UnknownThread(Uuid),
    /// The comment doesn't exist in the operation's causal past.
    #[error("comment {0} unknown to the writer")]
    UnknownComment(Uuid),
    /// A reply answers a comment of another thread.
    #[error("comment {0} is in another thread")]
    ReplyOutsideThread(Uuid),
    /// An edit by someone other than the author, or a delete by someone other
    /// than the author without the moderation flag.
    #[error("only the author may change comment {0}")]
    NotAuthor(Uuid),
}

/// Why an operation was not merged (the replica is unchanged).
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum CommentMergeError {
    /// Invalid operation or newer schema.
    #[error(transparent)]
    Invalid(#[from] CommentError),
    /// The id was already applied with different content.
    #[error("operation id {0} was already used for a different operation")]
    OpIdReuse(OperationId),
    /// Another operation already used this (device, sequence).
    #[error("device {device} sequence {seq} was already used by another operation")]
    SequenceReuse {
        /// The device.
        device: DeviceId,
        /// The reused sequence.
        seq: u64,
    },
    /// Not applicable yet: something it depends on is missing (keep it).
    #[error("waiting for {0:?}")]
    MissingDependency(OperationRef),
}

/// A space's comment threads, merged from the comment-stream operations of
/// any number of devices. See the module docs for the rules.
#[derive(Debug, Clone, Default)]
pub struct CommentReplica {
    frontier: Frontier,
    /// Applied id → (device, sequence) and SHA-256 prefix of the encoding.
    applied: HashMap<OperationId, (OpKey, [u8; 16])>,
    by_key: HashMap<OpKey, OperationId>,
    threads: BTreeMap<Uuid, ThreadCell>,
    comments: BTreeMap<Uuid, CommentCell>,
}

fn fingerprint(op: &CommentOperation) -> [u8; 16] {
    let h = Sha256::digest(op.encode());
    h[..16].try_into().expect("16")
}

impl CommentReplica {
    /// An empty replica.
    pub fn new() -> Self {
        Self::default()
    }

    /// Highest applied sequence per device: the frontier a new local comment
    /// operation depends on.
    pub fn frontier(&self) -> Vec<OperationRef> {
        self.frontier
            .iter()
            .map(|(d, s)| OperationRef {
                device_id: *d,
                device_seq: *s,
            })
            .collect()
    }

    /// Whether an operation id has been applied (or rejected).
    pub fn contains(&self, id: OperationId) -> bool {
        self.applied.contains_key(&id)
    }

    /// Applies one operation. On error nothing changed.
    ///
    /// Checks causal delivery: each device's operations must arrive in
    /// sequence, and every dependency must already be applied. The caller
    /// must have checked attribution ([`CommentOperation::check_attribution`]).
    pub fn apply(&mut self, op: &CommentOperation) -> Result<CommentApplied, CommentMergeError> {
        op.validate()?;
        let key = OpKey {
            device: op.device_id,
            seq: op.sequence,
        };
        let fp = fingerprint(op);
        if let Some((k, f)) = self.applied.get(&op.id) {
            return if *k == key && *f == fp {
                Ok(CommentApplied::Duplicate)
            } else {
                Err(CommentMergeError::OpIdReuse(op.id))
            };
        }
        if self.by_key.contains_key(&key) {
            return Err(CommentMergeError::SequenceReuse {
                device: op.device_id,
                seq: op.sequence,
            });
        }
        let have = self.frontier.get(&op.device_id).copied().unwrap_or(0);
        if have + 1 != op.sequence {
            return Err(CommentMergeError::MissingDependency(OperationRef {
                device_id: op.device_id,
                device_seq: op.sequence - 1,
            }));
        }
        let mut frontier = Frontier::new();
        for d in &op.causal_dependencies {
            if d.device_id == op.device_id {
                continue; // implied by the sequence (validated to be earlier)
            }
            if self.frontier.get(&d.device_id).copied().unwrap_or(0) < d.device_seq {
                return Err(CommentMergeError::MissingDependency(*d));
            }
            frontier.insert(d.device_id, d.device_seq);
        }

        let outcome = match self.merge(op, key, &frontier) {
            Ok(()) => CommentApplied::Applied,
            Err(r) => CommentApplied::Rejected(r),
        };
        self.applied.insert(op.id, (key, fp));
        self.by_key.insert(key, op.id);
        self.frontier.insert(op.device_id, op.sequence);
        Ok(outcome)
    }

    /// The thread `id` if the operation (`key`, `frontier`) had seen it.
    fn seen_thread(&self, id: Uuid, key: OpKey, frontier: &Frontier) -> Result<(), Rejection> {
        match self.threads.get(&id) {
            Some(t) if dominated(t.key, key, frontier) => Ok(()),
            _ => Err(Rejection::UnknownThread(id)),
        }
    }

    fn seen_comment(&self, id: Uuid, key: OpKey, frontier: &Frontier) -> Result<(), Rejection> {
        match self.comments.get(&id) {
            Some(c) if dominated(c.key, key, frontier) => Ok(()),
            _ => Err(Rejection::UnknownComment(id)),
        }
    }

    /// The effect of a delivered, valid operation. Every check here reads
    /// only data fixed by operations in the writer's causal past, so the
    /// verdict is the same on every replica.
    fn merge(
        &mut self,
        op: &CommentOperation,
        key: OpKey,
        frontier: &Frontier,
    ) -> Result<(), Rejection> {
        let author: Arc<str> = Arc::from(op.author.as_str());
        // a generic helper rather than a closure: registers hold different types
        fn mk<T>(op: &CommentOperation, key: OpKey, author: &Arc<str>, value: T) -> Write<T> {
            Write {
                id: op.id,
                key,
                author: author.clone(),
                at: op.at,
                value,
            }
        }
        macro_rules! write {
            ($v:expr) => {
                mk(op, key, &author, $v)
            };
        }
        match &op.op {
            CommentOp::ThreadCreated {
                thread_id,
                anchor,
                body,
            } => {
                // ids equal the operation id, which is new: no collision possible
                self.threads.insert(
                    *thread_id,
                    ThreadCell {
                        key,
                        created_by: author.clone(),
                        created_at: op.at,
                        anchor: Register::new(write!(anchor.clone())),
                        status: Register::new(write!(StatusWrite::Created)),
                        comments: vec![*thread_id],
                    },
                );
                self.comments.insert(
                    *thread_id,
                    CommentCell {
                        thread: *thread_id,
                        key,
                        author: author.clone(),
                        reply_to: None,
                        created_at: op.at,
                        body: Register::new(write!((body.clone(), false))),
                        deletions: Vec::new(),
                    },
                );
            }
            CommentOp::Replied {
                thread_id,
                comment_id,
                body,
                reply_to,
            } => {
                self.seen_thread(*thread_id, key, frontier)?;
                if let Some(r) = reply_to {
                    self.seen_comment(*r, key, frontier)?;
                    if self.comments[r].thread != *thread_id {
                        return Err(Rejection::ReplyOutsideThread(*r));
                    }
                }
                self.comments.insert(
                    *comment_id,
                    CommentCell {
                        thread: *thread_id,
                        key,
                        author: author.clone(),
                        reply_to: *reply_to,
                        created_at: op.at,
                        body: Register::new(write!((body.clone(), false))),
                        deletions: Vec::new(),
                    },
                );
                let t = self.threads.get_mut(thread_id).expect("seen");
                t.comments.push(*comment_id);
                // a reply is an "open" write: reopens a resolve it saw or raced
                t.status.write(write!(StatusWrite::Reply), frontier);
            }
            CommentOp::CommentEdited { comment_id, body } => {
                self.seen_comment(*comment_id, key, frontier)?;
                let c = self.comments.get_mut(comment_id).expect("seen");
                if *c.author != *op.author {
                    return Err(Rejection::NotAuthor(*comment_id));
                }
                // a tombstone wins over edits; don't keep text it removed
                if c.deletions.is_empty() {
                    c.body.write(write!((body.clone(), true)), frontier);
                }
            }
            CommentOp::CommentDeleted { comment_id } => {
                self.seen_comment(*comment_id, key, frontier)?;
                let c = self.comments.get_mut(comment_id).expect("seen");
                let by_author = *c.author == *op.author;
                if !by_author && !op.moderation {
                    return Err(Rejection::NotAuthor(*comment_id));
                }
                c.deletions.push(Deletion {
                    id: op.id,
                    at: op.at,
                    by_author,
                });
                // drop the text but keep one head so the register stays valid
                let head = c.body.winner().clone();
                c.body.heads = vec![Write {
                    value: (String::new(), head.value.1),
                    ..head
                }];
            }
            CommentOp::ThreadResolved { thread_id } | CommentOp::ThreadReopened { thread_id } => {
                self.seen_thread(*thread_id, key, frontier)?;
                let v = if matches!(op.op, CommentOp::ThreadResolved { .. }) {
                    StatusWrite::Resolved
                } else {
                    StatusWrite::Reopened
                };
                let t = self.threads.get_mut(thread_id).expect("seen");
                t.status.write(write!(v), frontier);
            }
            CommentOp::AnchorReattached { thread_id, anchor } => {
                self.seen_thread(*thread_id, key, frontier)?;
                let t = self.threads.get_mut(thread_id).expect("seen");
                t.anchor.write(write!(anchor.clone()), frontier);
            }
        }
        Ok(())
    }

    /// Every thread, materialized, ordered by (creation time, thread id).
    pub fn threads(&self) -> Vec<CommentThread> {
        let mut out: Vec<CommentThread> = self
            .threads
            .keys()
            .filter_map(|id| self.thread(*id))
            .collect();
        out.sort_by_key(|t| (t.created_at, t.thread_id));
        out
    }

    /// One thread, materialized.
    pub fn thread(&self, id: Uuid) -> Option<CommentThread> {
        let t = self.threads.get(&id)?;
        // replies win over a concurrent resolve; otherwise the highest id decides
        let decider = t
            .status
            .heads
            .iter()
            .rev()
            .find(|h| h.value == StatusWrite::Reply)
            .unwrap_or_else(|| t.status.winner());
        let status = if decider.value == StatusWrite::Resolved {
            ThreadStatus::Resolved
        } else {
            ThreadStatus::Open
        };
        let status_change = match decider.value {
            StatusWrite::Created => None,
            v => Some(StatusChange {
                kind: match v {
                    StatusWrite::Resolved => StatusChangeKind::Resolved,
                    StatusWrite::Reopened => StatusChangeKind::Reopened,
                    _ => StatusChangeKind::Replied,
                },
                by: decider.author.to_string(),
                op_id: decider.id,
                at: decider.at,
            }),
        };
        let mut comments: Vec<Comment> =
            t.comments.iter().filter_map(|c| self.comment(*c)).collect();
        // the first comment stays first; replies by (time, id)
        comments[1..].sort_by_key(|c| (c.created_at, c.comment_id));
        let anchor = t.anchor.winner();
        Some(CommentThread {
            thread_id: id,
            anchor: anchor.value.clone(),
            anchor_alternates: alternates(&t.anchor.heads)
                .iter()
                .map(|h| h.value.clone())
                .collect(),
            created_by: t.created_by.to_string(),
            created_at: t.created_at,
            status,
            status_change,
            comments,
        })
    }

    fn comment(&self, id: Uuid) -> Option<Comment> {
        let c = self.comments.get(&id)?;
        // author deletion first (accurate either way), then the lowest id
        let deletion = c.deletions.iter().min_by_key(|d| (!d.by_author, d.id));
        if let Some(d) = deletion {
            return Some(Comment {
                comment_id: id,
                thread_id: c.thread,
                author: c.author.to_string(),
                reply_to: c.reply_to,
                created_at: c.created_at,
                state: if d.by_author {
                    CommentState::Deleted
                } else {
                    CommentState::RemovedByOwner
                },
                deleted_at: Some(d.at),
                body: None,
                mentions: Vec::new(),
                edited: false,
                edited_at: None,
                alternate_bodies: Vec::new(),
            });
        }
        let w = c.body.winner();
        Some(Comment {
            comment_id: id,
            thread_id: c.thread,
            author: c.author.to_string(),
            reply_to: c.reply_to,
            created_at: c.created_at,
            state: CommentState::Visible,
            deleted_at: None,
            body: Some(w.value.0.clone()),
            mentions: parse_mentions(&w.value.0),
            edited: w.value.1,
            edited_at: w.value.1.then_some(w.at),
            alternate_bodies: alternates(&c.body.heads)
                .iter()
                .map(|h| h.value.0.clone())
                .collect(),
        })
    }
}

/// The heads that lost, at most [`limits::MAX_ALTERNATES`] of them: those with the highest
/// operation ids (heads are sorted by id, the winner last), so every replica lists the same ones.
fn alternates<T>(heads: &[Write<T>]) -> &[Write<T>] {
    let losers = &heads[..heads.len() - 1];
    &losers[losers.len().saturating_sub(limits::MAX_ALTERNATES)..]
}

/// Whether a thread is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadStatus {
    /// Open (new, reopened, or replied to after or while being resolved).
    Open,
    /// Resolved.
    Resolved,
}

/// What last decided a thread's [`ThreadStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StatusChangeKind {
    /// A resolve.
    Resolved,
    /// A reopen.
    Reopened,
    /// A reply (which reopens a resolved thread).
    Replied,
}

/// The operation that decided a thread's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChange {
    /// What it was.
    pub kind: StatusChangeKind,
    /// Its author's account id.
    pub by: String,
    /// Its operation id.
    pub op_id: OperationId,
    /// When it was made (display only).
    pub at: Timestamp,
}

/// A comment's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommentState {
    /// Shown.
    Visible,
    /// Deleted by its author.
    Deleted,
    /// Removed by an owner.
    RemovedByOwner,
}

/// One materialized comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    /// Its id (the thread id for a thread's first comment).
    pub comment_id: Uuid,
    /// Its thread.
    pub thread_id: Uuid,
    /// Its author's account id.
    pub author: String,
    /// The comment it answers, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Uuid>,
    /// When it was written (display only).
    pub created_at: Timestamp,
    /// Shown, deleted or removed.
    pub state: CommentState,
    /// When it was deleted, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<Timestamp>,
    /// The current text; `None` once deleted (the merged view hides it; the deleting
    /// operation doesn't remove it from the comment stream).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Account ids mentioned in [`Self::body`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
    /// The shown text is an edit.
    pub edited: bool,
    /// When the shown edit was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<Timestamp>,
    /// Concurrent edits not shown, by operation id: the [`limits::MAX_ALTERNATES`] highest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_bodies: Vec<String>,
}

/// One materialized thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentThread {
    /// Its id.
    pub thread_id: Uuid,
    /// What it is attached to (resolve with [`resolve`]).
    pub anchor: CommentAnchor,
    /// Concurrent reattachments not shown, by operation id: the [`limits::MAX_ALTERNATES`]
    /// highest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchor_alternates: Vec<CommentAnchor>,
    /// Account id of whoever started it.
    pub created_by: String,
    /// When it was started (display only).
    pub created_at: Timestamp,
    /// Open or resolved.
    pub status: ThreadStatus,
    /// What decided the status; `None` for a thread nobody resolved,
    /// reopened or replied to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_change: Option<StatusChange>,
    /// Its comments: the first one, then replies by (time, id). Deleted
    /// comments stay, without text.
    pub comments: Vec<Comment>,
}

// ---------------------------------------------------------------------------
// Anchor resolution

/// Why an anchor can't be placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrphanReason {
    /// The page is in the notebook's trash.
    PageTrashed,
    /// The page is not in the notebook any more.
    PageDeleted,
    /// The page is live but the caller didn't supply its content.
    PageUnavailable,
    /// The element was deleted.
    ElementDeleted,
    /// The text range could not be located (the sticky indexes no longer
    /// resolve, or the element is not a text block any more).
    TextRangeLost,
    /// The range's current text differs from what was quoted.
    QuoteMismatch,
    /// The page shows no such PDF page (or more than one image of it).
    PdfPageMissing,
}

/// Where an anchor is now. JSON is tagged by `kind` (`placed`, `orphaned`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Resolved {
    /// Shown at `rect` on `page_id`.
    #[serde(rename_all = "camelCase")]
    Placed {
        /// The page (always the anchor's own page).
        page_id: Uuid,
        /// Document coordinates; a point anchor gives a zero-size rect. For a
        /// text range this is the block's bounds: `text_range` lets the renderer highlight the exact characters.
        rect: Rect,
        /// For a text range: the current `[start, end)` in UTF-16 code units.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_range: Option<[usize; 2]>,
    },
    /// Its target is gone; listed in the sidebar with locate and reattach.
    #[serde(rename_all = "camelCase")]
    Orphaned {
        /// Why.
        reason: OrphanReason,
        /// The page it was on.
        last_known_page: Uuid,
        /// For a text range, the quoted text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quote: Option<String>,
    },
}

/// Byte offset of UTF-16 offset `u` in `s`, if it falls on a character boundary.
fn utf16_to_byte(s: &str, u: usize) -> Option<usize> {
    let mut n = 0;
    for (b, c) in s.char_indices() {
        if n == u {
            return Some(b);
        }
        n += c.len_utf16();
        if n > u {
            return None;
        }
    }
    (n == u).then_some(s.len())
}

/// Turns a normalized rect measured on a page displayed at clockwise rotation
/// `from` into the same area on the page displayed at rotation `to`.
fn rerotate(r: &Rect, from: u32, to: u32) -> Rect {
    // a clockwise quarter turn maps (u, v) to (1 - v, u)
    let quarter = |(u, v): (f64, f64)| (1.0 - v, u);
    let turns = ((to % 360 + 360 - from % 360) / 90) % 4;
    let c = r.corners();
    let (mut a, mut b) = ((c[0], c[1]), (c[2], c[3]));
    for _ in 0..turns {
        a = quarter(a);
        b = quarter(b);
    }
    Rect::from_corners([a.0, a.1, b.0, b.1])
}

/// Places a normalized rect in the box at (`x`, `y`) of size `w` × `h`.
fn in_box(n: &Rect, x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect {
        x: x + n.x * w,
        y: y + n.y * h,
        w: n.w * w,
        h: n.h * h,
    }
}

/// Finds a text block's current offset for a sticky index: `(page, element,
/// sticky index bytes)` → offset in UTF-16 code units, or `None` if the index
/// no longer resolves. Implemented over the text CRDT by the caller.
pub type TextOffset<'a> = dyn Fn(Uuid, Uuid, &[u8]) -> Option<usize> + 'a;

/// Places `anchor` on the notebook as it is now.
///
/// `pages` holds the content of (at least) the anchor's page; a live page
/// missing from it gives [`OrphanReason::PageUnavailable`]. `text_offset`
/// turns a sticky index of a text block into its current offset, in UTF-16
/// code units, in that block's text (the text CRDT does this; `None` if it no
/// longer resolves).
///
/// Rules:
/// * The page must be live (in `page_order`), otherwise the anchor is
///   orphaned. It never moves to another page.
/// * Points and rects are placed as given, negative coordinates included.
/// * An element anchor takes the element's current [`Element::bounds`] (its
///   `fallback_rect` if it has none); a deleted element is orphaned.
/// * A text range is placed only if the text between its resolved offsets
///   still has the anchor's `quote_hash`; otherwise it is orphaned with the
///   quote, so it never points at different text.
/// * A PDF region goes through the page background with that PDF page index
///   (filling the page's displayed size), or else the one image element
///   showing it; the region is turned from the rotation it was measured at to
///   the PDF page's current rotation.
pub fn resolve(
    anchor: &CommentAnchor,
    notebook: &Notebook,
    pages: &[Page],
    text_offset: &TextOffset<'_>,
) -> Resolved {
    let page_id = anchor.page_id();
    let quote = match anchor {
        CommentAnchor::TextRange { quote, .. } => Some(quote.clone()),
        _ => None,
    };
    let orphan = |reason| Resolved::Orphaned {
        reason,
        last_known_page: page_id,
        quote: quote.clone(),
    };
    let placed = |rect| Resolved::Placed {
        page_id,
        rect,
        text_range: None,
    };
    if !notebook.page_order.contains(&page_id) {
        let trashed = notebook.trashed_pages.iter().any(|t| t.id == page_id);
        return orphan(if trashed {
            OrphanReason::PageTrashed
        } else {
            OrphanReason::PageDeleted
        });
    }
    let Some(page) = pages.iter().find(|p| p.id == page_id) else {
        return orphan(OrphanReason::PageUnavailable);
    };
    let element = |id: Uuid| page.elements.iter().find(|e| e.id() == id);
    match anchor {
        CommentAnchor::PagePoint { x, y, .. } => placed(Rect {
            x: *x,
            y: *y,
            w: 0.0,
            h: 0.0,
        }),
        CommentAnchor::PageRect { rect, .. } => placed(*rect),
        CommentAnchor::Element {
            element_id,
            fallback_rect,
            ..
        } => match element(*element_id) {
            None => orphan(OrphanReason::ElementDeleted),
            Some(e) => match e.bounds().map(Rect::from_corners).or(*fallback_rect) {
                Some(r) => placed(r),
                None => orphan(OrphanReason::ElementDeleted),
            },
        },
        CommentAnchor::TextRange {
            element_id,
            start,
            end,
            quote_hash: want,
            ..
        } => {
            let block = match element(*element_id) {
                None => return orphan(OrphanReason::ElementDeleted),
                Some(Element::Text(t)) => t,
                Some(_) => return orphan(OrphanReason::TextRangeLost),
            };
            let (Some(s), Some(e)) = (
                text_offset(page_id, *element_id, start),
                text_offset(page_id, *element_id, end),
            ) else {
                return orphan(OrphanReason::TextRangeLost);
            };
            let bytes = (s <= e)
                .then(|| {
                    Some((
                        utf16_to_byte(&block.text, s)?,
                        utf16_to_byte(&block.text, e)?,
                    ))
                })
                .flatten();
            let Some((bs, be)) = bytes else {
                return orphan(OrphanReason::TextRangeLost);
            };
            if quote_hash(&block.text[bs..be]) != *want {
                return orphan(OrphanReason::QuoteMismatch);
            }
            Resolved::Placed {
                page_id,
                rect: Rect {
                    x: block.x,
                    y: block.y,
                    w: block.width,
                    h: block.height,
                },
                text_range: Some([s, e]),
            }
        }
        CommentAnchor::PdfRegion {
            pdf_page_index,
            normalized_rect,
            rotation,
            pdf_asset_sha256,
            ..
        } => {
            // the same page of the same PDF (when the anchor names the PDF)
            let same = |p: &crate::model::PdfBackgroundRef| {
                p.page_index == *pdf_page_index
                    && pdf_asset_sha256.is_none_or(|h| {
                        p.asset_hash == h.iter().map(|b| format!("{b:02x}")).collect::<String>()
                    })
            };
            if let Some(bg) = page.background.as_ref().filter(|b| same(b)) {
                let n = rerotate(normalized_rect, u32::from(*rotation), bg.rotation);
                return placed(in_box(&n, 0.0, 0.0, page.size.width, page.size.height));
            }
            let mut shown = page.elements.iter().filter_map(|e| match e {
                Element::Image(i) => i.pdf_page.as_ref().filter(|p| same(p)).map(|p| (i, p)),
                _ => None,
            });
            match (shown.next(), shown.next()) {
                (Some((img, pdf)), None) => {
                    let n = rerotate(normalized_rect, u32::from(*rotation), pdf.rotation);
                    placed(in_box(&n, img.x, img.y, img.width, img.height))
                }
                // none, or ambiguous: never guess which copy was meant
                _ => orphan(OrphanReason::PdfPageMissing),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_zigzag_round_trip() {
        for v in [0i64, 1, -1, 63, -64, i64::MAX, i64::MIN, 1_700_000_000_000] {
            assert_eq!(unzigzag(zigzag(v)), v);
            let mut w = Vec::new();
            put_varint(&mut w, zigzag(v));
            assert_eq!(Reader { b: &w }.varint().unwrap(), zigzag(v));
        }
    }

    #[test]
    fn utf16_offsets_respect_characters() {
        let s = "a👩b";
        assert_eq!(utf16_to_byte(s, 0), Some(0));
        assert_eq!(utf16_to_byte(s, 1), Some(1));
        assert_eq!(utf16_to_byte(s, 2), None); // inside the surrogate pair
        assert_eq!(utf16_to_byte(s, 3), Some(5));
        assert_eq!(utf16_to_byte(s, 4), Some(6));
        assert_eq!(utf16_to_byte(s, 5), None);
    }

    #[test]
    fn rerotation_is_a_group_action() {
        let r = Rect::from_corners([0.1, 0.2, 0.3, 0.5]);
        for from in [0, 90, 180, 270] {
            for to in [0, 90, 180, 270] {
                let back = rerotate(&rerotate(&r, from, to), to, from).corners();
                for (got, want) in back.iter().zip(r.corners()) {
                    assert!((got - want).abs() < 1e-12, "{from}->{to}");
                }
            }
        }
    }
}
