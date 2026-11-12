//! Whole-library backups from the app: to a file the user picks, to a folder on a schedule,
//! and to a WebDAV server, plus restoring them.
//!
//! A backup is built one notebook at a time in the staging folder, taking the library lock
//! for each notebook only, so writing can continue during a long backup. It is then read back
//! through the restore checks before it reaches its destination. Each step reports through the
//! `portability-progress` event. Format and guarantees: `docs/formats/portability.md`.
//! Notebooks in a vault are left out, with their search corrections: a backup isn't encrypted.
//!
//! Folder and WebDAV destinations are backup storage: whole files come and go, nothing is
//! merged. The WebDAV password lives in the OS keychain with the sync keys.

use crate::commands::{
    file_path, invalid, io_err, stage_user_file, CmdError, StagedFile, GENERATOR,
};
use crate::AppState;
use penroam_core::now_ms;
use penroam_storage::backup::{BackupManifest, BackupProblem};
use penroam_storage::{
    BackupPreview, BackupReader, BackupWriter, ErrorCode, RestoreOptions, RestoreReport,
};
use penroam_webdav::{RemoteFile, WebDav, BACKUP_PREFIX, BACKUP_SUFFIX};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions as StdOpen};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_fs::{FsExt, OpenOptions};
use uuid::Uuid;

type R<T> = Result<T, CmdError>;

/// Dated backups kept in a folder or on a server.
const KEEP_BACKUPS: usize = 7;
/// Backup entry holding search corrections.
const CORRECTIONS: &str = "extras/search-corrections.json";
/// Backup entry with the library's folders.
const FOLDERS: &str = "extras/folders.json";
/// Keychain entry with the WebDAV password.
const WEBDAV_SECRET: &str = "webdav-password";

/// Sent as `portability-progress` while a long operation runs.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    /// What is happening, for the user.
    label: &'static str,
    done: u32,
    total: u32,
}

fn report(app: &AppHandle, label: &'static str, done: usize, total: usize) {
    let _ = app.emit(
        "portability-progress",
        Progress {
            label,
            done: done as u32,
            total: total as u32,
        },
    );
}

fn storage(e: penroam_storage::StorageError) -> CmdError {
    e.into()
}

fn lock<'a>(
    state: &'a State<'_, AppState>,
) -> R<std::sync::MutexGuard<'a, penroam_storage::Library>> {
    state.lib.lock().map_err(|_| CmdError {
        code: ErrorCode::Io,
        message: "library lock poisoned".into(),
    })
}

fn web_err(e: penroam_webdav::WebDavError) -> CmdError {
    CmdError {
        code: ErrorCode::Io,
        message: e.to_string(),
    }
}

/// Builds and checks a backup in the staging folder.
fn build(app: &AppHandle, state: &State<'_, AppState>) -> R<(StagedFile, BackupManifest)> {
    let (ids, staged) = {
        let lib = lock(state)?;
        // Vault notebooks stay out: a backup isn't encrypted, and the vault keeps them.
        let owners = penroam_sync::vault::NotebookOwners::load(lib.root()).map_err(io_err)?;
        let mut ids: Vec<Uuid> = lib
            .notebook_ids()
            .into_iter()
            .filter(|id| owners.owner(id).is_none())
            .collect();
        ids.sort();
        let name = format!("backup-{}.penroam-backup", Uuid::new_v4());
        (ids, StagedFile(lib.staging_dir().join(name)))
    };
    let f = StdOpen::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&staged.0)
        .map_err(io_err)?;
    let mut w = BackupWriter::new(f, GENERATOR).map_err(storage)?;
    let staging = lock(state)?.staging_dir();
    for (i, id) in ids.iter().enumerate() {
        report(app, "Saving notebooks", i, ids.len());
        // A short lock per page and asset read, never while encoding or compressing, so an open
        // editor keeps saving while the backup runs.
        let left_out = w
            .add_notebook_from(&mut crate::commands::Unlocked(state), &staging, *id)
            .map_err(storage)?;
        if let Some(warning) = left_out {
            lock(state)?.recovery.warnings.push(warning);
        }
    }
    let folders = serde_json::to_vec(lock(state)?.folders()).map_err(|e| invalid(e.to_string()))?;
    w.add_extra(FOLDERS, &folders).map_err(storage)?;
    let corrections = state.search.all_corrections();
    if !corrections.is_empty() {
        let json = serde_json::to_vec(&corrections).map_err(|e| invalid(e.to_string()))?;
        w.add_extra(CORRECTIONS, &json).map_err(storage)?;
    }
    let manifest = w.finish().map_err(storage)?;

    let f = File::open(&staged.0).map_err(io_err)?;
    let mut r = BackupReader::open(f).map_err(storage)?;
    let total = manifest.notebooks.len();
    let mut preview = r.empty_preview();
    for i in 0..total {
        report(app, "Checking the backup", i, total);
        r.preview_one(&*lock(state)?, i, &mut preview);
    }
    if let Some(bad) = preview.damaged.first() {
        return Err(CmdError {
            code: ErrorCode::Corrupt,
            message: format!(
                "the backup was not saved: {} failed its check ({})",
                bad.title, bad.error
            ),
        });
    }
    Ok((staged, manifest))
}

