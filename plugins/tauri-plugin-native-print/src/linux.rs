//! The GTK print dialog.
//!
//! `GtkPrintUnixDialog` and `GtkPrintJob` belong to GTK's separate unix-print API, which the
//! `gtk` crate does not bind. Their symbols are in the libgtk-3 that Tauri already links, so the
//! few functions used here are declared by hand and wrapped as ordinary GObject types.
//!
//! All GTK calls run on the main thread. [`print`] blocks its caller on a channel until the job
//! is finished, and the dialog's callbacks answer on that channel.
//!
//! A job built from a finished file skips the page selection GTK does for rendered jobs, and
//! CUPS is not told about the range either. So when the user picks a range or the current page
//! the file is cut down to those pages first, off the main thread, and the settings are reset to
//! "all". Copies, collation, reverse order, odd/even and pages per sheet stay with CUPS.

use super::{PrintJob, PrintOutcome};
use gtk::prelude::*;
use gtk::{glib, glib::translate::*, PageRange, PrintPages, PrintSettings};
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc::{channel, Sender};
use tauri::Runtime;

mod ffi {
    use super::*;
    use gtk::ffi::{GtkPageSetup, GtkPrintSettings, GtkWidget, GtkWindow};
    use gtk::glib::{ffi::GError, ffi::GType};

    #[repr(C)]
    pub struct GtkPrintUnixDialog(c_void);
    #[repr(C)]
    pub struct GtkPrinter(c_void);
    #[repr(C)]
    pub struct GtkPrintJob(c_void);

    /// `GTK_PRINT_CAPABILITY_GENERATE_PDF`: the document is a PDF, so GTK only offers printers
    /// that accept one. Every other capability is left to the print system.
    pub const GENERATE_PDF: c_uint = 1 << 5;

    pub type CompleteFunc =
        unsafe extern "C" fn(job: *mut GtkPrintJob, data: *mut c_void, error: *const GError);

    extern "C" {
        pub fn gtk_print_unix_dialog_get_type() -> GType;
        pub fn gtk_printer_get_type() -> GType;
        pub fn gtk_print_job_get_type() -> GType;
        pub fn gtk_print_unix_dialog_new(
            title: *const c_char,
            parent: *mut GtkWindow,
        ) -> *mut GtkWidget;
        pub fn gtk_print_unix_dialog_set_manual_capabilities(
            dialog: *mut GtkPrintUnixDialog,
            capabilities: c_uint,
        );
        pub fn gtk_print_unix_dialog_set_current_page(
            dialog: *mut GtkPrintUnixDialog,
            current_page: c_int,
        );
        pub fn gtk_print_unix_dialog_set_settings(
            dialog: *mut GtkPrintUnixDialog,
            settings: *mut GtkPrintSettings,
        );
        pub fn gtk_print_unix_dialog_get_settings(
            dialog: *mut GtkPrintUnixDialog,
        ) -> *mut GtkPrintSettings;
        pub fn gtk_print_unix_dialog_get_selected_printer(
            dialog: *mut GtkPrintUnixDialog,
        ) -> *mut GtkPrinter;
        pub fn gtk_print_unix_dialog_get_page_setup(
            dialog: *mut GtkPrintUnixDialog,
        ) -> *mut GtkPageSetup;
        pub fn gtk_print_job_new(
            title: *const c_char,
            printer: *mut GtkPrinter,
            settings: *mut GtkPrintSettings,
            page_setup: *mut GtkPageSetup,
        ) -> *mut GtkPrintJob;
        pub fn gtk_print_job_set_source_file(
            job: *mut GtkPrintJob,
            filename: *const c_char,
            error: *mut *mut GError,
        ) -> glib::ffi::gboolean;
        pub fn gtk_print_job_send(
            job: *mut GtkPrintJob,
            callback: CompleteFunc,
            data: *mut c_void,
            dnotify: Option<unsafe extern "C" fn(*mut c_void)>,
        );
    }
}

glib::wrapper! {
    struct PrintUnixDialog(Object<ffi::GtkPrintUnixDialog>)
        @extends gtk::Dialog, gtk::Window, gtk::Bin, gtk::Container, gtk::Widget;
    match fn { type_ => || ffi::gtk_print_unix_dialog_get_type(), }
}
glib::wrapper! {
    struct Printer(Object<ffi::GtkPrinter>);
    match fn { type_ => || ffi::gtk_printer_get_type(), }
}
glib::wrapper! {
    struct Job(Object<ffi::GtkPrintJob>);
    match fn { type_ => || ffi::gtk_print_job_get_type(), }
}

type Done = Sender<Result<PrintOutcome, String>>;

thread_local! {
    /// The last choices (printer, copies, …), so the next dialog opens the same way. Only the
    /// main thread touches it.
    static LAST_SETTINGS: RefCell<Option<PrintSettings>> = const { RefCell::new(None) };
}

