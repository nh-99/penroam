//! Version numbers and protocol negotiation.
//!
//! Every number in this module is **independent**. None of them may be
//! derived from another: bumping the page file format does not bump the
//! protocol, bumping the protocol minor does not bump the envelope version,
//! and so on.

use serde::{Deserialize, Serialize};

use crate::dto::ServerMeta;
use crate::error::ErrorCode;

/// Sync protocol major version. Different majors are never compatible.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Sync protocol minor version. Minors within a major are additive; new
/// behaviour is gated by capabilities, never assumed from the minor number.
pub const PROTOCOL_MINOR: u16 = 6;
/// Version of the opaque remote container (the envelope header + framing).
pub const ENVELOPE_VERSION: u16 = 1;

/// Current semantic operation payload schema (inside the ciphertext).
pub const OP_SCHEMA_VERSION: u16 = 3;
/// Oldest semantic operation payload schema this client can still apply.
pub const OP_SCHEMA_MIN_SUPPORTED: u16 = 1;

/// Local `.prp` page file format version advertised in the contract manifest.
///
/// Duplicated here on purpose (this crate must not depend on
/// `penroam-storage`); a cross-check test in the storage crate keeps them in
/// sync.
pub const FILE_FORMAT_VERSION: u16 = 1;
/// Local operation journal format version (duplicated, see above).
pub const JOURNAL_FORMAT_VERSION: u16 = 1;
/// `.penroam` archive format version (duplicated, see above).
pub const ARCHIVE_FORMAT_VERSION: u16 = 2;
/// Notebook model schema version (duplicated, see above).
pub const NOTEBOOK_SCHEMA_VERSION: u16 = 2;
/// Page model schema version (duplicated, see above).
pub const PAGE_SCHEMA_VERSION: u16 = 1;

/// A `major.minor` protocol version. Ordered by major, then minor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion {
    /// Major version (breaking).
    pub major: u16,
    /// Minor version (additive).
    pub minor: u16,
}

impl ProtocolVersion {
    /// The version implemented by this crate.
    pub const CURRENT: ProtocolVersion = ProtocolVersion {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
    };

    /// Construct a version.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// An inclusive range of protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolRange {
    /// Lowest supported version (inclusive).
    pub min: ProtocolVersion,
    /// Highest supported version (inclusive).
    pub max: ProtocolVersion,
}

impl ProtocolRange {
    /// The range this client implementation speaks: `1.0` through `1.5`.
    pub const CLIENT: ProtocolRange = ProtocolRange {
        min: ProtocolVersion::new(PROTOCOL_MAJOR, 0),
        max: ProtocolVersion::CURRENT,
    };

    /// `true` when `min <= max`.
    pub fn is_well_formed(&self) -> bool {
        self.min <= self.max
    }

    /// `true` when `v` lies inside the range.
    pub fn contains(&self, v: ProtocolVersion) -> bool {
        self.min <= v && v <= self.max
    }
}

/// Why negotiation failed. Every variant maps to
/// [`ErrorCode::UpgradeRequired`]: the client must stop syncing, keep its
/// outbox and local data, and surface an actionable "update needed" state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Incompatible {
    /// The server's minimum is newer than anything this client speaks:
    /// the **client** must be upgraded.
    #[error("client protocol {client_max} is older than the server minimum {server_min}; upgrade the app")]
    ClientTooOld {
        /// Highest version the client speaks.
        client_max: ProtocolVersion,
        /// Lowest version the server accepts.
        server_min: ProtocolVersion,
    },
    /// The server's maximum is older than anything this client speaks:
    /// the **server** is outdated (or the client dropped support).
    #[error("server protocol {server_max} is older than the client minimum {client_min}")]
    ServerTooOld {
        /// Lowest version the client speaks.
        client_min: ProtocolVersion,
        /// Highest version the server accepts.
        server_max: ProtocolVersion,
    },
    /// One of the advertised ranges has `min > max`.
    #[error("malformed protocol range (min > max)")]
    MalformedRange,
}

impl Incompatible {
    /// The normalized error code to surface.
    pub fn error_code(&self) -> ErrorCode {
        ErrorCode::UpgradeRequired
    }
}