/// Result of a backup.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupResult {
    bytes: u64,
    notebooks: u32,
    /// Notebooks that are not in the backup because they could not be read.
    unreadable: Vec<BackupProblem>,
}

fn result(staged: &StagedFile, m: BackupManifest) -> R<BackupResult> {
    Ok(BackupResult {
        bytes: fs::metadata(&staged.0).map_err(io_err)?.len(),
        notebooks: m.notebooks.len() as u32,
        unreadable: m.unreadable,
    })
}

/// Writes a backup of the whole library to a user-chosen destination, which is only
/// touched once the finished file has passed its checks.
#[tauri::command(async)]
pub fn export_backup(app: AppHandle, state: State<'_, AppState>, path: String) -> R<BackupResult> {
    let (staged, manifest) = build(&app, &state)?;
    report(&app, "Saving the file", 0, 0);
    let mut from = File::open(&staged.0).map_err(io_err)?;
    let mut to = app
        .fs()
        .open(
            file_path(&path)?,
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .clone(),
        )
        .map_err(io_err)?;
    std::io::copy(&mut from, &mut to).map_err(io_err)?;
    to.flush().map_err(io_err)?;
    let _ = to.sync_all();
    result(&staged, manifest)
}

/// Checks a backup and describes it, for the confirmation before a restore. Changes nothing.
#[tauri::command(async)]
pub fn preview_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> R<BackupPreview> {
    report(&app, "Reading the backup", 0, 0);
    let staged = stage_user_file(&app, &state, &path)?;
    let f = File::open(&staged.0).map_err(io_err)?;
    let mut r = BackupReader::open(f).map_err(storage)?;
    let total = r.manifest().notebooks.len();
    let mut preview = r.empty_preview();
    for i in 0..total {
        report(&app, "Checking the backup", i, total);
        r.preview_one(&*lock(&state)?, i, &mut preview);
    }
    Ok(preview)
}

/// Restores notebooks from a backup. Never overwrites: see [`RestoreOptions`].
#[tauri::command(async)]
pub fn restore_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    options: RestoreOptions,
) -> R<RestoreReport> {
    state.sync.nudge();
    report(&app, "Reading the backup", 0, 0);
    let staged = stage_user_file(&app, &state, &path)?;
    let f = File::open(&staged.0).map_err(io_err)?;
    let mut r = BackupReader::open(f).map_err(storage)?;
    let total = r.manifest().notebooks.len();
    let mut out = RestoreReport::default();
    // before the notebooks, which keep their folder only if it exists by then
    if let Ok(Some(bytes)) = r.extra(FOLDERS) {
        if let Ok(folders) = serde_json::from_slice(&bytes) {
            lock(&state)?.restore_folders(&folders).map_err(storage)?;
        }
    }
    for i in 0..total {
        report(&app, "Restoring notebooks", i, total);
        r.restore(&mut *lock(&state)?, i, &options, &mut out);
    }
    // search rebuilds from the notes: the index is not part of a backup
    for id in &out.restored {
        state.search.notebook_changed(*id);
    }
    if let Ok(Some(bytes)) = r.extra(CORRECTIONS) {
        restore_corrections(&state, &bytes, &out.mapped);
    }
    Ok(out)
}

/// Brings back search corrections for the notebooks that were restored. A copy has a new
/// notebook id and the same pages, so the map from the report is applied.
fn restore_corrections(state: &State<'_, AppState>, bytes: &[u8], mapped: &[(Uuid, Uuid)]) {
    for (nb, page, region, text) in corrections_for(bytes, mapped) {
        state.search.restore_correction(nb, page, region, text);
    }
}

