//! Sign-in for servers that advertise `auth.v1` (protocol §12a).
//!
//! The app never handles the user's password. [`BrowserLogin`] runs the OpenID Connect
//! authorization-code flow with PKCE (RFC 7636, `S256`) in the **system browser** and receives
//! the answer on a one-shot loopback redirect `http://127.0.0.1:{port}/callback` (RFC 8252
//! §7.3): any port, or the first free one of the ports the server lists for providers that
//! need exact redirect URIs. On mobile, [`AppRedirectLogin`] uses the server's private-use
//! URI scheme redirect instead (RFC 8252 §7.1): the operating system hands the answer to the
//! app as a link. Both check `state` (stops a forged callback) and the ID token's `nonce`
//! (stops an injected token), then hand the ID token to the sync server, which verifies the
//! signature, issuer, audience and expiry and returns an **account session**.
//!
//! A device that is an active member of the key log then gets a **device session** from
//! [`device_session`]: it signs the server's one-time challenge with its Ed25519 key over
//! `canonical_device_auth_bytes`, so a stolen account session alone cannot read or write
//! notes. [`refresh`] rotates tokens (each refresh token works once) and [`logout`] revokes a
//! session.
//!
//! Tokens are returned to the caller, who stores them in the keychain. Nothing here logs a
//! token, code or verifier, and `Debug` output of [`BrowserLogin`] hides them.

use crate::crypto::DeviceKeys;
use crate::transport::TransportError;
use ed25519_dalek::Signer;
use penroam_sync_protocol::auth::{
    check_mobile_redirect_uri, LOOPBACK_CALLBACK_PATH, REDIRECT_LOOPBACK, REDIRECT_LOOPBACK_PORTS,
};
use penroam_sync_protocol::routes;
use penroam_sync_protocol::{
    base64url, canonical_device_auth_bytes, AccountId, AuthConfig, DeviceChallenge,
    DeviceChallengeRequest, DeviceId, DeviceSessionRequest, ErrorBody, RefreshRequest,
    SessionRequest, SessionTokens,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// Why sign-in failed.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The sync server does not advertise `auth.v1` (for example the development test server).
    #[error("this server has no sign-in (auth.v1 not advertised)")]
    NotSupported,
    /// Talking to the sync server failed (includes its error bodies).
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The identity provider's discovery document or token endpoint misbehaved.
    #[error("identity provider error: {0}")]
    Provider(String),
    /// The user (or the provider) cancelled or refused the sign-in.
    #[error("sign-in was refused: {0}")]
    Denied(String),
    /// The redirect carried a different `state`: possibly a forged callback.
    #[error("sign-in callback did not match this sign-in attempt")]
    StateMismatch,
    /// The ID token's `nonce` differs from the one sent: possibly an injected token.
    #[error("the identity provider returned a token for a different sign-in attempt")]
    NonceMismatch,
    /// No callback arrived in time.
    #[error("sign-in timed out")]
    Timeout,
    /// Local I/O (the loopback listener).
    #[error("sign-in listener failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias for this module.
pub type AuthResult<T> = Result<T, AuthError>;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into()
}

const JSON_LIMIT: u64 = 1 << 20;

fn base(url: &str) -> &str {
    url.trim_end_matches('/')
}

fn net(e: ureq::Error) -> TransportError {
    TransportError::Network(e.to_string())
}

/// Reads a JSON body, turning protocol error bodies into [`TransportError::Api`].
fn read_json<T: serde::de::DeserializeOwned>(
    mut r: ureq::http::Response<ureq::Body>,
) -> Result<Option<T>, TransportError> {
    let status = r.status().as_u16();
    let body = r
        .body_mut()
        .with_config()
        .limit(JSON_LIMIT)
        .read_to_vec()
        .map_err(net)?;
    if status >= 400 {
        return Err(match serde_json::from_slice::<ErrorBody>(&body) {
            Ok(b) => TransportError::Api(b),
            Err(_) => TransportError::Protocol(format!("HTTP {status} without an error body")),
        });
    }
    if status == 204 || body.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| TransportError::Protocol(e.to_string()))
}

fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    url: String,
    bearer: Option<&str>,
    body: &B,
) -> Result<T, TransportError> {
    let mut req = agent().post(url);
    if let Some(t) = bearer {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    read_json(req.send_json(body).map_err(net)?)?
        .ok_or_else(|| TransportError::Protocol("empty response".into()))
}

/// `GET /v1/auth/config`. Call only when `/v1/meta` lists `auth.v1`. Refuses settings that
/// fail [`AuthConfig::validate`] (unknown redirect kind, bad ports or mobile redirect).
pub fn auth_config(server_url: &str) -> AuthResult<AuthConfig> {
    let r = agent()
        .get(format!("{}{}", base(server_url), routes::AUTH_CONFIG))
        .call()
        .map_err(net)?;
    let cfg: AuthConfig = read_json(r)?.ok_or(AuthError::NotSupported)?;
    cfg.validate()
        .map_err(|e| AuthError::Provider(format!("server sign-in settings: {e}")))?;
    Ok(cfg)
}

/// The parts of the provider's discovery document the app uses.
#[derive(Debug, Clone, Deserialize)]
pub struct Discovery {
    /// Must equal the configured issuer.
    pub issuer: String,
    /// Where the browser goes.
    pub authorization_endpoint: String,
    /// Where the app exchanges the code.
    pub token_endpoint: String,
}

/// Fetches `{issuer}/.well-known/openid-configuration` and checks that it names the same
/// issuer (OpenID Connect Discovery §4.3).
pub fn discover(issuer: &str) -> AuthResult<Discovery> {
    let url = format!("{}/.well-known/openid-configuration", base(issuer));
    let r = agent()
        .get(url)
        .call()
        .map_err(|e| AuthError::Provider(e.to_string()))?;
    let d: Discovery = read_json(r)
        .map_err(|e| AuthError::Provider(e.to_string()))?
        .ok_or_else(|| AuthError::Provider("empty discovery document".into()))?;
    if base(&d.issuer) != base(issuer) {
        return Err(AuthError::Provider(
            "discovery document names another issuer".into(),
        ));
    }
    Ok(d)
}

fn random_token() -> AuthResult<String> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b)
        .map_err(|e| AuthError::Io(std::io::Error::other(e.to_string())))?;
    Ok(base64url::encode(&b))
}

/// PKCE `S256` challenge for a verifier (RFC 7636 §4.2).
pub fn pkce_challenge(verifier: &str) -> String {
    base64url::encode(&Sha256::digest(verifier.as_bytes()))
}

/// Percent-encodes a query value (RFC 3986 unreserved characters pass through).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Decodes a `application/x-www-form-urlencoded` value; `None` if malformed.
fn dec(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The secrets (`state`, `nonce`, PKCE verifier) and endpoints of one sign-in attempt, shared
/// by both ways the answer can come back.
struct Attempt {
    redirect_uri: String,
    authorize_url: String,
    token_endpoint: String,
    client_id: String,
    state: String,
    nonce: String,
    verifier: String,
}

/// What the provider's token endpoint returns (only the field we use).
#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct IdClaims {
    nonce: Option<String>,
}

impl Attempt {
    fn new(cfg: &AuthConfig, discovery: &Discovery, redirect_uri: String) -> AuthResult<Self> {
        let state = random_token()?;
        let nonce = random_token()?;
        let verifier = random_token()?;
        let scopes = if cfg.scopes.iter().any(|s| s == "openid") {
            cfg.scopes.join(" ")
        } else {
            let mut s = vec!["openid".to_string()];
            s.extend(cfg.scopes.iter().cloned());
            s.join(" ")
        };
        let sep = if discovery.authorization_endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        let authorize_url = format!(
            "{}{sep}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
            discovery.authorization_endpoint,
            enc(&cfg.client_id),
            enc(&redirect_uri),
            enc(&scopes),
            enc(&state),
            enc(&nonce),
            enc(&pkce_challenge(&verifier)),
        );
        Ok(Self {
            redirect_uri,
            authorize_url,
            token_endpoint: discovery.token_endpoint.clone(),
            client_id: cfg.client_id.clone(),
            state,
            nonce,
            verifier,
        })
    }

