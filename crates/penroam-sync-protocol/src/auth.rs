//! Sign-in and session DTOs (capability `auth.v1`, protocol 1.1).
//!
//! Sync servers that run real accounts advertise [`crate::dto::capabilities::AUTH_V1`]. The flow:
//!
//! 1. `GET /v1/auth/config` (no token) returns an [`AuthConfig`]: the OpenID Connect issuer and
//!    the public client id the app uses. The client reads the issuer's discovery document itself
//!    and signs in with the authorization-code flow plus PKCE in the **system browser**, with a
//!    loopback redirect (RFC 8252 §7.3). The server never sees the user's password.
//! 2. `POST /v1/auth/session` exchanges the provider's ID token ([`SessionRequest`]) for an
//!    **account session** ([`SessionTokens`] without `device_id`). It can register devices and
//!    read and append the key log, but not push, pull or touch blobs.
//! 3. When the device is a member of the account's key log, it proves possession of its signing
//!    key: `POST /v1/auth/device-challenge` returns a one-time [`DeviceChallenge`], the device
//!    signs [`canonical_device_auth_bytes`] with Ed25519 and sends a [`DeviceSessionRequest`] to
//!    `POST /v1/auth/device-session`. The answer is a **device session** ([`SessionTokens`] with
//!    `device_id`) that every route accepts.
//! 4. Access tokens are short-lived. `POST /v1/auth/refresh` trades the refresh token for a new
//!    pair; refresh tokens are single-use (rotation), and presenting a used one revokes the whole
//!    session. `POST /v1/auth/logout` revokes the session.
//!
//! Tokens are opaque strings. Clients keep them in the platform keychain and never log them.
//! A server without `auth.v1` obtains tokens some other way (the development test server has a
//! stub); nothing else in the protocol depends on how a token was issued.
//!
//! Normative description: `docs/protocol/sync-protocol-v1.md` §12a.

use serde::{Deserialize, Serialize};

use crate::base64url;
use crate::ids::{AccountId, DeviceId};

/// Maximum length of a token, challenge or ID token string accepted by these DTOs' validators.
pub const AUTH_FIELD_MAX_LEN: usize = 8192;
/// Domain prefix of [`canonical_device_auth_bytes`].
pub const DEVICE_AUTH_DOMAIN: &[u8] = b"penroam/device-auth/v1\0";
/// Redirect kind in [`AuthConfig::redirect`]: an `http://127.0.0.1:{any port}/callback` URI
/// served by the app for the duration of one sign-in (RFC 8252 §7.3).
pub const REDIRECT_LOOPBACK: &str = "loopback";
/// Path of the loopback redirect URI.
pub const LOOPBACK_CALLBACK_PATH: &str = "/callback";

/// `GET /v1/auth/config` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    /// OpenID Connect issuer URL. The client fetches
    /// `{issuer}/.well-known/openid-configuration` and requires its `issuer` to equal this value.
    pub issuer: String,
    /// Public OAuth client id of the app (no client secret: native apps cannot keep one).
    pub client_id: String,
    /// Scopes to request (always includes `openid`).
    pub scopes: Vec<String>,
    /// Redirect kind; [`REDIRECT_LOOPBACK`] is the only one defined.
    pub redirect: String,
}

/// `POST /v1/auth/session` request: the ID token the provider returned to the app.
///
/// The server checks its signature against the issuer's published keys, the issuer, the
/// audience (the client id), expiry and issue time. The client has already checked the nonce
/// it sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRequest {
    /// Compact JWS ID token.
    pub id_token: String,
}

/// `POST /v1/auth/refresh` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRequest {
    /// The refresh token from the latest [`SessionTokens`]. Each one works once.
    pub refresh_token: String,
}

/// A session: returned by `/v1/auth/session`, `/v1/auth/device-session` and `/v1/auth/refresh`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTokens {
    /// The account the session belongs to. Assigned by the server.
    pub account_id: AccountId,
    /// Set for a device session; absent for an account session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    /// Bearer token for `Authorization`.
    pub access_token: String,
    /// Seconds until `access_token` expires (counted from the response).
    pub expires_in_s: u64,
    /// Single-use refresh token.
    pub refresh_token: String,
}

