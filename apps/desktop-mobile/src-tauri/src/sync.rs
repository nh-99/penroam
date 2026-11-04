//! Sync integration (developer preview): a worker thread owns the sync
//! engine and reconciles in the background. The library lock is taken only
//! for the local phases, so saving and pen input never wait for the network.
//!
//! Sign-in depends on the server:
//! * Servers that advertise `auth.v1` (protocol §12a): the user signs in with
//!   the server's identity provider in the **system browser**
//!   ([`sync_sign_in`], PKCE). Desktop receives the answer on a loopback
//!   redirect; Android and iOS receive it as an `ink.penroam.app:` link that
//!   the operating system hands to the app ([`deliver_sign_in_link`]), and
//!   only if the server offers a mobile redirect. The account session's
//!   refresh token is kept in the OS keychain next to the device keys; the
//!   worker trades it for a device session once this device is a key-log
//!   member, and keeps that one in the keychain too. Refresh tokens rotate on
//!   every use. When the sign-in ends (refresh refused) the status becomes
//!   `needsReauth` and the settings offer "Sign in again"; queued changes stay.
//! * The development test server: its session stub, with a typed account name.
//!
//! Server URLs must be `https://`, or `http://` on loopback only (any
//! `http://` host with `PENROAM_SYNC_ALLOW_ANY_URL=1`, for development).
//! Tokens and keys never reach the WebView or logs.
//!
//! Lifecycle:
//! * At launch, [`resume_on_launch`] opens an existing setup and starts the
//!   worker; the `sync_setup_*` commands create a setup and start it.
//! * The worker thread owns the [`SyncEngine`] and loops on a channel of
//!   [`Cmd`]s with a timeout: local edits send `Nudge` (debounced
//!   [`DEBOUNCE`]), the UI can ask for `SyncNow`, and otherwise it reconciles
//!   every [`IDLE_INTERVAL`] or at the backoff deadline after a failure.
//! * After each reconcile it emits `sync://status` and, when merged changes
//!   were installed, `sync://changed` so the editor can reload pages.
//! * Commands that need the engine (devices, conflicts, approvals, …) are
//!   sent to the worker with a reply channel, so the engine is only ever used
//!   from one thread.
//! * `sync_disconnect` stops the worker and removes keys and sync metadata;
//!   notebooks are untouched.

use crate::commands::CmdError;
use crate::AppState;
use penroam_core::replica::{Conflict, ConflictTarget};
use penroam_storage::ErrorCode;
use penroam_sync::auth;
use penroam_sync::secrets::KeychainSecretStore;
use penroam_sync::store::SyncStore;
use penroam_sync::transport::HttpTransport;
use penroam_sync::SyncTransport as _;
use penroam_sync::{
    DeviceView, FileSecretStore, SecretStore, Setup, SyncEngine, SyncError, SyncStatus,
};
use penroam_sync_protocol::account::actions;
use penroam_sync_protocol::dto::capabilities;
use penroam_sync_protocol::{
    AccountState, BillingLinkRequest, ErrorCode as WireCode, SessionTokens,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

type Engine = SyncEngine<HttpTransport>;
type Reply<T> = Sender<Result<T, String>>;

const IDLE_INTERVAL: Duration = Duration::from_secs(60);
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// How long a browser sign-in may take before it is abandoned.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// Pause between attempts while the sign-in has ended (the user must act).
const SIGNED_OUT_RETRY: Duration = Duration::from_secs(60);

/// URI scheme this app claims for the mobile sign-in redirect (its identifier; registered in
/// `tauri.conf.json > plugins > deep-link`).
#[cfg(mobile)]
const APP_SCHEME: &str = "ink.penroam.app";

/// Where links for a waiting mobile sign-in go. One sign-in at a time: a new one replaces the
/// sender, and the old one runs into its timeout.
#[cfg(mobile)]
static SIGN_IN_LINKS: Mutex<Option<Sender<String>>> = Mutex::new(None);

/// Called with every link the operating system opens the app with. Links in the app's scheme
/// go to the waiting sign-in, if any; the sign-in itself checks that the link is its answer.
/// A link that arrives with no sign-in waiting (the app was restarted meanwhile) is dropped:
/// its code is useless without the attempt's PKCE verifier.
#[cfg(mobile)]
pub fn deliver_sign_in_link(url: &str) {
    let ours = url
        .split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case(APP_SCHEME));
    if !ours {
        return;
    }
    if let Ok(slot) = SIGN_IN_LINKS.lock() {
        if let Some(tx) = slot.as_ref() {
            let _ = tx.send(url.to_string());
        }
    }
}

/// Desktop: loopback redirect (any port, or the first free registered port).
#[cfg(desktop)]
fn browser_sign_in(
    app: &AppHandle,
    cfg: &penroam_sync_protocol::AuthConfig,
    disc: &auth::Discovery,
) -> Result<String, CmdError> {
    let login = auth::BrowserLogin::start(cfg, disc).map_err(|e| err(e.to_string()))?;
    app.opener()
        .open_url(login.authorize_url(), None::<&str>)
        .map_err(|e| err(format!("could not open the browser: {e}")))?;
    login
        .finish(SIGN_IN_TIMEOUT)
        .map_err(|e| err(e.to_string()))
}