/// A correction: notebook, page, region and the corrected text.
type Correction = (Uuid, Uuid, String, String);

/// The corrections in a backup's extra that belong to restored notebooks, moved to the id each
/// notebook has now. Corrections of notebooks that weren't restored (skipped, failed or not
/// chosen) are dropped, so they can't attach to the library's own notebook with that id. An
/// unreadable extra gives none: corrections are a convenience, not notes.
fn corrections_for(bytes: &[u8], mapped: &[(Uuid, Uuid)]) -> Vec<Correction> {
    let Ok(rows) = serde_json::from_slice::<Vec<Correction>>(bytes) else {
        return Vec::new();
    };
    rows.into_iter()
        .filter_map(|(nb, page, region, text)| {
            let (_, now) = mapped.iter().find(|(old, _)| *old == nb)?;
            Some((*now, page, region, text))
        })
        .collect()
}

/// Today as `YYYY-MM-DD` in UTC, for file names.
fn date_stamp(ms: i64) -> String {
    // civil-from-days, Howard Hinnant's algorithm
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn dated_name() -> String {
    format!("{BACKUP_PREFIX}{}{BACKUP_SUFFIX}", date_stamp(now_ms()))
}

/// Puts a finished backup in a folder and keeps the newest few. Only files with
/// this module's own names are ever removed.
fn to_folder(dir: &Path, staged: &Path) -> std::io::Result<()> {
    let name = dated_name();
    let part = dir.join(format!("{name}.part"));
    let mut from = File::open(staged)?;
    let mut to = File::create(&part)?;
    std::io::copy(&mut from, &mut to)?;
    to.sync_all()?;
    fs::rename(&part, dir.join(&name))?;
    let mut ours: Vec<PathBuf> = fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(BACKUP_PREFIX) && n.ends_with(BACKUP_SUFFIX))
        })
        .collect();
    ours.sort();
    let extra = ours.len().saturating_sub(KEEP_BACKUPS);
    for p in &ours[..extra] {
        fs::remove_file(p)?;
    }
    Ok(())
}

/// Where automatic backups go.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AutoTarget {
    /// A folder on this device (or one a cloud drive app keeps in step).
    Folder {
        /// The folder.
        dir: String,
    },
    /// A WebDAV folder; the password is in the keychain.
    Webdav {
        /// Folder address.
        url: String,
        /// User name.
        user: String,
    },
}

fn webdav_client(app: &AppHandle, state: &State<'_, AppState>, url: &str, user: &str) -> R<WebDav> {
    let root = lock(state)?.root().to_path_buf();
    let secrets = crate::sync::secrets(app, &root)?;
    let password = secrets
        .get(WEBDAV_SECRET)
        .map_err(io_err)?
        .ok_or_else(|| invalid("connect to the WebDAV server first"))?;
    WebDav::new(url, user, &password).map_err(web_err)
}

/// Runs a backup to the configured destination, in the background.
#[tauri::command(async)]
pub fn auto_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    target: AutoTarget,
) -> R<BackupResult> {
    let (staged, manifest) = build(&app, &state)?;
    report(&app, "Saving the file", 0, 0);
    match target {
        AutoTarget::Folder { dir } => {
            // In the Mac App Store sandbox the folder is reachable only through its saved bookmark.
            let _access = crate::folder_access::open(&app, &dir);
            to_folder(Path::new(&dir), &staged.0).map_err(io_err)?
        }
        AutoTarget::Webdav { url, user } => {
            let dav = webdav_client(&app, &state, &url, &user)?;
            dav.upload(&staged.0, &dated_name()).map_err(web_err)?;
            dav.rotate(KEEP_BACKUPS).map_err(web_err)?;
        }
    }
    result(&staged, manifest)
}

/// Checks a WebDAV folder and, if it works, keeps the password in the keychain.
#[tauri::command(async)]
pub fn webdav_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    user: String,
    password: String,
) -> R<Vec<RemoteFile>> {
    let dav = WebDav::new(&url, &user, &password).map_err(web_err)?;
    let backups = dav.backups().map_err(web_err)?;
    let root = lock(&state)?.root().to_path_buf();
    crate::sync::secrets(&app, &root)?
        .set(WEBDAV_SECRET, &password)
        .map_err(io_err)?;
    Ok(backups)
}

