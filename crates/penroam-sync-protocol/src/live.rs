//! Live co-editing channel (capability `live.v1`, protocol 1.8).
//!
//! `GET /v1/live` ([`crate::routes::LIVE`]) upgrades to a WebSocket for one sharing space,
//! authenticated with a **device session on that space** (`Authorization: Bearer`, never a query
//! string). Every frame is a JSON text frame of at most `max_frame_bytes` bytes, tagged by
//! `"type"`: [`ClientFrame`] for client → server, [`ServerFrame`] for server → client.
//!
//! The socket **never writes notebook data**. Durable truth stays the operation log and its
//! `server_seq` (protocol §8 and §12f). The channel carries only:
//!
//! * `committed` hints: the space's latest `server_seq`, sent after the server committed new
//!   operations. A hint makes the client pull over HTTP now. Hints may be coalesced, delayed,
//!   duplicated or lost; a missed hint costs latency, never data, because clients always catch up
//!   over HTTP on connect, on reconnect and on every hint.
//! * Encrypted awareness (presence, carets, selections, viewports). The payload is sealed with a key
//!   derived from the space root ([`AWARENESS_KEY_INFO`], associated data [`awareness_aad`]); the
//!   server relays it to the space's other sessions with the **verified** sender identity filled
//!   in, overwriting whatever the sender claimed.
//! * Control: `membership` changes, `bye` with a reason ([`ByeCode`]), and `ping`/`pong`
//!   heartbeats.
//! * With `comments.v1` (protocol 1.9): `comment_committed` hints for the comment stream and
//!   `notification` hints (ids only), sent only to sessions whose hello said 1.9 or later
//!   ([`COMMENTS_MIN_PROTOCOL`]).
//!
//! This module holds the frame types, their size and value limits, and parsing helpers that
//! enforce those limits before deserializing. It has no network code. The normative description is
//! `docs/protocol/sync-protocol-v1.md` §12g.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, DeviceId, LiveSessionId};
use crate::sharing::{MembershipState, Role};
use crate::version::{ProtocolVersion, PROTOCOL_MAJOR};

/// Default `max_frame_bytes` a server advertises in [`Welcome`]: 16 KiB.
pub const DEFAULT_MAX_FRAME_BYTES: u32 = 16 * 1024;
/// Smallest `max_frame_bytes` a client accepts. Below this an awareness frame wouldn't fit.
pub const MIN_MAX_FRAME_BYTES: u32 = 4 * 1024;
/// Largest `max_frame_bytes` a client accepts (64 KiB). A hostile server must not be able to make
/// a client buffer arbitrarily large frames.
pub const MAX_MAX_FRAME_BYTES: u32 = 64 * 1024;

/// Largest awareness plaintext a client seals: 2 KiB of JSON.
pub const MAX_AWARENESS_PLAINTEXT_BYTES: usize = 2048;
/// Sealing overhead of the awareness payload: 24-byte nonce plus 16-byte tag (protocol §13).
pub const AWARENESS_SEAL_OVERHEAD: usize = 40;
/// Largest `payload` string: unpadded base64url of the largest sealed plaintext (2784 characters).
pub const MAX_AWARENESS_PAYLOAD_CHARS: usize =
    (MAX_AWARENESS_PLAINTEXT_BYTES + AWARENESS_SEAL_OVERHEAD).div_ceil(3) * 4;

/// Default heartbeat interval a server advertises: 20 s.
pub const DEFAULT_HEARTBEAT_MS: u32 = 20_000;
/// Shortest heartbeat interval a client accepts (5 s), so a server can't make clients spin.
pub const MIN_HEARTBEAT_MS: u32 = 5_000;
/// Longest heartbeat interval allowed (30 s). Servers re-check membership at least this often.
pub const MAX_HEARTBEAT_MS: u32 = 30_000;

/// Shortest awareness `ttl_ms` accepted (1 s).
pub const MIN_TTL_MS: u32 = 1_000;
/// Longest awareness `ttl_ms` accepted (60 s, twice [`MAX_HEARTBEAT_MS`]). Servers clamp it further
/// to twice their own heartbeat, so nobody stays "online" beyond that after going quiet.
pub const MAX_TTL_MS: u32 = 2 * MAX_HEARTBEAT_MS;