/// Mobile: the answer arrives as an `ink.penroam.app:` link ([`deliver_sign_in_link`]).
#[cfg(mobile)]
fn browser_sign_in(
    app: &AppHandle,
    cfg: &penroam_sync_protocol::AuthConfig,
    disc: &auth::Discovery,
) -> Result<String, CmdError> {
    let login = match auth::AppRedirectLogin::start(cfg, disc, APP_SCHEME) {
        Ok(l) => l,
        Err(auth::AuthError::NotSupported) => {
            return Err(err(
                "this server does not offer sign-in from phones and tablets yet",
            ))
        }
        Err(e) => return Err(err(e.to_string())),
    };
    let (tx, rx) = mpsc::channel();
    if let Ok(mut slot) = SIGN_IN_LINKS.lock() {
        *slot = Some(tx);
    }
    let result = (|| {
        app.opener()
            .open_url(login.authorize_url(), None::<&str>)
            .map_err(|e| err(format!("could not open the browser: {e}")))?;
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                // Another link in our scheme (not this sign-in's answer): keep waiting.
                Ok(link) if !login.is_callback(&link) => continue,
                Ok(link) => return login.finish(&link).map_err(|e| err(e.to_string())),
                Err(_) => return Err(err(auth::AuthError::Timeout.to_string())),
            }
        }
    })();
    if let Ok(mut slot) = SIGN_IN_LINKS.lock() {
        *slot = None;
    }
    result
}

/// Messages to the worker thread. Variants with a [`Reply`] are answered on
/// that channel.
enum Cmd {
    Nudge,
    SyncNow,
    Pause(bool),
    Status(Reply<SyncStatus>),
    Devices(Reply<Vec<DeviceView>>),
    Approve(String, String, Reply<String>),
    Remove(String, Reply<()>),
    Conflicts(Uuid, Reply<Vec<Conflict>>),
    Resolve(Uuid, ConflictTarget, Uuid, Reply<()>),
    PollPairing(Reply<Option<String>>),
    Diagnostics(Reply<String>),
    /// A new account session was stored: drop the current token and retry.
    Reauth,
    /// Plan and storage (`account.v1`); `None` when the server has no accounts.
    Account(Reply<Option<AccountView>>),
    /// A billing page URL (`account.v1`): action and offer id.
    BillingLink(String, Option<String>, Reply<String>),
    /// Withdraw a pending account deletion.
    CancelDeletion(Reply<()>),
    Disconnect(Reply<()>),
}

/// The sync slot in [`AppState`].
/// The sync slot in [`AppState`]: the worker's command channel, if running.
#[derive(Default)]
pub struct SyncSlot {
    tx: Mutex<Option<Sender<Cmd>>>,
}

impl SyncSlot {
    /// Tells the worker that local data changed (debounced there).
    pub fn nudge(&self) {
        if let Ok(g) = self.tx.lock() {
            if let Some(tx) = g.as_ref() {
                let _ = tx.send(Cmd::Nudge);
            }
        }
    }

    fn send(&self, c: Cmd) -> Result<(), CmdError> {
        let g = self.tx.lock().map_err(|_| err("sync state poisoned"))?;
        g.as_ref()
            .ok_or_else(|| err("sync is not set up"))?
            .send(c)
            .map_err(|_| err("sync worker stopped"))
    }

    fn set(&self, tx: Option<Sender<Cmd>>) {
        if let Ok(mut g) = self.tx.lock() {
            *g = tx;
        }
    }
}

fn err(m: impl Into<String>) -> CmdError {
    CmdError {
        code: ErrorCode::Invalid,
        message: m.into(),
    }
}

fn sync_err(e: SyncError) -> CmdError {
    err(e.to_string())
}

/// Sends a request to the worker and waits (bounded) for its answer.
fn ask<T>(slot: &SyncSlot, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T, CmdError> {
    let (tx, rx) = mpsc::channel();
    slot.send(make(tx))?;
    rx.recv_timeout(Duration::from_secs(120))
        .map_err(|_| err("the sync worker did not answer"))?
        .map_err(err)
}

/// The sync server of this app: Penroam Cloud.
pub const DEFAULT_SERVER_URL: &str = "https://sync.penroam.ink";

/// Debug builds may sync with any server (developer mode in the settings);
/// release builds only with [`DEFAULT_SERVER_URL`].
const CUSTOM_SERVERS: bool = cfg!(debug_assertions);

/// What the app says when the sync server cannot be reached. Release builds
/// keep the address and the network error out of it (both are details the user
/// cannot act on); debug builds show them.
fn unreachable_message(detail: &str) -> String {
    if CUSTOM_SERVERS {
        format!("could not reach the server: {detail}")
    } else {
        "Could not reach Penroam Cloud. Check your internet connection and try again.".into()
    }
}

/// Developer preview gate: debug builds, or `PENROAM_DEV_SYNC=1`.
pub fn available() -> bool {
    cfg!(debug_assertions) || std::env::var("PENROAM_DEV_SYNC").is_ok_and(|v| v == "1")
}

/// Release builds: only [`DEFAULT_SERVER_URL`]. Debug builds: `https://`
/// URLs, and `http://` only on loopback unless `PENROAM_SYNC_ALLOW_ANY_URL=1`
/// (tokens must never cross a network in clear).
fn check_url(url: &str) -> Result<String, CmdError> {
    check_url_with(url, CUSTOM_SERVERS)
}

fn check_url_with(url: &str, custom_servers: bool) -> Result<String, CmdError> {
    let u = url.trim().trim_end_matches('/').to_string();
    // The settings only offer another server in debug builds; refuse it here
    // too, whatever the WebView sends.
    if !custom_servers {
        return if u.eq_ignore_ascii_case(DEFAULT_SERVER_URL) {
            Ok(DEFAULT_SERVER_URL.to_string())
        } else {
            Err(err("this version of Penroam syncs only with Penroam Cloud"))
        };
    }
    let any = std::env::var("PENROAM_SYNC_ALLOW_ANY_URL").is_ok_and(|v| v == "1");
    let rest = u
        .strip_prefix("http://")
        .or_else(|| u.strip_prefix("https://"))
        .ok_or_else(|| err("the server URL must start with http:// or https://"))?;
    let host = rest.split(['/', ':']).next().unwrap_or("");
    let host = if rest.starts_with('[') {
        rest.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host
    };
    let tls = u.starts_with("https://");
    if !tls && !any && !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err(err(
            "use an https:// server address (plain http:// is only allowed for a server on this computer)",
        ));
    }
    if host.is_empty() {
        return Err(err("the server URL has no host"));
    }
    Ok(u)
}

