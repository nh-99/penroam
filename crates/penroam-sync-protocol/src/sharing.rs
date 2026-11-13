//! Shared notebooks (capability `sharing.v1`, protocol 1.7).
//!
//! A shared notebook lives in a **sharing space**: a hidden account, like a vault, that the
//! sharing account owns and that holds exactly one notebook. The space has its own operation log,
//! key log, blobs and checkpoints, so the envelope and every sync rule stay unchanged inside it.
//! What a space adds is **membership**: other accounts join it by accepting an invitation, and each
//! member has a [`Role`].
//!
//! * The space's root secret is stored on the server (`GET`/`PUT` [`crate::routes::SHARED_NOTEBOOK_KEY`]),
//!   exactly like an ordinary account key copy, so **the server can read shared notebooks**. Shared
//!   notebooks are not zero-knowledge, and a vault notebook cannot be shared.
//! * Removing a member is enforced by the server refusing them (`access_revoked`), not by
//!   cryptography: they keep the space root, so they could unwrap later epoch keys if they ever got
//!   the ciphertext, and nothing can erase what their devices already downloaded.
//! * The notebook's title travels only inside `sealed_meta`, sealed with a key derived from the
//!   space root (see [`MAX_SEALED_META_BYTES`]); servers never parse it.
//! * Invitations are matched by the invitee's **verified email**. A server answers an invitation
//!   request identically whether or not the email belongs to an account, and answers every request
//!   about a space the caller is not a member of with `not_found`, so neither emails nor space ids
//!   can be probed.
//!
//! This module holds the wire types and their validation only; authorization is the server's job.
//! The normative description is `docs/protocol/sync-protocol-v1.md` §12f.

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::ids::{validate_id, AccountId, InvitationId};

/// A sharing space's id. It is the space's own account id (same charset and limits), and the
/// `{id}` in every `/v1/shared-notebooks/{id}/…` route.
pub type SpaceId = AccountId;

/// Largest `sealed_meta` a server stores: the base64url **string**, in bytes.
///
/// The content is `seal(HKDF(space_root, info = "penroam/space-meta/v1"))` of the JSON
/// `{"notebook_id": …, "title": …}`, with associated data `"penroam/space-meta/v1"`, sealed as in
/// protocol §13.
pub const MAX_SEALED_META_BYTES: usize = 4096;
/// Largest email address accepted, in bytes after [`normalize_email`] (RFC 5321's path limit).
pub const MAX_EMAIL_BYTES: usize = 254;
/// Largest `idempotency_key`, in bytes. Keys use the id charset `[A-Za-z0-9_-]`.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
/// Largest `display` / `invited_by` string a client accepts, in bytes.
pub const MAX_DISPLAY_BYTES: usize = 254;
/// How long an invitation stays acceptable after it is created: 14 days, in milliseconds.
pub const INVITATION_TTL_MS: u64 = 14 * 24 * 60 * 60 * 1000;

/// What a member may do in a space. Lowercase strings on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Created the space. Everything an editor may do, plus changing the sealed title, publishing
    /// checkpoints, inviting, changing roles, removing members, cancelling invitations and storing
    /// the space key. Exactly one per space; never granted by invitation or role change.
    Owner,
    /// Reads and writes the notebook (push, blob upload). Publishing a checkpoint and changing the
    /// sealed title are the owner's alone: an editor could leave other members' operations out of
    /// a checkpoint, or point the sealed notebook id elsewhere.
    Editor,
    /// Reads the notebook only. Push, blob upload and checkpoint upload answer `forbidden`. Reads
    /// comments but can't write them.
    Viewer,
    /// Reads the notebook and writes comments (protocol 1.9, `comments.v1`), nothing else: the
    /// document stream, blobs, checkpoints, the sealed title and members answer `forbidden`.
    Commenter,
    /// A role this client doesn't know (from a newer server). Treat it as read-only; it is never
    /// granted or sent.
    #[serde(other)]
    Unknown,
}