    /// Reads the callback's query string: checks `state` first (a callback for another attempt
    /// is refused whatever else it says), then returns the code or the provider's refusal.
    fn code_from_query(&self, query: &str) -> AuthResult<String> {
        let mut code = None;
        let mut state = None;
        let mut error = None;
        for kv in query.split('&') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let v = dec(v);
            match k {
                "code" => code = v,
                "state" => state = v,
                "error" => error = v,
                _ => {}
            }
        }
        if state.as_deref() != Some(self.state.as_str()) {
            return Err(AuthError::StateMismatch);
        }
        if let Some(e) = error {
            return Err(AuthError::Denied(e));
        }
        match code {
            Some(c) if !c.is_empty() => Ok(c),
            _ => Err(AuthError::Provider("callback without code".into())),
        }
    }

    /// Exchanges the code at the provider (with the PKCE verifier) and checks the ID token's
    /// `nonce`. Returns the ID token.
    fn exchange(&self, code: &str) -> AuthResult<String> {
        let r = agent()
            .post(&self.token_endpoint)
            .send_form([
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("client_id", self.client_id.as_str()),
                ("code_verifier", self.verifier.as_str()),
            ])
            .map_err(|e| AuthError::Provider(e.to_string()))?;
        let status = r.status().as_u16();
        let tr: Option<TokenResponse> = read_json(r)
            .map_err(|_| AuthError::Provider(format!("token endpoint: HTTP {status}")))?;
        let id_token = tr
            .and_then(|t| t.id_token)
            .ok_or_else(|| AuthError::Provider("token response without id_token".into()))?;
        let claims = id_token
            .split('.')
            .nth(1)
            .and_then(|p| base64url::decode(p).ok())
            .and_then(|b| serde_json::from_slice::<IdClaims>(&b).ok())
            .ok_or_else(|| AuthError::Provider("malformed id_token".into()))?;
        if claims.nonce.as_deref() != Some(self.nonce.as_str()) {
            return Err(AuthError::NonceMismatch);
        }
        Ok(id_token)
    }
}

/// Binds the loopback listener the server's redirect kind asks for: any free port, or the first
/// free one of the registered ports.
fn bind_loopback(cfg: &AuthConfig) -> AuthResult<TcpListener> {
    match cfg.redirect.as_str() {
        REDIRECT_LOOPBACK => Ok(TcpListener::bind("127.0.0.1:0")?),
        REDIRECT_LOOPBACK_PORTS => {
            for port in &cfg.loopback_ports {
                // Busy (another program, or a sign-in already waiting): try the next one.
                if let Ok(l) = TcpListener::bind(("127.0.0.1", *port)) {
                    return Ok(l);
                }
            }
            Err(AuthError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!(
                    "none of the sign-in ports {:?} is free on this device",
                    cfg.loopback_ports
                ),
            )))
        }
        other => Err(AuthError::Provider(format!(
            "unsupported redirect kind {other:?}"
        ))),
    }
}

/// One desktop sign-in attempt: a loopback listener plus the attempt's secrets.
pub struct BrowserLogin {
    listener: TcpListener,
    attempt: Attempt,
}

