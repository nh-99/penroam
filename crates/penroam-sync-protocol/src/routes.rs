//! Route paths for the v1 HTTP API.
//!
//! Frozen for protocol 1.0 (see the HTTP mapping in the protocol document).
//! No HTTP client or server is implemented in this crate. All routes except
//! [`META`] require `Authorization: Bearer <token>`. There are no billing
//! routes in the public protocol.

/// `GET` — protocol range, capabilities, limits, server instance id. Unauthenticated.
pub const META: &str = "/v1/meta";
/// `POST` register a device / `GET` list devices.
pub const DEVICES: &str = "/v1/devices";
/// `DELETE` revoke a device. `{id}` is a [`crate::DeviceId`].
pub const DEVICE: &str = "/v1/devices/{id}";
/// `POST` framed batch of envelopes; returns [`crate::PushOpsResponse`].
pub const OPS_BATCH: &str = "/v1/ops/batch";
/// `GET ?cursor=..&limit=..` paginated envelopes; see [`crate::PullOpsResponse`].
pub const OPS: &str = "/v1/ops";
/// `POST` initiate a scoped blob upload (not implemented).
pub const BLOB_UPLOADS: &str = "/v1/blobs/uploads";
/// `PUT` upload / `GET` download an encrypted immutable blob (raw bytes;
/// `PUT` carries [`HEADER_CONTENT_SHA256`]).
pub const BLOB: &str = "/v1/blobs/{id}";
/// `POST` advertise / `GET` fetch encrypted checkpoints (not implemented).
pub const CHECKPOINTS: &str = "/v1/checkpoints";
/// `POST` per-device applied-cursor acknowledgement ([`crate::AckRequest`]).
pub const ACKS: &str = "/v1/acks";
/// `GET` usage counters ([`crate::UsageResponse`]).
pub const USAGE: &str = "/v1/usage";
/// `GET ?from=N` / `POST` the opaque account key log
/// ([`crate::dto::KeyLogPage`], [`crate::dto::KeyLogAppendRequest`]).
pub const KEYLOG: &str = "/v1/keylog";

/// JSON control documents.
pub const MEDIA_JSON: &str = "application/json";
/// Framed batches (push bodies, framed pull bodies).
pub const MEDIA_BATCH: &str = "application/vnd.penroam.batch.v1";
/// Raw encrypted blob bytes.
pub const MEDIA_OCTETS: &str = "application/octet-stream";
/// Request id echoed on every response (also inside error bodies).
pub const HEADER_REQUEST_ID: &str = "Penroam-Request-Id";
/// Lowercase hex SHA-256 of a `PUT` blob body.
pub const HEADER_CONTENT_SHA256: &str = "Penroam-Content-Sha256";
/// Response header carrying the next cursor of a framed pull.
pub const HEADER_NEXT_CURSOR: &str = "Penroam-Next-Cursor";
/// Response header carrying `has_more` (`true`/`false`) of a framed pull.
pub const HEADER_HAS_MORE: &str = "Penroam-Has-More";
/// Request header carrying the negotiated protocol version (`1.0`).
pub const HEADER_PROTOCOL: &str = "Penroam-Protocol";

/// Every route, for documentation/compat tests.
pub const ALL: [&str; 11] = [
    META,
    DEVICES,
    DEVICE,
    OPS_BATCH,
    OPS,
    BLOB_UPLOADS,
    BLOB,
    CHECKPOINTS,
    ACKS,
    USAGE,
    KEYLOG,
];

#[cfg(test)]
mod tests {
    #[test]
    fn all_versioned() {
        for r in super::ALL {
            assert!(r.starts_with("/v1/"), "{r}");
        }
    }
}