impl Role {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Editor => "editor",
            Self::Viewer => "viewer",
            Self::Commenter => "commenter",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this role may change the notebook's content (push, blobs). Checkpoints and the
    /// sealed title need [`Role::Owner`].
    pub fn may_write(self) -> bool {
        matches!(self, Self::Owner | Self::Editor)
    }

    /// Whether this role may write the comment stream (owner, editor, commenter).
    pub fn may_comment(self) -> bool {
        matches!(self, Self::Owner | Self::Editor | Self::Commenter)
    }

    /// Whether this role can be granted through an invitation or a role change: `editor`,
    /// `commenter` and `viewer`. Asking for `owner` (or `unknown`) is `invalid_role`.
    pub fn is_grantable(self) -> bool {
        matches!(self, Self::Editor | Self::Commenter | Self::Viewer)
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A membership's state. A removed member (or one who left) stays listed as `revoked`, so their
/// clients can tell access was lost rather than guessing from errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MembershipState {
    /// The member may use the space.
    Active,
    /// The member was removed or left. Every space route answers `access_revoked`.
    Revoked,
}

/// An invitation's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvitationState {
    /// Waiting for the invitee.
    Pending,
    /// The invitee joined.
    Accepted,
    /// The invitee said no. The owner may invite the same email again.
    Declined,
    /// The owner withdrew it.
    Cancelled,
    /// It was not answered within [`INVITATION_TTL_MS`].
    Expired,
}

/// Why a sharing request or response failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SharingError {
    /// `sealed_meta` is empty, longer than [`MAX_SEALED_META_BYTES`] or not canonical base64url.
    #[error("sealed_meta is empty, too long or not base64url")]
    SealedMeta,
    /// The email address is not acceptable (see [`normalize_email`]).
    #[error("invalid email address")]
    Email,
    /// The idempotency key is empty, too long or outside `[A-Za-z0-9_-]`.
    #[error("invalid idempotency key")]
    IdempotencyKey,
    /// The role cannot be granted (`owner`).
    #[error("the owner role cannot be granted")]
    Role,
    /// A display string is longer than [`MAX_DISPLAY_BYTES`] or contains control or bidirectional
    /// formatting characters.
    #[error("display string too long or not printable")]
    Display,
}

impl SharingError {
    /// The wire error code a server answers with: `invalid_role` for [`SharingError::Role`],
    /// `validation_failed` otherwise.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::Role => ErrorCode::InvalidRole,
            _ => ErrorCode::ValidationFailed,
        }
    }
}

/// Normalizes an email address for matching invitations against verified sign-in emails.
///
/// Exactly: trim leading and trailing whitespace (Unicode `White_Space`), then lowercase the
/// **whole** address with Unicode simple lowercasing (`str::to_lowercase`). No Unicode
/// normalization form is applied, because this crate has no Unicode tables; clients should send
/// NFC, which every common input method produces. Local parts are technically case-sensitive, but
/// sign-in providers and mail hosts treat them case-insensitively, and matching must not depend on
/// how someone typed an address.
///
/// The result must be at most [`MAX_EMAIL_BYTES`] bytes, contain exactly one `@` with a non-empty
/// part on each side, a domain containing a `.` that neither starts nor ends it, and no
/// whitespace, control characters or bidirectional formatting characters ([`is_bidi_control`]:
/// an override in a verified email would make a member's name read differently from what it is).
/// This is a sanity check, not RFC 5322 validation: quoted local
/// parts are refused.
pub fn normalize_email(raw: &str) -> Result<String, SharingError> {
    let e = raw.trim().to_lowercase();
    if e.is_empty() || e.len() > MAX_EMAIL_BYTES {
        return Err(SharingError::Email);
    }
    if e.chars()
        .any(|c| c.is_whitespace() || c.is_control() || is_bidi_control(c))
    {
        return Err(SharingError::Email);
    }
    let mut parts = e.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(SharingError::Email);
    };
    if local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
    {
        return Err(SharingError::Email);
    }
    Ok(e)
}