impl std::fmt::Debug for BrowserLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserLogin")
            .field("redirect_uri", &self.attempt.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl BrowserLogin {
    /// Binds the loopback listener ([`REDIRECT_LOOPBACK`]: any port;
    /// [`REDIRECT_LOOPBACK_PORTS`]: the first free registered port) and builds the
    /// authorization URL. Open [`Self::authorize_url`] in the system browser, then call
    /// [`Self::finish`]. Fails with [`AuthError::Io`] when every registered port is busy.
    pub fn start(cfg: &AuthConfig, discovery: &Discovery) -> AuthResult<Self> {
        let listener = bind_loopback(cfg)?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}{LOOPBACK_CALLBACK_PATH}");
        Ok(Self {
            listener,
            attempt: Attempt::new(cfg, discovery, redirect_uri)?,
        })
    }

    /// The URL to open in the system browser.
    pub fn authorize_url(&self) -> &str {
        &self.attempt.authorize_url
    }

    /// The loopback redirect URI used by this attempt.
    pub fn redirect_uri(&self) -> &str {
        &self.attempt.redirect_uri
    }

    /// Waits for the browser to come back (at most `timeout`), checks `state`, exchanges the
    /// code at the provider and checks the ID token's `nonce`. Returns the ID token for
    /// [`session_from_id_token`].
    pub fn finish(self, timeout: Duration) -> AuthResult<String> {
        let code = self.wait_for_code(timeout)?;
        self.attempt.exchange(&code)
    }

    /// Serves the loopback listener until the callback request arrives. Other paths (a
    /// browser's favicon request) get 404 and are ignored.
    fn wait_for_code(&self, timeout: Duration) -> AuthResult<String> {
        let deadline = Instant::now() + timeout;
        self.listener.set_nonblocking(true)?;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(result) = self.handle_callback(stream) {
                        return result;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(AuthError::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// `None` = not the callback, keep waiting.
    fn handle_callback(&self, stream: TcpStream) -> Option<AuthResult<String>> {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        let mut reader = BufReader::new(&stream);
        if reader
            .by_ref()
            .take(16 * 1024)
            .read_line(&mut line)
            .is_err()
        {
            return None;
        }
        // drain the headers so the browser sees a clean response
        let mut h = String::new();
        while reader
            .by_ref()
            .take(16 * 1024)
            .read_line(&mut h)
            .is_ok_and(|n| n > 2)
        {
            h.clear();
        }
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let respond = |status: &str, msg: &str| {
            let body =
                format!("<!doctype html><meta charset=utf-8><title>Penroam</title><p>{msg}</p>");
            let _ = (&stream).write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        };
        if path != LOOPBACK_CALLBACK_PATH {
            respond("404 Not Found", "Not found.");
            return None;
        }
        let result = self.attempt.code_from_query(query);
        match &result {
            Ok(_) => respond("200 OK", "Signed in to Penroam. You can close this tab."),
            Err(AuthError::StateMismatch) => respond(
                "400 Bad Request",
                "This sign-in link is not for this attempt.",
            ),
            Err(AuthError::Denied(_)) => {
                respond("200 OK", "Sign-in was cancelled. You can close this tab.")
            }
            Err(_) => respond("400 Bad Request", "The sign-in answer had no code."),
        }
        Some(result)
    }
}

/// One mobile sign-in attempt: the answer comes back as a link to the server's
/// [`AuthConfig::mobile_redirect_uri`], which the operating system hands to the app that claims
/// its scheme (RFC 8252 §7.1). The app opens [`Self::authorize_url`] in the system browser and
/// passes the link it receives to [`Self::finish`]. PKCE matters most here: another app that
/// claims the same scheme could see the link, but cannot redeem the code without the verifier.
pub struct AppRedirectLogin {
    attempt: Attempt,
}

impl std::fmt::Debug for AppRedirectLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppRedirectLogin")
            .field("redirect_uri", &self.attempt.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl AppRedirectLogin {
    /// Builds the attempt. `app_scheme` is the URI scheme this app claims (e.g.
    /// `ink.penroam.app`); the server's mobile redirect must use it, or the operating system
    /// would deliver the answer to another app. Fails with [`AuthError::NotSupported`] when
    /// the server has no mobile redirect.
    pub fn start(cfg: &AuthConfig, discovery: &Discovery, app_scheme: &str) -> AuthResult<Self> {
        let uri = cfg
            .mobile_redirect_uri
            .clone()
            .ok_or(AuthError::NotSupported)?;
        check_mobile_redirect_uri(&uri).map_err(|e| AuthError::Provider(e.to_string()))?;
        let scheme = uri.split_once(':').map(|(s, _)| s).unwrap_or_default();
        if !scheme.eq_ignore_ascii_case(app_scheme) {
            return Err(AuthError::Provider(format!(
                "the server's mobile sign-in returns to {scheme:?}, not to this app"
            )));
        }
        Ok(Self {
            attempt: Attempt::new(cfg, discovery, uri)?,
        })
    }

    /// The URL to open in the system browser.
    pub fn authorize_url(&self) -> &str {
        &self.attempt.authorize_url
    }

    /// The redirect URI used by this attempt.
    pub fn redirect_uri(&self) -> &str {
        &self.attempt.redirect_uri
    }

    /// `true` if `url` is an answer for this kind of attempt (same redirect URI), so the app can
    /// route incoming links. Does not check `state`; [`Self::finish`] does.
    pub fn is_callback(&self, url: &str) -> bool {
        callback_base(url).eq_ignore_ascii_case(&self.attempt.redirect_uri)
    }

    /// Takes the link the app received: checks that it is the redirect URI, checks `state`,
    /// exchanges the code and checks the ID token's `nonce`. Returns the ID token for
    /// [`session_from_id_token`].
    pub fn finish(self, url: &str) -> AuthResult<String> {
        if !self.is_callback(url) {
            return Err(AuthError::StateMismatch);
        }
        let without_fragment = url.split('#').next().unwrap_or_default();
        let query = without_fragment
            .split_once('?')
            .map(|(_, q)| q)
            .unwrap_or_default();
        let code = self.attempt.code_from_query(query)?;
        self.attempt.exchange(&code)
    }
}

/// The part of a link before its query and fragment.
fn callback_base(url: &str) -> &str {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    &url[..end]
}

/// `POST /v1/auth/session`: exchanges a verified-by-the-server ID token for an account session.
pub fn session_from_id_token(server_url: &str, id_token: &str) -> AuthResult<SessionTokens> {
    Ok(post(
        format!("{}{}", base(server_url), routes::AUTH_SESSION),
        None,
        &SessionRequest {
            id_token: id_token.to_string(),
        },
    )?)
}

/// `POST /v1/auth/refresh`: rotates the session. The old refresh token stops working; keep
/// the returned one. `unauthenticated` means the session ended and the user must sign in again
/// (local notes and the outbox are unaffected).
pub fn refresh(server_url: &str, refresh_token: &str) -> AuthResult<SessionTokens> {
    Ok(post(
        format!("{}{}", base(server_url), routes::AUTH_REFRESH),
        None,
        &RefreshRequest {
            refresh_token: refresh_token.to_string(),
        },
    )?)
}

/// `POST /v1/auth/logout`: revokes the session the access token belongs to.
pub fn logout(server_url: &str, access_token: &str) -> AuthResult<()> {
    let r = agent()
        .post(format!("{}{}", base(server_url), routes::AUTH_LOGOUT))
        .header("Authorization", format!("Bearer {access_token}"))
        .send_empty()
        .map_err(net)?;
    read_json::<serde_json::Value>(r)?;
    Ok(())
}

/// Obtains a device session for `keys`' device with an account (or device) session's access
/// token: fetches a challenge, signs the canonical bytes with the device's Ed25519 key and
/// returns the new tokens. `server_instance_id` comes from `/v1/meta`. The server refuses
/// (`forbidden`/`device_revoked`) a device that is not an active member of the key log.
pub fn device_session(
    server_url: &str,
    access_token: &str,
    account: &AccountId,
    keys: &DeviceKeys,
    server_instance_id: &str,
) -> AuthResult<SessionTokens> {
    let device: DeviceId = keys
        .device_id
        .parse()
        .map_err(|_| AuthError::Provider("invalid device id".into()))?;
    let ch: DeviceChallenge = post(
        format!("{}{}", base(server_url), routes::AUTH_DEVICE_CHALLENGE),
        Some(access_token),
        &DeviceChallengeRequest {
            device_id: device.clone(),
        },
    )?;
    let msg = canonical_device_auth_bytes(account, &device, &ch.challenge, server_instance_id);
    let sig = keys.signing.sign(&msg).to_bytes();
    Ok(post(
        format!("{}{}", base(server_url), routes::AUTH_DEVICE_SESSION),
        Some(access_token),
        &DeviceSessionRequest {
            device_id: device,
            challenge: ch.challenge,
            signature: base64url::encode(&sig),
        },
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_rfc7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn query_encoding_round_trips() {
        let s = "http://127.0.0.1:1234/callback?a=b c&d=é";
        assert_eq!(dec(&enc(s)).unwrap(), s);
        assert_eq!(dec("a+b%2Fc").unwrap(), "a b/c");
        assert!(dec("%G1").is_none());
    }

    fn login() -> BrowserLogin {
        BrowserLogin::start(
            &AuthConfig {
                issuer: "https://idp.invalid".into(),
                client_id: "penroam-app".into(),
                scopes: vec!["email".into()],
                redirect: REDIRECT_LOOPBACK.into(),
                loopback_ports: vec![],
                mobile_redirect_uri: None,
            },
            &Discovery {
                issuer: "https://idp.invalid".into(),
                authorization_endpoint: "https://idp.invalid/auth".into(),
                token_endpoint: "http://127.0.0.1:9/token".into(),
            },
        )
        .unwrap()
    }

    fn get(url: &str) -> u16 {
        agent()
            .get(url)
            .call()
            .map(|r| r.status().as_u16())
            .unwrap_or(0)
    }

    #[test]
    fn authorize_url_carries_pkce_and_scopes() {
        let l = login();
        let u = l.authorize_url();
        assert!(u.starts_with("https://idp.invalid/auth?response_type=code&client_id=penroam-app"));
        assert!(u.contains("scope=openid%20email"));
        assert!(u.contains("code_challenge_method=S256"));
        assert!(u.contains(&format!(
            "code_challenge={}",
            pkce_challenge(&l.attempt.verifier)
        )));
        assert!(
            !u.contains(&l.attempt.verifier),
            "verifier must not leave the app"
        );
        assert!(!format!("{l:?}").contains(&l.attempt.state));
    }

    #[test]
    fn callback_with_wrong_state_is_refused() {
        let l = login();
        let url = format!("{}?code=abc&state=forged", l.redirect_uri());
        let t = std::thread::spawn(move || get(&url));
        assert!(matches!(
            l.wait_for_code(Duration::from_secs(5)),
            Err(AuthError::StateMismatch)
        ));
        assert_eq!(t.join().unwrap(), 400);
    }

    #[test]
    fn callback_returns_code_and_ignores_other_paths() {
        let l = login();
        let root = l.redirect_uri().replace("/callback", "/favicon.ico");
        let url = format!(
            "{}?code=c%2Fd&state={}",
            l.redirect_uri(),
            enc(&l.attempt.state)
        );
        let t = std::thread::spawn(move || (get(&root), get(&url)));
        assert_eq!(l.wait_for_code(Duration::from_secs(5)).unwrap(), "c/d");
        assert_eq!(t.join().unwrap(), (404, 200));
    }

    #[test]
    fn denied_and_timeout() {
        let l = login();
        let url = format!(
            "{}?error=access_denied&state={}",
            l.redirect_uri(),
            enc(&l.attempt.state)
        );
        let t = std::thread::spawn(move || get(&url));
        assert!(matches!(
            l.wait_for_code(Duration::from_secs(5)),
            Err(AuthError::Denied(e)) if e == "access_denied"
        ));
        t.join().unwrap();
        let l = login();
        assert!(matches!(
            l.wait_for_code(Duration::from_millis(100)),
            Err(AuthError::Timeout)
        ));
    }

    fn disc(token_endpoint: String) -> Discovery {
        Discovery {
            issuer: "https://idp.invalid".into(),
            authorization_endpoint: "https://idp.invalid/auth".into(),
            token_endpoint,
        }
    }

    fn cfg(redirect: &str, ports: Vec<u16>, mobile: Option<&str>) -> AuthConfig {
        AuthConfig {
            issuer: "https://idp.invalid".into(),
            client_id: "penroam-app".into(),
            scopes: vec!["openid".into()],
            redirect: redirect.into(),
            loopback_ports: ports,
            mobile_redirect_uri: mobile.map(Into::into),
        }
    }

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[test]
    fn fixed_ports_skip_a_busy_one() {
        let busy = TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = busy.local_addr().unwrap().port();
        let free = free_port();
        let l = BrowserLogin::start(
            &cfg(REDIRECT_LOOPBACK_PORTS, vec![taken, free], None),
            &disc("http://127.0.0.1:9/token".into()),
        )
        .unwrap();
        assert_eq!(
            l.redirect_uri(),
            format!("http://127.0.0.1:{free}/callback")
        );
        assert!(l.authorize_url().contains(&format!(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A{free}%2Fcallback"
        )));
        // every registered port busy: a clear error, not a random port
        let err = BrowserLogin::start(
            &cfg(REDIRECT_LOOPBACK_PORTS, vec![taken], None),
            &disc("http://127.0.0.1:9/token".into()),
        )
        .unwrap_err();
        assert!(
            matches!(&err, AuthError::Io(e) if e.kind() == std::io::ErrorKind::AddrInUse),
            "{err}"
        );
        drop(busy);
    }

    /// A one-request token endpoint that answers with an ID token carrying `nonce`, and
    /// returns the form it received.
    fn token_endpoint(nonce: String) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0usize;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                line.clear();
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            let claims = base64url::encode(format!(r#"{{"nonce":"{nonce}"}}"#).as_bytes());
            let json = format!(r#"{{"id_token":"e30.{claims}.c2ln"}}"#);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                json.len()
            )
            .unwrap();
            String::from_utf8(body).unwrap()
        });
        (url, h)
    }

    #[test]
    fn mobile_sign_in_round_trip() {
        let uri = "ink.penroam.app://callback";
        let c = cfg(REDIRECT_LOOPBACK_PORTS, vec![47011], Some(uri));
        // the endpoint is created after the attempt, which knows the nonce
        let probe = AppRedirectLogin::start(&c, &disc(String::new()), "ink.penroam.app").unwrap();
        let (endpoint, server) = token_endpoint(probe.attempt.nonce.clone());
        let l = AppRedirectLogin {
            attempt: Attempt {
                token_endpoint: endpoint,
                ..probe.attempt
            },
        };
        assert_eq!(l.redirect_uri(), uri);
        assert!(l
            .authorize_url()
            .contains("redirect_uri=ink.penroam.app%3A%2F%2Fcallback"));
        let link = format!("{uri}?code=the-code&state={}", enc(&l.attempt.state));
        assert!(l.is_callback(&link));
        assert!(!l.is_callback("ink.penroam.app://other?code=x"));
        let verifier = l.attempt.verifier.clone();
        let id_token = l.finish(&link).unwrap();
        assert!(id_token.starts_with("e30."));
        let form = server.join().unwrap();
        assert!(form.contains("code=the-code"));
        assert!(form.contains(&format!("code_verifier={verifier}")));
        assert!(form.contains("redirect_uri=ink.penroam.app%3A%2F%2Fcallback"));
    }

    #[test]
    fn mobile_sign_in_refusals() {
        let d = disc("http://127.0.0.1:9/token".into());
        assert!(matches!(
            AppRedirectLogin::start(&cfg(REDIRECT_LOOPBACK, vec![], None), &d, "ink.penroam.app"),
            Err(AuthError::NotSupported)
        ));
        let other = cfg(
            REDIRECT_LOOPBACK,
            vec![],
            Some("com.example.other://callback"),
        );
        assert!(matches!(
            AppRedirectLogin::start(&other, &d, "ink.penroam.app"),
            Err(AuthError::Provider(_))
        ));
        let c = cfg(REDIRECT_LOOPBACK, vec![], Some("ink.penroam.app:/callback"));
        let l = AppRedirectLogin::start(&c, &d, "ink.penroam.app").unwrap();
        assert!(matches!(
            l.finish("ink.penroam.app:/callback?code=x&state=forged"),
            Err(AuthError::StateMismatch)
        ));
        let l = AppRedirectLogin::start(&c, &d, "ink.penroam.app").unwrap();
        let denied = format!(
            "ink.penroam.app:/callback?error=access_denied&state={}",
            enc(&l.attempt.state)
        );
        assert!(matches!(l.finish(&denied), Err(AuthError::Denied(e)) if e == "access_denied"));
        let l = AppRedirectLogin::start(&c, &d, "ink.penroam.app").unwrap();
        assert!(!format!("{l:?}").contains(&l.attempt.state));
        assert!(matches!(
            l.finish("https://evil.invalid/callback?code=x"),
            Err(AuthError::StateMismatch)
        ));
    }
}
