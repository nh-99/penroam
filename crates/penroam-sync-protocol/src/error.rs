//! Normalized error codes, error bodies and a deterministic retry policy.

use serde::{Deserialize, Serialize};

/// Machine-readable error code (snake_case on the wire).
///
/// Unknown codes from a newer server deserialize to [`ErrorCode::Unknown`]
/// and are treated as non-retryable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 400: request not understood (bad query, bad JSON body).
    MalformedRequest,
    /// 401: missing/expired credentials. Re-authenticate; never drop the outbox.
    Unauthenticated,
    /// 403: authenticated but not allowed (e.g. pushing another device's ops).
    Forbidden,
    /// 403: the device has been revoked; requires re-enrollment.
    DeviceRevoked,
    /// 403: the action needs a recent sign-in (account deletion). Sign in again and retry
    /// with the new account session (protocol 1.3).
    ReauthRequired,
    /// 403: the caller's membership of a shared notebook was removed (or they left). Every
    /// request on that space answers this; stop syncing it and keep local data (protocol 1.7).
    AccessRevoked,
    /// 404: unknown resource.
    NotFound,
    /// 409: generic state conflict.
    Conflict,
    /// 409: `op_id` (or device sequence) reused with different content.
    OpIdConflict,
    /// 409: the client must run a recovery/rebase flow before continuing.
    RecoveryRequired,
    /// 410: cursor is older than the server retention horizon.
    CursorExpired,
    /// 410: no tail available from here; fetch a checkpoint first.
    CheckpointRequired,
    /// 410: the invitation can no longer be accepted; ask for a new one (protocol 1.7).
    InvitationExpired,
    /// 413: body, batch or item larger than the advertised limits.
    PayloadTooLarge,
    /// 422: envelope header failed validation.
    InvalidEnvelope,
    /// 422: batch framing is malformed.
    MalformedBatch,
    /// 422: ciphertext length/SHA-256 does not match the header.
    IntegrityMismatch,
    /// 422: cursor is malformed, tampered with or belongs to another stream.
    InvalidCursor,
    /// 422: other semantic validation failure.
    ValidationFailed,
    /// 422: the role cannot be granted (only `editor` and `viewer` can) (protocol 1.7).
    InvalidRole,
    /// 426: protocol/envelope/schema version not supported; update the app.
    UpgradeRequired,
    /// 429: rate limited; honour `retry_after_ms`.
    RateLimited,
    /// 507 (equivalent): account storage quota exceeded.
    QuotaExceeded,
    /// 500: unexpected server error.
    InternalError,
    /// 503: temporarily unavailable.
    ServiceUnavailable,
    /// A code unknown to this client version.
    #[serde(other)]
    Unknown,
}

impl ErrorCode {
    /// Every known code, in table order (used by fixtures and docs).
    pub const ALL: [ErrorCode; 25] = [
        Self::MalformedRequest,
        Self::Unauthenticated,
        Self::Forbidden,
        Self::DeviceRevoked,
        Self::NotFound,
        Self::Conflict,
        Self::OpIdConflict,
        Self::RecoveryRequired,
        Self::CursorExpired,
        Self::CheckpointRequired,
        Self::PayloadTooLarge,
        Self::InvalidEnvelope,
        Self::MalformedBatch,
        Self::IntegrityMismatch,
        Self::InvalidCursor,
        Self::ValidationFailed,
        Self::UpgradeRequired,
        Self::RateLimited,
        Self::QuotaExceeded,
        Self::InternalError,
        Self::ServiceUnavailable,
        Self::ReauthRequired,
        Self::AccessRevoked,
        Self::InvitationExpired,
        Self::InvalidRole,
    ];

