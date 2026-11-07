//! Request/response DTOs for the v1 API (JSON control documents).

use serde::{Deserialize, Serialize};

use crate::base64url;
use crate::envelope::EnvelopeHeader;
use crate::error::ErrorBody;
use crate::ids::{AccountId, DeviceId, OpId};
use crate::limits::Limits;
use crate::version::ProtocolVersion;

/// Maximum cursor length in characters.
pub const CURSOR_MAX_LEN: usize = 512;

/// Well-known capability strings. Minor-version features are only used when
/// the server lists the matching capability.
pub mod capabilities {
    /// Framed batch push (`POST /v1/ops/batch`).
    pub const OPS_BATCH_V1: &str = "ops.batch.v1";
    /// Paginated pull (`GET /v1/ops`).
    pub const OPS_PULL_V1: &str = "ops.pull.v1";
    /// Per-device applied-cursor acks (`POST /v1/acks`).
    pub const ACKS_V1: &str = "acks.v1";
    /// Usage counters (`GET /v1/usage`).
    pub const USAGE_V1: &str = "usage.v1";
    /// Append-only, opaque account key log (`GET`/`POST /v1/keylog`).
    pub const KEYLOG_V1: &str = "keylog.v1";
    /// Encrypted immutable blobs (`PUT`/`GET /v1/blobs/{id}`).
    pub const BLOBS_V1: &str = "blobs.v1";
    /// Signed checkpoint records (`POST /v1/checkpoints`,
    /// `GET /v1/checkpoints/latest`).
    pub const CHECKPOINTS_V1: &str = "checkpoints.v1";
    /// OpenID Connect sign-in and device sessions (`/v1/auth/*`, protocol
    /// 1.1; see [`crate::auth`]).
    pub const AUTH_V1: &str = "auth.v1";
    /// Plan, storage allowance, billing link and deletion request (`/v1/account*`, protocol
    /// 1.3; see [`crate::account`]).
    pub const ACCOUNT_V1: &str = "account.v1";
    /// Signed handwriting model catalog and model downloads (`/v1/models/*`, protocol 1.4; see
    /// [`crate::models`]).
    pub const MODELS_V1: &str = "models.v1";
    /// Paid plans bought through an app store (`POST /v1/account/store-purchase` and the store
    /// fields of [`crate::account`], protocol 1.5).
    pub const STORE_PURCHASES_V1: &str = "store-purchases.v1";
}

/// `GET /v1/meta` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerMeta {
    /// Oldest client protocol accepted.
    pub min_client_protocol: ProtocolVersion,
    /// Newest client protocol accepted.
    pub max_client_protocol: ProtocolVersion,
    /// Optional features (see [`capabilities`]); unknown strings are ignored.
    pub capabilities: Vec<String>,
    /// Effective limits (clients must [`Limits::validate`] them).
    pub limits: Limits,
    /// Opaque id of this server deployment/instance.
    pub server_instance_id: String,
}

impl ServerMeta {
    /// Whether the server advertises `cap`.
    pub fn has_capability(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

/// Opaque pagination cursor issued by the server. Clients never parse it.
/// Bounded unpadded-base64url text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cursor(String);

/// Not a well-formed cursor string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cursor must be 1..={CURSOR_MAX_LEN} characters of unpadded base64url")]
pub struct CursorFormatError;

impl Cursor {
    /// Validate the textual form (not the server-side binding).
    pub fn new(s: impl Into<String>) -> Result<Self, CursorFormatError> {
        let s = s.into();
        if s.is_empty() || s.len() > CURSOR_MAX_LEN || base64url::decode(&s).is_err() {
            return Err(CursorFormatError);
        }
        Ok(Self(s))
    }

    /// The cursor text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Cursor {
    type Error = CursorFormatError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<Cursor> for String {
    fn from(c: Cursor) -> String {
        c.0
    }
}

/// Result of one pushed envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AckResult {
    /// Newly persisted with this account sequence.
    Accepted {
        /// Assigned account sequence.
        server_seq: u64,
    },
    /// Already persisted earlier with identical content (idempotent retry).
    Duplicate {
        /// Sequence assigned on first acceptance.
        server_seq: u64,
    },
    /// Not persisted.
    Rejected {
        /// Why.
        error: ErrorBody,
    },
}

/// Per-op acknowledgement. Clients ack exact operations, not "the batch".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpAck {
    /// The pushed op.
    pub op_id: OpId,
    /// Outcome.
    pub result: AckResult,
}

/// `POST /v1/ops/batch` response: one ack per framed item, in frame order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushOpsResponse {
    /// Acks in request order.
    pub acks: Vec<OpAck>,
}

