//! Penroam Tauri shell. Rust owns the notebook model and persistence
//! (`penroam-storage`); the Vue frontend owns UI, editor state and the web ink
//! renderer. Commands are thin typed wrappers with structured errors.
//!
//! The renderer never sees library paths. Import/export take a path or
//! content URI that the user chose in a system file dialog (passed through
//! `tauri-plugin-fs`, which also handles Android `content://` URIs).

// Release builds must not contain DEBUG/TRACE `log` records: the sync HTTP client logs raw
// requests (bearer tokens included) at TRACE. The cap comes from the `log` feature
// `release_max_level_info` in Cargo.toml; this fails the build if it is ever lost.
#[cfg(not(debug_assertions))]
const _: () = assert!(
    matches!(
        log::STATIC_MAX_LEVEL,
        log::LevelFilter::Off
            | log::LevelFilter::Error
            | log::LevelFilter::Warn
            | log::LevelFilter::Info
    ),
    "release builds must compile out DEBUG/TRACE logging"
);

mod backup;
mod bench;
mod commands;
mod desktop_entry;
mod diagnostics;
mod models;
#[cfg(target_os = "linux")]
mod pinch;
mod power;
mod search;
mod sync;
mod updates;

use penroam_storage::Library;
use std::sync::Mutex;
use tauri::{Emitter, Manager};

/// State shared by every command.
pub struct AppState {
    /// The open library. One lock serialises all storage access; the sync
    /// worker takes it only for its local phases.
    pub lib: Mutex<Library>,
    /// Background sync worker, if set up.
    pub sync: sync::SyncSlot,
    /// Background search indexing worker.
    pub search: search::SearchSlot,
    /// Handwriting model downloads.
    pub models: models::ModelsSlot,
}

/// App entry point (desktop `main` and the mobile entry point): opens the
/// library (running crash recovery), resumes sync if it was set up, wires the
/// close handler that lets the editor flush first, and registers commands.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    bench::mark_start();
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_native_ink::init())
        .plugin(tauri_plugin_native_print::init())
        .plugin(tauri_plugin_store_billing::init())
        // used from Rust only (system-browser sign-in, PDFs for printing on Windows and macOS);
        // the WebView gets no opener permission
        .plugin(tauri_plugin_opener::init());
    // Mobile sign-in: the identity provider answers with an `ink.penroam.app:` link, which
    // the operating system hands to this app (RFC 8252 §7.1). Desktop uses a loopback listener.
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_deep_link::init());
    // Desktop only: stores and sideloaded APKs update mobile builds (see updates.rs).
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    builder
        .setup(|app| {
            #[cfg(mobile)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                app.deep_link().on_open_url(|event| {
                    for url in event.urls() {
                        sync::deliver_sign_in_link(url.as_str());
                    }
                });
            }
            let root = std::env::var("PENROAM_LIBRARY_DIR")
                .map(std::path::PathBuf::from)
                .or_else(|_| app.path().app_data_dir().map(|d| d.join("library")))?;
            // Beside the library rather than in it: a report is not notebook data.
            diagnostics::install(root.with_file_name("crash-reports"));
            // a locked vault's leftover plaintext must be gone before the library loads it
            sync::recover_vaults_before_open(&root);
            let lib = Library::open(&root)?;
            if !lib.recovery.warnings.is_empty() {
                eprintln!("penroam: recovery: {:?}", lib.recovery);
            }
            app.manage(AppState {
                lib: Mutex::new(lib),
                sync: sync::SyncSlot::default(),
                search: search::SearchSlot::default(),
                models: models::ModelsSlot::default(),
            });
            #[cfg(target_os = "linux")]
            if let Some(window) = app.get_webview_window("main") {
                pinch::install(&window)?;
            }
            search::start(app.handle());
            sync::recover_vaults_after_open(app.handle());
            models::start(app.handle());
            sync::resume_on_launch(app.handle());
            commands::start_history_retention(app.handle());
            desktop_entry::install_on_launch(app.handle());
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
            commands::export_pages,
            backup::export_backup,
            backup::preview_backup,
            backup::restore_backup,
            backup::auto_backup,
            backup::webdav_connect,
            backup::webdav_disconnect,
            backup::webdav_backups,
            backup::webdav_fetch,
            backup::discard_fetched,
            commands::inspect_notebook,
            commands::import_document,
            commands::export_notebook_text,
            commands::page_plain_text,
            commands::export_page_svg,
            commands::save_rendered_file,
            commands::page_history,
            commands::page_version,
            commands::restore_page_version,
            commands::restore_notebook_version,
            commands::history_stats,
            commands::clear_history,
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
            commands::notebook_math,
            commands::print_notebook,
            commands::open_link,
            commands::import_stroke_document,
            commands::exit_app,
            sync::sync_info,
            sync::sync_server_info,
            sync::sync_sign_in,
            sync::sync_sign_out,
            sync::sync_connect,
            sync::sync_vaults,
            sync::sync_vault_create,
            sync::sync_vault_unlock,
            sync::sync_vault_set_passphrase,
            sync::sync_vault_stop_sync,
            sync::sync_vault_restore,
            sync::sync_vault_lock,
            sync::sync_vault_forget,
            sync::vault_prefs,
            sync::vault_set_prefs,
            commands::move_notebook_to_vault,
            sync::sync_remove_device,
            sync::sync_rename_device,
            sync::sync_erased_notebooks,
            sync::sync_restore_erased,
            sync::sync_devices,
            sync::sync_now,
            sync::sync_pause,
            sync::sync_conflicts,
            sync::sync_resolve_conflict,
            sync::sync_diagnostics,
            sync::sync_disconnect,
            sync::sync_account,
            sync::sync_open_billing,
            sync::sync_store_purchase,
            sync::sync_store_restore,
            sync::sync_store_reconcile,
            sync::sync_request_account_deletion,
            sync::sync_cancel_account_deletion,
            diagnostics::diagnostics_report,
            diagnostics::diagnostics_ui_error,
            diagnostics::diagnostics_clear,
            diagnostics::third_party_notices,
            diagnostics::open_help,
            updates::update_support,
            updates::update_check,
            updates::update_install,
            power::power_report,
            desktop_entry::desktop_entry_status,
            desktop_entry::desktop_entry_install,
            desktop_entry::desktop_entry_remove,
            search::search_query,
            search::search_status,
            search::search_set_settings,
            search::search_rebuild,
            search::search_correct,
            search::search_inspect_model,
            search::search_install_model,
            search::search_remove_model,
            models::models_status,
            models::models_check,
            models::models_download,
            models::models_cancel,
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
                sync::lock_vaults_on_exit(app);
            }
        });
}
