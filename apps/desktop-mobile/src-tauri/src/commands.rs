//! Tauri commands for the library, pages, assets, import/export and app exit.
//!
//! Each command is a thin wrapper over [`penroam_storage::Library`] (held in
//! [`AppState`] behind a mutex) or [`penroam_pdf`]. Errors reach the UI as
//! [`CmdError`] `{ code, message }`. Commands run on Tauri's async pool
//! (`#[tauri::command(async)]`), so file and storage work never blocks the
//! UI thread.
//!
//! Commands that change notebook data call [`crate::sync::SyncSlot::nudge`]
//! first. That only schedules a debounced background sync (and does nothing
//! when sync is not set up); the sync worker waits for the library lock, so
//! nudging before the change is safe.
//!
//! Large payloads (pages, thumbnails, assets) travel as raw IPC bodies
//! instead of JSON-encoded byte arrays.

use crate::AppState;
use penroam_core::folder::Folder;
use penroam_core::*;
use penroam_storage::{
    ApplyOutcome, ArchiveSummary, ErrorCode, ExportOptions, HistoryPoint, HistoryRetention,
    HistoryStats, ImportReport, NewAsset, NewPage, NotebookRestore, NotebookSummary,
    RecoveryReport, StorageError,
};
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom, Write};
use std::str::FromStr;
use tauri::ipc::Response;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_fs::{FilePath, FsExt, OpenOptions};
use uuid::Uuid;

/// Structured error returned to the UI: `{ code, message }`.
#[derive(Debug, Serialize)]
pub struct CmdError {
    /// Machine-readable category (shared with the TS bindings).
    pub code: ErrorCode,
    /// Safe, human-readable message.
    pub message: String,
}

impl From<StorageError> for CmdError {
    fn from(e: StorageError) -> Self {
        CmdError {
            code: e.code(),
            message: e.to_string(),
        }
    }
}

impl From<penroam_pdf::PdfError> for CmdError {
    fn from(e: penroam_pdf::PdfError) -> Self {
        CmdError {
            code: ErrorCode::Pdf,
            message: e.to_string(),
        }
    }
}

pub(crate) fn io_err(e: std::io::Error) -> CmdError {
    StorageError::from(e).into()
}

pub(crate) fn invalid(msg: impl Into<String>) -> CmdError {
    CmdError {
        code: ErrorCode::Invalid,
        message: msg.into(),
    }
}

type R<T> = Result<T, CmdError>;

macro_rules! lib {
    ($state:expr) => {
        $state.lib.lock().map_err(|_| CmdError {
            code: ErrorCode::Io,
            message: "library lock poisoned".into(),
        })?
    };
}

/// Returns freed heap pages to the OS after large transient allocations
/// (multi-MB page JSON, archives). glibc keeps them otherwise, which shows up
/// as process RSS long after the data is gone.
pub fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Checkpoints everything to disk (window close, app exit, watchdog).
pub fn flush(app: &AppHandle) {
    if let Some(s) = app.try_state::<AppState>() {
        if let Ok(mut l) = s.lib.lock() {
            if let Err(e) = l.flush_all() {
                eprintln!("penroam: flush failed: {e}");
            }
        }
    }
}

pub(crate) const GENERATOR: &str = concat!("Penroam ", env!("CARGO_PKG_VERSION"));
const MAX_IMPORT_BYTES: u64 = 2 << 30;

/// Result of creating a notebook or a page.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookAndPage {
    notebook: Notebook,
    page: Page,
}

/// Result of creating several pages (PDF import).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookAndPages {
    notebook: Notebook,
    pages: Vec<Page>,
}

/// What start-up recovery did (truncated journals, replayed batches, warnings).
#[tauri::command(async)]
pub fn library_status(state: State<'_, AppState>) -> R<RecoveryReport> {
    Ok(lib!(state).recovery.clone())
}

/// Summaries of every notebook (live and trashed) for the library view.
#[tauri::command(async)]
pub fn list_notebooks(state: State<'_, AppState>) -> R<Vec<NotebookSummary>> {
    Ok(lib!(state).list_notebooks())
}

/// One notebook document (page order, trash, assets).
#[tauri::command(async)]
pub fn get_notebook(state: State<'_, AppState>, id: Uuid) -> R<Notebook> {
    Ok(lib!(state).notebook(id)?.clone())
}

/// Creates a notebook with one empty page. Only Letter and A4 are accepted
/// for new notebooks. With `infinite_canvas` its pages are infinite canvases.
#[tauri::command(async)]
pub fn create_notebook(
    state: State<'_, AppState>,
    title: String,
    paper: Option<Paper>,
    template: Option<Template>,
    cover: Option<String>,
    vault: Option<String>,
    infinite_canvas: Option<bool>,
) -> R<NotebookAndPage> {
    let defaults = PageDefaults {
        paper: paper.unwrap_or(Paper::Letter),
        template: template.unwrap_or_default(),
    };
    if !matches!(defaults.paper, Paper::Letter | Paper::A4) {
        return Err(invalid("new notebooks use Letter or A4 pages"));
    }
    let (notebook, page) = {
        // one critical section: the sync worker takes this lock before it looks at the library,
        // so it can't find a vault's notebook unassigned and upload it to the main account
        let mut lib = lib!(state);
        let root = lib.root().to_path_buf();
        if let Some(v) = &vault {
            crate::sync::require_open_vault(&root, v)?;
        }
        let mut made = lib.create_notebook(&title, defaults, cover)?;
        if let Some(v) = &vault {
            crate::sync::assign_to_vault(&root, made.0.id, v)?;
        }
        if infinite_canvas == Some(true) {
            made.0 = lib.set_infinite_canvas(made.0.id, true)?;
        }
        made
    };
    state.sync.nudge();
    state.search.notebook_changed(notebook.id);
    Ok(NotebookAndPage { notebook, page })
}

