//! Opaque, validated identifiers.
//!
//! All ids are opaque strings: non-empty, at most [`ID_MAX_LEN`] bytes, using
//! only `[A-Za-z0-9_-]`. The charset keeps ids safe in URLs, filenames, logs
//! and canonical byte encodings without escaping. Ids are validated on
//! construction *and* on deserialization, so an `OpId` value is always valid.
//!
//! Generating ids is the caller's job: clients generate `OpId`s (a UUID in its
//! hyphenated form fits the charset) and stable notebook routing ids; servers
//! assign `AccountId`s and may assign `DeviceId`s.

use serde::{Deserialize, Serialize};

/// Hard upper bound on the byte length of any id. Server [`crate::Limits`]
/// may advertise a lower `max_id_length`, never a higher one.
pub const ID_MAX_LEN: usize = 128;

/// Why a string is not a valid id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// The id is empty.
    #[error("id is empty")]
    Empty,
    /// The id is longer than allowed.
    #[error("id is {len} bytes, maximum is {max}")]
    TooLong {
        /// Actual byte length.
        len: usize,
        /// Maximum byte length.
        max: usize,
    },
    /// The id contains a byte outside `[A-Za-z0-9_-]`.
    #[error("id contains an invalid character at byte {position}")]
    InvalidChar {
        /// Byte offset of the first invalid character.
        position: usize,
    },
}

/// Validate an id string against the shared charset and [`ID_MAX_LEN`].
pub fn validate_id(s: &str) -> Result<(), IdError> {
    if s.is_empty() {
        return Err(IdError::Empty);
    }
    if s.len() > ID_MAX_LEN {
        return Err(IdError::TooLong {
            len: s.len(),
            max: ID_MAX_LEN,
        });
    }
    if let Some(position) = s
        .bytes()
        .position(|b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    {
        return Err(IdError::InvalidChar { position });
    }
    Ok(())
}

macro_rules! opaque_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Validate and wrap a string.
            pub fn new(s: impl Into<String>) -> Result<Self, IdError> {
                let s = s.into();
                validate_id(&s)?;
                Ok(Self(s))
            }

            /// The id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(s: String) -> Result<Self, IdError> {
                Self::new(s)
            }
        }

        impl std::str::FromStr for $name {
            type Err = IdError;
            fn from_str(s: &str) -> Result<Self, IdError> {
                Self::new(s)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> String {
                id.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

opaque_id!(
    /// Server-registered device identity (to be bound to a device signing key).
    DeviceId
);
opaque_id!(
    /// Globally unique, client-generated operation id; stable across retries.
    /// Sortable ids (UUIDv7/ULID) are allowed but never imply causal order.
    OpId
);
opaque_id!(
    /// Opaque, privacy-preserving notebook routing id (never a title).
    NotebookRoutingId
);
opaque_id!(
    /// Random server-assigned account id.
    AccountId
);
opaque_id!(
    /// Opaque id of an encrypted, immutable blob (assets; not used yet).
    BlobId
);
opaque_id!(
    /// Server-assigned id of a shared-notebook invitation (`sharing.v1`, protocol 1.7).
    InvitationId
);
opaque_id!(
    /// Server-assigned id of one live connection (`live.v1`, protocol 1.8).
    LiveSessionId
);
opaque_id!(
    /// Client-chosen routing id of a comment thread, visible to the server (`comments.v1`,
    /// protocol 1.9). Random; never derived from the comment's content.
    ThreadId
);
opaque_id!(
    /// Server-assigned id of a notification (`comments.v1`, protocol 1.9).
    NotificationId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_uuid_and_simple_ids() {
        assert!(OpId::new("0190c6f2-7b1a-7cc0-9d1e-2f3a4b5c6d7e").is_ok());
        assert!(DeviceId::new("device_A-1").is_ok());
        assert!(AccountId::new("a".repeat(ID_MAX_LEN)).is_ok());
    }

    #[test]
    fn rejects_invalid_ids() {
        assert_eq!(OpId::new(""), Err(IdError::Empty));
        assert_eq!(
            OpId::new("a".repeat(ID_MAX_LEN + 1)),
            Err(IdError::TooLong {
                len: ID_MAX_LEN + 1,
                max: ID_MAX_LEN
            })
        );
        assert_eq!(OpId::new("ab/c"), Err(IdError::InvalidChar { position: 2 }));
        assert_eq!(OpId::new("a b"), Err(IdError::InvalidChar { position: 1 }));
        assert_eq!(OpId::new("é"), Err(IdError::InvalidChar { position: 0 }));
        assert_eq!(OpId::new("a\0"), Err(IdError::InvalidChar { position: 1 }));
    }

    #[test]
    fn deserialization_validates() {
        assert!(serde_json::from_str::<DeviceId>("\"ok-id\"").is_ok());
        assert!(serde_json::from_str::<DeviceId>("\"\"").is_err());
        assert!(serde_json::from_str::<DeviceId>("\"no.dots\"").is_err());
        let id: BlobId = serde_json::from_str("\"blob_1\"").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"blob_1\"");
    }
}