/// Recommended per-session awareness rate: 30 frames per second ...
pub const AWARENESS_RATE_PER_S: u32 = 30;
/// ... with bursts of up to 60.
pub const AWARENESS_BURST: u32 = 60;

/// Oldest protocol a live hello may name: the version that introduced `live.v1`.
pub const LIVE_MIN_PROTOCOL: ProtocolVersion = ProtocolVersion::new(1, 8);
/// Oldest hello protocol that receives `comment_committed` and `notification` frames and sees the
/// `commenter` role (`comments.v1`). Servers send older sessions neither, and show `commenter` to
/// them as `viewer`.
pub const COMMENTS_MIN_PROTOCOL: ProtocolVersion = ProtocolVersion::new(1, 9);

/// HKDF-SHA256 `info` for the awareness key, derived from the space root (protocol §13 rules).
pub const AWARENESS_KEY_INFO: &str = "penroam/awareness/v1";
/// Domain prefix of [`awareness_aad`].
pub const AWARENESS_AAD_DOMAIN: &[u8] = b"penroam/awareness/v1\0";

/// Associated data for an awareness payload: [`AWARENESS_AAD_DOMAIN`], then the space id and the
/// sender's device id, each as a u32 little-endian byte length followed by the bytes (the string
/// encoding of protocol §5), so no two id pairs share an encoding.
///
/// The sender seals with its own device id; a receiver opens with the `device_id` the server put
/// in the relayed frame, so a payload replayed under another device fails to open.
pub fn awareness_aad(space: &AccountId, sender: &DeviceId) -> Vec<u8> {
    let mut out = AWARENESS_AAD_DOMAIN.to_vec();
    for s in [space.as_str(), sender.as_str()] {
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    out
}

/// Why a frame was refused before or after deserializing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// The frame is longer than the negotiated `max_frame_bytes`.
    #[error("frame is {len} bytes, maximum is {max}")]
    TooLarge {
        /// Actual length.
        len: usize,
        /// Allowed length.
        max: usize,
    },
    /// Not valid JSON for any frame of this direction (or an unknown `type`).
    #[error("malformed frame: {0}")]
    Malformed(String),
    /// A field is out of range (payload size, `ttl_ms`, heartbeat, frame limit).
    #[error("invalid frame: {0}")]
    Invalid(&'static str),
}

impl FrameError {
    /// The [`ByeCode`] a server closes with for this error.
    pub fn bye_code(&self) -> ByeCode {
        ByeCode::InvalidFrame
    }
}

/// Why the server ends a live session. Unknown codes from a newer server read as
/// [`ByeCode::Unknown`]; a client treats every code as "reconnect later with jitter" except where
/// noted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeCode {
    /// The caller's membership was removed (or their device in the space was). Don't reconnect;
    /// switch the notebook to the access-removed state.
    AccessRevoked,
    /// The client didn't read fast enough and its queue overflowed. Reconnect and catch up over
    /// HTTP.
    SlowConsumer,
    /// The client sent too many frames. Reconnect after `retry_after_ms`.
    RateLimited,
    /// The server is shutting down. Reconnect after `retry_after_ms` (with jitter).
    ServerRestart,
    /// The session's credentials expired or don't match the hello. Refresh the session first.
    Unauthenticated,
    /// The hello named a protocol this server doesn't speak for live. Don't reconnect until
    /// updated; ordinary sync is unaffected.
    UpgradeRequired,
    /// A frame was too large, malformed, binary, or had out-of-range fields (a client bug).
    InvalidFrame,
    /// No frame (not even `ping`) arrived within two heartbeat intervals.
    IdleTimeout,
    /// A code unknown to this client version.
    #[serde(other)]
    Unknown,
}

impl ByeCode {
    /// Every known code, in documentation order.
    pub const ALL: [ByeCode; 8] = [
        Self::AccessRevoked,
        Self::SlowConsumer,
        Self::RateLimited,
        Self::ServerRestart,
        Self::Unauthenticated,
        Self::UpgradeRequired,
        Self::InvalidFrame,
        Self::IdleTimeout,
    ];

    /// Whether a client should reconnect automatically after this code.
    pub fn may_reconnect(self) -> bool {
        !matches!(
            self,
            Self::AccessRevoked | Self::UpgradeRequired | Self::Unauthenticated
        )
    }
}