fn library_root(app: &AppHandle) -> PathBuf {
    app.state::<AppState>()
        .lib
        .lock()
        .map(|l| l.root().to_path_buf())
        .unwrap_or_default()
}

const KEYCHAIN_SERVICE: &str = "ink.penroam.app.sync";
const DEVICE_SECRET: &str = "sync-device-keys";

/// The secret store for this library's sync keys: the OS keychain/keystore.
/// Keys left in the older file store are moved into it (then the file is
/// deleted). Without an OS keychain sync is refused, unless
/// `PENROAM_SYNC_ALLOW_FILE_SECRETS=1` (headless development only).
fn secrets(app: &AppHandle, root: &std::path::Path) -> Result<Box<dyn SecretStore>, CmdError> {
    let file = FileSecretStore::new(SyncStore::dir_for(root).join("secrets"));
    // scope by the library's device id so profiles on one machine never share keys
    let scope = app
        .state::<AppState>()
        .lib
        .lock()
        .map(|l| l.device_id().to_string())
        .map_err(|_| err("library lock poisoned"))?;
    match KeychainSecretStore::open(KEYCHAIN_SERVICE, &scope) {
        Ok(kc) => {
            if let Ok(Some(old)) = file.get(DEVICE_SECRET) {
                if kc.get(DEVICE_SECRET).map_err(|e| err(e.to_string()))?.is_none() {
                    kc.set(DEVICE_SECRET, &old).map_err(|e| err(e.to_string()))?;
                }
                if kc.get(DEVICE_SECRET).ok().flatten().as_deref() == Some(old.as_str()) {
                    file.delete(DEVICE_SECRET).map_err(|e| err(e.to_string()))?;
                }
            }
            Ok(Box::new(kc))
        }
        Err(e) if std::env::var("PENROAM_SYNC_ALLOW_FILE_SECRETS").is_ok_and(|v| v == "1") => {
            eprintln!("penroam: no OS keychain ({e}); using the file secret store (development override)");
            Ok(Box::new(file))
        }
        Err(e) => Err(err(format!(
            "sync needs the system keychain to store this device's keys, and it is not available ({e}). \
             On Linux, make sure a Secret Service provider such as GNOME Keyring or KWallet is running."
        ))),
    }
}

/// Keychain entry with the account session (`auth.v1` servers).
const ACCOUNT_SESSION: &str = "sync-account-session";
/// Keychain entry with the device session (`auth.v1` servers).
const DEVICE_SESSION: &str = "sync-device-session";

/// A session kept in the keychain: only the refresh token is stored (access
/// tokens live in memory), bound to the server it belongs to.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSession {
    server_url: String,
    account_id: String,
    refresh_token: String,
}

fn load_session(store: &dyn SecretStore, name: &str, url: &str) -> Option<StoredSession> {
    let raw = store.get(name).ok().flatten()?;
    serde_json::from_str::<StoredSession>(&raw)
        .ok()
        .filter(|s| s.server_url == url)
}