/// `GET /v1/ops` query parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullOpsRequest {
    /// Resume after this cursor; `None` starts from the beginning of the
    /// retained log.
    pub cursor: Option<Cursor>,
    /// Requested page size; clamped by the server to `max_pull_page_size`.
    pub limit: u32,
}

impl PullOpsRequest {
    /// URL query string (`cursor` is base64url, so no escaping is needed).
    pub fn to_query(&self) -> String {
        match &self.cursor {
            Some(c) => format!("cursor={}&limit={}", c.as_str(), self.limit),
            None => format!("limit={}", self.limit),
        }
    }
}

/// One pulled envelope header with its account sequence. Also the item
/// header JSON of a framed pull body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PulledOp {
    /// Account sequence (delivery order, not causality).
    pub server_seq: u64,
    /// The envelope header as pushed.
    pub header: EnvelopeHeader,
}

/// `GET /v1/ops` JSON control document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullOpsResponse {
    /// Items in strictly increasing `server_seq`.
    pub items: Vec<PulledOp>,
    /// Cursor to resume after the last item (or the input position when the
    /// page is empty). Persist it only after durably applying the items.
    pub next_cursor: Cursor,
    /// Whether more items existed at query time.
    pub has_more: bool,
}

/// `POST /v1/acks`: the cursor this device has *durably applied* (not merely
/// received). Device identity comes from authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckRequest {
    /// Applied cursor.
    pub applied_cursor: Cursor,
}

/// `GET /v1/usage` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageResponse {
    /// Account.
    pub account_id: AccountId,
    /// Number of stored op envelopes.
    pub op_count: u64,
    /// Stored ciphertext bytes of ops.
    pub op_bytes: u64,
    /// Number of stored blobs.
    pub blob_count: u64,
    /// Stored blob bytes.
    pub blob_bytes: u64,
    /// Quota in bytes, if any (server policy).
    pub quota_bytes: Option<u64>,
}

/// `POST /v1/devices` request. Public keys are opaque unpadded-base64url
/// strings to the server. Clients never trust keys the server returns: device
/// membership is decided by the signed key log (see the protocol document).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegistrationRequest {
    /// Client-chosen device id (it is part of signed key-log entries). The
    /// server uses it as given or rejects the request with `conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    /// Device signing public key (opaque base64url).
    pub signing_public_key: String,
    /// Device encryption/key-agreement public key (opaque base64url).
    pub encryption_public_key: String,
    /// Optional label. Visible to the server — never put note content here.
    pub label: Option<String>,
}

/// Maximum length of a key field / label in a registration.
pub const DEVICE_FIELD_MAX_LEN: usize = 256;

impl DeviceRegistrationRequest {
    /// Check encodings and bounds.
    pub fn validate(&self) -> Result<(), &'static str> {
        for (name, k) in [
            ("signing_public_key", &self.signing_public_key),
            ("encryption_public_key", &self.encryption_public_key),
        ] {
            if k.is_empty() || k.len() > DEVICE_FIELD_MAX_LEN || base64url::decode(k).is_err() {
                return Err(name);
            }
        }
        if let Some(l) = &self.label {
            if l.len() > DEVICE_FIELD_MAX_LEN || l.chars().any(char::is_control) {
                return Err("label");
            }
        }
        Ok(())
    }
}

/// `POST /v1/devices` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegistrationResponse {
    /// Assigned device id.
    pub device_id: DeviceId,
}

/// One device as the server knows it (`GET /v1/devices`). Informational
/// only: clients take membership and keys from the verified key log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Device id.
    pub device_id: DeviceId,
    /// Signing public key as registered.
    pub signing_public_key: String,
    /// Key-agreement public key as registered.
    pub encryption_public_key: String,
    /// Server-visible label.
    pub label: Option<String>,
    /// Revoked devices can no longer write.
    pub revoked: bool,
}

/// `GET /v1/devices` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceListResponse {
    /// Devices of the authenticated account.
    pub devices: Vec<DeviceInfo>,
}

/// Maximum decoded size of one key-log entry.
pub const KEYLOG_ENTRY_MAX_BYTES: usize = 64 * 1024;
/// Maximum entries returned by one `GET /v1/keylog`.
pub const KEYLOG_PAGE_MAX: usize = 256;

/// One opaque key-log entry. `data` is unpadded base64url of bytes the
/// server never interprets (signed and verified by clients).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLogEntry {
    /// Position in the account key log, starting at 0.
    pub index: u64,
    /// Opaque entry bytes (unpadded base64url).
    pub data: String,
}

