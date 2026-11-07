//! Sync integration: a worker thread owns the sync
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
//!   worker trades it for a device session and then ends the account session:
//!   an account session can fetch the account key, and a removed device must
//!   not be left holding one. Refresh tokens rotate on every use. When the sign-in ends (refresh refused) the status becomes
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
//! * Commands that need the engine (devices, conflicts, vaults, …) are
//!   sent to the worker with a reply channel, so the engine is only ever used
//!   from one thread.
//! * `sync_disconnect` stops the worker and removes keys and sync metadata;
//!   notebooks are untouched.
//!
//! Adding a device takes a sign-in and nothing else ([`sync_connect`]). The
//! device names itself from its host name ([`device_label`]); the name can be
//! changed afterwards in the settings.
//!
//! Vaults (`penroam_sync::vault`): the worker owns one more engine per vault
//! unlocked here, and reconciles them after the main one. A vault's session
//! comes from the main session, so vaults stop with the sign-in.

use crate::commands::CmdError;
use crate::AppState;
use penroam_core::replica::{Conflict, ConflictTarget};
use penroam_core::{Notebook, Page};
use penroam_storage::ErrorCode;
use penroam_sync::auth;
use penroam_sync::crypto::RootSecret;
use penroam_sync::secrets::KeychainSecretStore;
use penroam_sync::store::SyncStore;
use penroam_sync::transport::HttpTransport;
use penroam_sync::vault::{self, NotebookOwners, Scope, VaultKey};
use penroam_sync::SyncTransport as _;
use penroam_sync::{
    DeviceView, ErasedNotebook, FileSecretStore, SecretStore, Setup, SyncEngine, SyncError,
    SyncStatus,
};
use penroam_sync_protocol::account::{actions, plan_managers, stores};
use penroam_sync_protocol::dto::capabilities;
use penroam_sync_protocol::{
    AccountId, AccountState, BillingLinkRequest, CreateVaultRequest, ErrorCode as WireCode,
    SessionTokens, StorePurchaseRequest, UpdateVaultLockRequest, VaultRecord,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

type Engine = SyncEngine<HttpTransport>;
type Reply<T> = Sender<Result<T, String>>;

const IDLE_INTERVAL: Duration = Duration::from_secs(60);
/// Replaces [`IDLE_INTERVAL`] while the OS battery saver is on. Local edits still sync at once.
const SAVER_INTERVAL: Duration = Duration::from_secs(300);
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
    PageVersions(Uuid, Uuid, Reply<Vec<penroam_sync::SyncedVersion>>),
    PageAt(Uuid, Uuid, u64, Reply<Option<Page>>),
    NotebookAt(Uuid, i64, Reply<Option<(Notebook, Vec<Page>)>>),
    SyncNow,
    Pause(bool),
    Status(Reply<SyncStatus>),
    Devices(Reply<Vec<DeviceView>>),
    Remove(String, Reply<()>),
    /// Device id and new label.
    Rename(String, String, Reply<()>),
    /// Notebooks in the account that were permanently deleted here.
    Erased(Reply<Vec<ErasedNotebook>>),
    RestoreErased(Uuid, Reply<()>),
    Conflicts(Uuid, Reply<Vec<Conflict>>),
    Resolve(Uuid, ConflictTarget, Uuid, Reply<()>),
    /// The account's vaults and which notebooks belong to them.
    Vaults(Reply<VaultsView>),
    /// Name and passphrase. Answers the new vault's recovery code.
    VaultCreate(String, String, Reply<String>),
    /// Vault id, then its passphrase or recovery code.
    VaultUnlock(String, Option<String>, Option<String>, Reply<()>),
    /// Vault id, its current passphrase or recovery code, and the new passphrase.
    VaultSetPassphrase(String, Option<String>, Option<String>, String, Reply<()>),
    /// Erases a vault's cloud copy.
    VaultDelete(String, Reply<()>),
    Diagnostics(Reply<String>),
    /// A new account session was stored: drop the current token and retry.
    Reauth,
    /// Plan and storage (`account.v1`); `None` when the server has no accounts.
    Account(Reply<Option<AccountView>>),
    /// A billing page URL (`account.v1`): action and offer id.
    BillingLink(String, Option<String>, Reply<String>),
    /// Reports a store transaction to the server (`store-purchases.v1`).
    StorePurchase(String, Reply<StoreReply>),
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
    /// Whether sync is set up and its worker is running.
    pub fn running(&self) -> bool {
        self.tx.lock().is_ok_and(|g| g.is_some())
    }

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

/// Versions of a page in the account's operation log. Empty when sync isn't running.
pub fn page_versions(
    state: &State<'_, AppState>,
    nb: Uuid,
    page: Uuid,
) -> Result<Vec<penroam_sync::SyncedVersion>, CmdError> {
    if !state.sync.running() {
        return Ok(Vec::new());
    }
    ask(&state.sync, |r| Cmd::PageVersions(nb, page, r))
}

/// A page as of a position in the account's operation log.
pub fn page_at(
    state: &State<'_, AppState>,
    nb: Uuid,
    page: Uuid,
    position: u64,
) -> Result<Option<Page>, CmdError> {
    ask(&state.sync, |r| Cmd::PageAt(nb, page, position, r))
}

/// A notebook and its pages as of a time, from the account's operation log.
pub fn notebook_at(
    state: &State<'_, AppState>,
    nb: Uuid,
    at: i64,
) -> Result<Option<(Notebook, Vec<Page>)>, CmdError> {
    ask(&state.sync, |r| Cmd::NotebookAt(nb, at, r))
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
pub(crate) fn secrets(
    app: &AppHandle,
    root: &std::path::Path,
) -> Result<Box<dyn SecretStore>, CmdError> {
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
/// refreshes, else a new device session signed with this device's key. Getting
/// one ends the account session, which from then on would only be something to
/// steal: it can fetch the account key, and a removed device keeps no way back
/// without a new sign-in. Test server: its stub.
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
            let _ = auth::logout(&url, &acct.access_token);
            let _ = store.delete(ACCOUNT_SESSION);
            Ok(t.access_token)
        }
        Err(_) => Ok(acct.access_token),
    }
}