/// Moves a notebook into a vault: a copy under a new id goes in, and the original is erased from
/// this device and the account. The copy starts without the original's version history.
#[tauri::command(async)]
pub fn move_notebook_to_vault(state: State<'_, AppState>, id: Uuid, vault: String) -> R<Notebook> {
    let copy = {
        let mut lib = lib!(state);
        let root = lib.root().to_path_buf();
        crate::sync::require_open_vault(&root, &vault)?;
        let mut archive = std::io::Cursor::new(Vec::new());
        lib.export_archive(id, &mut archive, "penroam vault move")?;
        archive.set_position(0);
        // the folder stays behind: folders belong to the main account, not to vaults
        let report = lib.import_archive_with(
            archive,
            penroam_storage::ImportOptions {
                keep_trash: true,
                keep_folder: false,
            },
        )?;
        let copy = report.notebook;
        if copy.id == id {
            return Err(invalid("the notebook could not be copied"));
        }
        crate::sync::assign_to_vault(&root, copy.id, &vault)?;
        lib.purge_notebook(id)?;
        copy
    };
    state.sync.nudge();
    state.search.notebook_changed(id);
    state.search.notebook_changed(copy.id);
    Ok(copy)
}

/// Makes a copy of a notebook, titled "`<title>` (copy)", in the same folder. The copy of a vault
/// notebook stays in that vault.
#[tauri::command(async)]
pub fn duplicate_notebook(state: State<'_, AppState>, id: Uuid) -> R<Notebook> {
    let copy = {
        let mut lib = lib!(state);
        let root = lib.root().to_path_buf();
        let vault = crate::sync::vault_of(&root, id)?;
        if let Some(v) = &vault {
            crate::sync::require_open_vault(&root, v)?;
        }
        let copy = lib.duplicate_notebook(id)?;
        // assigned before the library lock is released, so the main engine never uploads it
        if let Some(v) = &vault {
            crate::sync::assign_to_vault(&root, copy.id, v)?;
        }
        copy
    };
    state.sync.nudge();
    state.search.notebook_changed(copy.id);
    Ok(copy)
}

/// Renames a notebook.
#[tauri::command(async)]
pub fn rename_notebook(state: State<'_, AppState>, id: Uuid, title: String) -> R<Notebook> {
    state.sync.nudge();
    let nb = lib!(state).rename_notebook(id, &title)?;
    state.search.notebook_changed(id);
    Ok(nb)
}

/// The library's folders, for the library view.
#[tauri::command(async)]
pub fn list_folders(state: State<'_, AppState>) -> R<Vec<Folder>> {
    Ok(lib!(state).list_folders())
}

/// Creates a folder, inside `parent` or at the top level.
#[tauri::command(async)]
pub fn create_folder(state: State<'_, AppState>, name: String, parent: Option<Uuid>) -> R<Folder> {
    state.sync.nudge();
    Ok(lib!(state).create_folder(&name, parent)?)
}

/// Renames a folder.
#[tauri::command(async)]
pub fn rename_folder(state: State<'_, AppState>, id: Uuid, name: String) -> R<()> {
    state.sync.nudge();
    Ok(lib!(state).rename_folder(id, &name)?)
}

/// Moves a folder under `parent`, or to the top level.
#[tauri::command(async)]
pub fn move_folder(state: State<'_, AppState>, id: Uuid, parent: Option<Uuid>) -> R<()> {
    state.sync.nudge();
    Ok(lib!(state).move_folder(id, parent)?)
}

/// Deletes a folder; what was inside moves to its parent.
#[tauri::command(async)]
pub fn delete_folder(state: State<'_, AppState>, id: Uuid) -> R<()> {
    state.sync.nudge();
    Ok(lib!(state).delete_folder(id)?)
}

/// Files a notebook in a folder, or at the top level. Vault notebooks can't
/// be filed: folders sync through the main account, and a vault's contents
/// must not depend on it.
#[tauri::command(async)]
pub fn file_notebook(state: State<'_, AppState>, id: Uuid, folder: Option<Uuid>) -> R<Notebook> {
    let nb = {
        let mut lib = lib!(state);
        if crate::sync::in_vault(lib.root(), id)? {
            return Err(invalid("notebooks in a vault can't be put in folders"));
        }
        lib.file_notebook(id, folder)?
    };
    state.sync.nudge();
    Ok(nb)
}

/// Sets the cover colour (`#rrggbb`).
#[tauri::command(async)]
pub fn set_notebook_cover(state: State<'_, AppState>, id: Uuid, cover: String) -> R<Notebook> {
    state.sync.nudge();
    Ok(lib!(state).set_notebook_cover(id, &cover)?)
}

/// Moves a notebook to the trash (restorable).
#[tauri::command(async)]
pub fn delete_notebook(state: State<'_, AppState>, id: Uuid) -> R<Notebook> {
    state.sync.nudge();
    let nb = lib!(state).delete_notebook(id)?;
    state.search.notebook_changed(id);
    Ok(nb)
}

/// Takes a notebook out of the trash.
#[tauri::command(async)]
pub fn restore_notebook(state: State<'_, AppState>, id: Uuid) -> R<Notebook> {
    state.sync.nudge();
    let nb = lib!(state).restore_notebook(id)?;
    state.search.notebook_changed(id);
    Ok(nb)
}

/// Erases a trashed notebook. With sync on this reaches every device, and the
/// account can restore it.
#[tauri::command(async)]
pub fn purge_notebook(state: State<'_, AppState>, id: Uuid) -> R<()> {
    state.sync.nudge();
    lib!(state).purge_notebook(id)?;
    state.search.notebook_changed(id);
    Ok(())
}

/// Shows a notebook's pages as infinite canvases, or as sheets again. With
/// `collapse` (turning infinite canvases on), every page is first gathered
/// onto the first one and the emptied pages go to the notebook's trash.
#[tauri::command(async)]
pub fn set_infinite_canvas(
    state: State<'_, AppState>,
    id: Uuid,
    infinite: bool,
    collapse: Option<bool>,
) -> R<Notebook> {
    state.sync.nudge();
    let nb = {
        let mut lib = lib!(state);
        if infinite && collapse == Some(true) {
            lib.collapse_pages_into_canvas(id)?;
        }
        lib.set_infinite_canvas(id, infinite)?
    };
    state.search.notebook_changed(id);
    Ok(nb)
}

