//! Hands a finished PDF to the operating system's print dialog.
//!
//! Penroam prints the PDF that `penroam_pdf::export_annotated` produces, so a printout matches
//! an exported PDF and ink stays vector. This plugin covers only the last step:
//!
//! * Linux: GTK's print dialog, which sends the PDF to CUPS. The dialog offers page
//!   ranges and CUPS does not apply them to a finished PDF, so the chosen pages are cut out here.
//! * Android: the system print UI through `PrintManager` (Kotlin `NativePrintPlugin`).
//! * iOS/iPadOS: `UIPrintInteractionController` (Swift `NativePrintPlugin`).
//!
//! There is no print dialog here for Windows and macOS. The app opens the PDF in the default
//! viewer there; see `print_notebook` in the app crate.
//!
//! The plugin exposes no commands. The only entry point is [`print()`], called from Rust, so the
//! WebView can never ask for an arbitrary file to be read and printed.
//!
//! See `docs/architecture/overview.md` §8.

#![warn(missing_docs)]

#[cfg(target_os = "linux")]
mod linux;

use std::path::PathBuf;
#[cfg(mobile)]
use tauri::Manager;
use tauri::{
    plugin::{Builder, TauriPlugin},
    Runtime,
};

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_native_print);

/// One document to print.
#[derive(Debug, Clone)]
pub struct PrintJob {
    /// A PDF written by `penroam_pdf::export_annotated`. [`print()`] takes ownership of this file
    /// and deletes it once the operating system no longer needs it.
    pub file: PathBuf,
    /// Shown by the dialog and in the print queue.
    pub title: String,
    /// Pages in `file`.
    pub pages: u32,
    /// Zero-based page the user is looking at, which enables the dialog's "current page" choice
    /// where it has one.
    pub current_page: Option<u32>,
}

/// How a call to [`print()`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintOutcome {
    /// The document reached the print system.
    Sent,
    /// The user closed the dialog without printing.
    Cancelled,
    /// The system print UI is open and takes over from here; its result is not reported
    /// (Android).
    SystemDialog,
}

/// Handle to the registered Kotlin/Swift plugin instance.
#[cfg(mobile)]
struct Native<R: Runtime>(tauri::plugin::PluginHandle<R>);

/// Answer from the native `printPdf` method.
#[cfg(mobile)]
#[derive(serde::Deserialize)]
struct NativeAnswer {
    status: String,
}

/// Shows the system print dialog for `job` and blocks until the job is sent, cancelled or
/// handed over, so call it off the main thread and without holding locks. `window` is the
/// dialog's parent.
///
/// # Errors
/// A user-readable message when no print system is available or the job could not be sent. The
/// file in `job` is deleted on every path except a mobile bridge failure, where the library's
/// staging folder clears it on the next launch.
#[cfg(any(target_os = "linux", mobile))]
pub fn print<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    job: PrintJob,
) -> Result<PrintOutcome, String> {
    #[cfg(target_os = "linux")]
    {
        linux::print(window, job)
    }
    #[cfg(mobile)]
    {
        let answer: NativeAnswer = window
            .state::<Native<R>>()
            .0
            .run_mobile_plugin(
                "printPdf",
                serde_json::json!({ "path": job.file, "title": job.title, "pages": job.pages }),
            )
            .map_err(|e| e.to_string())?;
        Ok(match answer.status.as_str() {
            "sent" => PrintOutcome::Sent,
            "cancelled" => PrintOutcome::Cancelled,
            _ => PrintOutcome::SystemDialog,
        })
    }
}

/// The plugin (`native-print`). Registers the native class on Android and iOS and does nothing
/// on desktop.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("native-print")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                let handle =
                    _api.register_android_plugin("ink.penroam.nativeprint", "NativePrintPlugin")?;
                _app.manage(Native(handle));
            }
            #[cfg(target_os = "ios")]
            {
                let handle = _api.register_ios_plugin(init_plugin_native_print)?;
                _app.manage(Native(handle));
            }
            Ok(())
        })
        .build()
}