fn save_session(
    store: &dyn SecretStore,
    name: &str,
    url: &str,
    t: &SessionTokens,
) -> Result<(), String> {
    let s = StoredSession {
        server_url: url.to_string(),
        account_id: t.account_id.to_string(),
        refresh_token: t.refresh_token.clone(),
    };
    store
        .set(name, &serde_json::to_string(&s).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// Whether the server signs users in itself (`auth.v1`) rather than being the
/// development test server.
fn uses_sign_in(url: &str) -> Result<bool, String> {
    HttpTransport::new(url, String::new())
        .meta()
        .map(|m| m.has_capability(capabilities::AUTH_V1))
        .map_err(|e| unreachable_message(&e.to_string()))
}

/// Why no token could be obtained.
enum TokenError {
    /// The sign-in ended (no stored session, or its refresh was refused).
    SignedOut,
    /// Anything else (network, server); retried later.
    Other(String),
}

fn is_unauthenticated(e: &penroam_sync::TransportError) -> bool {
    e.code() == Some(WireCode::Unauthenticated)
}

/// A fresh account session from the stored refresh token. The rotated refresh
/// token is saved before the new access token is used; a refused refresh
/// deletes the stored session (the user must sign in again).
fn refresh_account(store: &dyn SecretStore, url: &str) -> Result<SessionTokens, TokenError> {
    let s = load_session(store, ACCOUNT_SESSION, url).ok_or(TokenError::SignedOut)?;
    match auth::refresh(url, &s.refresh_token) {
        Ok(t) => {
            save_session(store, ACCOUNT_SESSION, url, &t).map_err(TokenError::Other)?;
            Ok(t)
        }
        Err(auth::AuthError::Transport(e)) if is_unauthenticated(&e) => {
            let _ = store.delete(ACCOUNT_SESSION);
            Err(TokenError::SignedOut)
        }
        Err(e) => Err(TokenError::Other(e.to_string())),
    }
}

/// An account-level token for the setup commands: the signed-in session on
/// `auth.v1` servers (the account is the server's, not typed), or the test
/// server stub with the typed account name. Returns (token, account id).
fn account_access(
    store: &dyn SecretStore,
    url: &str,
    account: &str,
) -> Result<(String, String), CmdError> {
    if uses_sign_in(url).map_err(err)? {
        return match refresh_account(store, url) {
            Ok(t) => Ok((t.access_token, t.account_id.to_string())),
            Err(TokenError::SignedOut) => Err(err("sign in to this server first")),
            Err(TokenError::Other(e)) => Err(err(e)),
        };
    }
    if account.trim().is_empty() {
        return Err(err("enter an account name"));
    }
    let t = test_token(url, account, None).map_err(err)?;
    Ok((t, account.trim().to_string()))
}

/// The token the worker uses. `auth.v1`: the stored device session if it still
/// refreshes, else a new device session signed with this device's key (only
/// possible once it is a key-log member), else the account session (enough for
/// pairing). Test server: its stub.
fn worker_token(app: &AppHandle, engine: &mut Engine) -> Result<String, TokenError> {
    let (url, account, device) = {
        let st = &engine.store.state;
        (
            st.server_url.clone(),
            st.account_id.clone(),
            st.device_id.clone(),
        )
    };
    if !uses_sign_in(&url).map_err(TokenError::Other)? {
        return test_token(&url, &account, Some(&device)).map_err(TokenError::Other);
    }
    let root = library_root(app);
    let store = secrets(app, &root).map_err(|e| TokenError::Other(e.message))?;
    if let Some(d) = load_session(store.as_ref(), DEVICE_SESSION, &url) {
        match auth::refresh(&url, &d.refresh_token) {
            Ok(t) => {
                save_session(store.as_ref(), DEVICE_SESSION, &url, &t)
                    .map_err(TokenError::Other)?;
                return Ok(t.access_token);
            }
            Err(auth::AuthError::Transport(e))
                if !matches!(e, penroam_sync::TransportError::Network(_)) =>
            {
                // ended or refused (for example the device was removed): fall through
                let _ = store.delete(DEVICE_SESSION);
            }
            Err(e) => return Err(TokenError::Other(e.to_string())),
        }
    }
    let acct = refresh_account(store.as_ref(), &url)?;
    if acct.account_id.as_str() != account {
        return Err(TokenError::Other(
            "signed in to a different account than this device syncs with".into(),
        ));
    }
    match engine.device_session(&url, &acct.access_token) {
        Ok(t) => {
            save_session(store.as_ref(), DEVICE_SESSION, &url, &t).map_err(TokenError::Other)?;
            Ok(t.access_token)
        }
        Err(_) => Ok(acct.access_token),
    }
}

/// Best-effort logout of every stored session for `url`, then removal from
/// the keychain.
fn forget_sessions(store: &dyn SecretStore, url: &str) {
    for name in [DEVICE_SESSION, ACCOUNT_SESSION] {
        if let Some(s) = load_session(store, name, url) {
            if let Ok(t) = auth::refresh(url, &s.refresh_token) {
                let _ = auth::logout(url, &t.access_token);
            }
        }
        let _ = store.delete(name);
    }
}

/// Gets a session token from the development test server's sign-in stub:
/// a device token when the device is registered, else an account token.
fn test_token(url: &str, account: &str, device: Option<&str>) -> Result<String, String> {
    if let Some(d) = device {
        if let Ok((t, _)) = HttpTransport::test_session(url, account, Some(d)) {
            return Ok(t);
        }
    }
    HttpTransport::test_session(url, account, None)
        .map(|(t, _)| t)
        .map_err(|e| e.to_string())
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Changed {
    notebook_ids: Vec<Uuid>,
    pages: Vec<(Uuid, Uuid)>,
}

fn emit_status(app: &AppHandle, e: &Engine) {
    let last = app
        .state::<AppState>()
        .lib
        .lock()
        .map(|l| l.last_seq())
        .unwrap_or(0);
    let _ = app.emit("sync://status", e.status(last));
}

/// The worker loop (see the module docs). Reconcile errors are not fatal:
/// the next attempt is scheduled from the engine's backoff policy, and a
/// rejected token triggers a fresh sign-in.
fn worker(app: AppHandle, mut engine: Engine, rx: Receiver<Cmd>) {
    let mut due: Option<Instant> = Some(Instant::now());
    let mut have_token = false;
    let mut signed_out = false;
    // Gets a token when none is held. Returns false when none could be had;
    // after a sign-out the empty token makes the next request fail with
    // `unauthenticated`, which the engine reports as `needsReauth`.
    let ensure = |engine: &mut Engine, have: &mut bool, out: &mut bool| -> bool {
        if *have {
            return true;
        }
        match worker_token(&app, engine) {
            Ok(t) => {
                engine.transport_mut().set_token(t);
                *have = true;
                *out = false;
                true
            }
            Err(TokenError::SignedOut) => {
                engine.transport_mut().set_token(String::new());
                *out = true;
                false
            }
            Err(TokenError::Other(_)) => false,
        }
    };
    let unauth = |e: &SyncError| matches!(e, SyncError::Transport(t) if is_unauthenticated(t));
    loop {
        let wait = due.map_or(IDLE_INTERVAL, |d| {
            d.saturating_duration_since(Instant::now())
        });
        match rx.recv_timeout(wait) {
            Ok(Cmd::Nudge) => {
                let d = Instant::now() + DEBOUNCE;
                due = Some(
                    due.map_or(d, |x| x.min(d))
                        .max(Instant::now() + Duration::from_millis(300)),
                );
                continue;
            }
            Ok(Cmd::SyncNow) => {
                engine.attempt = 0;
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::Pause(p)) => {
                let _ = engine.set_paused(p);
                emit_status(&app, &engine);
                if !p {
                    due = Some(Instant::now());
                }
                continue;
            }
            Ok(Cmd::Status(r)) => {
                let last = app
                    .state::<AppState>()
                    .lib
                    .lock()
                    .map(|l| l.last_seq())
                    .unwrap_or(0);
                let _ = r.send(Ok(engine.status(last)));
                continue;
            }
            Ok(Cmd::Devices(r)) => {
                let _ = r.send(Ok(engine.devices()));
                continue;
            }
            Ok(Cmd::Approve(code, label, r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let mut res = engine.approve_device(&code, &label);
                if res.as_ref().is_err_and(unauth) {
                    have_token = false;
                    if ensure(&mut engine, &mut have_token, &mut signed_out) {
                        res = engine.approve_device(&code, &label);
                    }
                }
                let _ = r.send(res.map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::Remove(id, r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let mut res = engine.remove_device(&id);
                if res.as_ref().is_err_and(unauth) {
                    have_token = false;
                    if ensure(&mut engine, &mut have_token, &mut signed_out) {
                        res = engine.remove_device(&id);
                    }
                }
                let _ = r.send(res.map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::Conflicts(nb, r)) => {
                let _ = r.send(engine.conflicts(nb).map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::Resolve(nb, target, choose, r)) => {
                let res = {
                    let state = app.state::<AppState>();
                    let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
                    engine.resolve_conflict(&mut lib, nb, &target, choose)
                };
                let _ = r.send(res.map_err(|e| e.to_string()));
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::PollPairing(r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let mut res = engine.poll_enrollment();
                if res.as_ref().is_err_and(unauth) {
                    have_token = false;
                    if ensure(&mut engine, &mut have_token, &mut signed_out) {
                        res = engine.poll_enrollment();
                    }
                }
                let res = res.map_err(|e| e.to_string());
                if matches!(res, Ok(Some(_))) {
                    have_token = false; // a device session is possible now
                    due = Some(Instant::now());
                }
                let _ = r.send(res);
                emit_status(&app, &engine);
                continue;
            }
            Ok(Cmd::Diagnostics(r)) => {
                let last = app
                    .state::<AppState>()
                    .lib
                    .lock()
                    .map(|l| l.last_seq())
                    .unwrap_or(0);
                let report = serde_json::json!({
                    "status": engine.status(last),
                    "events": engine.events,
                    "note": "privacy-safe: no note content, keys or tokens",
                });
                let _ = r.send(Ok(serde_json::to_string_pretty(&report).unwrap_or_default()));
                continue;
            }
            Ok(Cmd::Account(r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let mut res = account_view(engine.transport_mut());
                if res.as_ref().is_err_and(is_unauthenticated) {
                    have_token = false;
                    if ensure(&mut engine, &mut have_token, &mut signed_out) {
                        res = account_view(engine.transport_mut());
                    }
                }
                let _ = r.send(res.map_err(|e| account_error(&e)));
                continue;
            }
            Ok(Cmd::BillingLink(action, offer, r)) => {
                let req = BillingLinkRequest {
                    action,
                    offer_id: offer,
                };
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let mut res = engine.transport_mut().billing_link(&req);
                if res.as_ref().is_err_and(is_unauthenticated) {
                    have_token = false;
                    if ensure(&mut engine, &mut have_token, &mut signed_out) {
                        res = engine.transport_mut().billing_link(&req);
                    }
                }
                let _ = r.send(res.map(|l| l.url).map_err(|e| account_error(&e)));
                continue;
            }
            Ok(Cmd::CancelDeletion(r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let mut res = engine.transport_mut().cancel_deletion();
                if res.as_ref().is_err_and(is_unauthenticated) {
                    have_token = false;
                    if ensure(&mut engine, &mut have_token, &mut signed_out) {
                        res = engine.transport_mut().cancel_deletion();
                    }
                }
                let _ = r.send(res.map_err(|e| account_error(&e)));
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::Reauth) => {
                have_token = false;
                signed_out = false;
                engine.attempt = 0;
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::Disconnect(r)) => {
                let root = library_root(&app);
                if let Ok(store) = secrets(&app, &root) {
                    forget_sessions(store.as_ref(), &engine.store.state.server_url);
                }
                let _ = r.send(engine.disconnect(&root).map_err(|e| e.to_string()));
                app.state::<AppState>().sync.set(None);
                let _ = app.emit("sync://status", serde_json::Value::Null);
                return;
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // reconcile
        if !engine.store.state.enrolled {
            due = Some(Instant::now() + Duration::from_secs(5));
            emit_status(&app, &engine);
            continue;
        }
        if !ensure(&mut engine, &mut have_token, &mut signed_out) && !signed_out {
            due = Some(Instant::now() + Duration::from_secs(30));
            emit_status(&app, &engine);
            continue;
        }
        let state = app.state::<AppState>();
        let result = engine.reconcile(&state.lib);
        match &result {
            Ok(report) => {
                if !report.installed_notebooks.is_empty() {
                    let _ = app.emit(
                        "sync://changed",
                        Changed {
                            notebook_ids: report.installed_notebooks.clone(),
                            pages: report.installed_pages.clone(),
                        },
                    );
                }
                due = None; // idle until a nudge or the interval
            }
            Err(e) => {
                if signed_out {
                    // reported as needsReauth; wait for "Sign in again" (Cmd::Reauth)
                    due = Some(Instant::now() + SIGNED_OUT_RETRY);
                } else if unauth(e) {
                    have_token = false;
                    due = Some(Instant::now() + Duration::from_secs(5));
                } else {
                    due = engine
                        .retry_delay_ms(e)
                        .map(|ms| Instant::now() + Duration::from_millis(ms.max(250)));
                }
            }
        }
        emit_status(&app, &engine);
    }
}

/// Registers the worker's channel in [`AppState`] and spawns the thread.
fn start(app: &AppHandle, engine: Engine) {
    let (tx, rx) = mpsc::channel();
    app.state::<AppState>().sync.set(Some(tx));
    let h = app.clone();
    std::thread::Builder::new()
        .name("penroam-sync".into())
        .spawn(move || worker(h, engine, rx))
        .expect("spawn sync worker");
}

/// Starts the worker at launch when this library is already set up.
pub fn resume_on_launch(app: &AppHandle) {
    if !available() {
        return;
    }
    let root = library_root(app);
    if !SyncStore::exists(&root) {
        return;
    }
    let url = SyncStore::open(&root)
        .map(|s| s.state.server_url)
        .unwrap_or_default();
    let store = match secrets(app, &root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("penroam: sync not started: {}", e.message);
            return;
        }
    };
    match Engine::open(&root, HttpTransport::new(&url, String::new()), store) {
        Ok(e) => start(app, e),
        Err(e) => eprintln!("penroam: sync not started: {e}"),
    }
}

// ---------------------------------------------------------------- commands --

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
/// What the settings screen needs to decide what to show.
pub struct SyncInfo {
    available: bool,
    configured: bool,
    status: Option<SyncStatus>,
    /// The server new setups use unless developer mode picks another.
    default_server_url: &'static str,
    /// Debug build: the settings may offer developer mode (custom servers).
    dev_build: bool,
}

/// Whether sync is available in this build, configured, and its status.
#[tauri::command]
pub fn sync_info(app: AppHandle, state: State<'_, AppState>) -> Result<SyncInfo, CmdError> {
    let configured = state.sync.tx.lock().map(|g| g.is_some()).unwrap_or(false);
    let status = if configured {
        ask(&state.sync, Cmd::Status).ok()
    } else {
        None
    };
    let _ = app;
    Ok(SyncInfo {
        available: available(),
        configured,
        status,
        default_server_url: DEFAULT_SERVER_URL,
        dev_build: CUSTOM_SERVERS,
    })
}

/// Setup preconditions: available in this build and not already set up.
fn guard(state: &State<'_, AppState>) -> Result<(), CmdError> {
    if !available() {
        return Err(err("sync is not available in this build"));
    }
    if state.sync.tx.lock().map(|g| g.is_some()).unwrap_or(false) {
        return Err(err("sync is already set up; disconnect first"));
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
/// Codes to show after a setup step (the recovery code is shown only once).
pub struct SetupResult {
    recovery_code: Option<String>,
    pairing_code: Option<String>,
}

/// First device of an account: creates the account keys and returns the
/// recovery code.
#[tauri::command(async)]
pub fn sync_setup_first(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    account: String,
    label: String,
) -> Result<SetupResult, CmdError> {
    guard(&state)?;
    let url = check_url(&server_url)?;
    let root = library_root(&app);
    let store = secrets(&app, &root)?;
    let (token, account) = account_access(store.as_ref(), &url, &account)?;
    let t = HttpTransport::new(&url, token);
    let (engine, code) = Engine::create_account(
        Setup {
            library_root: root.clone(),
            server_url: url,
            account_id: account,
            label,
        },
        t,
        store,
    )
    .map_err(sync_err)?;
    start(&app, engine);
    Ok(SetupResult {
        recovery_code: Some(code),
        pairing_code: None,
    })
}

/// New device, pairing: registers this device and returns its pairing code
/// for a trusted device to approve.
#[tauri::command(async)]
pub fn sync_setup_pair(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    account: String,
    label: String,
) -> Result<SetupResult, CmdError> {
    guard(&state)?;
    let url = check_url(&server_url)?;
    let root = library_root(&app);
    let store = secrets(&app, &root)?;
    let (token, account) = account_access(store.as_ref(), &url, &account)?;
    let t = HttpTransport::new(&url, token);
    let (engine, code) = Engine::begin_pairing(
        Setup {
            library_root: root.clone(),
            server_url: url,
            account_id: account,
            label,
        },
        t,
        store,
    )
    .map_err(sync_err)?;
    start(&app, engine);
    Ok(SetupResult {
        recovery_code: None,
        pairing_code: Some(code),
    })
}

/// New device, enrolled with the recovery code (no other device needed).
#[tauri::command(async)]
pub fn sync_setup_recovery(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    account: String,
    label: String,
    code: String,
) -> Result<(), CmdError> {
    guard(&state)?;
    let url = check_url(&server_url)?;
    let root = library_root(&app);
    let store = secrets(&app, &root)?;
    let (token, account) = account_access(store.as_ref(), &url, &account)?;
    let t = HttpTransport::new(&url, token);
    let engine = Engine::enroll_with_recovery(
        Setup {
            library_root: root.clone(),
            server_url: url,
            account_id: account,
            label,
        },
        t,
        store,
        &code,
    )
    .map_err(sync_err)?;
    start(&app, engine);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
/// What the setup form needs to know about a server.
pub struct ServerInfo {
    /// The server signs users in through the browser (`auth.v1`); otherwise
    /// it is the development test server and takes a typed account name.
    sign_in: bool,
    /// The account this device is signed in to on that server, if any.
    signed_in_account: Option<String>,
}

/// Checks a server address before setup.
#[tauri::command(async)]
pub fn sync_server_info(app: AppHandle, server_url: String) -> Result<ServerInfo, CmdError> {
    let url = check_url(&server_url)?;
    let sign_in = uses_sign_in(&url).map_err(err)?;
    let signed_in_account = if sign_in {
        let root = library_root(&app);
        let store = secrets(&app, &root)?;
        load_session(store.as_ref(), ACCOUNT_SESSION, &url).map(|s| s.account_id)
    } else {
        None
    };
    Ok(ServerInfo {
        sign_in,
        signed_in_account,
    })
}

/// Signs in with the server's identity provider in the system browser and
/// keeps the session in the keychain. Also used to sign in again after the
/// sign-in ended: then the account must be the one this device syncs with.
/// Returns the account id.
#[tauri::command(async)]
pub fn sync_sign_in(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
) -> Result<String, CmdError> {
    if !available() {
        return Err(err("sync is not available in this build"));
    }
    let url = check_url(&server_url)?;
    if !uses_sign_in(&url).map_err(err)? {
        return Err(err(
            "this server has no sign-in (it is a development test server)",
        ));
    }
    let cfg = auth::auth_config(&url).map_err(|e| err(e.to_string()))?;
    let disc = auth::discover(&cfg.issuer).map_err(|e| err(e.to_string()))?;
    let id_token = browser_sign_in(&app, &cfg, &disc)?;
    let t = auth::session_from_id_token(&url, &id_token).map_err(|e| err(e.to_string()))?;
    let root = library_root(&app);
    let configured = state.sync.tx.lock().map(|g| g.is_some()).unwrap_or(false);
    if configured {
        let synced = SyncStore::open(&root)
            .map(|s| s.state.account_id)
            .unwrap_or_default();
        if synced != t.account_id.as_str() {
            let _ = auth::logout(&url, &t.access_token);
            return Err(err(
                "that sign-in belongs to a different account than this device syncs with",
            ));
        }
    }
    let store = secrets(&app, &root)?;
    save_session(store.as_ref(), ACCOUNT_SESSION, &url, &t).map_err(err)?;
    let _ = store.delete(DEVICE_SESSION);
    if configured {
        let _ = state.sync.send(Cmd::Reauth);
    }
    Ok(t.account_id.to_string())
}

/// Signs out of a server before sync is set up (to use another account).
#[tauri::command(async)]
pub fn sync_sign_out(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
) -> Result<(), CmdError> {
    guard(&state)?;
    let url = check_url(&server_url)?;
    let root = library_root(&app);
    let store = secrets(&app, &root)?;
    forget_sessions(store.as_ref(), &url);
    Ok(())
}

/// While pairing: the comparison code once a trusted device approved us.
#[tauri::command(async)]
pub fn sync_poll_pairing(state: State<'_, AppState>) -> Result<Option<String>, CmdError> {
    ask(&state.sync, Cmd::PollPairing)
}

/// Trusted device: approves the device showing `code`; returns the comparison code.
#[tauri::command(async)]
pub fn sync_approve_device(
    state: State<'_, AppState>,
    code: String,
    label: String,
) -> Result<String, CmdError> {
    ask(&state.sync, |r| Cmd::Approve(code, label, r))
}

/// Removes a device (new key epoch, server revocation).
#[tauri::command(async)]
pub fn sync_remove_device(state: State<'_, AppState>, device_id: String) -> Result<(), CmdError> {
    ask(&state.sync, |r| Cmd::Remove(device_id, r))
}

/// Devices from the verified key log.
#[tauri::command(async)]
pub fn sync_devices(state: State<'_, AppState>) -> Result<Vec<DeviceView>, CmdError> {
    ask(&state.sync, Cmd::Devices)
}

/// Reconcile as soon as possible (resets the backoff).
#[tauri::command]
pub fn sync_now(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.sync.send(Cmd::SyncNow)
}

/// Pauses or resumes background sync (local saving is unaffected).
#[tauri::command]
pub fn sync_pause(state: State<'_, AppState>, paused: bool) -> Result<(), CmdError> {
    state.sync.send(Cmd::Pause(paused))
}

/// Unresolved conflicts of a notebook, with every retained version.
#[tauri::command(async)]
pub fn sync_conflicts(
    state: State<'_, AppState>,
    notebook_id: Uuid,
) -> Result<Vec<Conflict>, CmdError> {
    ask(&state.sync, |r| Cmd::Conflicts(notebook_id, r))
}

/// Keeps one version of a conflict (an ordinary local edit that supersedes
/// the others everywhere once synced).
#[tauri::command(async)]
pub fn sync_resolve_conflict(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    target: serde_json::Value,
    choose: Uuid,
) -> Result<(), CmdError> {
    let target: ConflictTarget = serde_json::from_value(target).map_err(|e| err(e.to_string()))?;
    ask(&state.sync, |r| {
        Cmd::Resolve(notebook_id, target, choose, r)
    })
}

/// Privacy-safe diagnostics (status and events; no content, keys or tokens).
#[tauri::command(async)]
pub fn sync_diagnostics(state: State<'_, AppState>) -> Result<String, CmdError> {
    ask(&state.sync, Cmd::Diagnostics)
}

/// Forgets the sync setup on this device (keys and sync metadata). Notes stay.
/// Falls back to removing the files directly when the worker is not running.
#[tauri::command(async)]
pub fn sync_disconnect(app: AppHandle, state: State<'_, AppState>) -> Result<(), CmdError> {
    match ask(&state.sync, Cmd::Disconnect) {
        Ok(()) => Ok(()),
        Err(e) => {
            // no worker (it never started or stopped): remove the setup directly.
            // Only sync metadata and keys are removed; notes stay in the library.
            let root = library_root(&app);
            if !SyncStore::exists(&root) {
                return Err(e);
            }
            if let Ok(store) = secrets(&app, &root) {
                let _ = store.delete(DEVICE_SECRET);
                let url = SyncStore::open(&root)
                    .map(|s| s.state.server_url)
                    .unwrap_or_default();
                forget_sessions(store.as_ref(), &url);
            }
            std::fs::remove_dir_all(SyncStore::dir_for(&root)).map_err(|e| err(e.to_string()))?;
            state.sync.set(None);
            let _ = app.emit("sync://status", serde_json::Value::Null);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_builds_sync_only_with_penroam_cloud() {
        assert_eq!(
            check_url_with("https://sync.penroam.ink/", false).unwrap(),
            DEFAULT_SERVER_URL
        );
        assert_eq!(
            check_url_with(" HTTPS://SYNC.PENROAM.INK ", false).unwrap(),
            DEFAULT_SERVER_URL
        );
        for other in [
            "https://sync-staging.penroam.ink",
            "http://127.0.0.1:8787",
            "https://sync.penroam.ink.evil.example",
        ] {
            assert!(check_url_with(other, false).is_err(), "{other}");
        }
    }

    #[test]
    fn debug_builds_accept_other_servers_but_not_plain_http_remotely() {
        assert!(check_url_with("http://127.0.0.1:8787", true).is_ok());
        assert!(check_url_with("https://sync-staging.penroam.ink", true).is_ok());
        assert!(check_url_with("http://sync.example.com", true).is_err());
    }
}

// ------------------------------------------------------- Penroam Cloud account --

/// Whether this build may show prices, purchase buttons or open billing pages. Store-distributed
/// mobile apps must not link to outside purchases, so the iOS and Android builds never do; the
/// account's plan and storage are still shown there. Enforced here, not only in the UI.
const PURCHASES_ALLOWED: bool = cfg!(desktop);

/// A paid plan, for the settings.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OfferView {
    id: String,
    name: String,
    price_label: String,
    quota_bytes: u64,
}

/// Plan and storage of the synced account (`account.v1`), for the settings.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    plan_name: String,
    /// `free`, `trialing`, `active`, `canceling`, `past_due`, `granted` or a newer value.
    plan_status: String,
    renews_at_ms: Option<u64>,
    ends_at_ms: Option<u64>,
    used_bytes: u64,
    quota_bytes: Option<u64>,
    free_quota_bytes: Option<u64>,
    writable: bool,
    read_only_reason: Option<String>,
    /// Empty when this build may not sell (mobile) or nothing is for sale.
    offers: Vec<OfferView>,
    can_manage_billing: bool,
    can_request_deletion: bool,
    can_cancel_deletion: bool,
    deletion_erase_after_ms: Option<u64>,
    /// This build may show purchases and billing links.
    purchases_allowed: bool,
}

fn ms(s: Option<u64>) -> Option<u64> {
    s.map(|s| s.saturating_mul(1000))
}

fn to_view(a: AccountState) -> AccountView {
    let sells = PURCHASES_ALLOWED && a.allows(actions::SUBSCRIBE);
    AccountView {
        plan_name: a.plan.name.clone(),
        plan_status: a.plan.status.clone(),
        renews_at_ms: ms(a.plan.renews_at_unix_s),
        ends_at_ms: ms(a.plan.ends_at_unix_s),
        used_bytes: a.storage.used_bytes,
        quota_bytes: a.storage.quota_bytes,
        free_quota_bytes: a.storage.free_quota_bytes,
        writable: a.writable,
        read_only_reason: a.read_only_reason.clone(),
        offers: if sells {
            a.offers
                .iter()
                .map(|o| OfferView {
                    id: o.id.clone(),
                    name: o.name.clone(),
                    price_label: o.price_label.clone(),
                    quota_bytes: o.quota_bytes,
                })
                .collect()
        } else {
            Vec::new()
        },
        can_manage_billing: PURCHASES_ALLOWED && a.allows(actions::MANAGE_BILLING),
        can_request_deletion: a.allows(actions::REQUEST_DELETION),
        can_cancel_deletion: a.allows(actions::CANCEL_DELETION),
        deletion_erase_after_ms: ms(a.deletion.as_ref().map(|d| d.erase_after_unix_s)),
        purchases_allowed: PURCHASES_ALLOWED,
    }
}

fn account_view(
    t: &mut HttpTransport,
) -> Result<Option<AccountView>, penroam_sync::TransportError> {
    if !t.meta()?.has_capability(capabilities::ACCOUNT_V1) {
        return Ok(None);
    }
    t.account_state().map(|a| Some(to_view(a)))
}

/// What the settings show for a failed account request: the server's safe message, without
/// network details in release builds.
fn account_error(e: &penroam_sync::TransportError) -> String {
    match e {
        penroam_sync::TransportError::Api(b) => b.message.clone(),
        other => unreachable_message(&other.to_string()),
    }
}

/// Plan and storage of the account this device syncs with; `None` when sync is not set up or
/// the server has no plans.
#[tauri::command(async)]
pub fn sync_account(state: State<'_, AppState>) -> Result<Option<AccountView>, CmdError> {
    if !state.sync.tx.lock().map(|g| g.is_some()).unwrap_or(false) {
        return Ok(None);
    }
    ask(&state.sync, Cmd::Account)
}

/// Opens the server's checkout (`subscribe` with an offer id) or billing portal
/// (`manage_billing`) in the system browser. Desktop builds only. The link is checked to be
/// `https://` by the transport before it is opened.
#[tauri::command(async)]
pub fn sync_open_billing(
    app: AppHandle,
    state: State<'_, AppState>,
    action: String,
    offer_id: Option<String>,
) -> Result<(), CmdError> {
    if !PURCHASES_ALLOWED {
        return Err(err("billing is managed outside this app"));
    }
    if action != actions::SUBSCRIBE && action != actions::MANAGE_BILLING {
        return Err(err("unknown billing action"));
    }
    let url = ask(&state.sync, |r| Cmd::BillingLink(action, offer_id, r))?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| err(format!("could not open the browser: {e}")))
}

/// When the cloud copy of a deleted account is erased.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionView {
    erase_after_ms: u64,
}

/// Asks the server to erase the account's cloud copy after its cooling-off period. Needs a
/// fresh sign-in in the browser (the server requires it); that one-off session is logged out
/// afterwards. Notes on this and every other device are not touched.
#[tauri::command(async)]
pub fn sync_request_account_deletion(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DeletionView, CmdError> {
    let root = library_root(&app);
    let synced = SyncStore::open(&root).map_err(|_| err("sync is not set up"))?;
    let url = synced.state.server_url.clone();
    if !uses_sign_in(&url).map_err(err)? {
        return Err(err("this server has no accounts to delete"));
    }
    let cfg = auth::auth_config(&url).map_err(|e| err(e.to_string()))?;
    let disc = auth::discover(&cfg.issuer).map_err(|e| err(e.to_string()))?;
    let id_token = browser_sign_in(&app, &cfg, &disc)?;
    let t = auth::session_from_id_token(&url, &id_token).map_err(|e| err(e.to_string()))?;
    let result = if t.account_id.as_str() != synced.state.account_id {
        Err(err(
            "that sign-in belongs to a different account than this device syncs with",
        ))
    } else {
        HttpTransport::new(&url, t.access_token.clone())
            .request_deletion()
            .map(|d| DeletionView {
                erase_after_ms: d.erase_after_unix_s.saturating_mul(1000),
            })
            .map_err(|e| err(account_error(&e)))
    };
    let _ = auth::logout(&url, &t.access_token);
    state.sync.nudge();
    result
}

/// Withdraws a pending account deletion.
#[tauri::command(async)]
pub fn sync_cancel_account_deletion(state: State<'_, AppState>) -> Result<(), CmdError> {
    ask(&state.sync, Cmd::CancelDeletion)
}