/// C → S, the first frame: who is connecting and from where to hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The protocol version the client speaks (at least [`LIVE_MIN_PROTOCOL`], same major).
    pub protocol: ProtocolVersion,
    /// The client's device in the space; must equal the session's device.
    pub device_id: DeviceId,
    /// The `server_seq` the client has durably applied (informational; the client still pulls).
    pub last_server_seq: u64,
    /// Whether the client will send awareness. With `false` it still receives hints and others'
    /// awareness, and the server drops any awareness it sends.
    pub presence: bool,
}

impl Hello {
    /// Checks the protocol version. A failure is [`ByeCode::UpgradeRequired`].
    pub fn check_protocol(&self) -> Result<(), ByeCode> {
        if self.protocol.major != PROTOCOL_MAJOR || self.protocol < LIVE_MIN_PROTOCOL {
            return Err(ByeCode::UpgradeRequired);
        }
        Ok(())
    }
}

/// S → C, the answer to [`Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// This session's id, as other members see it in awareness.
    pub session_id: LiveSessionId,
    /// The space's latest `server_seq` at connect time.
    pub server_seq: u64,
    /// How often the client sends `ping` (and the server re-checks membership).
    pub heartbeat_ms: u32,
    /// Largest frame either side may send, in bytes.
    pub max_frame_bytes: u32,
    /// The caller's role in the space.
    pub role: Role,
}

impl Welcome {
    /// Checks the server's advertised limits against [`MIN_HEARTBEAT_MS`]..[`MAX_HEARTBEAT_MS`]
    /// and [`MIN_MAX_FRAME_BYTES`]..[`MAX_MAX_FRAME_BYTES`]. Call before trusting them.
    pub fn validate(&self) -> Result<(), FrameError> {
        if !(MIN_HEARTBEAT_MS..=MAX_HEARTBEAT_MS).contains(&self.heartbeat_ms) {
            return Err(FrameError::Invalid("heartbeat_ms out of range"));
        }
        if !(MIN_MAX_FRAME_BYTES..=MAX_MAX_FRAME_BYTES).contains(&self.max_frame_bytes) {
            return Err(FrameError::Invalid("max_frame_bytes out of range"));
        }
        Ok(())
    }
}

/// S → C: the space has committed operations up to `server_seq`. Pull over HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Committed {
    /// The space's latest `server_seq` when the hint was sent. Hints may be coalesced, so one hint
    /// can cover many operations.
    pub server_seq: u64,
}

/// C ↔ S: encrypted awareness.
///
/// From a client, `session_id`, `member` and `device_id` are ignored (send them absent). From the
/// server they are always present and verified: the relaying server fills them in from the
/// sender's authenticated session, overwriting any claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Awareness {
    /// The sender's live session (server-set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<LiveSessionId>,
    /// The sender's account (server-set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<AccountId>,
    /// The sender's device in the space (server-set); receivers open the payload with it in
    /// [`awareness_aad`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    /// Unpadded base64url of `nonce ‖ ciphertext ‖ tag`, at most [`MAX_AWARENESS_PAYLOAD_CHARS`].
    /// The plaintext is JSON `{page_id, kind: "here"|"caret"|"selection"|"viewport", rect?,
    /// caret?, block_id?, color_hint?}` in page coordinates, at most
    /// [`MAX_AWARENESS_PLAINTEXT_BYTES`].
    pub payload: String,
    /// How long the receiver shows it, [`MIN_TTL_MS`]..=[`MAX_TTL_MS`] (servers may lower it).
    pub ttl_ms: u32,
}

impl Awareness {
    /// Checks the payload size and charset and the TTL range.
    pub fn validate(&self) -> Result<(), FrameError> {
        if self.payload.is_empty() || self.payload.len() > MAX_AWARENESS_PAYLOAD_CHARS {
            return Err(FrameError::Invalid("awareness payload empty or too long"));
        }
        if crate::base64url::decode(&self.payload).is_err() {
            return Err(FrameError::Invalid("awareness payload is not base64url"));
        }
        if !(MIN_TTL_MS..=MAX_TTL_MS).contains(&self.ttl_ms) {
            return Err(FrameError::Invalid("ttl_ms out of range"));
        }
        Ok(())
    }
}

