//! Public Penroam sync protocol v1 contract.
//!
//! This crate defines the *wire contract* shared by Penroam clients, the
//! public development test server (`penroam-sync-testkit`) and any private
//! server implementation:
//!
//! * protocol / format version numbers and negotiation ([`version`]),
//! * opaque, validated identifiers ([`ids`]),
//! * size limits with defaults and hard maxima ([`limits`]),
//! * the envelope control header and its validation ([`envelope`]),
//! * deterministic canonical byte layouts for signatures and AEAD associated
//!   data ([`canonical`]) — **no cryptography is implemented here**; the client
//!   crate `penroam-sync` signs and encrypts over these bytes,
//! * binary batch framing for push/pull bodies ([`framing`]),
//! * request/response DTOs and the HTTP routes, media types and headers
//!   ([`dto`], [`routes`]),
//! * normalized error codes and a deterministic retry policy ([`error`]),
//! * the contract manifest and fixture helpers ([`contract`]).
//!
//! The document payloads themselves are opaque ciphertext: this crate never
//! sees plaintext notes and deliberately does not depend on `penroam-core`.
//!
//! The normative description lives in `docs/protocol/sync-protocol-v1.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod base64url;
pub mod canonical;
pub mod contract;
pub mod dto;
pub mod envelope;
pub mod error;
pub mod framing;
pub mod hex;
pub mod ids;
pub mod limits;
pub mod routes;
pub mod version;

pub use canonical::{canonical_aad_bytes, canonical_signing_bytes};
pub use contract::ContractManifest;
pub use dto::{
    AckRequest, AckResult, BlobPutResponse, Cursor, DeviceInfo, DeviceListResponse,
    DeviceRegistrationRequest, DeviceRegistrationResponse, KeyLogAppendRequest,
    KeyLogAppendResponse, KeyLogEntry, KeyLogPage, OpAck, PullOpsRequest, PullOpsResponse,
    PulledOp, PushOpsResponse, ServerMeta, UsageResponse,
};
pub use envelope::{verify_ciphertext, EnvelopeError, EnvelopeHeader, IntegrityError, OpRef};
pub use error::{ErrorBody, ErrorCode, RetryPolicy};
pub use framing::{FrameError, FrameItem};
pub use ids::{AccountId, BlobId, DeviceId, IdError, NotebookRoutingId, OpId};
pub use limits::Limits;
pub use version::{negotiate, Incompatible, ProtocolRange, ProtocolVersion};
