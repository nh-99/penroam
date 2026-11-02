#![no_main]
//! Sync batch framing: hostile bodies never panic or over-allocate.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = penroam_sync_protocol::framing::decode_batch(data, &penroam_sync_protocol::limits::Limits::DEFAULT);
});
