//! Public Penroam sync testkit.
//!
//! * [`InMemoryServer`]: a protocol-compatible **fake** server for tests and
//!   local development (scaffolding for the M03 client sync engine). It is
//!   not a production server: no auth, no persistence, no real crypto.
//! * [`fixtures`]: the deterministic generator behind the
//!   `penroam-sync-fixtures` binary, which writes/checks the committed
//!   contract fixtures in `crates/penroam-sync-protocol/fixtures/v1/`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cursor;
pub mod fixtures;
mod server;

pub use server::{InMemoryServer, PullPage, RequestError};
