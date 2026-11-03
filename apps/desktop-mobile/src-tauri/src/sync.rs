//! Sync integration (developer preview): a worker thread owns the sync
//! engine and reconciles in the background. The library lock is taken only
//! for the local phases, so saving and pen input never wait for the network.
//!
//! Only the development test server exists today, so sign-in uses its
//! session stub and server URLs are limited to loopback unless
//! `PENROAM_SYNC_ALLOW_ANY_URL=1` (for alternate server implementations).
//! Tokens and keys never reach the WebView or logs.

use crate::commands::CmdError;
use crate::AppState;
use penroam_core::replica::{Conflict, ConflictTarget};
use penroam_storage::ErrorCode;
use penroam_sync::secrets::KeychainSecretStore;
use penroam_sync::store::SyncStore;
use penroam_sync::transport::HttpTransport;
use penroam_sync::{
    DeviceView, FileSecretStore, SecretStore, Setup, SyncEngine, SyncError, SyncStatus,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

type Engine = SyncEngine<HttpTransport>;
type Reply<T> = Sender<Result<T, String>>;

const IDLE_INTERVAL: Duration = Duration::from_secs(60);
const DEBOUNCE: Duration = Duration::from_millis(1500);

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
    Disconnect(Reply<()>),
}

/// The sync slot in [`AppState`].
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

fn ask<T>(slot: &SyncSlot, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T, CmdError> {
    let (tx, rx) = mpsc::channel();
    slot.send(make(tx))?;
    rx.recv_timeout(Duration::from_secs(120))
        .map_err(|_| err("the sync worker did not answer"))?
        .map_err(err)
}

/// Developer preview gate: debug builds, or `PENROAM_DEV_SYNC=1`.
pub fn available() -> bool {
    cfg!(debug_assertions) || std::env::var("PENROAM_DEV_SYNC").is_ok_and(|v| v == "1")
}

fn check_url(url: &str) -> Result<String, CmdError> {
    let u = url.trim().trim_end_matches('/').to_string();
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
    if !any && !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err(err(
            "only a local development test server (127.0.0.1) is allowed in this preview",
        ));
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

const KEYCHAIN_SERVICE: &str = "dev.penroam.app.sync";
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

fn token(url: &str, account: &str, device: Option<&str>) -> Result<String, String> {
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

fn worker(app: AppHandle, mut engine: Engine, rx: Receiver<Cmd>) {
    let mut due: Option<Instant> = Some(Instant::now());
    let mut have_token = false;
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
                let _ = r.send(
                    engine
                        .approve_device(&code, &label)
                        .map_err(|e| e.to_string()),
                );
                continue;
            }
            Ok(Cmd::Remove(id, r)) => {
                let _ = r.send(engine.remove_device(&id).map_err(|e| e.to_string()));
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
                let res = engine.poll_enrollment().map_err(|e| e.to_string());
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
            Ok(Cmd::Disconnect(r)) => {
                let root = library_root(&app);
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
        if !have_token {
            let st = &engine.store.state;
            match token(&st.server_url, &st.account_id, Some(&st.device_id)) {
                Ok(t) => {
                    engine.transport_mut().set_token(t);
                    have_token = true;
                }
                Err(_) => {
                    due = Some(Instant::now() + Duration::from_secs(30));
                    emit_status(&app, &engine);
                    continue;
                }
            }
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
                if matches!(e, SyncError::Transport(t) if t.code() == Some(penroam_sync_protocol::ErrorCode::Unauthenticated))
                {
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
pub struct SyncInfo {
    available: bool,
    configured: bool,
    status: Option<SyncStatus>,
}

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
    })
}

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
pub struct SetupResult {
    recovery_code: Option<String>,
    pairing_code: Option<String>,
}

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
    let t = HttpTransport::new(&url, token(&url, &account, None).map_err(err)?);
    let (engine, code) = Engine::create_account(
        Setup {
            library_root: root.clone(),
            server_url: url,
            account_id: account,
            label,
        },
        t,
        secrets(&app, &root)?,
    )
    .map_err(sync_err)?;
    start(&app, engine);
    Ok(SetupResult {
        recovery_code: Some(code),
        pairing_code: None,
    })
}

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
    let t = HttpTransport::new(&url, token(&url, &account, None).map_err(err)?);
    let (engine, code) = Engine::begin_pairing(
        Setup {
            library_root: root.clone(),
            server_url: url,
            account_id: account,
            label,
        },
        t,
        secrets(&app, &root)?,
    )
    .map_err(sync_err)?;
    start(&app, engine);
    Ok(SetupResult {
        recovery_code: None,
        pairing_code: Some(code),
    })
}

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
    let t = HttpTransport::new(&url, token(&url, &account, None).map_err(err)?);
    let engine = Engine::enroll_with_recovery(
        Setup {
            library_root: root.clone(),
            server_url: url,
            account_id: account,
            label,
        },
        t,
        secrets(&app, &root)?,
        &code,
    )
    .map_err(sync_err)?;
    start(&app, engine);
    Ok(())
}

#[tauri::command(async)]
pub fn sync_poll_pairing(state: State<'_, AppState>) -> Result<Option<String>, CmdError> {
    ask(&state.sync, Cmd::PollPairing)
}

#[tauri::command(async)]
pub fn sync_approve_device(
    state: State<'_, AppState>,
    code: String,
    label: String,
) -> Result<String, CmdError> {
    ask(&state.sync, |r| Cmd::Approve(code, label, r))
}

#[tauri::command(async)]
pub fn sync_remove_device(state: State<'_, AppState>, device_id: String) -> Result<(), CmdError> {
    ask(&state.sync, |r| Cmd::Remove(device_id, r))
}

#[tauri::command(async)]
pub fn sync_devices(state: State<'_, AppState>) -> Result<Vec<DeviceView>, CmdError> {
    ask(&state.sync, Cmd::Devices)
}

#[tauri::command]
pub fn sync_now(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.sync.send(Cmd::SyncNow)
}

#[tauri::command]
pub fn sync_pause(state: State<'_, AppState>, paused: bool) -> Result<(), CmdError> {
    state.sync.send(Cmd::Pause(paused))
}

#[tauri::command(async)]
pub fn sync_conflicts(
    state: State<'_, AppState>,
    notebook_id: Uuid,
) -> Result<Vec<Conflict>, CmdError> {
    ask(&state.sync, |r| Cmd::Conflicts(notebook_id, r))
}

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

#[tauri::command(async)]
pub fn sync_diagnostics(state: State<'_, AppState>) -> Result<String, CmdError> {
    ask(&state.sync, Cmd::Diagnostics)
}

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
            }
            std::fs::remove_dir_all(SyncStore::dir_for(&root)).map_err(|e| err(e.to_string()))?;
            state.sync.set(None);
            let _ = app.emit("sync://status", serde_json::Value::Null);
            Ok(())
        }
    }
}
