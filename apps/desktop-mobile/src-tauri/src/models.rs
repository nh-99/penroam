//! Handwriting model downloads: the first-run download and updates from Settings → Advanced.
//!
//! Models live outside the app so they can be replaced without an app release. The server
//! publishes a signed catalog (`penroam_sync::models`; §12c of
//! `docs/protocol/sync-protocol-v1.md`), and an archive that passes the catalog's checks goes
//! through the same install path as a file the user picks.
//!
//! When the app talks to the server, and why:
//!
//! * **First run, once, without asking**, and only on a device where search would otherwise
//!   have no way to read handwriting ([`crate::search::needs_a_model`]). It retries at later
//!   launches until it succeeds, and stops for good once a model has been installed and the
//!   user has removed it (`first_run_handled`): removing a model must stick.
//! * **When the user asks**: the check and download buttons. Nothing polls for updates, and
//!   updates are never installed unasked.
//!
//! Requests are anonymous (see `penroam_sync::models`), whether or not sync is set up.
//! [`TRUSTED_KEYS`] is what a release build trusts; with no key there is no catalog to believe,
//! so the app makes no request at all and models can only be installed from a file.

use crate::commands::CmdError;
use crate::AppState;
use penroam_search::model::FORMAT_VERSION;
use penroam_sync::models::{self as remote, ModelFetchError, TrustedKey};
use penroam_sync_protocol::models::CatalogModel;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

/// Keys allowed to sign the model catalog, as `(key id, base64url public key)`. Add the new key
/// here one release before rotating to it.
const TRUSTED_KEYS: &[(&str, &str)] = &[];

/// Lets the app settle before a first-run download competes with opening the library.
const FIRST_RUN_DELAY: Duration = Duration::from_secs(5);
/// Fewest milliseconds between progress events.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Debug builds only: use a catalog signed with a development key.
/// `PENROAM_DEV_MODEL_KEY=<key id>:<base64url public key>`.
fn trusted_keys() -> Vec<TrustedKey> {
    let mut keys: Vec<TrustedKey> = TRUSTED_KEYS
        .iter()
        .filter_map(|(id, public)| TrustedKey::parse(id, public))
        .collect();
    if cfg!(debug_assertions) {
        if let Ok(v) = std::env::var("PENROAM_DEV_MODEL_KEY") {
            keys.extend(
                v.split_once(':')
                    .and_then(|(id, public)| TrustedKey::parse(id, public)),
            );
        }
    }
    keys
}

/// Debug builds only: `PENROAM_DEV_MODEL_SERVER` replaces the server.
fn server_url() -> String {
    if cfg!(debug_assertions) {
        if let Ok(v) = std::env::var("PENROAM_DEV_MODEL_SERVER") {
            return v;
        }
    }
    crate::sync::DEFAULT_SERVER_URL.to_string()
}

/// What was remembered between runs (`<app data>/models/state.json`).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Persisted {
    /// Highest catalog sequence accepted, to refuse replays.
    catalog_sequence: u64,
    /// The first-run download is over: it succeeded, or the user removed a model.
    first_run_handled: bool,
}

fn state_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?.join("models");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("state.json"))
}

