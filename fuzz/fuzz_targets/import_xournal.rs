#![no_main]
//! Xournal import: never panics, and everything it accepts passes the model's validation.
use libfuzzer_sys::fuzz_target;
use penroam_core::validate::validate_page;
use penroam_core::{Page, Paper};
use penroam_interchange::xournal::import_xournal;
use uuid::Uuid;

fuzz_target!(|data: &[u8]| {
    if let Ok(doc) = import_xournal(data, "fuzz") {
        for ip in doc.pages {
            let mut p = Page::new(Uuid::nil(), Paper::Custom, ip.size, ip.template, 0);
            p.elements = ip.elements;
            validate_page(&p).expect("imported pages are valid");
        }
    }
});