/// Inserts a page at `index` (default: end). An explicit `size` creates a
/// custom-size page (used by the benchmark harness).
#[tauri::command(async)]
pub fn create_page(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    index: Option<usize>,
    template: Option<Template>,
    paper: Option<Paper>,
    size: Option<Size>,
) -> R<NotebookAndPage> {
    state.sync.nudge();
    let mut lib = lib!(state);
    if let Some(size) = size {
        // explicit size (custom paper): used by the benchmark harness
        let t = template.unwrap_or(Template {
            kind: TemplateKind::Blank,
            spacing: 24.0,
        });
        let (notebook, mut pages) =
            lib.create_pages(notebook_id, index, vec![(Paper::Custom, size, t, None)])?;
        state.search.notebook_changed(notebook_id);
        return Ok(NotebookAndPage {
            notebook,
            page: pages.remove(0),
        });
    }
    let (notebook, page) = lib.create_page(notebook_id, index, template, paper)?;
    state.search.notebook_changed(notebook_id);
    Ok(NotebookAndPage { notebook, page })
}

/// Reorders live pages; `order` must contain exactly the notebook's live pages.
#[tauri::command(async)]
pub fn update_page_order(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    order: Vec<Uuid>,
) -> R<Notebook> {
    state.sync.nudge();
    let nb = lib!(state).update_page_order(notebook_id, order)?;
    state.search.notebook_changed(notebook_id);
    Ok(nb)
}

/// Moves a page to the notebook's trash.
#[tauri::command(async)]
pub fn delete_page(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<Notebook> {
    state.sync.nudge();
    let nb = lib!(state).delete_page(notebook_id, page_id)?;
    state.search.notebook_changed(notebook_id);
    Ok(nb)
}

/// Restores a trashed page to its former position.
#[tauri::command(async)]
pub fn restore_page(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<Notebook> {
    state.sync.nudge();
    let nb = lib!(state).restore_page(notebook_id, page_id)?;
    state.search.notebook_changed(notebook_id);
    Ok(nb)
}

/// Returns the page as raw JSON bytes (avoids a second serde pass for large
/// pages). After a very large page the heap is trimmed shortly afterwards.
#[tauri::command(async)]
pub fn load_page(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<Response> {
    let bytes = lib!(state)
        .with_page(notebook_id, page_id, serde_json::to_vec)?
        .map_err(|e| invalid(e.to_string()))?;
    let big = bytes.len() > (4 << 20);
    let r = Response::new(bytes);
    if big {
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(500));
            trim_heap();
        });
    }
    Ok(r)
}

/// Applies an editor batch; returns once it is durable on disk ("Saved
/// locally"). A stale `base_revision` fails with a `conflict` error and the
/// editor reloads the page; retrying the same batch id returns the original
/// outcome.
#[tauri::command(async)]
pub fn apply_page_operations(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    batch: OperationBatch,
) -> R<ApplyOutcome> {
    state.sync.nudge();
    let out = lib!(state).apply_page_operations(notebook_id, page_id, &batch)?;
    state.search.page_changed(notebook_id, page_id);
    Ok(out)
}

/// Checkpoints one page (writes its snapshot) when the editor leaves it.
#[tauri::command(async)]
pub fn flush_page(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<()> {
    Ok(lib!(state).flush_page(notebook_id, page_id)?)
}

/// Checkpoints everything (Ctrl+S, app close).
#[tauri::command(async)]
pub fn flush_all(state: State<'_, AppState>) -> R<()> {
    Ok(lib!(state).flush_all()?)
}

/// Asset bytes (verified against their hash) as a raw body.
#[tauri::command(async)]
pub fn read_asset(state: State<'_, AppState>, notebook_id: Uuid, hash: String) -> R<Response> {
    Ok(Response::new(lib!(state).read_asset(notebook_id, &hash)?))
}

/// Stores a page thumbnail (derived cache, never exported). Raw PNG body;
/// ids in headers (`x-notebook`, `x-page`).
#[tauri::command(async)]
pub fn save_thumbnail(state: State<'_, AppState>, request: tauri::ipc::Request<'_>) -> R<()> {
    let h = |k: &str| {
        request
            .headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| invalid(format!("missing {k}")))
    };
    let (nb, page) = (h("x-notebook")?, h("x-page")?);
    // Desktop delivers the Uint8Array as a raw body; the Android/iOS IPC bridge
    // serialises it as a JSON array of bytes.
    let owned;
    let bytes: &[u8] = match request.body() {
        tauri::ipc::InvokeBody::Raw(b) => b,
        tauri::ipc::InvokeBody::Json(serde_json::Value::Array(a)) => {
            owned = a
                .iter()
                .map(|v| v.as_u64().filter(|n| *n < 256).map(|n| n as u8))
                .collect::<Option<Vec<u8>>>()
                .ok_or_else(|| invalid("thumbnail body is not a byte array"))?;
            &owned
        }
        _ => return Err(invalid("expected a PNG body")),
    };
    Ok(lib!(state).save_thumbnail(nb, page, bytes)?)
}