/// Pick the protocol version to speak with a server.
///
/// Chooses the highest version supported by both sides. Fails safely (never
/// "tries anyway") when the ranges do not overlap, which in particular covers
/// every incompatible-major case. Minor-version features must still be
/// checked through [`ServerMeta::has_capability`]; a negotiated minor does not
/// imply any optional feature.
pub fn negotiate(
    client: ProtocolRange,
    server: &ServerMeta,
) -> Result<ProtocolVersion, Incompatible> {
    let server_range = ProtocolRange {
        min: server.min_client_protocol,
        max: server.max_client_protocol,
    };
    if !client.is_well_formed() || !server_range.is_well_formed() {
        return Err(Incompatible::MalformedRange);
    }
    if client.max < server_range.min {
        return Err(Incompatible::ClientTooOld {
            client_max: client.max,
            server_min: server_range.min,
        });
    }
    if server_range.max < client.min {
        return Err(Incompatible::ServerTooOld {
            client_min: client.min,
            server_max: server_range.max,
        });
    }
    Ok(client.max.min(server_range.max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    fn meta(min: (u16, u16), max: (u16, u16)) -> ServerMeta {
        ServerMeta {
            min_client_protocol: ProtocolVersion::new(min.0, min.1),
            max_client_protocol: ProtocolVersion::new(max.0, max.1),
            capabilities: vec![],
            limits: Limits::default(),
            server_instance_id: "test".into(),
        }
    }

    #[test]
    fn exact_match() {
        assert_eq!(
            negotiate(ProtocolRange::CLIENT, &meta((1, 0), (1, 0))),
            Ok(ProtocolVersion::new(1, 0))
        );
    }

    #[test]
    fn newer_server_minor_negotiates_down_to_client_max() {
        assert_eq!(
            negotiate(ProtocolRange::CLIENT, &meta((1, 0), (1, 7))),
            Ok(ProtocolVersion::CURRENT)
        );
    }

    #[test]
    fn newer_client_minor_negotiates_down_to_server_max() {
        let client = ProtocolRange {
            min: ProtocolVersion::new(1, 0),
            max: ProtocolVersion::new(1, 3),
        };
        assert_eq!(
            negotiate(client, &meta((1, 0), (1, 1))),
            Ok(ProtocolVersion::new(1, 1))
        );
    }

    #[test]
    fn server_requires_newer_major() {
        let err = negotiate(ProtocolRange::CLIENT, &meta((2, 0), (2, 4))).unwrap_err();
        assert!(matches!(err, Incompatible::ClientTooOld { .. }));
        assert_eq!(err.error_code(), ErrorCode::UpgradeRequired);
    }

    #[test]
    fn server_requires_newer_minor() {
        let err = negotiate(ProtocolRange::CLIENT, &meta((1, 7), (1, 8))).unwrap_err();
        assert!(matches!(err, Incompatible::ClientTooOld { .. }));
    }

    #[test]
    fn server_too_old() {
        let client = ProtocolRange {
            min: ProtocolVersion::new(2, 0),
            max: ProtocolVersion::new(2, 0),
        };
        let err = negotiate(client, &meta((1, 0), (1, 9))).unwrap_err();
        assert!(matches!(err, Incompatible::ServerTooOld { .. }));
    }

    #[test]
    fn spanning_server_range_picks_client_major() {
        assert_eq!(
            negotiate(ProtocolRange::CLIENT, &meta((1, 0), (3, 0))),
            Ok(ProtocolVersion::CURRENT)
        );
    }

    #[test]
    fn malformed_ranges_rejected() {
        assert_eq!(
            negotiate(ProtocolRange::CLIENT, &meta((1, 5), (1, 0))),
            Err(Incompatible::MalformedRange)
        );
        let client = ProtocolRange {
            min: ProtocolVersion::new(1, 1),
            max: ProtocolVersion::new(1, 0),
        };
        assert_eq!(
            negotiate(client, &meta((1, 0), (1, 0))),
            Err(Incompatible::MalformedRange)
        );
    }

    #[test]
    fn ordering_is_major_then_minor() {
        assert!(ProtocolVersion::new(1, 99) < ProtocolVersion::new(2, 0));
        assert!(ProtocolVersion::new(1, 0) < ProtocolVersion::new(1, 1));
    }
}
