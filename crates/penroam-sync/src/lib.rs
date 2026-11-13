//! Offline-first, encrypted sync client for Penroam.
//!
//! The client sits next to a `penroam_storage::Library`: local edits keep
//! being saved by the library exactly as without sync, and the engine copies
//! them into its own operation log, encrypts and signs them, exchanges them
//! with a server that stores ciphertext, merges remote operations with
//! `penroam_core::replica::Replica`, and installs the merged result back into
//! the library.
//!
//! * [`crypto`]: key derivation, payload encryption, signatures, key wrapping.
//! * [`keylog`]: the signed, hash-chained account key log (device membership).
//! * [`store`]: durable sync metadata and the operation log.
//! * [`secrets`]: where device private keys live (OS keychain or a file).
//! * [`transport`]: protocol calls over HTTP or in-process.
//! * [`engine`]: the reconcile loop (prepare → exchange → apply).
//! * [`models`]: the signed handwriting model catalog and model downloads.
//! * [`vault`]: passphrase-locked vaults, and which notebooks belong to them.
//! * [`locker`]: vaults locked on this device, encrypted at rest.
//! * [`live`]: the live channel of a shared notebook (commit hints, presence).
//! * [`shared`]: shared notebooks: sharing an existing notebook, joining a shared one, roles
//!   and access loss.
//! * `auth` (feature `http`): OpenID Connect sign-in with PKCE in the system
//!   browser and device sessions, for servers that advertise `auth.v1`.
//!
//! Cargo features: `http` (default; `transport::HttpTransport`), `keychain`
//! (OS credential stores, `secrets::KeychainSecretStore`), `testkit`
//! (`transport::InProcessTransport` over the public test server).
//!
//! The wire format and client rules are in `docs/protocol/sync-protocol-v1.md`;
//! the design is summarised in `docs/architecture/overview.md`.
//!
//! Ordinary notebooks are not private from the server, which keeps a copy of the
//! account's root secret so that signing in is enough. Notebooks in a [`vault`]
//! are: their root secret reaches the server only inside a passphrase lock.
//!
//! **Security status:** the cryptographic design has not been independently
//! reviewed (`security_review_pending`). See `docs/security/threat-model.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "http")]
pub mod attribution;
pub mod auth;
pub mod comments;
pub mod crypto;
pub mod engine;
pub mod keylog;
pub mod live;
pub mod locker;
pub mod models;
pub mod secrets;
pub mod shared;
pub mod store;
pub mod transport;
pub mod vault;

pub use engine::{
    DeviceView, ErasedNotebook, Phase, ReconcileReport, Setup, SyncEngine, SyncError, SyncStatus,
    SyncedVersion,
};
pub use secrets::{FileSecretStore, SecretStore};
pub use transport::{SyncTransport, TransportError};