/// Forgets the WebDAV password. Files on the server stay.
#[tauri::command(async)]
pub fn webdav_disconnect(app: AppHandle, state: State<'_, AppState>) -> R<()> {
    let root = lock(&state)?.root().to_path_buf();
    crate::sync::secrets(&app, &root)?
        .delete(WEBDAV_SECRET)
        .map_err(io_err)
}

/// The backups in the WebDAV folder, oldest first.
#[tauri::command(async)]
pub fn webdav_backups(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    user: String,
) -> R<Vec<RemoteFile>> {
    webdav_client(&app, &state, &url, &user)?
        .backups()
        .map_err(web_err)
}

/// Downloads a backup to the staging folder and returns its path, which works with
/// [`preview_backup`] and [`restore_backup`]. Pass it to [`discard_fetched`] afterwards.
#[tauri::command(async)]
pub fn webdav_fetch(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    user: String,
    name: String,
) -> R<String> {
    report(&app, "Downloading the backup", 0, 0);
    let dav = webdav_client(&app, &state, &url, &user)?;
    let path = lock(&state)?
        .staging_dir()
        .join(format!("fetched-{}.penroam-backup", Uuid::new_v4()));
    let mut f = File::create(&path).map_err(io_err)?;
    dav.download(&name, &mut f).map_err(|e| {
        let _ = fs::remove_file(&path);
        web_err(e)
    })?;
    f.seek(SeekFrom::Start(0)).map_err(io_err)?;
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| invalid("unusable staging path"))
}

/// Deletes a file from [`webdav_fetch`]. Refuses anything else.
#[tauri::command(async)]
pub fn discard_fetched(state: State<'_, AppState>, path: String) -> R<()> {
    let staging = lock(&state)?.staging_dir();
    let p = PathBuf::from(&path);
    let ours = p.parent() == Some(staging.as_path())
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("fetched-"));
    if !ours {
        return Err(invalid("not a downloaded backup"));
    }
    let _ = fs::remove_file(p);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrections_follow_restored_notebooks_and_copies() {
        let id = Uuid::from_u128;
        let rows = serde_json::to_vec(&[
            (id(1), id(10), "r1".to_string(), "hello".to_string()),
            (id(2), id(20), "r2".to_string(), "skipped".to_string()),
            (id(3), id(30), "r3".to_string(), "copied".to_string()),
        ])
        .unwrap();
        // 1 restored as itself, 2 skipped (already in the library), 3 restored as a copy
        let mapped = [(id(1), id(1)), (id(3), id(99))];
        assert_eq!(
            corrections_for(&rows, &mapped),
            vec![
                (id(1), id(10), "r1".to_string(), "hello".to_string()),
                (id(99), id(30), "r3".to_string(), "copied".to_string()),
            ]
        );
        assert!(corrections_for(b"not json", &mapped).is_empty());
    }

    #[test]
    fn dates_for_file_names() {
        assert_eq!(date_stamp(0), "1970-01-01");
        assert_eq!(date_stamp(1_793_923_200_000), "2026-11-06");
        assert_eq!(date_stamp(951_782_400_000), "2000-02-29");
    }

    #[test]
    fn folder_rotation_keeps_the_newest_and_nothing_else_is_touched() {
        let d = tempfile::tempdir().unwrap();
        for day in 10..20 {
            let n = format!("{BACKUP_PREFIX}2026-10-{day}{BACKUP_SUFFIX}");
            fs::write(d.path().join(n), b"old").unwrap();
        }
        fs::write(d.path().join("thesis.pdf"), b"mine").unwrap();
        fs::write(d.path().join("Penroam backup notes.txt"), b"mine").unwrap();
        let staged = d.path().join("staged");
        fs::write(&staged, b"new").unwrap();
        to_folder(d.path(), &staged).unwrap();
        let backups = fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(BACKUP_SUFFIX))
            .count();
        assert_eq!(backups, KEEP_BACKUPS);
        assert!(d.path().join("thesis.pdf").exists());
        assert!(d.path().join("Penroam backup notes.txt").exists());
        assert!(!d
            .path()
            .join(format!("{BACKUP_PREFIX}2026-10-10{BACKUP_SUFFIX}"))
            .exists());
        assert!(d.path().join(dated_name()).exists());
    }
}