/// S → C: the space's comment stream has committed comments up to `server_seq`; pull
/// `/v1/comments`. Coalesced like [`Committed`]. Only sent to sessions whose hello said protocol
/// 1.9 or later (`comments.v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentCommitted {
    /// The comment stream's latest `server_seq`.
    pub server_seq: u64,
}

/// S → C: the caller has a new notification; fetch `/v1/notifications`. Ids only. Sent on every
/// live session of the recipient (whatever space it is for) whose hello said protocol 1.9 or
/// later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationHint {
    /// The new notification.
    pub notification_id: crate::ids::NotificationId,
    /// The space it is about.
    pub space_id: AccountId,
}

/// S → C: a member's role or state changed. A frame naming the receiver's own account with
/// `state: "revoked"` is followed by `bye { code: "access_revoked" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    /// The member whose membership changed.
    pub account_id: AccountId,
    /// Their role now.
    pub role: Role,
    /// Their state now.
    pub state: MembershipState,
}

/// S → C: the server ends the session. The socket closes after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bye {
    /// Why.
    pub code: ByeCode,
    /// When to reconnect at the earliest (with full jitter on top), for `rate_limited` and
    /// `server_restart`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

/// A frame a client sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// The first frame of a session.
    Hello(Hello),
    /// Encrypted awareness for the space's other sessions.
    Awareness(Awareness),
    /// Heartbeat; the server answers `pong`.
    Ping,
    /// Answer to a server `ping`.
    Pong,
}

/// A frame a server sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// Answer to `hello`.
    Welcome(Welcome),
    /// New operations were committed; pull over HTTP.
    Committed(Committed),
    /// Relayed awareness with the verified sender.
    Awareness(Awareness),
    /// New comments were committed; pull `/v1/comments` (protocol 1.9).
    CommentCommitted(CommentCommitted),
    /// The caller has a new notification (protocol 1.9).
    Notification(NotificationHint),
    /// A membership change in the space.
    Membership(Membership),
    /// The session ends.
    Bye(Bye),
    /// Heartbeat; the client answers `pong`.
    Ping,
    /// Answer to a client `ping`.
    Pong,
}

fn check_len(text: &str, max_frame_bytes: u32) -> Result<(), FrameError> {
    if text.len() > max_frame_bytes as usize {
        return Err(FrameError::TooLarge {
            len: text.len(),
            max: max_frame_bytes as usize,
        });
    }
    Ok(())
}

impl ClientFrame {
    /// Parses a client frame (server side): the length is checked **before** deserializing, then
    /// the fields.
    pub fn parse(text: &str, max_frame_bytes: u32) -> Result<Self, FrameError> {
        check_len(text, max_frame_bytes)?;
        let f: Self =
            serde_json::from_str(text).map_err(|e| FrameError::Malformed(e.to_string()))?;
        if let Self::Awareness(a) = &f {
            a.validate()?;
        }
        Ok(f)
    }

    /// The frame as JSON text.
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).expect("frames always serialize")
    }
}

impl ServerFrame {
    /// Parses a server frame (client side): the length is checked before deserializing, then
    /// `welcome` limits and awareness fields.
    pub fn parse(text: &str, max_frame_bytes: u32) -> Result<Self, FrameError> {
        check_len(text, max_frame_bytes)?;
        let f: Self =
            serde_json::from_str(text).map_err(|e| FrameError::Malformed(e.to_string()))?;
        match &f {
            Self::Welcome(w) => w.validate()?,
            Self::Awareness(a) => a.validate()?,
            _ => {}
        }
        Ok(f)
    }

