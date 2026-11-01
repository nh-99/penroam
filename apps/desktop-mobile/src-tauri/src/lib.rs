//! Penroam Tauri shell. Rust owns the notebook model and persistence
//! (`penroam-storage`); the Vue frontend owns UI, editor state and the web ink
//! renderer. Commands are thin typed wrappers with structured errors.
//!
//! The renderer never sees library paths. Import/export take a path or
//! content URI that the user chose in a system file dialog (passed through
//! `tauri-plugin-fs`, which also handles Android `content://` URIs).

mod bench;
mod commands;

use penroam_storage::Library;
use std::sync::Mutex;
use tauri::{Emitter, Manager};

pub struct AppState {
    pub lib: Mutex<Library>,
}

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
            app.manage(AppState { lib: Mutex::new(lib) });
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
            commands::import_m00_strokes,
            commands::exit_app,
            bench::system_info,
            bench::proc_stats,
            bench::startup_ms,
            bench::write_perf,
            bench::autobench_requested,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Penroam")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                commands::flush(app);
            }
        });
}