/// One vault unlocked on this device.
struct VaultSlot {
    engine: Engine,
    have_token: bool,
}

/// The vault's id, as the protocol type.
fn vault_account(engine: &Engine) -> Result<AccountId, String> {
    engine
        .store
        .state
        .account_id
        .parse()
        .map_err(|_| "bad vault id".to_string())
}

/// A device session for a vault: the stored one if it refreshes, else a new one, by way of an
/// account session the main session asks for. `main` must hold a token.
fn vault_token(app: &AppHandle, main: &mut Engine, vault: &mut Engine) -> Result<String, String> {
    let (url, device) = {
        let st = &vault.store.state;
        (st.server_url.clone(), st.device_id.clone())
    };
    let id = vault_account(vault)?;
    if !uses_sign_in(&url)? {
        return test_token(&url, id.as_str(), Some(&device));
    }
    let root = library_root(app);
    let store = secrets(app, &root).map_err(|e| e.message)?;
    let name = vault.scope().secret_name(DEVICE_SESSION);
    if let Some(d) = load_session(store.as_ref(), &name, &url) {
        match auth::refresh(&url, &d.refresh_token) {
            Ok(t) => {
                save_session(store.as_ref(), &name, &url, &t)?;
                return Ok(t.access_token);
            }
            Err(auth::AuthError::Transport(penroam_sync::TransportError::Network(e))) => {
                return Err(e)
            }
            Err(_) => {
                let _ = store.delete(&name);
            }
        }
    }
    let account = main
        .transport_mut()
        .vault_session(&id)
        .map_err(|e| e.to_string())?;
    let t = vault
        .device_session(&url, &account.access_token)
        .map_err(|e| e.to_string())?;
    let _ = auth::logout(&url, &account.access_token);
    save_session(store.as_ref(), &name, &url, &t)?;
    Ok(t.access_token)
}

/// Engines of the vaults unlocked on this device. One that can't open is left out for this run:
/// its notebooks then just don't sync, and they can't reach the main account either.
fn open_vault_slots(app: &AppHandle) -> Vec<VaultSlot> {
    let root = library_root(app);
    let mut out = Vec::new();
    for id in vault::local_vaults(&root) {
        let Ok(scope) = Scope::vault(&id) else {
            continue;
        };
        let opened = SyncStore::open_at(&scope.dir(&root))
            .map_err(|e| e.to_string())
            .and_then(|s| {
                let store = secrets(app, &root).map_err(|e| e.message)?;
                Engine::open_scope(
                    &root,
                    scope.clone(),
                    HttpTransport::new(&s.state.server_url, String::new()),
                    store,
                )
                .map_err(|e| e.to_string())
            });
        match opened {
            Ok(engine) => out.push(VaultSlot {
                engine,
                have_token: false,
            }),
            Err(e) => eprintln!("penroam: a vault was not started: {e}"),
        }
    }
    out
}

/// The account's vaults, as the settings show them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultsView {
    /// Whether this server has vaults (`vaults.v1`).
    supported: bool,
    vaults: Vec<VaultView>,
    /// Notebook → vault id, for every notebook kept out of the main account.
    notebooks: BTreeMap<Uuid, String>,
}

/// One vault.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultView {
    vault_id: String,
    /// The name; unknown until the vault was unlocked on this device.
    name: Option<String>,
    unlocked: bool,
    /// Why this vault isn't syncing, if it isn't.
    problem: Option<String>,
}

/// Runs `f` under the library lock with the owners file loaded, and saves what it changed.
fn edit_owners(app: &AppHandle, f: impl FnOnce(&mut NotebookOwners)) -> Result<(), String> {
    let state = app.state::<AppState>();
    let lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
    let root = lib.root().to_path_buf();
    let mut owners = NotebookOwners::load(&root).map_err(|e| e.to_string())?;
    f(&mut owners);
    owners.save(&root).map_err(|e| e.to_string())
}

fn vault_setup(app: &AppHandle, main: &Engine, id: &str) -> Result<Setup, String> {
    let st = &main.store.state;
    Ok(Setup {
        library_root: library_root(app),
        scope: Scope::vault(id).map_err(|e| e.to_string())?,
        device_id: Some(st.device_id.clone()),
        server_url: st.server_url.clone(),
        account_id: id.to_string(),
        label: st.label.clone(),
    })
}

/// A transport for a vault's own account, from a session the main session asks for.
fn vault_transport(main: &mut Engine, id: &AccountId) -> Result<HttpTransport, String> {
    let t = main
        .transport_mut()
        .vault_session(id)
        .map_err(|e| e.to_string())?;
    Ok(HttpTransport::new(
        &main.store.state.server_url,
        t.access_token,
    ))
}

fn checked_vault_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err("give the vault a name of at most 100 characters".into());
    }
    Ok(name)
}

/// Creates a vault and its engine. A vault whose setup fails is removed from the server again, or
/// it would exist without the user ever having seen its recovery code.
fn create_vault(
    app: &AppHandle,
    main: &mut Engine,
    name: &str,
    passphrase: &str,
) -> Result<(VaultSlot, String), String> {
    let name = checked_vault_name(name)?;
    let root = RootSecret::generate().map_err(|e| e.to_string())?;
    let lock = vault::new_lock(&root, passphrase).map_err(|e| e.to_string())?;
    let record = main
        .transport_mut()
        .vault_create(&CreateVaultRequest {
            lock,
            name: vault::seal_name(&root, name),
        })
        .map_err(|e| e.to_string())?;
    let id = record.vault_id.clone();
    let opened = (|| {
        let setup = vault_setup(app, main, id.as_str())?;
        let t = vault_transport(main, &id)?;
        let store = secrets(app, &library_root(app)).map_err(|e| e.message)?;
        Engine::open_vault(setup, t, store, &root).map_err(|e| e.to_string())
    })();
    match opened {
        Ok(engine) => {
            edit_owners(app, |o| {
                o.names.insert(id.to_string(), name.to_string());
            })?;
            Ok((
                VaultSlot {
                    engine,
                    have_token: false,
                },
                root.recovery_code(),
            ))
        }
        Err(e) => {
            let _ = main.transport_mut().vault_delete(&id);
            Err(e)
        }
    }
}