    /// HTTP status for this code.
    pub fn http_status(self) -> u16 {
        match self {
            Self::MalformedRequest => 400,
            Self::Unauthenticated => 401,
            Self::Forbidden | Self::DeviceRevoked | Self::ReauthRequired | Self::AccessRevoked => {
                403
            }
            Self::NotFound => 404,
            Self::Conflict | Self::OpIdConflict | Self::RecoveryRequired => 409,
            Self::CursorExpired | Self::CheckpointRequired | Self::InvitationExpired => 410,
            Self::PayloadTooLarge => 413,
            Self::InvalidEnvelope
            | Self::MalformedBatch
            | Self::IntegrityMismatch
            | Self::InvalidCursor
            | Self::ValidationFailed
            | Self::InvalidRole => 422,
            Self::UpgradeRequired => 426,
            Self::RateLimited => 429,
            Self::QuotaExceeded => 507,
            Self::InternalError | Self::Unknown => 500,
            Self::ServiceUnavailable => 503,
        }
    }

    /// Whether the *same* request may be retried automatically (with
    /// [`RetryPolicy`]). Only 429 and 5xx are retryable; everything else
    /// needs a state change (re-auth, checkpoint, upgrade, user action).
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::InternalError | Self::ServiceUnavailable
        )
    }

    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MalformedRequest => "malformed_request",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::DeviceRevoked => "device_revoked",
            Self::ReauthRequired => "reauth_required",
            Self::AccessRevoked => "access_revoked",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::OpIdConflict => "op_id_conflict",
            Self::RecoveryRequired => "recovery_required",
            Self::CursorExpired => "cursor_expired",
            Self::CheckpointRequired => "checkpoint_required",
            Self::InvitationExpired => "invitation_expired",
            Self::PayloadTooLarge => "payload_too_large",
            Self::InvalidEnvelope => "invalid_envelope",
            Self::MalformedBatch => "malformed_batch",
            Self::IntegrityMismatch => "integrity_mismatch",
            Self::InvalidCursor => "invalid_cursor",
            Self::ValidationFailed => "validation_failed",
            Self::InvalidRole => "invalid_role",
            Self::UpgradeRequired => "upgrade_required",
            Self::RateLimited => "rate_limited",
            Self::QuotaExceeded => "quota_exceeded",
            Self::InternalError => "internal_error",
            Self::ServiceUnavailable => "service_unavailable",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error response body. `message` is safe for display and never contains
/// secrets or plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Machine code.
    pub code: ErrorCode,
    /// Safe human-readable message.
    pub message: String,
    /// Server request id for support correlation.
    pub request_id: String,
    /// Server retry hint in milliseconds (429/503).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl ErrorBody {
    /// Construct a body without retry hint.
    pub fn new(code: ErrorCode, message: impl Into<String>, request_id: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            request_id: request_id.into(),
            retry_after_ms: None,
        }
    }
}

/// Exponential backoff with full jitter, a cap and a maximum attempt count.
///
/// Randomness is an *input* so behaviour is deterministic in tests: pass a
/// uniformly distributed `u64` from any RNG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Base delay for attempt 0.
    pub base_delay_ms: u64,
    /// Upper bound of the exponential ceiling.
    pub max_delay_ms: u64,
    /// Number of retries allowed (attempts `0..max_attempts`).
    pub max_attempts: u32,
    /// Upper bound applied to a server `retry_after_ms` hint, so a hostile
    /// or buggy server cannot park the client forever.
    pub max_retry_after_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base_delay_ms: 500,
            max_delay_ms: 60_000,
            max_attempts: 8,
            max_retry_after_ms: 15 * 60_000,
        }
    }
}

impl RetryPolicy {
    /// Exponential ceiling for retry `attempt` (0-based): `min(cap, base * 2^attempt)`.
    pub fn ceiling_ms(&self, attempt: u32) -> u64 {
        let factor = 1u64.checked_shl(attempt.min(63)).unwrap_or(u64::MAX);
        self.base_delay_ms
            .saturating_mul(factor)
            .min(self.max_delay_ms)
    }

    /// Delay before retry `attempt`, or `None` when attempts are exhausted.
    /// Full jitter: uniform in `0..=ceiling`, derived from `random`.
    pub fn delay_ms(&self, attempt: u32, random: u64) -> Option<u64> {
        if attempt >= self.max_attempts {
            return None;
        }
        let ceiling = self.ceiling_ms(attempt);
        Some(if ceiling == u64::MAX {
            random
        } else {
            random % (ceiling + 1)
        })
    }