/// Checks a `sealed_meta` value: non-empty canonical unpadded base64url of at most
/// [`MAX_SEALED_META_BYTES`] characters.
pub fn validate_sealed_meta(s: &str) -> Result<(), SharingError> {
    if s.is_empty() || s.len() > MAX_SEALED_META_BYTES || crate::base64url::decode(s).is_err() {
        return Err(SharingError::SealedMeta);
    }
    Ok(())
}

/// Checks an idempotency key: the id charset `[A-Za-z0-9_-]`, 1 to
/// [`MAX_IDEMPOTENCY_KEY_BYTES`] bytes. A UUID in hyphenated form fits.
pub fn validate_idempotency_key(s: &str) -> Result<(), SharingError> {
    if s.len() > MAX_IDEMPOTENCY_KEY_BYTES || validate_id(s).is_err() {
        return Err(SharingError::IdempotencyKey);
    }
    Ok(())
}

/// Whether `c` is a bidirectional formatting character: the embeddings and overrides
/// U+202A–U+202E, the isolates U+2066–U+2069, the marks U+200E and U+200F, and the Arabic letter
/// mark U+061C. They change how surrounding text is displayed without being visible themselves.
pub fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

fn validate_display(s: &str) -> Result<(), SharingError> {
    if s.len() > MAX_DISPLAY_BYTES || s.chars().any(|c| c.is_control() || is_bidi_control(c)) {
        return Err(SharingError::Display);
    }
    Ok(())
}

/// `POST /v1/shared-notebooks`: creates a sharing space owned by the caller.
///
/// Retrying with the same `idempotency_key` returns the space created the first time (and
/// ignores the new `sealed_meta`), so a client that lost the response does not create a second
/// space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSpaceRequest {
    /// Client-chosen key, stable across retries (see [`validate_idempotency_key`]).
    pub idempotency_key: String,
    /// The sealed notebook id and title (see [`MAX_SEALED_META_BYTES`]).
    pub sealed_meta: String,
}

impl CreateSpaceRequest {
    /// Checks both fields.
    pub fn validate(&self) -> Result<(), SharingError> {
        validate_idempotency_key(&self.idempotency_key)?;
        validate_sealed_meta(&self.sealed_meta)
    }
}

/// `PUT /v1/shared-notebooks/{id}/meta`: replaces the sealed notebook id and title (owner only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSpaceMetaRequest {
    /// The new sealed notebook id and title.
    pub sealed_meta: String,
}

impl UpdateSpaceMetaRequest {
    /// Checks `sealed_meta`.
    pub fn validate(&self) -> Result<(), SharingError> {
        validate_sealed_meta(&self.sealed_meta)
    }
}

/// One sharing space as seen by the caller (`GET /v1/shared-notebooks` and the create/accept
/// responses).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedNotebookRecord {
    /// The space's id (its account id).
    pub space_id: SpaceId,
    /// The caller's role in it.
    pub role: Role,
    /// The caller's membership state. `revoked` means access was removed (or the caller left).
    pub state: MembershipState,
    /// Whether the caller owns the space.
    pub owner: bool,
    /// The sealed notebook id and title. Opaque to the server.
    pub sealed_meta: String,
    /// Increases on every change to `sealed_meta` (display and caching only).
    pub version: u64,
    /// Creation time, Unix milliseconds (display only).
    pub created_at_ms: u64,
}

impl SharedNotebookRecord {
    /// Checks the fields a client relies on. Call on records from the server.
    pub fn validate(&self) -> Result<(), SharingError> {
        validate_sealed_meta(&self.sealed_meta)?;
        if self.owner != (self.role == Role::Owner) {
            return Err(SharingError::Role);
        }
        Ok(())
    }
}

/// `GET /v1/shared-notebooks`: the spaces the caller owns or belongs to, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedNotebookList {
    /// Owned spaces, active memberships, and revoked memberships (listed with
    /// `state: "revoked"`, once per space).
    pub spaces: Vec<SharedNotebookRecord>,
}