fn vault_key<'a>(
    passphrase: &'a Option<String>,
    recovery: &'a Option<String>,
) -> Result<VaultKey<'a>, String> {
    match (passphrase, recovery) {
        (Some(p), None) => Ok(VaultKey::Passphrase(p)),
        (None, Some(c)) => Ok(VaultKey::RecoveryCode(c)),
        _ => Err("enter the vault's passphrase or recovery code".into()),
    }
}

fn find_vault(main: &mut Engine, id: &str) -> Result<VaultRecord, String> {
    main.transport_mut()
        .vault_list()
        .map_err(|e| e.to_string())?
        .vaults
        .into_iter()
        .find(|v| v.vault_id.as_str() == id)
        .ok_or_else(|| "that vault no longer exists".to_string())
}

/// Opens a vault on this device. The key log proves the root is right before anything is
/// trusted; only then is its name read.
fn unlock_vault(
    app: &AppHandle,
    main: &mut Engine,
    id: &str,
    key: &VaultKey<'_>,
) -> Result<VaultSlot, String> {
    let record = find_vault(main, id)?;
    let root = vault::root_for(&record.lock, key).map_err(|e| e.to_string())?;
    let setup = vault_setup(app, main, id)?;
    let t = vault_transport(main, &record.vault_id)?;
    let store = secrets(app, &library_root(app)).map_err(|e| e.message)?;
    let engine = Engine::open_vault(setup, t, store, &root).map_err(|e| e.to_string())?;
    if let Ok(name) = vault::open_name(&root, &record.name) {
        edit_owners(app, |o| {
            o.names.insert(id.to_string(), name);
        })?;
    }
    Ok(VaultSlot {
        engine,
        have_token: false,
    })
}

/// The server's vaults, with what this device knows about each. A vault with an engine here that
/// the server no longer lists was deleted elsewhere: its engine stops, and its notebooks stay on
/// this device, unsynced (they remain assigned, so the main account can't pick them up).
fn vaults_view(
    app: &AppHandle,
    main: &mut Engine,
    slots: &mut Vec<VaultSlot>,
) -> Result<VaultsView, String> {
    let url = main.store.state.server_url.clone();
    let supported = HttpTransport::new(&url, String::new())
        .meta()
        .map(|m| m.has_capability(capabilities::VAULTS_V1))
        .map_err(|e| unreachable_message(&e.to_string()))?;
    let records = if supported {
        main.transport_mut()
            .vault_list()
            .map_err(|e| e.to_string())?
            .vaults
    } else {
        Vec::new()
    };
    if supported {
        let gone: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                !records
                    .iter()
                    .any(|r| r.vault_id.as_str() == s.engine.store.state.account_id)
            })
            .map(|(i, _)| i)
            .collect();
        for i in gone.into_iter().rev() {
            let s = slots.remove(i);
            if let Err(e) = s.engine.disconnect() {
                eprintln!("penroam: a deleted vault was not cleaned up: {e}");
            }
        }
    }
    let owners = NotebookOwners::load(&library_root(app)).map_err(|e| e.to_string())?;
    let vaults = records
        .iter()
        .map(|r| {
            let slot = slots
                .iter()
                .find(|s| s.engine.store.state.account_id == r.vault_id.as_str());
            VaultView {
                vault_id: r.vault_id.to_string(),
                name: owners.names.get(r.vault_id.as_str()).cloned(),
                unlocked: slot.is_some(),
                problem: slot.and_then(|s| s.engine.status(0).last_error),
            }
        })
        .collect();
    Ok(VaultsView {
        supported,
        vaults,
        notebooks: owners.notebooks,
    })
}

/// The name this device goes by until the user changes it: its host name, or on Android, where
/// that is usually `localhost`, its model.
pub fn device_label() -> String {
    #[cfg(target_os = "android")]
    let raw = std::process::Command::new("getprop")
        .arg("ro.product.model")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    #[cfg(not(target_os = "android"))]
    let raw = gethostname::gethostname().to_string_lossy().into_owned();
    label_from_host(&raw).unwrap_or_else(|| fallback_label().to_string())
}

/// A usable label from a host name, without a `.local` or other domain suffix.
fn label_from_host(raw: &str) -> Option<String> {
    let name = raw.trim();
    let name = name.split('.').next().unwrap_or(name).trim();
    if name.is_empty() || name.eq_ignore_ascii_case("localhost") {
        return None;
    }
    Some(name.chars().take(64).collect())
}

fn fallback_label() -> &'static str {
    if cfg!(target_os = "android") {
        "Android device"
    } else if cfg!(target_os = "ios") {
        "iPad"
    } else if cfg!(target_os = "macos") {
        "Mac"
    } else if cfg!(target_os = "windows") {
        "Windows PC"
    } else {
        "Linux computer"
    }
}

/// Best-effort logout of every stored session for `url`, then removal from
/// the keychain.
fn forget_sessions(store: &dyn SecretStore, url: &str) {
    for name in [DEVICE_SESSION, ACCOUNT_SESSION] {
        forget_session(store, name, url);
    }
}