/// See [`crate::print()`].
pub fn print<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    job: PrintJob,
) -> Result<PrintOutcome, String> {
    let (done, answer) = channel();
    let parent = window.clone();
    let file = job.file.clone();
    let scheduled = window.run_on_main_thread(move || {
        let failed = done.clone();
        let file = job.file.clone();
        if let Err(e) = show_dialog(&parent, job, done) {
            let _ = std::fs::remove_file(file);
            let _ = failed.send(Err(e));
        }
    });
    if let Err(e) = scheduled {
        let _ = std::fs::remove_file(file);
        return Err(e.to_string());
    }
    answer
        .recv()
        .map_err(|_| "the print dialog closed unexpectedly".to_string())?
}

fn show_dialog<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    job: PrintJob,
    done: Done,
) -> Result<(), String> {
    let parent = window.gtk_window().map_err(|e| e.to_string())?;
    let title = c_text("Print")?;
    // SAFETY: on the main thread (see `print`), with a valid title and parent. The dialog is a
    // new floating widget, which `from_glib_none` sinks.
    let dialog: PrintUnixDialog = unsafe {
        let widget = ffi::gtk_print_unix_dialog_new(
            title.as_ptr(),
            parent.upcast_ref::<gtk::Window>().to_glib_none().0,
        );
        gtk::Widget::from_glib_none(widget).unsafe_cast()
    };
    let raw: *mut ffi::GtkPrintUnixDialog = dialog.to_glib_none().0;
    // SAFETY: `raw` is alive for these calls and the settings are copied by GTK.
    unsafe {
        ffi::gtk_print_unix_dialog_set_manual_capabilities(raw, ffi::GENERATE_PDF);
        if let Some(page) = job.current_page {
            ffi::gtk_print_unix_dialog_set_current_page(raw, page as c_int);
        }
        LAST_SETTINGS.with_borrow(|last| {
            if let Some(settings) = last {
                ffi::gtk_print_unix_dialog_set_settings(raw, settings.to_glib_none().0);
            }
        });
    }
    dialog.set_modal(true);

    // `response` fires once for the buttons and once more if closing the dialog also deletes it.
    let pending = Cell::new(Some((job, done)));
    dialog.connect_response(move |dialog, response| {
        let Some((job, done)) = pending.take() else {
            return;
        };
        if response == gtk::ResponseType::Ok {
            submit(dialog, job, done);
        } else {
            let _ = std::fs::remove_file(&job.file);
            let _ = done.send(Ok(PrintOutcome::Cancelled));
        }
        // SAFETY: nothing uses the dialog after this; the choices were copied out above.
        unsafe { dialog.destroy() };
    });
    dialog.show();
    Ok(())
}

/// Reads the dialog's choices and sends the job, after cutting the file down to the chosen pages
/// when needed. Always answers on `done`.
fn submit(dialog: &PrintUnixDialog, job: PrintJob, done: Done) {
    let raw: *mut ffi::GtkPrintUnixDialog = dialog.to_glib_none().0;
    // SAFETY: `raw` is alive; `get_settings` hands over a new object, the others are borrowed
    // and get a reference of their own here.
    let (settings, printer, setup): (PrintSettings, Option<Printer>, gtk::PageSetup) = unsafe {
        (
            from_glib_full(ffi::gtk_print_unix_dialog_get_settings(raw)),
            from_glib_none(ffi::gtk_print_unix_dialog_get_selected_printer(raw)),
            from_glib_none(ffi::gtk_print_unix_dialog_get_page_setup(raw)),
        )
    };
    let keep = chosen_pages(&settings, &job);
    settings.set_print_pages(PrintPages::All);
    LAST_SETTINGS.with_borrow_mut(|last| *last = Some(settings.clone()));
    let Some(printer) = printer else {
        finish(&job.file, &done, Err("no printer is selected".into()));
        return;
    };
    let Some(keep) = keep else {
        send(job, &printer, &settings, &setup, done);
        return;
    };
    // Re-reading a large PDF takes long enough to freeze the window if done on the main thread.
    glib::MainContext::default().spawn_local(async move {
        let file = job.file.clone();
        let cut = gtk::gio::spawn_blocking(move || keep_pages(&file, &keep)).await;
        match cut.unwrap_or_else(|_| Err("preparing the pages failed".into())) {
            Ok(()) => send(job, &printer, &settings, &setup, done),
            Err(e) => finish(&job.file, &done, Err(e)),
        }
    });
}