    /// The frame as JSON text.
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).expect("frames always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aw(payload: String, ttl_ms: u32) -> Awareness {
        Awareness {
            session_id: None,
            member: None,
            device_id: None,
            payload,
            ttl_ms,
        }
    }

    #[test]
    fn frames_are_tagged_by_type() {
        assert_eq!(ClientFrame::Ping.to_text(), r#"{"type":"ping"}"#);
        let c = ServerFrame::Committed(Committed { server_seq: 7 });
        assert_eq!(c.to_text(), r#"{"type":"committed","server_seq":7}"#);
        let bye = ServerFrame::Bye(Bye {
            code: ByeCode::SlowConsumer,
            retry_after_ms: None,
        });
        assert_eq!(bye.to_text(), r#"{"type":"bye","code":"slow_consumer"}"#);
        assert_eq!(
            ServerFrame::parse(&c.to_text(), DEFAULT_MAX_FRAME_BYTES).unwrap(),
            c
        );
        // A client can't send server frames, and unknown types are malformed.
        assert!(matches!(
            ClientFrame::parse(&c.to_text(), DEFAULT_MAX_FRAME_BYTES),
            Err(FrameError::Malformed(_))
        ));
        assert!(matches!(
            ClientFrame::parse(r#"{"type":"teleport"}"#, DEFAULT_MAX_FRAME_BYTES),
            Err(FrameError::Malformed(_))
        ));
    }

    #[test]
    fn unknown_bye_codes_are_tolerated() {
        let f = ServerFrame::parse(r#"{"type":"bye","code":"moved"}"#, 1024).unwrap();
        assert_eq!(
            f,
            ServerFrame::Bye(Bye {
                code: ByeCode::Unknown,
                retry_after_ms: None
            })
        );
        for c in ByeCode::ALL {
            let s = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<ByeCode>(&s).unwrap(), c);
        }
        assert!(!ByeCode::AccessRevoked.may_reconnect());
        assert!(ByeCode::SlowConsumer.may_reconnect());
    }

    #[test]
    fn length_is_checked_before_parsing() {
        let big = format!(r#"{{"type":"ping","x":"{}"}}"#, "a".repeat(5000));
        assert!(matches!(
            ClientFrame::parse(&big, 4096),
            Err(FrameError::TooLarge { max: 4096, .. })
        ));
    }

    #[test]
    fn awareness_payload_and_ttl_are_bounded() {
        assert_eq!(MAX_AWARENESS_PAYLOAD_CHARS, 2784);
        let max = crate::base64url::encode(&vec![
            7u8;
            MAX_AWARENESS_PLAINTEXT_BYTES
                + AWARENESS_SEAL_OVERHEAD
        ]);
        assert_eq!(max.len(), MAX_AWARENESS_PAYLOAD_CHARS);
        assert!(aw(max.clone(), 5000).validate().is_ok());
        let over = crate::base64url::encode(&vec![
            7u8;
            MAX_AWARENESS_PLAINTEXT_BYTES
                + AWARENESS_SEAL_OVERHEAD
                + 1
        ]);
        assert!(aw(over, 5000).validate().is_err());
        assert!(aw(String::new(), 5000).validate().is_err());
        assert!(aw("not base64!".into(), 5000).validate().is_err());
        assert!(aw("AAAA".into(), MIN_TTL_MS - 1).validate().is_err());
        assert!(aw("AAAA".into(), MAX_TTL_MS + 1).validate().is_err());
        // The largest awareness frame fits the smallest frame limit.
        let frame = ClientFrame::Awareness(aw(max, MAX_TTL_MS)).to_text();
        assert!(frame.len() <= MIN_MAX_FRAME_BYTES as usize);
    }

    #[test]
    fn hello_protocol_and_welcome_limits() {
        let mut h = Hello {
            protocol: ProtocolVersion::CURRENT,
            device_id: "d".parse().unwrap(),
            last_server_seq: 0,
            presence: true,
        };
        assert!(h.check_protocol().is_ok());
        h.protocol = ProtocolVersion::new(1, 7);
        assert_eq!(h.check_protocol(), Err(ByeCode::UpgradeRequired));
        h.protocol = ProtocolVersion::new(2, 0);
        assert_eq!(h.check_protocol(), Err(ByeCode::UpgradeRequired));
        let mut w = Welcome {
            session_id: "ls-1".parse().unwrap(),
            server_seq: 0,
            heartbeat_ms: DEFAULT_HEARTBEAT_MS,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            role: Role::Viewer,
        };
        assert!(w.validate().is_ok());
        w.heartbeat_ms = 100;
        assert!(w.validate().is_err());
        w.heartbeat_ms = DEFAULT_HEARTBEAT_MS;
        w.max_frame_bytes = 1 << 30;
        assert!(w.validate().is_err());
    }

    #[test]
    fn aad_is_unambiguous() {
        let a = awareness_aad(&"ab".parse().unwrap(), &"c".parse().unwrap());
        let b = awareness_aad(&"a".parse().unwrap(), &"bc".parse().unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with(AWARENESS_AAD_DOMAIN));
    }
}
