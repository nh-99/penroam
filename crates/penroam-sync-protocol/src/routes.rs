//! Route paths for the v1 HTTP API.
//!
//! **Proposed, not frozen.** These names follow §4 of the protocol design and
//! may still change before the contract is frozen. No HTTP client or
//! server is implemented in this crate. All routes except [`META`] require
//! authentication. There are no billing routes in the public protocol.

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
/// `PUT` upload / `GET` download an encrypted blob (not implemented).
pub const BLOB: &str = "/v1/blobs/{id}";
/// `POST` advertise / `GET` fetch encrypted checkpoints (not implemented).
pub const CHECKPOINTS: &str = "/v1/checkpoints";
/// `POST` per-device applied-cursor acknowledgement ([`crate::AckRequest`]).
pub const ACKS: &str = "/v1/acks";
/// `GET` usage counters ([`crate::UsageResponse`]).
pub const USAGE: &str = "/v1/usage";

/// Proposed response header carrying the next cursor of a framed pull.
pub const HEADER_NEXT_CURSOR: &str = "Penroam-Next-Cursor";
/// Proposed response header carrying `has_more` (`true`/`false`) of a framed pull.
pub const HEADER_HAS_MORE: &str = "Penroam-Has-More";
/// Proposed request header carrying the negotiated protocol version (`1.0`).
pub const HEADER_PROTOCOL: &str = "Penroam-Protocol";

/// Every route, for documentation/compat tests.
pub const ALL: [&str; 10] = [
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
