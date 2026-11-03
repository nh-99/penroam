//! Public Penroam sync testkit.
//!
//! * [`InMemoryServer`]: a protocol-compatible **development test server**
//!   for tests and local development. In-process callers pass the account
//!   and device directly. It can persist its state to a directory
//!   ([`InMemoryServer::open_persistent`]). It is not a production server:
//!   it has no real authentication, billing, rate limiting or hardening.
//! * [`http`]: an HTTP/1.1 front end with a stub login (also shipped as the
//!   `penroam-sync-testserver` binary).
//! * [`fixtures`]: the deterministic generator behind the
//!   `penroam-sync-fixtures` binary, which writes/checks the committed
//!   contract fixtures in `crates/penroam-sync-protocol/fixtures/v1/`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cursor;
pub mod fixtures;
pub mod http;
mod server;

pub use server::{InMemoryServer, PullPage, RequestError};