/// A page thumbnail as a raw PNG body (empty when none was generated yet).
#[tauri::command(async)]
pub fn get_thumbnail(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<Response> {
    Ok(Response::new(
        lib!(state)
            .thumbnail(notebook_id, page_id)
            .unwrap_or_default(),
    ))
}

pub(crate) fn file_path(s: &str) -> R<FilePath> {
    FilePath::from_str(s).map_err(|_| invalid("invalid file location"))
}

/// Reads a user-chosen file (path or content URI) with a size limit.
pub(crate) fn read_user_file(app: &AppHandle, path: &str, limit: u64) -> R<Vec<u8>> {
    let f = app
        .fs()
        .open(file_path(path)?, OpenOptions::new().read(true).clone())
        .map_err(io_err)?;
    let mut buf = Vec::new();
    f.take(limit + 1).read_to_end(&mut buf).map_err(io_err)?;
    if buf.len() as u64 > limit {
        return Err(CmdError {
            code: ErrorCode::TooLarge,
            message: "the file is too large".into(),
        });
    }
    Ok(buf)
}

/// Writes to a user-chosen destination. The bytes are fully produced before
/// the destination is opened, so a failed export never truncates a file.
pub(crate) fn write_user_file(app: &AppHandle, path: &str, bytes: &[u8]) -> R<()> {
    let mut f = app
        .fs()
        .open(
            file_path(path)?,
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .clone(),
        )
        .map_err(io_err)?;
    f.write_all(bytes).map_err(io_err)?;
    f.flush().map_err(io_err)?;
    let _ = f.sync_all();
    Ok(())
}

/// Imports a user-chosen PDF (≤ 1 GiB) as pages: the original bytes become a
/// notebook asset and every PDF page becomes a page with that background.
#[tauri::command(async)]
pub fn import_pdf(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    path: String,
    file_name: Option<String>,
    index: Option<usize>,
) -> R<NotebookAndPages> {
    state.sync.nudge();
    let bytes = read_pdf_file(&app, &path)?;
    let info = penroam_pdf::inspect(&bytes)?;
    let mut lib = lib!(state);
    let asset = lib.add_asset(notebook_id, &bytes, "application/pdf", file_name)?;
    let pages = info
        .iter()
        .map(|i| {
            let bg = i.background(&asset.sha256);
            (
                Paper::Pdf,
                bg.display_size(),
                Template {
                    kind: TemplateKind::Blank,
                    spacing: 24.0,
                },
                Some(bg),
            )
        })
        .collect();
    let (notebook, pages) = lib.create_pages(notebook_id, index, pages)?;
    state.search.notebook_changed(notebook_id);
    Ok(NotebookAndPages { notebook, pages })
}

/// A PDF added to a notebook as an asset, with a reference to each of its pages.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfAsset {
    notebook: Notebook,
    pages: Vec<PdfBackgroundRef>,
}

/// Adds a user-chosen PDF (≤ 1 GiB) to a notebook as an asset without making
/// any pages: the editor places its pages on an infinite canvas itself.
#[tauri::command(async)]
pub fn add_pdf_asset(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    path: String,
    file_name: Option<String>,
) -> R<PdfAsset> {
    state.sync.nudge();
    let bytes = read_pdf_file(&app, &path)?;
    let info = penroam_pdf::inspect(&bytes)?;
    let mut lib = lib!(state);
    let asset = lib.add_asset(notebook_id, &bytes, "application/pdf", file_name)?;
    let pages = info.iter().map(|i| i.background(&asset.sha256)).collect();
    Ok(PdfAsset {
        notebook: lib.notebook(notebook_id)?.clone(),
        pages,
    })
}

/// Reads a user-chosen file that must be a PDF (≤ 1 GiB).
fn read_pdf_file(app: &AppHandle, path: &str) -> R<Vec<u8>> {
    let bytes = read_user_file(app, path, 1 << 30)?;
    if !bytes.starts_with(b"%PDF") && !bytes.windows(5).take(1024).any(|w| w == b"%PDF-") {
        return Err(CmdError {
            code: ErrorCode::Pdf,
            message: "this file is not a PDF".into(),
        });
    }
    Ok(bytes)
}

/// Copies a user-chosen file into the staging folder and returns a handle that deletes
/// it. Backups can be far larger than memory, so they are read from a local copy.
pub(crate) fn stage_user_file(
    app: &AppHandle,
    state: &State<'_, AppState>,
    path: &str,
) -> R<StagedFile> {
    let dest = lib!(state)
        .staging_dir()
        .join(format!("incoming-{}", Uuid::new_v4()));
    let mut from = app
        .fs()
        .open(file_path(path)?, OpenOptions::new().read(true).clone())
        .map_err(io_err)?;
    let staged = StagedFile(dest);
    let mut to = std::fs::File::create(&staged.0).map_err(io_err)?;
    std::io::copy(&mut from, &mut to).map_err(io_err)?;
    Ok(staged)
}

/// A staging file, removed when dropped.
pub(crate) struct StagedFile(pub(crate) std::path::PathBuf);

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Size of an exported archive.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    bytes: u64,
    pages: u32,
}

/// Exports a notebook as a `.penroam` archive to a user-chosen destination.
/// The archive is built in the staging folder first, so a failure never
/// leaves a truncated file at the destination.
#[tauri::command(async)]
pub fn export_notebook(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    path: String,
) -> R<ExportResult> {
    let (buf, pages) = {
        let mut lib = lib!(state);
        let tmp = lib
            .staging_dir()
            .join(format!("export-{}.penroam", Uuid::new_v4()));
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(io_err)?;
        let res = lib.export_archive(notebook_id, &mut f, GENERATOR);
        let out = res.and_then(|m| {
            f.seek(SeekFrom::Start(0))?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            Ok((buf, m.pages.len() as u32))
        });
        let _ = std::fs::remove_file(&tmp);
        out?
    };
    write_user_file(&app, &path, &buf)?;
    let n = buf.len() as u64;
    drop(buf);
    trim_heap();
    Ok(ExportResult { bytes: n, pages })
}

/// Exports the given live pages as their own `.penroam` archive.
#[tauri::command(async)]
pub fn export_pages(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    pages: Vec<Uuid>,
    path: String,
) -> R<ExportResult> {
    let mut out = std::io::Cursor::new(Vec::new());
    let opts = ExportOptions {
        pages: Some(pages),
        ..Default::default()
    };
    let m = lib!(state).export_archive_with(notebook_id, &mut out, GENERATOR, &opts)?;
    let buf = out.into_inner();
    write_user_file(&app, &path, &buf)?;
    Ok(ExportResult {
        bytes: buf.len() as u64,
        pages: m.pages.len() as u32,
    })
}

/// Checks a `.penroam` file and describes it without importing it.
#[tauri::command(async)]
pub fn inspect_notebook(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> R<ArchiveSummary> {
    let staged = stage_user_file(&app, &state, &path)?;
    let f = std::fs::File::open(&staged.0).map_err(io_err)?;
    Ok(lib!(state).verify_archive(f)?)
}

/// A document imported from another format.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentImport {
    notebook: Notebook,
    report: penroam_interchange::ImportLoss,
}