fn load_persisted(app: &AppHandle) -> Persisted {
    state_path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_persisted(app: &AppHandle, p: &Persisted) {
    if let (Some(path), Ok(bytes)) = (state_path(app), serde_json::to_vec(p)) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

/// Stops the first-run download from happening (again).
pub fn mark_first_run_handled(app: &AppHandle) {
    let mut p = load_persisted(app);
    if !p.first_run_handled {
        p.first_run_handled = true;
        save_persisted(app, &p);
    }
}

/// Whether the first-run download should start. Separate from the I/O so the rule is testable.
fn first_run_due(has_keys: bool, handled: bool, needs_a_model: bool) -> bool {
    has_keys && !handled && needs_a_model
}

/// What the catalog offers this device, as shown in settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferedModel {
    /// Model id.
    pub id: String,
    /// Model version.
    pub version: String,
    /// Display name.
    pub name: String,
    /// Licence text.
    pub license: String,
    /// Languages it reads.
    pub languages: Vec<String>,
    /// Download size.
    pub bytes: u64,
}

impl From<&CatalogModel> for OfferedModel {
    fn from(m: &CatalogModel) -> Self {
        OfferedModel {
            id: m.id.clone(),
            version: m.version.clone(),
            name: m.name.clone(),
            license: m.license.clone(),
            languages: m.languages.clone(),
            bytes: m.bytes,
        }
    }
}

/// What the downloader is doing, pushed on `models://status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStatus {
    /// This build can download models at all (it trusts a catalog key).
    pub enabled: bool,
    /// `idle`, `checking`, `downloading` or `installing`.
    pub phase: &'static str,
    /// The work in progress is the first-run download, not something the user started.
    pub automatic: bool,
    /// Bytes received of `total`.
    pub received: u64,
    /// Size of the download in progress.
    pub total: u64,
    /// The server was asked at least once since the app started.
    pub checked: bool,
    /// The recommended model, after a check.
    pub available: Option<OfferedModel>,
    /// Why the last user-started action failed.
    pub error: Option<String>,
}

impl Default for ModelsStatus {
    fn default() -> Self {
        ModelsStatus {
            enabled: false,
            phase: "idle",
            automatic: false,
            received: 0,
            total: 0,
            checked: false,
            available: None,
            error: None,
        }
    }
}

/// Downloader state shared with the commands.
#[derive(Default)]
pub struct ModelsSlot {
    status: Mutex<ModelsStatus>,
    /// The full entry behind `status.available`.
    offer: Mutex<Option<CatalogModel>>,
    busy: Arc<AtomicBool>,
    cancel: AtomicBool,
}

impl ModelsSlot {
    fn update(&self, app: &AppHandle, f: impl FnOnce(&mut ModelsStatus)) {
        if let Ok(mut s) = self.status.lock() {
            f(&mut s);
            let _ = app.emit("models://status", s.clone());
        }
    }
}

/// Clears the busy flag on every way out of a task.
struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// A short message for settings. Release builds keep addresses and network errors out of it,
/// as sync does: they are nothing the user can act on.
fn describe(e: &ModelFetchError) -> String {
    match e {
        ModelFetchError::Network(_) if !cfg!(debug_assertions) => {
            "Could not reach Penroam Cloud. Check your internet connection and try again.".into()
        }
        e => e.to_string(),
    }
}

/// Asks the server what is on offer and remembers the catalog's sequence.
fn check(app: &AppHandle) -> Result<Option<CatalogModel>, ModelFetchError> {
    let slot = &app.state::<AppState>().models;
    let mut persisted = load_persisted(app);
    let chosen = match remote::fetch_catalog(&server_url())? {
        None => None,
        Some(signed) => {
            let catalog =
                remote::verify_catalog(&signed, &trusted_keys(), persisted.catalog_sequence)?;
            persisted.catalog_sequence = catalog.sequence;
            save_persisted(app, &persisted);
            let version = app.package_info().version.to_string();
            catalog.recommended(&version, FORMAT_VERSION).cloned()
        }
    };
    if let Ok(mut o) = slot.offer.lock() {
        o.clone_from(&chosen);
    }
    slot.update(app, |s| {
        s.checked = true;
        s.available = chosen.as_ref().map(OfferedModel::from);
    });
    Ok(chosen)
}

/// Downloads and installs the offered model, checking first if nothing is on offer yet.
fn fetch_and_install(app: &AppHandle) -> Result<(), String> {
    let slot = &app.state::<AppState>().models;
    let known = slot.offer.lock().ok().and_then(|o| o.clone());
    let model = match known {
        Some(m) => m,
        None => check(app)
            .map_err(|e| describe(&e))?
            .ok_or("No handwriting model is available right now.")?,
    };
    let dir = crate::search::models_dir(app).ok_or("no app data directory")?;
    let parent = dir.parent().ok_or("no app data directory")?.to_path_buf();
    std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    let archive = parent.join("download.penroam-model");

    slot.update(app, |s| {
        s.phase = "downloading";
        s.received = 0;
        s.total = model.bytes;
    });
    let mut last = Instant::now();
    let downloaded = remote::download(&server_url(), &model, &archive, &slot.cancel, |n| {
        if last.elapsed() >= PROGRESS_INTERVAL {
            last = Instant::now();
            slot.update(app, |s| s.received = n);
        }
    });
    if let Err(e) = downloaded {
        return Err(if matches!(e, ModelFetchError::Cancelled) {
            String::new()
        } else {
            describe(&e)
        });
    }
    slot.update(app, |s| {
        s.phase = "installing";
        s.received = s.total;
    });
    let installed = crate::search::install_downloaded(app, archive.clone());
    let _ = std::fs::remove_file(&archive);
    installed?;
    mark_first_run_handled(app);
    Ok(())
}