fn forget_session(store: &dyn SecretStore, name: &str, url: &str) {
    if let Some(s) = load_session(store, name, url) {
        if let Ok(t) = auth::refresh(url, &s.refresh_token) {
            let _ = auth::logout(url, &t.access_token);
        }
    }
    let _ = store.delete(name);
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
    let mut slots = open_vault_slots(&app);
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
        let idle = if crate::power::saver() {
            SAVER_INTERVAL
        } else {
            IDLE_INTERVAL
        };
        let wait = due.map_or(idle, |d| d.saturating_duration_since(Instant::now()));
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
            Ok(Cmd::Remove(id, r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                // Each vault first: the device's vault memberships use its main device id, and
                // removing it from a vault rotates that vault's key too. A vault not open here
                // is left to the server, which revokes the same id there.
                for s in &mut slots {
                    if !s
                        .engine
                        .devices()
                        .iter()
                        .any(|d| d.device_id == id && d.active)
                    {
                        continue;
                    }
                    match vault_token(&app, &mut engine, &mut s.engine) {
                        Ok(t) => {
                            s.engine.transport_mut().set_token(t);
                            s.have_token = true;
                            if let Err(e) = s.engine.remove_device(&id) {
                                eprintln!("penroam: removal from a vault failed: {e}");
                            }
                        }
                        Err(e) => eprintln!("penroam: removal from a vault failed: {e}"),
                    }
                }
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
            Ok(Cmd::Rename(id, label, r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let mut res = engine.rename_device(&id, &label);
                if res.as_ref().is_err_and(unauth) {
                    have_token = false;
                    if ensure(&mut engine, &mut have_token, &mut signed_out) {
                        res = engine.rename_device(&id, &label);
                    }
                }
                let _ = r.send(res.map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::Erased(r)) => {
                let res = {
                    let state = app.state::<AppState>();
                    let lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
                    let mut all = engine.erased_notebooks(&lib);
                    for s in &mut slots {
                        if let (Ok(a), Ok(more)) = (&mut all, s.engine.erased_notebooks(&lib)) {
                            a.extend(more);
                        }
                    }
                    all
                };
                let _ = r.send(res.map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::RestoreErased(nb, r)) => {
                let res = {
                    let state = app.state::<AppState>();
                    let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
                    engine_for(&app, nb, &mut engine, &mut slots).restore_erased(&mut lib, nb)
                };
                let _ = r.send(res.map_err(|e| e.to_string()));
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::PageVersions(nb, page, r)) => {
                let e = engine_for(&app, nb, &mut engine, &mut slots);
                let _ = r.send(e.page_versions(nb, page).map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::PageAt(nb, page, position, r)) => {
                let e = engine_for(&app, nb, &mut engine, &mut slots);
                let _ = r.send(e.page_at(nb, page, position).map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::NotebookAt(nb, at, r)) => {
                let e = engine_for(&app, nb, &mut engine, &mut slots);
                let _ = r.send(e.notebook_at(nb, at).map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::Conflicts(nb, r)) => {
                let e = engine_for(&app, nb, &mut engine, &mut slots);
                let _ = r.send(e.conflicts(nb).map_err(|e| e.to_string()));
                continue;
            }
            Ok(Cmd::Resolve(nb, target, choose, r)) => {
                let res = {
                    let state = app.state::<AppState>();
                    let mut lib = state.lib.lock().unwrap_or_else(|p| p.into_inner());
                    engine_for(&app, nb, &mut engine, &mut slots)
                        .resolve_conflict(&mut lib, nb, &target, choose)
                };
                let _ = r.send(res.map_err(|e| e.to_string()));
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::Vaults(r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let _ = r.send(vaults_view(&app, &mut engine, &mut slots));
                continue;
            }
            Ok(Cmd::VaultCreate(name, passphrase, r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let res = create_vault(&app, &mut engine, &name, &passphrase).map(|(s, code)| {
                    slots.push(s);
                    code
                });
                let _ = r.send(res);
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::VaultUnlock(id, passphrase, recovery, r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let res = vault_key(&passphrase, &recovery)
                    .and_then(|k| unlock_vault(&app, &mut engine, &id, &k))
                    .map(|s| slots.push(s));
                let _ = r.send(res);
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::VaultSetPassphrase(id, passphrase, recovery, new, r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let res = (|| {
                    let key = vault_key(&passphrase, &recovery)?;
                    let record = find_vault(&mut engine, &id)?;
                    let root = vault::root_for(&record.lock, &key).map_err(|e| e.to_string())?;
                    // a mistyped recovery code must not lock the vault for good
                    if !matches!(key, VaultKey::Passphrase(_)) {
                        let slot = slots
                            .iter()
                            .find(|s| s.engine.store.state.account_id == id)
                            .ok_or("unlock this vault here first")?;
                        if !slot.engine.account_key_is(&root) {
                            return Err("that recovery code isn't this vault's".to_string());
                        }
                    }
                    let lock = vault::new_lock(&root, &new).map_err(|e| e.to_string())?;
                    engine
                        .transport_mut()
                        .vault_set_lock(&record.vault_id, &UpdateVaultLockRequest { lock })
                        .map_err(|e| e.to_string())
                })();
                let _ = r.send(res);
                continue;
            }
            Ok(Cmd::VaultDelete(id, r)) => {
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let res = id
                    .parse::<AccountId>()
                    .map_err(|_| "bad vault id".to_string())
                    .and_then(|a| {
                        engine
                            .transport_mut()
                            .vault_delete(&a)
                            .map_err(|e| e.to_string())
                    })
                    .map(|()| {
                        if let Some(i) = slots
                            .iter()
                            .position(|s| s.engine.store.state.account_id == id)
                        {
                            if let Err(e) = slots.remove(i).engine.disconnect() {
                                eprintln!("penroam: a deleted vault was not cleaned up: {e}");
                            }
                        }
                    });
                let _ = r.send(res);
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
            Ok(Cmd::StorePurchase(transaction, r)) => {
                let req = StorePurchaseRequest {
                    store: stores::APP_STORE.into(),
                    transaction,
                };
                ensure(&mut engine, &mut have_token, &mut signed_out);
                let mut res = engine.transport_mut().store_purchase(&req);
                if res.as_ref().is_err_and(is_unauthenticated) {
                    have_token = false;
                    if ensure(&mut engine, &mut have_token, &mut signed_out) {
                        res = engine.transport_mut().store_purchase(&req);
                    }
                }
                let _ = r.send(Ok(store_reply(res)));
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
                for s in &mut slots {
                    s.have_token = false;
                }
                due = Some(Instant::now());
                continue;
            }
            Ok(Cmd::Disconnect(r)) => {
                let root = library_root(&app);
                if let Ok(store) = secrets(&app, &root) {
                    let url = engine.store.state.server_url.clone();
                    for s in &slots {
                        let name = s.engine.scope().secret_name(DEVICE_SESSION);
                        forget_session(store.as_ref(), &name, &url);
                    }
                    forget_sessions(store.as_ref(), &url);
                }
                for s in slots.drain(..) {
                    if let Err(e) = s.engine.disconnect() {
                        eprintln!("penroam: a vault was not cleaned up: {e}");
                    }
                }
                let _ = r.send(engine.disconnect().map_err(|e| e.to_string()));
                app.state::<AppState>().sync.set(None);
                let _ = app.emit("sync://status", serde_json::Value::Null);
                return;
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // reconcile
        if !ensure(&mut engine, &mut have_token, &mut signed_out) && !signed_out {
            due = Some(Instant::now() + Duration::from_secs(30));
            emit_status(&app, &engine);
            continue;
        }
        let state = app.state::<AppState>();
        let result = engine.reconcile(&state.lib);
        // Vaults after the main stream, whatever became of it. Each failure stays with its vault
        // (shown in the settings), and the next pass retries it.
        if result.is_ok() {
            for s in &mut slots {
                if !s.have_token {
                    match vault_token(&app, &mut engine, &mut s.engine) {
                        Ok(t) => {
                            s.engine.transport_mut().set_token(t);
                            s.have_token = true;
                        }
                        Err(e) => {
                            eprintln!("penroam: no session for a vault: {e}");
                            continue;
                        }
                    }
                }
                match s.engine.reconcile(&state.lib) {
                    Ok(report) => announce(&app, &report),
                    Err(e) if unauth(&e) => s.have_token = false,
                    Err(_) => {}
                }
            }
        }
        match &result {
            Ok(report) => {
                announce(&app, report);
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

/// Re-indexes what a reconcile changed, and tells the editor.
fn announce(app: &AppHandle, report: &penroam_sync::ReconcileReport) {
    let state = app.state::<AppState>();
    // remote changes: re-index only the pages that were reinstalled
    let changed: Vec<Uuid> = report
        .installed_notebooks
        .iter()
        .chain(&report.erased_notebooks)
        .copied()
        .collect();
    for nb in &changed {
        state.search.notebook_changed(*nb);
    }
    for (nb, page) in &report.installed_pages {
        state.search.page_changed(*nb, *page);
    }
    if !changed.is_empty() {
        let _ = app.emit(
            "sync://changed",
            Changed {
                notebook_ids: changed,
                pages: report.installed_pages.clone(),
            },
        );
    }
}

/// The engine that syncs a notebook.
fn engine_for<'a>(
    app: &AppHandle,
    nb: Uuid,
    main: &'a mut Engine,
    slots: &'a mut [VaultSlot],
) -> &'a mut Engine {
    let root = library_root(app);
    if let Ok(owners) = NotebookOwners::load(&root) {
        if let Some(id) = owners.owner(&nb) {
            if let Some(s) = slots
                .iter_mut()
                .find(|s| s.engine.store.state.account_id == id)
            {
                return &mut s.engine;
            }
        }
    }
    main
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
    /// Always true from this backend. The frontend also runs with no backend (tests, a plain
    /// browser), where it treats sync as unavailable.
    available: bool,
    configured: bool,
    status: Option<SyncStatus>,
    /// The server new setups use unless developer mode picks another.
    default_server_url: &'static str,
    /// Debug build: the settings may offer developer mode (custom servers).
    dev_build: bool,
}

/// Whether sync is configured, and its status.
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
        available: true,
        configured,
        status,
        default_server_url: DEFAULT_SERVER_URL,
        dev_build: CUSTOM_SERVERS,
    })
}

/// Setup precondition: not already set up.
fn guard(state: &State<'_, AppState>) -> Result<(), CmdError> {
    if state.sync.tx.lock().map(|g| g.is_some()).unwrap_or(false) {
        return Err(err("sync is already set up; disconnect first"));
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
/// How connecting went.
pub struct ConnectResult {
    /// The account's keys aren't on the server (it predates sign-in-only sync): connect again
    /// with its recovery code, once.
    needs_recovery_code: bool,
}

/// Connects this device to the signed-in account. That's all it takes, on the first device and
/// on every later one. `recovery_code` is for accounts that need it ([`ConnectResult`]).
#[tauri::command(async)]
pub fn sync_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    account: String,
    recovery_code: Option<String>,
) -> Result<ConnectResult, CmdError> {
    guard(&state)?;
    let url = check_url(&server_url)?;
    let root = library_root(&app);
    let store = secrets(&app, &root)?;
    let (token, account) = account_access(store.as_ref(), &url, &account)?;
    let t = HttpTransport::new(&url, token);
    let setup = Setup {
        library_root: root.clone(),
        scope: Scope::Account,
        device_id: None,
        server_url: url,
        account_id: account,
        label: device_label(),
    };
    let engine = match recovery_code {
        Some(code) => Engine::enroll_with_recovery(setup, t, store, &code),
        None => Engine::connect(setup, t, store),
    };
    match engine {
        Ok(e) => {
            start(&app, e);
            Ok(ConnectResult {
                needs_recovery_code: false,
            })
        }
        Err(SyncError::NeedsRecoveryCode) => Ok(ConnectResult {
            needs_recovery_code: true,
        }),
        Err(e) => Err(sync_err(e)),
    }
}

/// The account's vaults and which notebooks belong to them.
#[tauri::command(async)]
pub fn sync_vaults(state: State<'_, AppState>) -> Result<VaultsView, CmdError> {
    ask(&state.sync, Cmd::Vaults)
}

/// Creates a vault; returns its recovery code, which is shown once.
#[tauri::command(async)]
pub fn sync_vault_create(
    state: State<'_, AppState>,
    name: String,
    passphrase: String,
) -> Result<String, CmdError> {
    ask(&state.sync, |r| Cmd::VaultCreate(name, passphrase, r))
}

/// Unlocks a vault on this device with its passphrase or its recovery code. Once is enough.
#[tauri::command(async)]
pub fn sync_vault_unlock(
    state: State<'_, AppState>,
    vault_id: String,
    passphrase: Option<String>,
    recovery_code: Option<String>,
) -> Result<(), CmdError> {
    ask(&state.sync, |r| {
        Cmd::VaultUnlock(vault_id, passphrase, recovery_code, r)
    })
}

/// Sets a vault's passphrase, proving knowledge of the old one or the recovery code.
#[tauri::command(async)]
pub fn sync_vault_set_passphrase(
    state: State<'_, AppState>,
    vault_id: String,
    passphrase: Option<String>,
    recovery_code: Option<String>,
    new_passphrase: String,
) -> Result<(), CmdError> {
    ask(&state.sync, |r| {
        Cmd::VaultSetPassphrase(vault_id, passphrase, recovery_code, new_passphrase, r)
    })
}

/// Erases a vault's cloud copy. Its notebooks stay on this device and stop syncing.
#[tauri::command(async)]
pub fn sync_vault_delete(state: State<'_, AppState>, vault_id: String) -> Result<(), CmdError> {
    ask(&state.sync, |r| Cmd::VaultDelete(vault_id, r))
}

/// Whether `vault` is unlocked on this device, so notebooks can go into it. Call with the library
/// lock held, the lock assignments are made under.
pub fn require_open_vault(root: &Path, vault: &str) -> Result<(), CmdError> {
    if vault::local_vaults(root).iter().any(|v| v == vault) {
        Ok(())
    } else {
        Err(err("unlock this vault on this device first"))
    }
}

/// Assigns a notebook to a vault. Call with the library lock held, in the same critical section
/// that created the notebook: once unlocked, the main engine would upload it.
pub fn assign_to_vault(root: &Path, notebook: Uuid, vault: &str) -> Result<(), CmdError> {
    let mut owners = NotebookOwners::load(root).map_err(|e| err(e.to_string()))?;
    owners.notebooks.insert(notebook, vault.to_string());
    owners.save(root).map_err(|e| err(e.to_string()))
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

/// Removes a device (new key epoch, server revocation).
#[tauri::command(async)]
pub fn sync_remove_device(state: State<'_, AppState>, device_id: String) -> Result<(), CmdError> {
    ask(&state.sync, |r| Cmd::Remove(device_id, r))
}

/// Renames a device for every device on the account.
#[tauri::command(async)]
pub fn sync_rename_device(
    state: State<'_, AppState>,
    device_id: String,
    label: String,
) -> Result<(), CmdError> {
    ask(&state.sync, |r| Cmd::Rename(device_id, label, r))
}

/// Notebooks the account holds that are not on this device.
#[tauri::command(async)]
pub fn sync_erased_notebooks(state: State<'_, AppState>) -> Result<Vec<ErasedNotebook>, CmdError> {
    ask(&state.sync, Cmd::Erased)
}

/// Puts a notebook from `sync_erased_notebooks` back on this device.
#[tauri::command(async)]
pub fn sync_restore_erased(state: State<'_, AppState>, notebook_id: Uuid) -> Result<(), CmdError> {
    ask(&state.sync, |r| Cmd::RestoreErased(notebook_id, r))
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
                for id in vault::local_vaults(&root) {
                    let scope = Scope::Vault(id);
                    let _ = store.delete(&scope.secret_name(DEVICE_SECRET));
                    let _ = store.delete(&scope.secret_name(DEVICE_SESSION));
                }
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
mod label_tests {
    use super::label_from_host;

    #[test]
    fn a_device_is_named_after_its_host_without_the_domain() {
        assert_eq!(
            label_from_host("noah-laptop\n").as_deref(),
            Some("noah-laptop")
        );
        assert_eq!(
            label_from_host("Noahs-iPad.local").as_deref(),
            Some("Noahs-iPad")
        );
    }

    #[test]
    fn a_host_name_that_says_nothing_is_not_used() {
        assert_eq!(label_from_host("localhost"), None);
        assert_eq!(label_from_host(" "), None);
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use std::cell::RefCell;

    fn txs(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// Runs `settle` with one scripted server answer per transaction.
    fn run(ids: &[&str], answers: Vec<Result<StoreReply, String>>) -> (StoreSettled, Vec<String>) {
        let finished = RefCell::new(Vec::new());
        let mut answers = answers.into_iter();
        let out = settle(
            &txs(ids),
            |_| answers.next().unwrap(),
            |t| finished.borrow_mut().push(t.to_string()),
        );
        (out, finished.into_inner())
    }

    #[test]
    fn only_what_the_server_has_is_finished() {
        let (out, finished) = run(&["1"], vec![Ok(StoreReply::Applied)]);
        assert_eq!(finished, ["1"]);
        assert_eq!(out.applied, 1);
    }

    #[test]
    fn an_outage_keeps_the_transaction_for_later() {
        let (out, finished) = run(&["1"], vec![Ok(StoreReply::Retry("try later".into()))]);
        assert!(
            finished.is_empty(),
            "finishing here would lose the purchase"
        );
        assert_eq!(
            (out.waiting, out.message.as_deref()),
            (1, Some("try later"))
        );
    }

    #[test]
    fn a_stuck_sync_worker_keeps_the_transaction_too() {
        let (out, finished) = run(&["1"], vec![Err("the sync worker did not answer".into())]);
        assert!(finished.is_empty());
        assert_eq!(out.waiting, 1);
    }

    #[test]
    fn a_purchase_owned_elsewhere_is_finished_and_explained() {
        let (out, finished) = run(
            &["1"],
            vec![Ok(StoreReply::Conflict("another account".into()))],
        );
        assert_eq!(finished, ["1"], "or it would come back forever");
        assert_eq!(out.conflicts, 1);
        assert_eq!(out.message.as_deref(), Some("another account"));
    }

    #[test]
    fn a_batch_settles_each_transaction_on_its_own() {
        let (out, finished) = run(
            &["1", "2", "3"],
            vec![
                Ok(StoreReply::Applied),
                Ok(StoreReply::Retry("later".into())),
                Ok(StoreReply::Applied),
            ],
        );
        assert_eq!(finished, ["1", "3"]);
        assert_eq!((out.applied, out.waiting), (2, 1));
    }

    fn account() -> AccountState {
        serde_json::from_value(serde_json::json!({
            "account_id": "acct-1",
            "plan": {"id": "free", "name": "Free", "status": "free"},
            "storage": {"used_bytes": 0, "quota_bytes": 100},
            "writable": true,
            "offers": [
                {"id": "stripe-only", "name": "A", "price_label": "$1", "quota_bytes": 5},
                {"id": "both", "name": "B", "price_label": "$2", "quota_bytes": 5,
                 "store_products": {"apple_product_id": "p.monthly"}},
            ],
            "actions": ["subscribe", "subscribe_in_store", "manage_billing"],
            "store_account_token": "3f2c1b7e-8d4a-4f6e-9b21-0c7d5e1a2b3c", // gitleaks:allow (made-up test value)
        }))
        .unwrap()
    }

    #[test]
    fn a_store_build_never_sees_stripe_offers_or_billing_links() {
        let v = to_view_for(account(), false, true);
        assert!(v.offers.is_empty() && !v.can_manage_billing);
        assert_eq!(v.store_offers.len(), 1, "only offers sold in the store");
        assert_eq!(v.store_offers[0].product_id, "p.monthly");
    }

    #[test]
    fn a_desktop_build_never_sees_store_offers() {
        let v = to_view_for(account(), true, false);
        assert_eq!(v.offers.len(), 2);
        assert!(v.store_offers.is_empty());
    }

    #[test]
    fn a_store_plan_isnt_managed_on_the_billing_pages() {
        let mut a = account();
        a.plan.managed_in = Some(plan_managers::APP_STORE.into());
        let v = to_view_for(a, true, false);
        assert!(!v.can_manage_billing);
        assert_eq!(v.managed_in.as_deref(), Some("app_store"));
    }

    #[test]
    fn the_account_token_stays_out_of_the_webview() {
        let v = to_view_for(account(), false, true);
        assert!(v.store_account_token.is_some());
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("3f2c1b7e") && !json.contains("p.monthly"));
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

/// Whether this build may show Stripe prices or open billing pages. Store-distributed mobile apps
/// must not link to outside purchases, so the iOS and Android builds never do. Enforced here, not
/// only in the UI.
const PURCHASES_ALLOWED: bool = cfg!(desktop);

/// Whether this build sells plans through its app store instead.
const STORE_PURCHASES: bool = tauri_plugin_store_billing::AVAILABLE;

/// A paid plan, for the settings.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OfferView {
    id: String,
    name: String,
    price_label: String,
    quota_bytes: u64,
}

/// A paid plan sold through the app store, for the settings. The store's price, never the
/// server's label: it's what the user is charged.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StoreOfferView {
    id: String,
    name: String,
    quota_bytes: u64,
    /// Filled in from the store by [`fill_store_prices`].
    price: String,
    /// `month`, `year` or similar, from the store.
    period: Option<String>,
    #[serde(skip)]
    product_id: String,
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
    /// This build may show Stripe purchases and billing links.
    purchases_allowed: bool,
    /// This build sells through its app store. The Restore Purchases button depends on this and
    /// not on [`Self::store_offers`], because it's needed most when there's nothing to sell.
    store_purchases: bool,
    /// Plans this account can buy in the app store. Empty if the store doesn't know them.
    store_offers: Vec<StoreOfferView>,
    /// Where the plan is managed: `web`, `app_store`, `google_play` or a newer value.
    managed_in: Option<String>,
    /// Attached to store purchases. Stays in the backend.
    #[serde(skip)]
    store_account_token: Option<String>,
}

fn ms(s: Option<u64>) -> Option<u64> {
    s.map(|s| s.saturating_mul(1000))
}

fn to_view(a: AccountState) -> AccountView {
    to_view_for(a, PURCHASES_ALLOWED, STORE_PURCHASES)
}

/// Builds the view for a build with these abilities, so that both kinds can be tested.
fn to_view_for(a: AccountState, purchases: bool, store: bool) -> AccountView {
    let sells = purchases && a.allows(actions::SUBSCRIBE);
    let sells_in_store = store && a.allows(actions::SUBSCRIBE_IN_STORE);
    // A store plan can't be managed on the server's billing pages, whatever else exists.
    let managed_on_web = a
        .plan
        .managed_in
        .as_deref()
        .is_none_or(|m| m == plan_managers::WEB);
    let store_offers = if sells_in_store {
        a.offers
            .iter()
            .filter_map(|o| {
                let product_id = o.store_products.as_ref()?.apple_product_id.clone()?;
                Some(StoreOfferView {
                    id: o.id.clone(),
                    name: o.name.clone(),
                    quota_bytes: o.quota_bytes,
                    price: String::new(),
                    period: None,
                    product_id,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
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
        can_manage_billing: purchases && managed_on_web && a.allows(actions::MANAGE_BILLING),
        can_request_deletion: a.allows(actions::REQUEST_DELETION),
        can_cancel_deletion: a.allows(actions::CANCEL_DELETION),
        deletion_erase_after_ms: ms(a.deletion.as_ref().map(|d| d.erase_after_unix_s)),
        purchases_allowed: purchases,
        store_purchases: store,
        store_offers,
        managed_in: a.plan.managed_in.clone(),
        store_account_token: a.store_account_token.clone(),
    }
}

/// Replaces the offers with what the store can actually sell here, at its prices. An offer the
/// store doesn't know disappears instead of showing with no price, and a store that can't be
/// reached shows no plans at all, rather than a plan that can't be bought.
fn fill_store_prices(app: &AppHandle, view: &mut AccountView) {
    if view.store_offers.is_empty() {
        return;
    }
    let ids: Vec<String> = view
        .store_offers
        .iter()
        .map(|o| o.product_id.clone())
        .collect();
    let found = tauri_plugin_store_billing::products(app, &ids).unwrap_or_default();
    view.store_offers.retain_mut(|o| {
        let Some(p) = found.iter().find(|p| p.id == o.product_id) else {
            return false;
        };
        o.price = p.display_price.clone();
        o.period = p.period.clone();
        true
    });
}

/// What the server said about a store transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreReply {
    /// The purchase is on this account.
    Applied,
    /// It belongs to another account. Holds the server's message.
    Conflict(String),
    /// The server doesn't have it yet, for any other reason. Holds a message.
    Retry(String),
}

fn store_reply(res: Result<AccountState, penroam_sync::TransportError>) -> StoreReply {
    match res {
        Ok(_) => StoreReply::Applied,
        Err(penroam_sync::TransportError::Api(b)) if b.code == WireCode::Conflict => {
            StoreReply::Conflict(b.message)
        }
        Err(e) => StoreReply::Retry(account_error(&e)),
    }
}

/// How a batch of store transactions ended.
#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoreSettled {
    /// Now on this account.
    applied: u32,
    /// Owned by another account.
    conflicts: u32,
    /// Not delivered yet, and kept to try again.
    waiting: u32,
    /// The last thing worth telling the user.
    message: Option<String>,
}

/// Reports `transactions` and finishes the ones that are dealt with. **This is the rule that keeps
/// a purchase from being lost:** a transaction is finished only once the server has it, or has
/// said another account does. On any other answer it stays unfinished, and the store delivers it
/// again. Finishing a conflict is safe, because the plan stays with the account that owns it.
fn settle(
    transactions: &[String],
    mut report: impl FnMut(&str) -> Result<StoreReply, String>,
    mut finish: impl FnMut(&str),
) -> StoreSettled {
    let mut out = StoreSettled::default();
    for t in transactions {
        match report(t) {
            Ok(StoreReply::Applied) => {
                finish(t);
                out.applied += 1;
            }
            Ok(StoreReply::Conflict(message)) => {
                finish(t);
                out.conflicts += 1;
                out.message = Some(message);
            }
            Ok(StoreReply::Retry(message)) | Err(message) => {
                out.waiting += 1;
                out.message = Some(message);
            }
        }
    }
    out
}

fn settle_store(
    app: &AppHandle,
    state: &State<'_, AppState>,
    transactions: &[String],
) -> StoreSettled {
    settle(
        transactions,
        |t| {
            let transaction = t.to_string();
            ask_store(state, transaction)
        },
        |t| {
            // Failing here is harmless: it's delivered again, and the server is idempotent.
            if let Err(e) = tauri_plugin_store_billing::finish(app, t) {
                eprintln!("penroam: store transaction not finished: {e}");
            }
        },
    )
}

fn ask_store(state: &State<'_, AppState>, transaction: String) -> Result<StoreReply, String> {
    ask(&state.sync, |r| Cmd::StorePurchase(transaction, r)).map_err(|e| e.message)
}

/// The end of a store purchase.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StorePurchaseResult {
    /// `active`, `pending`, `cancelled`, `conflict`, or `waiting` when it's paid but not on the
    /// account yet.
    status: &'static str,
    message: Option<String>,
}

/// Buys an offer through the app store. App Store builds only, and the server decides what it's
/// worth. The offer id is looked up in the account's own offers, so the WebView can't name a
/// product.
#[tauri::command(async)]
pub fn sync_store_purchase(
    app: AppHandle,
    state: State<'_, AppState>,
    offer_id: String,
) -> Result<StorePurchaseResult, CmdError> {
    if !STORE_PURCHASES {
        return Err(err("this build doesn't sell through a store"));
    }
    let view = ask(&state.sync, Cmd::Account)?.ok_or_else(|| err("sync is not set up"))?;
    let offer = view
        .store_offers
        .iter()
        .find(|o| o.id == offer_id)
        .ok_or_else(|| err("this plan isn't available right now"))?;
    let token = view
        .store_account_token
        .as_deref()
        .ok_or_else(|| err("this account can't buy a plan right now"))?;
    let bought =
        tauri_plugin_store_billing::purchase(&app, &offer.product_id, token).map_err(err)?;
    let transaction = match bought {
        tauri_plugin_store_billing::PurchaseOutcome::Cancelled => {
            return Ok(StorePurchaseResult {
                status: "cancelled",
                message: None,
            })
        }
        tauri_plugin_store_billing::PurchaseOutcome::Pending => {
            return Ok(StorePurchaseResult {
                status: "pending",
                message: None,
            })
        }
        tauri_plugin_store_billing::PurchaseOutcome::Purchased(t) => t,
    };
    let s = settle_store(&app, &state, &[transaction]);
    Ok(StorePurchaseResult {
        status: if s.applied > 0 {
            "active"
        } else if s.conflicts > 0 {
            "conflict"
        } else {
            "waiting"
        },
        message: s.message,
    })
}

/// Restores purchases: asks the store what this store account holds, and reports it all.
/// Repeating a report is harmless on the server.
#[tauri::command(async)]
pub fn sync_store_restore(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StoreSettled, CmdError> {
    if !STORE_PURCHASES {
        return Err(err("this build doesn't sell through a store"));
    }
    let transactions = tauri_plugin_store_billing::restore(&app).map_err(err)?;
    Ok(settle_store(&app, &state, &transactions))
}

/// Reports whatever the store is still holding: a purchase interrupted by a crash or an outage, a
/// renewal, or an approved Ask to Buy. Runs at start-up and on returning to the app, and does
/// nothing without sync or on builds that don't sell through a store.
#[tauri::command(async)]
pub fn sync_store_reconcile(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<StoreSettled, CmdError> {
    let configured = state.sync.tx.lock().map(|g| g.is_some()).unwrap_or(false);
    if !STORE_PURCHASES || !configured {
        return Ok(StoreSettled::default());
    }
    let transactions = tauri_plugin_store_billing::unfinished(&app).map_err(err)?;
    Ok(settle_store(&app, &state, &transactions))
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
pub fn sync_account(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<AccountView>, CmdError> {
    if !state.sync.tx.lock().map(|g| g.is_some()).unwrap_or(false) {
        return Ok(None);
    }
    let mut view = ask(&state.sync, Cmd::Account)?;
    if let Some(v) = view.as_mut() {
        fill_store_prices(&app, v);
    }
    Ok(view)
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