impl From<penroam_interchange::ImportError> for CmdError {
    fn from(e: penroam_interchange::ImportError) -> Self {
        use penroam_interchange::ImportError as E;
        let code = match e {
            E::Malformed(_) => ErrorCode::Archive,
            E::TooLarge(_) => ErrorCode::TooLarge,
        };
        CmdError {
            code,
            message: e.to_string(),
        }
    }
}

/// The notebook title for a file: its name without the extension.
fn title_from(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s).trim();
    let title: String = stem
        .chars()
        .filter(|c| !c.is_control())
        .take(penroam_core::validate::limits::MAX_TITLE_CHARS)
        .collect();
    if title.trim().is_empty() {
        "Imported notebook".into()
    } else {
        title
    }
}

/// Imports a file from another format into a new notebook, chosen by its extension. The
/// existing library isn't touched, and the report says what could not come across.
#[tauri::command(async)]
pub fn import_document(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    file_name: String,
) -> R<DocumentImport> {
    use penroam_interchange::{enex, jex, text_import, xournal};
    state.sync.nudge();
    let ext = file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let title = title_from(&file_name);
    let (assets, pages, report) = match ext.as_str() {
        "pdf" => pdf_document(&app, &path, &file_name)?,
        "xopp" | "xoj" | "enex" | "jex" | "md" | "markdown" | "txt" => {
            let limit = match ext.as_str() {
                "xopp" | "xoj" | "enex" | "jex" => 1 << 30,
                _ => penroam_interchange::limits::MAX_TEXT_FILE as u64,
            };
            let bytes = read_user_file(&app, &path, limit)?;
            let mut doc = match ext.as_str() {
                "xopp" | "xoj" => xournal::import_xournal(&bytes, &title)?,
                "enex" => enex::import_enex(&bytes, &title)?,
                "jex" => jex::import_jex(&bytes, &title)?,
                "md" | "markdown" => text_import::import_markdown(&bytes, &title)?,
                _ => text_import::import_plain_text(&bytes, &title)?,
            };
            // a PDF packaged with the document sits behind its pages, measured like any other
            let mut pdfs: std::collections::HashMap<String, Vec<penroam_pdf::PdfPageInfo>> =
                Default::default();
            for a in doc
                .assets
                .iter()
                .filter(|a| a.media_type == "application/pdf")
            {
                match penroam_pdf::inspect(&a.bytes) {
                    Ok(info) => {
                        pdfs.insert(a.sha256.clone(), info);
                    }
                    Err(e) => doc
                        .report
                        .lost
                        .push(format!("A PDF in this file can't be read: {e}")),
                }
            }
            let assets = doc
                .assets
                .into_iter()
                .map(|a| NewAsset {
                    bytes: a.bytes,
                    media_type: a.media_type,
                    name: None,
                })
                .collect();
            let pages = doc
                .pages
                .into_iter()
                .map(|p| {
                    let background = p.pdf.as_ref().and_then(|(hash, index)| {
                        Some(pdfs.get(hash)?.get(*index as usize)?.background(hash))
                    });
                    NewPage {
                        paper: if background.is_some() {
                            Paper::Pdf
                        } else {
                            Paper::Custom
                        },
                        size: background.as_ref().map_or(p.size, |b| b.display_size()),
                        template: p.template,
                        background,
                        elements: p.elements,
                    }
                })
                .collect();
            (assets, pages, doc.report)
        }
        _ => {
            return Err(invalid(
                "Penroam can import PDF, Xournal++, Evernote, Joplin, Markdown and text files",
            ))
        }
    };
    let notebook = lib!(state).create_notebook_with(&title, assets, pages)?;
    trim_heap();
    state.search.notebook_changed(notebook.id);
    Ok(DocumentImport { notebook, report })
}

/// A PDF as a new notebook: the file is kept as it is and each page sits on top of it.
fn pdf_document(
    app: &AppHandle,
    path: &str,
    file_name: &str,
) -> R<(Vec<NewAsset>, Vec<NewPage>, penroam_interchange::ImportLoss)> {
    let bytes = read_user_file(app, path, 1 << 30)?;
    let info = penroam_pdf::inspect(&bytes)?;
    let hash = penroam_storage::sha256_hex(&bytes);
    let pages: Vec<NewPage> = info
        .iter()
        .map(|i| {
            let bg = i.background(&hash);
            NewPage {
                paper: Paper::Pdf,
                size: bg.display_size(),
                template: Template {
                    kind: TemplateKind::Blank,
                    spacing: 24.0,
                },
                background: Some(bg),
                elements: Vec::new(),
            }
        })
        .collect();
    let report = penroam_interchange::ImportLoss {
        format: "PDF".into(),
        kept: penroam_interchange::Kept {
            pages: pages.len() as u32,
            ..Default::default()
        },
        lost: Vec::new(),
    };
    let asset = NewAsset {
        bytes,
        media_type: "application/pdf".into(),
        name: Some(file_name.to_string()),
    };
    Ok((vec![asset], pages, report))
}

/// Result of a text export. The text itself goes straight to the file.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextResult {
    pages: u32,
    text_blocks: u32,
    warnings: Vec<String>,
}

fn export_text_of(
    state: &State<'_, AppState>,
    notebook_id: Uuid,
    only: Option<Uuid>,
    format: penroam_interchange::text_export::TextFormat,
) -> R<penroam_interchange::text_export::TextExport> {
    let (title, pages) = {
        let mut lib = lib!(state);
        let nb = lib.notebook(notebook_id)?.clone();
        let ids: Vec<Uuid> = match only {
            Some(p) => vec![p],
            None => nb.page_order.clone(),
        };
        let mut pages = Vec::with_capacity(ids.len());
        for p in ids {
            pages.push(lib.load_page(notebook_id, p)?);
        }
        (nb.title, pages)
    };
    // read after the library is released: the index has its own lock
    Ok(penroam_interchange::text_export::export_text(
        &title,
        &pages,
        format,
        |p| state.search.page_text(notebook_id, p),
    ))
}