/// Runs one task on its own thread, unless one is already running. `work` returns the message
/// for settings; an empty one means there is nothing to report (cancelled).
fn spawn(
    app: &AppHandle,
    phase: &'static str,
    automatic: bool,
    work: impl FnOnce(&AppHandle) -> Result<(), String> + Send + 'static,
) {
    let slot = &app.state::<AppState>().models;
    if slot.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let guard = BusyGuard(slot.busy.clone());
    slot.cancel.store(false, Ordering::SeqCst);
    slot.update(app, |s| {
        s.phase = phase;
        s.automatic = automatic;
        s.error = None;
    });
    let app = app.clone();
    std::thread::Builder::new()
        .name("penroam-models".into())
        .spawn(move || {
            let _guard = guard;
            let r = work(&app);
            let slot = &app.state::<AppState>().models;
            slot.update(&app, |s| {
                s.phase = "idle";
                s.received = 0;
                s.total = 0;
                // nobody asked for the first-run download, so nobody is told it failed; the
                // next launch tries again
                s.error = r.err().filter(|m| !m.is_empty() && !automatic);
            });
        })
        .expect("spawn model downloader");
}

/// Prepares the status and starts the first-run download if it is due. Called once at launch.
pub fn start(app: &AppHandle) {
    let has_keys = !trusted_keys().is_empty();
    app.state::<AppState>()
        .models
        .update(app, |s| s.enabled = has_keys);
    let handled = load_persisted(app).first_run_handled;
    if !first_run_due(has_keys, handled, crate::search::needs_a_model(app)) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_RUN_DELAY);
        // the user may have turned search off, or installed a file, in the meantime
        if crate::search::needs_a_model(&app) {
            spawn(&app, "checking", true, fetch_and_install);
        }
    });
}

/// The downloader's state.
#[tauri::command(async)]
pub fn models_status(state: State<'_, AppState>) -> Result<ModelsStatus, CmdError> {
    Ok(state
        .models
        .status
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default())
}

/// Asks the server what is on offer. The answer arrives on `models://status`.
#[tauri::command(async)]
pub fn models_check(app: AppHandle) {
    spawn(&app, "checking", false, |app| {
        check(app).map(|_| ()).map_err(|e| describe(&e))
    });
}

/// Downloads and installs the recommended model, replacing any installed one. Progress and the
/// result arrive on `models://status` and `search://status`.
#[tauri::command(async)]
pub fn models_download(app: AppHandle) {
    spawn(&app, "checking", false, fetch_and_install);
}

/// Stops a download in progress.
#[tauri::command(async)]
pub fn models_cancel(state: State<'_, AppState>) {
    state.models.cancel.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_download_needs_a_key_a_reason_and_no_earlier_outcome() {
        assert!(first_run_due(true, false, true));
        assert!(!first_run_due(false, false, true), "no trusted key");
        assert!(!first_run_due(true, true, true), "removed by the user");
        assert!(
            !first_run_due(true, false, false),
            "OS recognizer or a model"
        );
    }

    #[test]
    fn release_messages_hide_network_details() {
        let m = describe(&ModelFetchError::Network("dns: sync.example".into()));
        assert_eq!(m.contains("sync.example"), cfg!(debug_assertions));
    }

    #[test]
    fn this_build_pins_only_keys_that_parse() {
        for (id, public) in TRUSTED_KEYS {
            assert!(TrustedKey::parse(id, public).is_some(), "{id}");
        }
    }
}
