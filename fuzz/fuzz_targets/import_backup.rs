#![no_main]
//! Backups and notebook archives: restoring never panics, and leaves the library openable.
use libfuzzer_sys::fuzz_target;
use penroam_storage::{Library, RestoreOptions};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    let mut lib = Library::open(dir.path().join("lib")).unwrap();
    let _ = lib.preview_backup(Cursor::new(data));
    let _ = lib.restore_backup(Cursor::new(data), &RestoreOptions::default());
    let _ = lib.verify_archive(Cursor::new(data));
    drop(lib);
    Library::open(dir.path().join("lib")).expect("library still opens");
});