/// Exports a notebook's typed text as Markdown or plain text. Lossy: see the warnings.
#[tauri::command(async)]
pub fn export_notebook_text(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    path: String,
    markdown: bool,
) -> R<TextResult> {
    use penroam_interchange::text_export::TextFormat;
    let format = if markdown {
        TextFormat::Markdown
    } else {
        TextFormat::Plain
    };
    let r = export_text_of(&state, notebook_id, None, format)?;
    write_user_file(&app, &path, r.text.as_bytes())?;
    Ok(TextResult {
        pages: r.pages,
        text_blocks: r.text_blocks,
        warnings: r.warnings,
    })
}

/// The text of one page as plain text, for the clipboard.
#[tauri::command(async)]
pub fn page_plain_text(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
) -> R<penroam_interchange::text_export::TextExport> {
    use penroam_interchange::text_export::TextFormat;
    export_text_of(&state, notebook_id, Some(page_id), TextFormat::Plain)
}

/// A page as exports lay it out: an infinite canvas page becomes a sheet cut
/// to its content.
fn sheet_of(page: Page, infinite_canvas: bool) -> Page {
    if infinite_canvas {
        page.canvas_as_sheet(CANVAS_SHEET_MARGIN_PT)
    } else {
        page
    }
}

/// Exports one page as SVG and returns what the file doesn't show.
#[tauri::command(async)]
pub fn export_page_svg(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    path: String,
) -> R<Vec<String>> {
    let r = {
        let mut lib = lib!(state);
        let canvas = lib.notebook(notebook_id)?.infinite_canvas;
        let page = sheet_of(lib.load_page(notebook_id, page_id)?, canvas);
        let types: std::collections::HashMap<String, String> = lib
            .notebook(notebook_id)?
            .assets
            .iter()
            .map(|a| (a.sha256.clone(), a.media_type.clone()))
            .collect();
        let lib = &*lib;
        penroam_interchange::svg::page_to_svg(&page, |h| {
            Some((lib.read_asset(notebook_id, h).ok()?, types.get(h)?.clone()))
        })
    };
    write_user_file(&app, &path, r.svg.as_bytes())?;
    Ok(r.warnings)
}

/// Saves bytes the webview produced (a page rendered as PNG). Raw body; the destination is
/// the percent-encoded `x-path` header.
#[tauri::command(async)]
pub fn save_rendered_file(app: AppHandle, request: tauri::ipc::Request<'_>) -> R<()> {
    let path = request
        .headers()
        .get("x-path")
        .and_then(|v| v.to_str().ok())
        .map(percent_decode)
        .ok_or_else(|| invalid("missing x-path"))?;
    let owned;
    let bytes: &[u8] = match request.body() {
        tauri::ipc::InvokeBody::Raw(b) => b,
        tauri::ipc::InvokeBody::Json(serde_json::Value::Array(a)) => {
            owned = a
                .iter()
                .map(|v| v.as_u64().filter(|n| *n < 256).map(|n| n as u8))
                .collect::<Option<Vec<u8>>>()
                .ok_or_else(|| invalid("body is not a byte array"))?;
            &owned
        }
        _ => return Err(invalid("expected a file body")),
    };
    // the webview chooses the path, so it only gets to write what this command is for
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(invalid("expected a PNG"));
    }
    write_user_file(&app, &path, bytes)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%' && i + 2 < b.len())
            .then(|| std::str::from_utf8(&b[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One version offered in the history dialog. A version this device recorded has a
/// `revision`. Older ones, replayed from the account's operation log, have a `position`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRow {
    page_id: Uuid,
    revision: Option<u64>,
    position: Option<u64>,
    at: Timestamp,
    added: u32,
    removed: u32,
}

impl From<HistoryPoint> for HistoryRow {
    fn from(p: HistoryPoint) -> Self {
        Self {
            page_id: p.page_id,
            revision: Some(p.revision),
            position: None,
            at: p.at,
            added: p.added,
            removed: p.removed,
        }
    }
}

/// Restorable versions of one page, oldest first: what this device recorded, and before
/// that, with sync on, what the account's operation log holds. The log may reach further
/// back than this device does, for example on a replacement device. It isn't thinned, so
/// the same retention is applied to what is offered.
#[tauri::command(async)]
pub fn page_history(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
) -> R<Vec<HistoryRow>> {
    let local = lib!(state).page_history(notebook_id, page_id)?;
    let first_local = local.first().map(|p| p.at);
    let synced = crate::sync::page_versions(&state, notebook_id, page_id)?;
    let synced: Vec<_> = synced
        .into_iter()
        .filter(|v| first_local.is_none_or(|t| v.at < t))
        .collect();
    let times: Vec<Timestamp> = synced.iter().map(|v| v.at).collect();
    let keep = penroam_storage::history::retained(&times, now_ms(), &HistoryRetention::default());
    let mut rows: Vec<HistoryRow> = synced
        .into_iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(v, _)| HistoryRow {
            page_id,
            revision: None,
            position: Some(v.position),
            at: v.at,
            added: v.added,
            removed: v.removed,
        })
        .collect();
    rows.extend(local.into_iter().map(HistoryRow::from));
    Ok(rows)
}

/// A past version of a page, for preview. Changes nothing.
#[tauri::command(async)]
pub fn page_version(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    revision: Option<u64>,
    position: Option<u64>,
) -> R<Page> {
    version_of(&state, notebook_id, page_id, revision, position)
}

fn version_of(
    state: &State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    revision: Option<u64>,
    position: Option<u64>,
) -> R<Page> {
    match (revision, position) {
        (Some(r), _) => Ok(lib!(state).page_version(notebook_id, page_id, r)?),
        (None, Some(p)) => crate::sync::page_at(state, notebook_id, page_id, p)?
            .ok_or_else(|| invalid("that version isn't available")),
        (None, None) => Err(invalid("no version given")),
    }
}

/// Brings a page back to a past version with a new edit, which syncs and can be undone.
#[tauri::command(async)]
pub fn restore_page_version(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    revision: Option<u64>,
    position: Option<u64>,
) -> R<ApplyOutcome> {
    state.sync.nudge();
    let target = version_of(&state, notebook_id, page_id, revision, position)?;
    let r = lib!(state).restore_page_content(notebook_id, page_id, &target)?;
    state.search.page_changed(notebook_id, page_id);
    Ok(r)
}

/// Brings a whole notebook back to a past time. Pages made since go to the trash.
/// `from_account` asks for the state in the account's operation log, for times this
/// device has no record of.
#[tauri::command(async)]
pub fn restore_notebook_version(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    at: Timestamp,
    from_account: bool,
) -> R<NotebookRestore> {
    state.sync.nudge();
    let r = if from_account {
        let (nb, pages) = crate::sync::notebook_at(&state, notebook_id, at)?
            .ok_or_else(|| invalid("that version isn't available"))?;
        let targets = pages.into_iter().map(|p| (p.id, p)).collect();
        lib!(state).restore_notebook_content(notebook_id, &nb.title, &nb.page_order, &targets)?
    } else {
        lib!(state).restore_notebook_version(notebook_id, at)?
    };
    state.search.notebook_changed(notebook_id);
    Ok(r)
}

/// Bytes a notebook's history takes on this device.
#[tauri::command(async)]
pub fn history_stats(state: State<'_, AppState>, notebook_id: Uuid) -> R<HistoryStats> {
    Ok(lib!(state).history_stats(notebook_id)?)
}

/// Deletes a notebook's history on this device.
#[tauri::command(async)]
pub fn clear_history(state: State<'_, AppState>, notebook_id: Uuid) -> R<()> {
    Ok(lib!(state).clear_history(notebook_id)?)
}

/// Thins old versions in the background, one notebook at a time so the editor is
/// never kept waiting on the library for long.
pub fn start_history_retention(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        // leave the first moments after launch to the editor and to sync
        std::thread::sleep(std::time::Duration::from_secs(60));
        let state = app.state::<AppState>();
        let ids = lib_ids(&state);
        for id in ids {
            let retention = HistoryRetention::default();
            let Ok(mut lib) = state.lib.lock() else {
                return;
            };
            if let Err(e) = lib.compact_history(id, now_ms(), &retention) {
                lib.recovery.warnings.push(format!(
                    "old versions of notebook {id} were not thinned: {e}"
                ));
            }
        }
    });
}