/// `POST /v1/keylog`: append at exactly `index` (the current length).
/// A different length means someone else appended first: `conflict` (409),
/// and the client re-reads the log before trying again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLogAppendRequest {
    /// Expected index of the new entry (= current log length).
    pub index: u64,
    /// Opaque entry bytes (unpadded base64url).
    pub data: String,
}

impl KeyLogAppendRequest {
    /// Check encoding and size.
    pub fn validate(&self) -> Result<(), &'static str> {
        match base64url::decode(&self.data) {
            Ok(b) if !b.is_empty() && b.len() <= KEYLOG_ENTRY_MAX_BYTES => Ok(()),
            _ => Err("data"),
        }
    }
}

/// `POST /v1/keylog` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLogAppendResponse {
    /// Index the entry was stored at.
    pub index: u64,
}

/// `GET /v1/keylog?from=N` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLogPage {
    /// Entries from `from`, in order.
    pub entries: Vec<KeyLogEntry>,
    /// Current log length (index the next append must use).
    pub next_index: u64,
}

/// A checkpoint: an encrypted copy of a device's whole operation history
/// (stored as a blob), covering the account log up to `covers_cursor`. A
/// device whose cursor expired after server compaction imports it and
/// continues pulling from `covers_cursor`.
///
/// The server stores only the latest record per account and only accepts a
/// record whose `covers_server_seq` is not lower than the current one. It
/// never sees the content. Clients verify `signature` (Ed25519 by
/// `created_by` over [`crate::canonical::canonical_checkpoint_bytes`]) against
/// their verified key log before using a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    /// Server cursor the checkpoint covers (everything before it is included).
    pub covers_cursor: Cursor,
    /// Account sequence of the last envelope covered.
    pub covers_server_seq: u64,
    /// Blob holding the encrypted checkpoint.
    pub blob_id: crate::ids::BlobId,
    /// Lowercase hex SHA-256 of the blob ciphertext.
    pub blob_sha256: String,
    /// Length of the blob ciphertext in bytes.
    pub blob_length: u64,
    /// Content key epoch the blob is encrypted with.
    pub key_epoch: u32,
    /// Device that created and signed the checkpoint.
    pub created_by: DeviceId,
    /// Unpadded base64url Ed25519 signature.
    pub signature: String,
}

/// `PUT /v1/blobs/{id}` response. Blobs are immutable: the same id with the
/// same bytes is `duplicate`; different bytes are rejected with `conflict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPutResponse {
    /// The blob id.
    pub blob_id: crate::ids::BlobId,
    /// `true` when identical bytes were already stored.
    pub duplicate: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn ack_result_wire_shape() {
        let a = AckResult::Accepted { server_seq: 3 };
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            r#"{"status":"accepted","server_seq":3}"#
        );
        let r = AckResult::Rejected {
            error: ErrorBody::new(ErrorCode::OpIdConflict, "m", "r"),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.starts_with(r#"{"status":"rejected","error":{"code":"op_id_conflict""#));
        assert_eq!(serde_json::from_str::<AckResult>(&s).unwrap(), r);
    }

    #[test]
    fn cursor_validation() {
        assert!(Cursor::new("abcd_-AA").is_ok());
        assert!(Cursor::new("").is_err());
        assert!(Cursor::new("a=").is_err());
        assert!(Cursor::new("A".repeat(CURSOR_MAX_LEN + 1)).is_err());
        assert!(serde_json::from_str::<Cursor>("\"a/b\"").is_err());
    }

    #[test]
    fn pull_query() {
        let r = PullOpsRequest {
            cursor: None,
            limit: 10,
        };
        assert_eq!(r.to_query(), "limit=10");
        let r = PullOpsRequest {
            cursor: Some(Cursor::new("abcd").unwrap()),
            limit: 5,
        };
        assert_eq!(r.to_query(), "cursor=abcd&limit=5");
    }

    #[test]
    fn device_registration_validation() {
        let mut d = DeviceRegistrationRequest {
            device_id: None,
            signing_public_key: base64url::encode(&[1; 32]),
            encryption_public_key: base64url::encode(&[2; 32]),
            label: Some("Pixel".into()),
        };
        d.validate().unwrap();
        d.signing_public_key = "not base64!".into();
        assert_eq!(d.validate(), Err("signing_public_key"));
    }

    #[test]
    fn capability_lookup() {
        let m = ServerMeta {
            min_client_protocol: ProtocolVersion::new(1, 0),
            max_client_protocol: ProtocolVersion::new(1, 0),
            capabilities: vec![capabilities::ACKS_V1.into()],
            limits: Limits::DEFAULT,
            server_instance_id: "x".into(),
        };
        assert!(m.has_capability("acks.v1"));
        assert!(!m.has_capability("usage.v1"));
    }
}
