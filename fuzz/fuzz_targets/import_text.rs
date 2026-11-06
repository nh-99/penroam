#![no_main]
//! Markdown and plain text import: never panics, whatever the bytes.
use libfuzzer_sys::fuzz_target;
use penroam_interchange::text_import::{import_markdown, import_plain_text};

fuzz_target!(|data: &[u8]| {
    let _ = import_markdown(data, "fuzz");
    let _ = import_plain_text(data, "fuzz");
});
