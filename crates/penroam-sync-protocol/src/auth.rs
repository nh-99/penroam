//! Sign-in and session DTOs (capability `auth.v1`, protocol 1.1; fixed loopback ports and the
//! mobile redirect: 1.2).
//!
//! Sync servers that run real accounts advertise [`crate::dto::capabilities::AUTH_V1`]. The flow:
//!
//! 1. `GET /v1/auth/config` (no token) returns an [`AuthConfig`]: the OpenID Connect issuer and
//!    the public client id the app uses. The client reads the issuer's discovery document itself
//!    and signs in with the authorization-code flow plus PKCE in the **system browser**. Desktop
//!    apps receive the answer on a loopback redirect (RFC 8252 §7.3): any port, or one of the
//!    ports the provider has registered, for providers that cannot match any port. Mobile apps
//!    use the private-use URI scheme redirect in [`AuthConfig::mobile_redirect_uri`]
//!    (RFC 8252 §7.1). The server never sees the user's password.
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
/// Redirect kind in [`AuthConfig::redirect`]: `http://127.0.0.1:{port}/callback` with `port`
/// taken from [`AuthConfig::loopback_ports`], the first one free on the device. For providers
/// that require every redirect URI to be registered exactly, port included (protocol 1.2).
pub const REDIRECT_LOOPBACK_PORTS: &str = "loopback-ports";
/// Path of the loopback redirect URI.
pub const LOOPBACK_CALLBACK_PATH: &str = "/callback";
/// Most ports [`AuthConfig::loopback_ports`] may list.
pub const MAX_LOOPBACK_PORTS: usize = 8;
/// Longest accepted [`AuthConfig::mobile_redirect_uri`].
pub const MAX_REDIRECT_URI_LEN: usize = 512;

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
    /// How desktop apps receive the answer: [`REDIRECT_LOOPBACK`] or
    /// [`REDIRECT_LOOPBACK_PORTS`]. Clients refuse kinds they do not know.
    pub redirect: String,
    /// With [`REDIRECT_LOOPBACK_PORTS`]: the registered loopback ports, tried in order. Empty
    /// otherwise (protocol 1.2; older servers omit it).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loopback_ports: Vec<u16>,
    /// Redirect URI for the mobile apps, registered at the provider: a private-use URI scheme
    /// in reverse domain-name form (RFC 8252 §7.1), e.g. `ink.penroam.app://callback`. The
    /// operating system hands it to the app that claims the scheme. Absent: mobile apps cannot
    /// sign in to this server (protocol 1.2; older servers omit it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile_redirect_uri: Option<String>,
}

impl AuthConfig {
    /// Checks the redirect settings: a known [`Self::redirect`] kind, 1 to
    /// [`MAX_LOOPBACK_PORTS`] distinct non-zero ports exactly when the kind needs them, and a
    /// well-formed [`Self::mobile_redirect_uri`] (see [`check_mobile_redirect_uri`]). The error
    /// names the problem.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self.redirect.as_str() {
            REDIRECT_LOOPBACK => {
                if !self.loopback_ports.is_empty() {
                    return Err("loopback_ports is only used with the loopback-ports redirect");
                }
            }
            REDIRECT_LOOPBACK_PORTS => {
                let p = &self.loopback_ports;
                if p.is_empty() || p.len() > MAX_LOOPBACK_PORTS {
                    return Err("loopback-ports needs 1 to 8 loopback_ports");
                }
                if p.contains(&0) {
                    return Err("loopback port 0 is not a fixed port");
                }
                if p.iter().enumerate().any(|(i, x)| p[..i].contains(x)) {
                    return Err("duplicate loopback port");
                }
            }
            _ => return Err("unknown redirect kind"),
        }
        if let Some(uri) = &self.mobile_redirect_uri {
            check_mobile_redirect_uri(uri)?;
        }
        Ok(())
    }
}

/// Checks a mobile redirect URI: `scheme:/path` or `scheme://host/path` with a reverse
/// domain-name scheme (letters, digits, `+ - .`, at least one dot, starting with a letter; not
/// `http` or `https`), no query or fragment, at most [`MAX_REDIRECT_URI_LEN`] bytes.
pub fn check_mobile_redirect_uri(uri: &str) -> Result<(), &'static str> {
    if uri.len() > MAX_REDIRECT_URI_LEN {
        return Err("mobile redirect URI too long");
    }
    let (scheme, rest) = uri
        .split_once(':')
        .ok_or("mobile redirect URI has no scheme")?;
    let ok_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && scheme.contains('.');
    if !ok_scheme {
        return Err(
            "mobile redirect URI needs a reverse domain-name scheme (e.g. ink.penroam.app)",
        );
    }
    if !rest.starts_with('/') || rest.len() < 2 {
        return Err("mobile redirect URI needs a path after the scheme");
    }
    if rest.contains(['?', '#'])
        || rest
            .chars()
            .any(|c| c.is_ascii_whitespace() || c.is_control())
    {
        return Err("mobile redirect URI must not carry a query, fragment or spaces");
    }
    Ok(())
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

    fn cfg(redirect: &str, ports: Vec<u16>, mobile: Option<&str>) -> AuthConfig {
        AuthConfig {
            issuer: "https://idp.invalid/".into(),
            client_id: "c".into(),
            scopes: vec!["openid".into()],
            redirect: redirect.into(),
            loopback_ports: ports,
            mobile_redirect_uri: mobile.map(Into::into),
        }
    }

    #[test]
    fn redirect_settings_are_validated() {
        assert!(cfg(REDIRECT_LOOPBACK, vec![], None).validate().is_ok());
        assert!(cfg(REDIRECT_LOOPBACK, vec![1], None).validate().is_err());
        assert!(cfg(REDIRECT_LOOPBACK_PORTS, vec![47011, 47012], None)
            .validate()
            .is_ok());
        assert!(cfg(REDIRECT_LOOPBACK_PORTS, vec![], None)
            .validate()
            .is_err());
        assert!(cfg(REDIRECT_LOOPBACK_PORTS, vec![0], None)
            .validate()
            .is_err());
        assert!(cfg(REDIRECT_LOOPBACK_PORTS, vec![5, 5], None)
            .validate()
            .is_err());
        assert!(cfg(REDIRECT_LOOPBACK_PORTS, (1..=9).collect(), None)
            .validate()
            .is_err());
        assert!(cfg("custom", vec![], None).validate().is_err());
        for ok in ["ink.penroam.app://callback", "ink.penroam.app:/callback"] {
            assert!(
                cfg(REDIRECT_LOOPBACK, vec![], Some(ok)).validate().is_ok(),
                "{ok}"
            );
        }
        for bad in [
            "https://penroam.ink/callback",
            "penroam://callback",
            "ink.penroam.app:callback",
            "ink.penroam.app:/",
            "ink.penroam.app://callback?x=1",
            "ink.penroam.app://call back",
            "1nk.penroam.app://callback",
            "ink_penroam.app://callback",
        ] {
            assert!(
                cfg(REDIRECT_LOOPBACK, vec![], Some(bad))
                    .validate()
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn older_config_without_new_fields_still_parses() {
        let c: AuthConfig = serde_json::from_str(
            r#"{"issuer":"https://i/","client_id":"c","scopes":["openid"],"redirect":"loopback"}"#,
        )
        .unwrap();
        assert!(c.loopback_ports.is_empty() && c.mobile_redirect_uri.is_none());
        let back = serde_json::to_value(&c).unwrap();
        assert!(back.get("loopback_ports").is_none() && back.get("mobile_redirect_uri").is_none());
    }

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