fn lib_ids(state: &State<'_, AppState>) -> Vec<Uuid> {
    state
        .lib
        .lock()
        .map(|l| l.notebook_ids())
        .unwrap_or_default()
}

/// Imports a `.penroam` archive (≤ 2 GiB) as a new notebook.
#[tauri::command(async)]
pub fn import_notebook(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> R<ImportReport> {
    state.sync.nudge();
    let bytes = read_user_file(&app, &path, MAX_IMPORT_BYTES)?;
    let r = lib!(state).import_archive(std::io::Cursor::new(bytes));
    trim_heap();
    let r = r?;
    state.search.notebook_changed(r.notebook.id);
    Ok(r)
}

/// Renders a notebook's live pages as a PDF with vector ink and text over the original PDF
/// backgrounds; an infinite canvas page becomes as many PDF pages as it needs. Returns, for
/// each PDF page, the notebook page it shows, and holds the library lock only for the
/// duration of this call.
fn render_pdf(
    state: &State<'_, AppState>,
    notebook_id: Uuid,
    math: &[penroam_pdf::RenderedMath],
) -> R<(Vec<u8>, penroam_pdf::ExportReport, Vec<Uuid>)> {
    let mut lib = lib!(state);
    let nb = lib.notebook(notebook_id)?;
    let (ids, canvas) = (nb.page_order.clone(), nb.infinite_canvas);
    let paper = nb.defaults.paper.size().unwrap_or(Size {
        width: 612.0,
        height: 792.0,
    });
    let mut pages = Vec::with_capacity(ids.len());
    // one entry per PDF page: a canvas can take several
    let mut order = Vec::with_capacity(ids.len());
    for p in &ids {
        let page = lib.load_page(notebook_id, *p)?;
        if canvas {
            for sheet in page.canvas_as_sheets(CANVAS_SHEET_MARGIN_PT, paper) {
                pages.push(sheet);
                order.push(*p);
            }
        } else {
            pages.push(page);
            order.push(*p);
        }
    }
    let lib = &*lib;
    let (bytes, report) =
        penroam_pdf::export_annotated(&pages, math, |h| lib.read_asset(notebook_id, h).ok())?;
    Ok((bytes, report, order))
}

/// The distinct formulas on a notebook's live pages. The webview draws these
/// and passes them to [`export_annotated_pdf`] and [`print_notebook`].
#[tauri::command(async)]
pub fn notebook_math(
    state: State<'_, AppState>,
    notebook_id: Uuid,
) -> R<Vec<penroam_core::markdown::MathSpan>> {
    let mut lib = lib!(state);
    let order = lib.notebook(notebook_id)?.page_order.clone();
    let mut seen = std::collections::HashSet::new();
    for p in order {
        for e in lib.load_page(notebook_id, p)?.elements {
            if let penroam_core::Element::Text(t) = e {
                seen.extend(penroam_core::markdown::math_in(&t.text));
            }
        }
    }
    Ok(seen.into_iter().collect())
}

/// Exports the live pages as a PDF with vector ink and text over the original
/// PDF backgrounds.
#[tauri::command(async)]
pub fn export_annotated_pdf(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    path: String,
    math: Vec<penroam_pdf::RenderedMath>,
) -> R<penroam_pdf::ExportReport> {
    let (bytes, report, _) = render_pdf(&state, notebook_id, &math)?;
    write_user_file(&app, &path, &bytes)?;
    Ok(report)
}

/// How a print request ended.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PrintStatus {
    /// The document reached the print system.
    #[cfg(any(target_os = "linux", mobile))]
    Sent,
    /// The user closed the print dialog.
    #[cfg(any(target_os = "linux", mobile))]
    Cancelled,
    /// The system print UI took over and does not report back.
    #[cfg(any(target_os = "linux", mobile))]
    SystemDialog,
    /// The PDF opened in the default viewer, to be printed from there.
    #[cfg(not(any(target_os = "linux", mobile)))]
    Viewer,
}