/// The zero-based pages picked in the dialog, or `None` for the whole file.
fn chosen_pages(settings: &PrintSettings, job: &PrintJob) -> Option<Vec<u32>> {
    match settings.print_pages() {
        PrintPages::Ranges => {
            let ranges: Vec<_> = settings
                .page_ranges()
                .iter()
                .map(|r: &PageRange| (r.start(), r.end()))
                .collect();
            Some(pages_in_ranges(&ranges, job.pages))
        }
        PrintPages::Current => job.current_page.map(|page| vec![page]),
        _ => None,
    }
}

/// Expands GTK's zero-based, inclusive page ranges into sorted page indexes inside the document.
/// A negative end means "to the last page".
fn pages_in_ranges(ranges: &[(i32, i32)], pages: u32) -> Vec<u32> {
    let last = pages as i64 - 1;
    let mut keep: Vec<u32> = ranges
        .iter()
        .flat_map(|&(start, end)| {
            let end = if end < 0 {
                last
            } else {
                i64::from(end).min(last)
            };
            i64::from(start).max(0)..=end
        })
        .map(|page| page as u32)
        .collect();
    keep.sort_unstable();
    keep.dedup();
    keep
}

fn keep_pages(file: &Path, keep: &[u32]) -> Result<(), String> {
    let pdf = std::fs::read(file).map_err(|e| e.to_string())?;
    let cut = penroam_pdf::keep_pages(&pdf, keep).map_err(|e| e.to_string())?;
    std::fs::write(file, cut).map_err(|e| e.to_string())
}

/// What the job needs once GTK has finished with it.
struct Finished {
    file: PathBuf,
    done: Done,
    /// Held so the job outlives the send.
    _job: Job,
}

/// Queues the file on `printer`. GTK calls [`on_complete`] when the print system has it.
fn send(
    job: PrintJob,
    printer: &Printer,
    settings: &PrintSettings,
    setup: &gtk::PageSetup,
    done: Done,
) {
    let prepared = c_text(&job.title).and_then(|title| {
        let path = c_text(&job.file.to_string_lossy())?;
        // SAFETY: valid strings and live objects; `gtk_print_job_new` returns a new reference.
        let print_job: Job = unsafe {
            from_glib_full(ffi::gtk_print_job_new(
                title.as_ptr(),
                printer.to_glib_none().0,
                settings.to_glib_none().0,
                setup.to_glib_none().0,
            ))
        };
        let mut error = ptr::null_mut();
        // SAFETY: `error` is a valid out-pointer, and on failure ownership of it passes here.
        unsafe {
            let raw: *mut ffi::GtkPrintJob = print_job.to_glib_none().0;
            if ffi::gtk_print_job_set_source_file(raw, path.as_ptr(), &mut error) == 0 {
                return Err(glib::Error::from_glib_full(error).to_string());
            }
        }
        Ok(print_job)
    });
    let print_job = match prepared {
        Ok(print_job) => print_job,
        Err(e) => {
            finish(&job.file, &done, Err(e));
            return;
        }
    };
    let raw: *mut ffi::GtkPrintJob = print_job.to_glib_none().0;
    let finished = Box::new(Finished {
        file: job.file,
        done,
        _job: print_job,
    });
    // SAFETY: the box is reclaimed exactly once, in `on_complete`, and keeps the job alive.
    unsafe { ffi::gtk_print_job_send(raw, on_complete, Box::into_raw(finished).cast(), None) };
}

unsafe extern "C" fn on_complete(
    _job: *mut ffi::GtkPrintJob,
    data: *mut c_void,
    error: *const glib::ffi::GError,
) {
    // SAFETY: `data` is the box leaked in `send`, and GTK calls this once.
    let finished = unsafe { Box::from_raw(data.cast::<Finished>()) };
    let result = if error.is_null() {
        Ok(PrintOutcome::Sent)
    } else {
        // SAFETY: GTK guarantees a valid error here; it stays owned by GTK, so it is only read.
        let message: glib::Error = unsafe { from_glib_none(error) };
        Err(message.to_string())
    };
    finish(&finished.file, &finished.done, result);
}

fn finish(file: &Path, done: &Done, result: Result<PrintOutcome, String>) {
    let _ = std::fs::remove_file(file);
    let _ = done.send(result);
}

fn c_text(text: &str) -> Result<CString, String> {
    // A NUL cannot be typed into a title, but one could arrive in a synced notebook.
    CString::new(text.replace('\0', " ")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::pages_in_ranges;

    #[test]
    fn ranges_expand_in_document_order_without_repeats() {
        assert_eq!(
            pages_in_ranges(&[(4, 5), (0, 1), (1, 2)], 10),
            [0, 1, 2, 4, 5]
        );
    }

    #[test]
    fn ranges_stop_at_the_end_of_the_document() {
        assert_eq!(pages_in_ranges(&[(2, 99)], 4), [2, 3]);
        assert_eq!(pages_in_ranges(&[(2, -1)], 4), [2, 3]);
        assert!(pages_in_ranges(&[(7, 9)], 4).is_empty());
    }
}
