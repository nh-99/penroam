//! Offline-first, end-to-end encrypted sync client for Penroam.
//!
//! * [`crypto`]: key derivation, payload encryption, signatures, key wrapping.
//! * [`keylog`]: the signed, hash-chained account key log (device membership).
//! * [`store`]: durable sync metadata and the operation log.
//! * [`transport`]: protocol calls over HTTP or in-process.
//! * [`engine`]: the reconcile loop (prepare → exchange → apply).
//!
//! **Security status:** the cryptographic design has not been independently
//! reviewed (`security_review_pending`). See `docs/security/threat-model.md`.

#![forbid(unsafe_code)]

pub mod crypto;
pub mod engine;
pub mod keylog;
pub mod secrets;
pub mod store;
pub mod transport;

pub use engine::{DeviceView, Phase, ReconcileReport, Setup, SyncEngine, SyncError, SyncStatus};
pub use secrets::{FileSecretStore, SecretStore};
pub use transport::{SyncTransport, TransportError};
