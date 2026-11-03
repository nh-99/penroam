//! Offline-first, end-to-end encrypted sync client for Penroam.
//!
//! The client sits next to a `penroam_storage::Library`: local edits keep
//! being saved by the library exactly as without sync, and the engine copies
//! them into its own operation log, encrypts and signs them, exchanges them
//! with a server that only stores ciphertext, merges remote operations with
//! `penroam_core::replica::Replica`, and installs the merged result back into
//! the library.
//!
//! * [`crypto`]: key derivation, payload encryption, signatures, key wrapping.
//! * [`keylog`]: the signed, hash-chained account key log (device membership).
//! * [`store`]: durable sync metadata and the operation log.
//! * [`secrets`]: where device private keys live (OS keychain or a file).
//! * [`transport`]: protocol calls over HTTP or in-process.
//! * [`engine`]: the reconcile loop (prepare → exchange → apply).
//!
//! Cargo features: `http` (default; `transport::HttpTransport`), `keychain`
//! (OS credential stores, `secrets::KeychainSecretStore`), `testkit`
//! (`transport::InProcessTransport` over the public test server).
//!
//! The wire format and client rules are in `docs/protocol/sync-protocol-v1.md`;
//! the design is summarised in `docs/architecture/overview.md`.
//!
//! **Security status:** the cryptographic design has not been independently
//! reviewed (`security_review_pending`). See `docs/security/threat-model.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod crypto;
pub mod engine;
pub mod keylog;
pub mod secrets;
pub mod store;
pub mod transport;

pub use engine::{DeviceView, Phase, ReconcileReport, Setup, SyncEngine, SyncError, SyncStatus};
pub use secrets::{FileSecretStore, SecretStore};
pub use transport::{SyncTransport, TransportError};
