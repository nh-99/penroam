//! Penroam Tauri shell. Rust owns the notebook model and persistence
//! (`penroam-storage`); the Vue frontend owns UI, editor state and the web ink
//! renderer. Commands are thin typed wrappers with structured errors.
//!
//! The renderer never sees library paths. Import/export take a path or
//! content URI that the user chose in a system file dialog (passed through
//! `tauri-plugin-fs`, which also handles Android `content://` URIs).

mod bench;
mod commands;
mod sync;

use penroam_storage::Library;
use std::sync::Mutex;
use tauri::{Emitter, Manager};

/// State shared by every command.
pub struct AppState {
    /// The open library. One lock serialises all storage access; the sync
    /// worker takes it only for its local phases.
    pub lib: Mutex<Library>,
    /// Background sync worker (developer preview), if set up.
    pub sync: sync::SyncSlot,
}

/// App entry point (desktop `main` and the mobile entry point): opens the
/// library (running crash recovery), resumes sync if it was set up, wires the
/// close handler that lets the editor flush first, and registers commands.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    bench::mark_start();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_native_ink::init())
        .setup(|app| {
            let root = std::env::var("PENROAM_LIBRARY_DIR")
                .map(std::path::PathBuf::from)
                .or_else(|_| app.path().app_data_dir().map(|d| d.join("library")))?;
            let lib = Library::open(&root)?;
            if !lib.recovery.warnings.is_empty() {
                eprintln!("penroam: recovery: {:?}", lib.recovery);
            }
            app.manage(AppState {
                lib: Mutex::new(lib),
                sync: sync::SyncSlot::default(),
            });
            sync::resume_on_launch(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Let the editor flush pending edits first; it calls `exit_app`.
                // A watchdog exits anyway (after flushing Rust state) if the UI is stuck.
                api.prevent_close();
                let _ = window.emit("penroam://close-requested", ());
                let app = window.app_handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(6));
                    commands::flush(&app);
                    app.exit(0);
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::library_status,
            commands::list_notebooks,
            commands::get_notebook,
            commands::create_notebook,
            commands::rename_notebook,
            commands::set_notebook_cover,
            commands::delete_notebook,
            commands::restore_notebook,
            commands::purge_notebook,
            commands::create_page,
            commands::update_page_order,
            commands::delete_page,
            commands::restore_page,
            commands::load_page,
            commands::apply_page_operations,
            commands::flush_page,
            commands::flush_all,
            commands::read_asset,
            commands::save_thumbnail,
            commands::get_thumbnail,
            commands::import_pdf,
            commands::export_notebook,
            commands::import_notebook,
            commands::export_annotated_pdf,
            commands::import_stroke_document,
            commands::exit_app,
            sync::sync_info,
            sync::sync_setup_first,
            sync::sync_setup_pair,
            sync::sync_setup_recovery,
            sync::sync_poll_pairing,
            sync::sync_approve_device,
            sync::sync_remove_device,
            sync::sync_devices,
            sync::sync_now,
            sync::sync_pause,
            sync::sync_conflicts,
            sync::sync_resolve_conflict,
            sync::sync_diagnostics,
            sync::sync_disconnect,
            bench::system_info,
            bench::proc_stats,
            bench::startup_ms,
            bench::write_perf,
            bench::autobench_requested,
            bench::log_line,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Penroam")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                commands::flush(app);
            }
        });
}
