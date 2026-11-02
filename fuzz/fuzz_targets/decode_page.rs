#![no_main]
//! `.prp` decoder: never panics; anything it accepts re-encodes and decodes
//! to the same page.
use libfuzzer_sys::fuzz_target;
use penroam_core::binary::{decode_page, encode_page};

fuzz_target!(|data: &[u8]| {
    if let Ok(page) = decode_page(data) {
        let again = decode_page(&encode_page(&page)).expect("re-encoded page decodes");
        assert_eq!(
            serde_json_eq(&again),
            serde_json_eq(&page),
            "decode(encode(page)) == page"
        );
    }
});

fn serde_json_eq(p: &penroam_core::Page) -> String {
    // NaN-safe comparison through the debug form of the bit patterns
    format!("{p:?}")
}