/// One member of a space (`GET /v1/shared-notebooks/{id}/members`, `PATCH …/members/{account}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRecord {
    /// The member's account id.
    pub account_id: AccountId,
    /// The member's role.
    pub role: Role,
    /// Whether the membership is active.
    pub state: MembershipState,
    /// The member's verified email, shown only to members of this space so they know who is who.
    /// Empty when the server has none.
    pub display: String,
    /// Whether this record is the caller.
    pub is_you: bool,
    /// Increases on every role or state change. A role change must name the current version.
    pub version: u64,
    /// When the member joined (or was last re-admitted), Unix milliseconds.
    pub since_ms: u64,
}

impl MemberRecord {
    /// Checks `display`.
    pub fn validate(&self) -> Result<(), SharingError> {
        validate_display(&self.display)
    }
}

/// An invitation as the space owner sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationRecord {
    /// The invitation's id.
    pub invitation_id: InvitationId,
    /// The invited address, normalized ([`normalize_email`]).
    pub email: String,
    /// The role the invitee gets on accepting (`editor` or `viewer`).
    pub role: Role,
    /// Where the invitation stands.
    pub state: InvitationState,
    /// When it stops being acceptable, Unix milliseconds.
    pub expires_at_ms: u64,
    /// Increases on every state change.
    pub version: u64,
}

/// `GET /v1/shared-notebooks/{id}/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberList {
    /// Every member, the owner first, then in joining order.
    pub members: Vec<MemberRecord>,
    /// The space's invitations, oldest first. Only the owner receives them; for everyone else
    /// this is empty.
    #[serde(default)]
    pub invitations: Vec<InvitationRecord>,
}

/// `POST /v1/shared-notebooks/{id}/invitations` (owner only).
///
/// The answer has the same status and shape whether or not the address belongs to an account.
/// Retrying with the same `idempotency_key` returns the same invitation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateInvitationRequest {
    /// The invitee's email address (normalized by the server with [`normalize_email`]).
    pub email: String,
    /// `editor` or `viewer`; `owner` is `invalid_role`.
    pub role: Role,
    /// Client-chosen key, stable across retries.
    pub idempotency_key: String,
}

impl CreateInvitationRequest {
    /// Checks the fields and returns the normalized email.
    pub fn validate(&self) -> Result<String, SharingError> {
        if !self.role.is_grantable() {
            return Err(SharingError::Role);
        }
        validate_idempotency_key(&self.idempotency_key)?;
        normalize_email(&self.email)
    }
}

/// `PATCH /v1/shared-notebooks/{id}/members/{account_id}` (owner only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRoleRequest {
    /// `editor` or `viewer`; `owner` is `invalid_role`.
    pub role: Role,
    /// The member's current [`MemberRecord::version`]. A stale version is `conflict`.
    pub version: u64,
}

impl ChangeRoleRequest {
    /// Checks the role.
    pub fn validate(&self) -> Result<(), SharingError> {
        if !self.role.is_grantable() {
            return Err(SharingError::Role);
        }
        Ok(())
    }
}

/// An invitation addressed to the caller (`GET /v1/invitations`). It carries no notebook title or
/// content: the invitee cannot decrypt `sealed_meta` before joining.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingInvitation {
    /// The invitation's id.
    pub invitation_id: InvitationId,
    /// The space it admits to.
    pub space_id: SpaceId,
    /// The role on accepting.
    pub role: Role,
    /// The owner's verified email, so the invitee knows who is asking. Empty when the server has
    /// none.
    pub invited_by: String,
    /// When it stops being acceptable, Unix milliseconds.
    pub expires_at_ms: u64,
}

impl IncomingInvitation {
    /// Checks `invited_by`.
    pub fn validate(&self) -> Result<(), SharingError> {
        validate_display(&self.invited_by)
    }
}

