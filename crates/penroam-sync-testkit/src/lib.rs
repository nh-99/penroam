//! Public Penroam sync testkit.
//!
//! * [`InMemoryServer`]: a protocol-compatible **development test server**
//!   for tests and local development. In-process callers pass the account
//!   and device directly. It can persist its state to a directory
//!   ([`InMemoryServer::open_persistent`]). It is not a production server:
//!   it has no real authentication, billing, rate limiting or hardening.
//! * [`http`]: an HTTP/1.1 front end with a stub login and the `/v1/live`
//!   WebSocket (also shipped as the `penroam-sync-testserver` binary).
//! * The live channel hub ([`InMemoryServer::live_connect`] and friends): commit hints,
//!   verified awareness relay, membership control and fault knobs.
//! * [`fixtures`]: the deterministic generator behind the
//!   `penroam-sync-fixtures` binary, which writes/checks the committed
//!   contract fixtures in `crates/penroam-sync-protocol/fixtures/v1/`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cursor;
pub mod fixtures;
pub mod http;
mod server;

pub use server::{
    Caller, CommentPage, InMemoryServer, LiveConfig, LivePresence, LiveRefused, PullPage,
    RequestError,
};