    /// Delay for a failed request: `None` if the error is not retryable or
    /// attempts are exhausted; otherwise the jittered delay, but never less
    /// than the (capped) server `retry_after_ms` hint.
    pub fn delay_for(&self, error: &ErrorBody, attempt: u32, random: u64) -> Option<u64> {
        if !error.code.is_retryable() {
            return None;
        }
        let jitter = self.delay_ms(attempt, random)?;
        let hint = error
            .retry_after_ms
            .unwrap_or(0)
            .min(self.max_retry_after_ms);
        Some(jitter.max(hint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_serde() {
        for code in ErrorCode::ALL {
            assert_eq!(
                serde_json::to_string(&code).unwrap(),
                format!("\"{}\"", code.as_str())
            );
            assert_eq!(
                serde_json::from_str::<ErrorCode>(&format!("\"{code}\"")).unwrap(),
                code
            );
        }
    }

    #[test]
    fn unknown_code_tolerated() {
        let b: ErrorBody =
            serde_json::from_str(r#"{"code":"teapot","message":"m","request_id":"r"}"#).unwrap();
        assert_eq!(b.code, ErrorCode::Unknown);
        assert!(!b.code.is_retryable());
    }

    #[test]
    fn statuses_and_retryability() {
        use ErrorCode::*;
        let expect = [
            (Unauthenticated, 401, false),
            (Forbidden, 403, false),
            (OpIdConflict, 409, false),
            (RecoveryRequired, 409, false),
            (CursorExpired, 410, false),
            (CheckpointRequired, 410, false),
            (PayloadTooLarge, 413, false),
            (InvalidEnvelope, 422, false),
            (UpgradeRequired, 426, false),
            (RateLimited, 429, true),
            (QuotaExceeded, 507, false),
            (ReauthRequired, 403, false),
            (AccessRevoked, 403, false),
            (InvitationExpired, 410, false),
            (InvalidRole, 422, false),
            (InternalError, 500, true),
            (ServiceUnavailable, 503, true),
        ];
        for (c, s, r) in expect {
            assert_eq!(c.http_status(), s, "{c}");
            assert_eq!(c.is_retryable(), r, "{c}");
        }
    }

    #[test]
    fn retry_after_omitted_when_none() {
        let b = ErrorBody::new(ErrorCode::Conflict, "m", "r");
        assert!(!serde_json::to_string(&b)
            .unwrap()
            .contains("retry_after_ms"));
    }

    #[test]
    fn backoff_is_deterministic_bounded_and_capped() {
        let p = RetryPolicy {
            base_delay_ms: 100,
            max_delay_ms: 1000,
            max_attempts: 5,
            max_retry_after_ms: 5000,
        };
        assert_eq!(p.ceiling_ms(0), 100);
        assert_eq!(p.ceiling_ms(1), 200);
        assert_eq!(p.ceiling_ms(3), 800);
        assert_eq!(p.ceiling_ms(4), 1000);
        assert_eq!(p.ceiling_ms(200), 1000);
        assert_eq!(p.delay_ms(0, 0), Some(0));
        assert_eq!(p.delay_ms(0, 100), Some(100));
        assert_eq!(p.delay_ms(0, 101), Some(0));
        assert_eq!(p.delay_ms(2, 12345), p.delay_ms(2, 12345));
        for r in [0, 1, 999, u64::MAX] {
            assert!(p.delay_ms(4, r).unwrap() <= 1000);
        }
        assert_eq!(p.delay_ms(5, 0), None);
    }

    #[test]
    fn delay_for_respects_retryability_and_hint() {
        let p = RetryPolicy::default();
        let mut e = ErrorBody::new(ErrorCode::RateLimited, "slow down", "r");
        e.retry_after_ms = Some(2000);
        assert_eq!(p.delay_for(&e, 0, 0), Some(2000));
        e.retry_after_ms = Some(u64::MAX);
        assert_eq!(p.delay_for(&e, 0, 0), Some(p.max_retry_after_ms));
        let e = ErrorBody::new(ErrorCode::CursorExpired, "x", "r");
        assert_eq!(p.delay_for(&e, 0, 0), None);
    }
}