/// Result of [`print_notebook`].
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrintResult {
    status: PrintStatus,
    report: penroam_pdf::ExportReport,
}

/// Prints a notebook: renders it the way [`export_annotated_pdf`] does, then gives the PDF to
/// the system print dialog, where the user can pick pages. `current_page` is the page on screen,
/// offered as a choice in dialogs that have one. Blocks until the dialog is finished with, so
/// the library lock must not be held across that step.
#[tauri::command(async)]
pub fn print_notebook(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    current_page: Option<Uuid>,
    math: Vec<penroam_pdf::RenderedMath>,
) -> R<PrintResult> {
    let (bytes, report, order) = render_pdf(&state, notebook_id, &math)?;
    let (file, title) = {
        let lib = lib!(state);
        // The staging folder sits with the notes themselves and is emptied on every launch, which
        // catches files a viewer still needed or a crash left behind.
        let file = lib
            .staging_dir()
            .join(format!("print-{}.pdf", Uuid::new_v4()));
        (file, lib.notebook(notebook_id)?.title.clone())
    };
    std::fs::write(&file, &bytes).map_err(io_err)?;
    drop(bytes);
    trim_heap();
    let job = tauri_plugin_native_print::PrintJob {
        file,
        title,
        pages: order.len() as u32,
        current_page: current_page
            .and_then(|p| order.iter().position(|o| *o == p))
            .map(|i| i as u32),
    };
    Ok(PrintResult {
        status: print_job(&window, job)?,
        report,
    })
}

/// Linux, Android and iOS have a print dialog in `tauri-plugin-native-print`.
#[cfg(any(target_os = "linux", mobile))]
fn print_job(
    window: &tauri::WebviewWindow,
    job: tauri_plugin_native_print::PrintJob,
) -> R<PrintStatus> {
    use tauri_plugin_native_print::{print, PrintOutcome};
    let outcome = print(window, job).map_err(|message| CmdError {
        code: ErrorCode::Io,
        message,
    })?;
    Ok(match outcome {
        PrintOutcome::Sent => PrintStatus::Sent,
        PrintOutcome::Cancelled => PrintStatus::Cancelled,
        PrintOutcome::SystemDialog => PrintStatus::SystemDialog,
    })
}

/// Elsewhere the default PDF viewer prints. The viewer keeps reading the file, so it stays until
/// the staging folder is emptied on the next launch.
#[cfg(not(any(target_os = "linux", mobile)))]
fn print_job(
    _window: &tauri::WebviewWindow,
    job: tauri_plugin_native_print::PrintJob,
) -> R<PrintStatus> {
    tauri_plugin_opener::open_path(&job.file, None::<&str>).map_err(|e| CmdError {
        code: ErrorCode::Io,
        message: e.to_string(),
    })?;
    Ok(PrintStatus::Viewer)
}

/// Link schemes [`open_link`] hands to the system. Anything else (`file:`, custom app schemes)
/// could run or open something local, and notes can come from other people through sync.
const LINK_SCHEMES: [&str; 3] = ["http", "https", "mailto"];

/// Checks a link from a text block and returns it normalised, or why it can't be opened.
fn checked_link(raw: &str) -> Result<String, String> {
    let url = url::Url::parse(raw.trim()).map_err(|e| format!("not a valid address: {e}"))?;
    if !LINK_SCHEMES.contains(&url.scheme()) {
        return Err(format!("{}: links are not opened", url.scheme()));
    }
    Ok(url.into())
}

/// Opens a link the user tapped in a text block, in the system browser or mail app. Only
/// http, https and mailto links are opened; others fail with [`ErrorCode::Invalid`].
#[tauri::command]
pub fn open_link(app: AppHandle, url: String) -> R<()> {
    use tauri_plugin_opener::OpenerExt;
    let url = checked_link(&url).map_err(|message| CmdError {
        code: ErrorCode::Invalid,
        message,
    })?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| CmdError {
            code: ErrorCode::Platform,
            message: format!("could not open the link: {e}"),
        })
}

#[cfg(test)]
mod link_tests {
    use super::checked_link;

    #[test]
    fn opens_web_and_mail_links_only() {
        assert_eq!(
            checked_link("https://penroam.ink/a b").unwrap(),
            "https://penroam.ink/a%20b"
        );
        assert!(checked_link("mailto:someone@example.com").is_ok());
        assert!(checked_link("HTTP://example.com").is_ok());
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ink.penroam.app:x",
            "example.com",
            "",
        ] {
            assert!(checked_link(bad).is_err(), "{bad}");
        }
    }
}

/// Imports a canonical stroke document (`m00-strokes` v1) onto a page.
/// Used by the benchmark harness and for bringing native test captures forward.
#[tauri::command(async)]
pub fn import_stroke_document(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    json: String,
) -> R<ApplyOutcome> {
    state.sync.nudge();
    let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| invalid(e.to_string()))?;
    drop(json);
    let mut lib = lib!(state);
    let (rev, z) = lib.with_page(notebook_id, page_id, |p| (p.revision, p.max_z()))?;
    let elements = penroam_core::migrate::stroke_doc_to_elements(&v, z + 1)
        .map_err(|e| invalid(e.to_string()))?;
    drop(v);
    let batch = OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: rev,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind: OpKind::AddElements { elements },
        }],
    };
    let r = lib.apply_page_operations(notebook_id, page_id, &batch);
    drop(batch);
    drop(lib);
    trim_heap();
    let r = r?;
    state.search.page_changed(notebook_id, page_id);
    Ok(r)
}

/// Flushes and exits (the editor calls this after saving on window close).
#[tauri::command(async)]
pub fn exit_app(app: AppHandle) {
    flush(&app);
    app.exit(0);
}
