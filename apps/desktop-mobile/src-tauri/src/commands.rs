use crate::AppState;
use penroam_core::*;
use penroam_storage::{
    ApplyOutcome, ErrorCode, ImportReport, NotebookSummary, RecoveryReport, StorageError,
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
    pub code: ErrorCode,
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

fn io_err(e: std::io::Error) -> CmdError {
    StorageError::from(e).into()
}

fn invalid(msg: impl Into<String>) -> CmdError {
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

pub fn flush(app: &AppHandle) {
    if let Some(s) = app.try_state::<AppState>() {
        if let Ok(mut l) = s.lib.lock() {
            if let Err(e) = l.flush_all() {
                eprintln!("penroam: flush failed: {e}");
            }
        }
    }
}

const GENERATOR: &str = concat!("Penroam ", env!("CARGO_PKG_VERSION"));
const MAX_IMPORT_BYTES: u64 = 2 << 30;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookAndPage {
    notebook: Notebook,
    page: Page,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookAndPages {
    notebook: Notebook,
    pages: Vec<Page>,
}

#[tauri::command(async)]
pub fn library_status(state: State<'_, AppState>) -> R<RecoveryReport> {
    Ok(lib!(state).recovery.clone())
}

#[tauri::command(async)]
pub fn list_notebooks(state: State<'_, AppState>) -> R<Vec<NotebookSummary>> {
    Ok(lib!(state).list_notebooks())
}

#[tauri::command(async)]
pub fn get_notebook(state: State<'_, AppState>, id: Uuid) -> R<Notebook> {
    Ok(lib!(state).notebook(id)?.clone())
}

#[tauri::command(async)]
pub fn create_notebook(
    state: State<'_, AppState>,
    title: String,
    paper: Option<Paper>,
    template: Option<Template>,
    cover: Option<String>,
) -> R<NotebookAndPage> {
    let defaults = PageDefaults {
        paper: paper.unwrap_or(Paper::Letter),
        template: template.unwrap_or_default(),
    };
    if !matches!(defaults.paper, Paper::Letter | Paper::A4) {
        return Err(invalid("new notebooks use Letter or A4 pages"));
    }
    let (notebook, page) = lib!(state).create_notebook(&title, defaults, cover)?;
    Ok(NotebookAndPage { notebook, page })
}

#[tauri::command(async)]
pub fn rename_notebook(state: State<'_, AppState>, id: Uuid, title: String) -> R<Notebook> {
    Ok(lib!(state).rename_notebook(id, &title)?)
}

#[tauri::command(async)]
pub fn set_notebook_cover(state: State<'_, AppState>, id: Uuid, cover: String) -> R<Notebook> {
    Ok(lib!(state).set_notebook_cover(id, &cover)?)
}

#[tauri::command(async)]
pub fn delete_notebook(state: State<'_, AppState>, id: Uuid) -> R<Notebook> {
    Ok(lib!(state).delete_notebook(id)?)
}

#[tauri::command(async)]
pub fn restore_notebook(state: State<'_, AppState>, id: Uuid) -> R<Notebook> {
    Ok(lib!(state).restore_notebook(id)?)
}

#[tauri::command(async)]
pub fn purge_notebook(state: State<'_, AppState>, id: Uuid) -> R<()> {
    Ok(lib!(state).purge_notebook(id)?)
}

#[tauri::command(async)]
pub fn create_page(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    index: Option<usize>,
    template: Option<Template>,
    paper: Option<Paper>,
    size: Option<Size>,
) -> R<NotebookAndPage> {
    let mut lib = lib!(state);
    if let Some(size) = size {
        // explicit size (custom paper): used by the benchmark harness
        let t = template.unwrap_or(Template {
            kind: TemplateKind::Blank,
            spacing: 24.0,
        });
        let (notebook, mut pages) =
            lib.create_pages(notebook_id, index, vec![(Paper::Custom, size, t, None)])?;
        return Ok(NotebookAndPage {
            notebook,
            page: pages.remove(0),
        });
    }
    let (notebook, page) = lib.create_page(notebook_id, index, template, paper)?;
    Ok(NotebookAndPage { notebook, page })
}

#[tauri::command(async)]
pub fn update_page_order(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    order: Vec<Uuid>,
) -> R<Notebook> {
    Ok(lib!(state).update_page_order(notebook_id, order)?)
}

#[tauri::command(async)]
pub fn delete_page(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<Notebook> {
    Ok(lib!(state).delete_page(notebook_id, page_id)?)
}

#[tauri::command(async)]
pub fn restore_page(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<Notebook> {
    Ok(lib!(state).restore_page(notebook_id, page_id)?)
}

/// Returns the page as raw JSON bytes (avoids a second serde pass for large pages).
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

#[tauri::command(async)]
pub fn apply_page_operations(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    batch: OperationBatch,
) -> R<ApplyOutcome> {
    Ok(lib!(state).apply_page_operations(notebook_id, page_id, &batch)?)
}

#[tauri::command(async)]
pub fn flush_page(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<()> {
    Ok(lib!(state).flush_page(notebook_id, page_id)?)
}

#[tauri::command(async)]
pub fn flush_all(state: State<'_, AppState>) -> R<()> {
    Ok(lib!(state).flush_all()?)
}

#[tauri::command(async)]
pub fn read_asset(state: State<'_, AppState>, notebook_id: Uuid, hash: String) -> R<Response> {
    Ok(Response::new(lib!(state).read_asset(notebook_id, &hash)?))
}

/// Raw PNG body; ids in headers (`x-notebook`, `x-page`).
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

#[tauri::command(async)]
pub fn get_thumbnail(state: State<'_, AppState>, notebook_id: Uuid, page_id: Uuid) -> R<Response> {
    Ok(Response::new(
        lib!(state)
            .thumbnail(notebook_id, page_id)
            .unwrap_or_default(),
    ))
}

fn file_path(s: &str) -> R<FilePath> {
    FilePath::from_str(s).map_err(|_| invalid("invalid file location"))
}

/// Reads a user-chosen file (path or content URI) with a size limit.
fn read_user_file(app: &AppHandle, path: &str, limit: u64) -> R<Vec<u8>> {
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
fn write_user_file(app: &AppHandle, path: &str, bytes: &[u8]) -> R<()> {
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

#[tauri::command(async)]
pub fn import_pdf(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    path: String,
    file_name: Option<String>,
    index: Option<usize>,
) -> R<NotebookAndPages> {
    let bytes = read_user_file(&app, &path, 1 << 30)?;
    if !bytes.starts_with(b"%PDF") && !bytes.windows(5).take(1024).any(|w| w == b"%PDF-") {
        return Err(CmdError {
            code: ErrorCode::Pdf,
            message: "this file is not a PDF".into(),
        });
    }
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
    Ok(NotebookAndPages { notebook, pages })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    bytes: u64,
    pages: u32,
}

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

#[tauri::command(async)]
pub fn import_notebook(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> R<ImportReport> {
    let bytes = read_user_file(&app, &path, MAX_IMPORT_BYTES)?;
    let r = lib!(state).import_archive(std::io::Cursor::new(bytes));
    trim_heap();
    Ok(r?)
}

#[tauri::command(async)]
pub fn export_annotated_pdf(
    app: AppHandle,
    state: State<'_, AppState>,
    notebook_id: Uuid,
    path: String,
) -> R<penroam_pdf::ExportReport> {
    let (bytes, report) = {
        let mut lib = lib!(state);
        let order = lib.notebook(notebook_id)?.page_order.clone();
        let mut pages = Vec::with_capacity(order.len());
        for p in order {
            pages.push(lib.load_page(notebook_id, p)?);
        }
        let lib = &*lib;
        penroam_pdf::export_annotated(&pages, |h| lib.read_asset(notebook_id, h).ok())?
    };
    write_user_file(&app, &path, &bytes)?;
    Ok(report)
}

/// Imports an M00 canonical stroke document (`m00-strokes` v1) onto a page.
/// Used by the benchmark harness and for bringing M00 test captures forward.
#[tauri::command(async)]
pub fn import_m00_strokes(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    json: String,
) -> R<ApplyOutcome> {
    let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| invalid(e.to_string()))?;
    drop(json);
    let mut lib = lib!(state);
    let (rev, z) = lib.with_page(notebook_id, page_id, |p| (p.revision, p.max_z()))?;
    let elements = penroam_core::migrate::m00_strokes_to_elements(&v, z + 1)
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
    trim_heap();
    Ok(r?)
}

#[tauri::command(async)]
pub fn exit_app(app: AppHandle) {
    flush(&app);
    app.exit(0);
}
