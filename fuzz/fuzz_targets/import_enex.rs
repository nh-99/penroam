#![no_main]
//! Evernote import: never panics, and everything it accepts passes the model's validation.
use libfuzzer_sys::fuzz_target;
use penroam_core::validate::validate_page;
use penroam_core::{Page, Paper};
use penroam_interchange::enex::import_enex;
use uuid::Uuid;

fuzz_target!(|data: &[u8]| {
    if let Ok(doc) = import_enex(data, "fuzz") {
        for ip in doc.pages {
            let mut p = Page::new(Uuid::nil(), Paper::Custom, ip.size, ip.template, 0);
            p.elements = ip.elements;
            validate_page(&p).expect("imported pages are valid");
        }
    }
});