impl std::fmt::Display for SessionTokens {
    /// Never prints the tokens.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.device_id {
            Some(d) => write!(f, "device session {} of account {}", d, self.account_id),
            None => write!(f, "account session of {}", self.account_id),
        }
    }
}

/// `POST /v1/auth/device-challenge` request (account or device session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceChallengeRequest {
    /// The device that wants a device session.
    pub device_id: DeviceId,
}

/// `POST /v1/auth/device-challenge` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceChallenge {
    /// Random one-time value (unpadded base64url).
    pub challenge: String,
    /// Seconds the challenge stays valid.
    pub expires_in_s: u64,
}

/// `POST /v1/auth/device-session` request (account or device session).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSessionRequest {
    /// The device.
    pub device_id: DeviceId,
    /// The challenge from [`DeviceChallenge`].
    pub challenge: String,
    /// Unpadded base64url Ed25519 signature over [`canonical_device_auth_bytes`].
    pub signature: String,
}

impl DeviceSessionRequest {
    /// Check encodings and bounds.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.challenge.is_empty()
            || self.challenge.len() > AUTH_FIELD_MAX_LEN
            || base64url::decode(&self.challenge).is_err()
        {
            return Err("challenge");
        }
        match base64url::decode(&self.signature) {
            Ok(s) if s.len() == 64 => Ok(()),
            _ => Err("signature"),
        }
    }
}

/// Bytes a device signs to obtain a device session: the domain prefix, then account id,
/// device id, challenge and the server's `server_instance_id` (from `/v1/meta`), each as a
/// u32 LE length followed by the UTF-8 bytes (the string encoding of
/// [`crate::canonical`]). Binding the instance id stops a signature obtained by one server from
/// being replayed to another.
pub fn canonical_device_auth_bytes(
    account: &AccountId,
    device: &DeviceId,
    challenge: &str,
    server_instance_id: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(DEVICE_AUTH_DOMAIN);
    for s in [
        account.as_str(),
        device.as_str(),
        challenge,
        server_instance_id,
    ] {
        out.extend_from_slice(&u32::try_from(s.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_auth_bytes_layout() {
        let a: AccountId = "acct".parse().unwrap();
        let d: DeviceId = "dev".parse().unwrap();
        let b = canonical_device_auth_bytes(&a, &d, "ch", "inst");
        let mut want = DEVICE_AUTH_DOMAIN.to_vec();
        want.extend_from_slice(&[4, 0, 0, 0]);
        want.extend_from_slice(b"acct");
        want.extend_from_slice(&[3, 0, 0, 0]);
        want.extend_from_slice(b"dev");
        want.extend_from_slice(&[2, 0, 0, 0]);
        want.extend_from_slice(b"ch");
        want.extend_from_slice(&[4, 0, 0, 0]);
        want.extend_from_slice(b"inst");
        assert_eq!(b, want);
    }

    #[test]
    fn session_tokens_display_hides_tokens() {
        let t = SessionTokens {
            account_id: "a1".parse().unwrap(),
            device_id: None,
            access_token: "secret-access".into(),
            expires_in_s: 60,
            refresh_token: "secret-refresh".into(),
        };
        let s = t.to_string();
        assert!(!s.contains("secret"), "{s}");
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.contains("device_id"));
    }

    #[test]
    fn device_session_validation() {
        let mut r = DeviceSessionRequest {
            device_id: "d".parse().unwrap(),
            challenge: base64url::encode(&[7; 32]),
            signature: base64url::encode(&[1; 64]),
        };
        r.validate().unwrap();
        r.signature = base64url::encode(&[1; 63]);
        assert_eq!(r.validate(), Err("signature"));
        r.challenge = "bad=".into();
        assert_eq!(r.validate(), Err("challenge"));
    }
}