/// `GET /v1/invitations`: pending, unexpired invitations for the caller's verified email.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingInvitationList {
    /// Oldest first. Always empty when `reauth_required` is set.
    pub invitations: Vec<IncomingInvitation>,
    /// The caller's email verification is older than the server's re-authentication window, so
    /// the server didn't look for invitations. Sign in again to see them. A list request never
    /// fails for this reason, so periodic refreshes keep working. Absent on the wire when false;
    /// older servers never send it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reauth_required: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_are_lowercase_strings() {
        for (r, s) in [
            (Role::Owner, "\"owner\""),
            (Role::Editor, "\"editor\""),
            (Role::Viewer, "\"viewer\""),
        ] {
            assert_eq!(serde_json::to_string(&r).unwrap(), s);
            assert_eq!(serde_json::from_str::<Role>(s).unwrap(), r);
            assert_eq!(format!("\"{r}\""), s);
        }
        assert_eq!(
            serde_json::from_str::<Role>("\"commenter\"").unwrap(),
            Role::Commenter
        );
        // Unknown roles from a newer server read as `unknown`, which can do nothing.
        let unknown = serde_json::from_str::<Role>("\"admin\"").unwrap();
        assert_eq!(unknown, Role::Unknown);
        assert!(!unknown.may_write() && !unknown.may_comment() && !unknown.is_grantable());
        assert!(Role::Commenter.may_comment() && !Role::Commenter.may_write());
        assert!(Role::Commenter.is_grantable() && !Role::Viewer.may_comment());
        assert!(Role::Editor.may_write() && Role::Owner.may_write() && !Role::Viewer.may_write());
        assert!(!Role::Owner.is_grantable());
    }

    #[test]
    fn states_are_lowercase_strings() {
        assert_eq!(
            serde_json::to_string(&MembershipState::Revoked).unwrap(),
            "\"revoked\""
        );
        assert_eq!(
            serde_json::to_string(&InvitationState::Cancelled).unwrap(),
            "\"cancelled\""
        );
        assert_eq!(
            serde_json::from_str::<InvitationState>("\"expired\"").unwrap(),
            InvitationState::Expired
        );
    }

    #[test]
    fn emails_are_trimmed_and_lowercased() {
        assert_eq!(
            normalize_email("  Ada.Lovelace@Example.ORG \n").unwrap(),
            "ada.lovelace@example.org"
        );
        assert_eq!(
            normalize_email("ÉMILE@exemple.fr").unwrap(),
            "émile@exemple.fr"
        );
        for bad in [
            "",
            "   ",
            "no-at.example.org",
            "two@@example.org",
            "a@b@example.org",
            "@example.org",
            "ada@",
            "ada@localhost",
            "ada@.example.org",
            "ada@example.org.",
            "ada lovelace@example.org",
            "ada@exa\u{0}mple.org",
        ] {
            assert_eq!(normalize_email(bad), Err(SharingError::Email), "{bad:?}");
        }
        let long = format!("{}@example.org", "a".repeat(MAX_EMAIL_BYTES));
        assert_eq!(normalize_email(&long), Err(SharingError::Email));
    }

    #[test]
    fn bidi_controls_are_refused_in_emails_and_display_strings() {
        let bidi = [
            '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}',
            '\u{2068}', '\u{2069}', '\u{200E}', '\u{200F}', '\u{061C}',
        ];
        for c in bidi {
            assert!(is_bidi_control(c), "{c:?}");
            for email in [
                format!("ada{c}@example.org"),
                format!("{c}ada@example.org"),
                format!("ada@exa{c}mple.org"),
            ] {
                assert_eq!(
                    normalize_email(&email),
                    Err(SharingError::Email),
                    "{email:?}"
                );
            }
            let m = MemberRecord {
                account_id: "a".parse().unwrap(),
                role: Role::Viewer,
                state: MembershipState::Active,
                display: format!("gro.elpmaxe@{c}ada"),
                is_you: false,
                version: 1,
                since_ms: 0,
            };
            assert_eq!(m.validate(), Err(SharingError::Display));
        }
        // Neighbouring characters are fine: letters of right-to-left scripts and the zero-width
        // joiner are not formatting overrides.
        for c in [
            '\u{200D}', '\u{2065}', '\u{2070}', '\u{202F}', '\u{05D0}', '\u{0627}',
        ] {
            assert!(!is_bidi_control(c), "{c:?}");
        }
        assert!(normalize_email("\u{05D0}\u{05D1}@example.org").is_ok());
    }

    #[test]
    fn sealed_meta_and_keys_are_bounded() {
        assert!(validate_sealed_meta("AAAA").is_ok());
        assert!(validate_sealed_meta(&"A".repeat(MAX_SEALED_META_BYTES)).is_ok());
        assert!(validate_sealed_meta(&"A".repeat(MAX_SEALED_META_BYTES + 4)).is_err());
        assert!(validate_sealed_meta("").is_err());
        assert!(validate_sealed_meta("AA==").is_err());
        assert!(validate_sealed_meta("A+/A").is_err());
        assert!(validate_idempotency_key("0190c6f2-7b1a-7cc0-9d1e-2f3a4b5c6d7e").is_ok());
        assert!(validate_idempotency_key("").is_err());
        assert!(validate_idempotency_key("has space").is_err());
        assert!(validate_idempotency_key(&"k".repeat(MAX_IDEMPOTENCY_KEY_BYTES + 1)).is_err());
    }

    #[test]
    fn granting_owner_is_invalid_role() {
        let req = CreateInvitationRequest {
            email: "a@example.org".into(),
            role: Role::Owner,
            idempotency_key: "k1".into(),
        };
        let e = req.validate().unwrap_err();
        assert_eq!(e.error_code(), ErrorCode::InvalidRole);
        let ok = CreateInvitationRequest {
            role: Role::Viewer,
            email: " A@Example.org".into(),
            ..req
        };
        assert_eq!(ok.validate().unwrap(), "a@example.org");
        assert_eq!(
            ChangeRoleRequest {
                role: Role::Owner,
                version: 1
            }
            .validate()
            .unwrap_err()
            .error_code(),
            ErrorCode::InvalidRole
        );
        assert_eq!(
            SharingError::Email.error_code(),
            ErrorCode::ValidationFailed
        );
    }

    #[test]
    fn reauth_required_is_omitted_when_false() {
        let l: IncomingInvitationList = serde_json::from_str(r#"{"invitations":[]}"#).unwrap();
        assert!(!l.reauth_required);
        assert_eq!(serde_json::to_string(&l).unwrap(), r#"{"invitations":[]}"#);
        let stale = IncomingInvitationList {
            invitations: vec![],
            reauth_required: true,
        };
        assert_eq!(
            serde_json::to_string(&stale).unwrap(),
            r#"{"invitations":[],"reauth_required":true}"#
        );
    }

    #[test]
    fn member_list_invitations_default_to_empty() {
        let l: MemberList = serde_json::from_str(r#"{"members":[]}"#).unwrap();
        assert!(l.invitations.is_empty());
    }

    #[test]
    fn records_check_owner_flag_and_display() {
        let mut r = SharedNotebookRecord {
            space_id: "s-1".parse().unwrap(),
            role: Role::Editor,
            state: MembershipState::Active,
            owner: false,
            sealed_meta: "AAAA".into(),
            version: 1,
            created_at_ms: 0,
        };
        assert!(r.validate().is_ok());
        r.owner = true;
        assert!(r.validate().is_err());
        let m = MemberRecord {
            account_id: "a".parse().unwrap(),
            role: Role::Viewer,
            state: MembershipState::Active,
            display: "bob\u{7}@example.org".into(),
            is_you: false,
            version: 1,
            since_ms: 0,
        };
        assert_eq!(m.validate(), Err(SharingError::Display));
    }
}
