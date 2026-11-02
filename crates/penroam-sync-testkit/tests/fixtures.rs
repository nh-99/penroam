//! The committed fixtures must equal the generator output (same check as
//! `penroam-sync-fixtures --check`), and generation must be deterministic.

use std::path::PathBuf;

use penroam_sync_testkit::fixtures::{check, generate};

#[test]
fn generation_is_deterministic() {
    assert_eq!(generate(), generate());
}

#[test]
fn committed_fixtures_are_up_to_date() {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../penroam-sync-protocol/fixtures/v1");
    let diffs = check(&dir).unwrap();
    assert!(
        diffs.is_empty(),
        "fixtures differ (regenerate with `cargo run -p penroam-sync-testkit --bin penroam-sync-fixtures -- crates/penroam-sync-protocol/fixtures/v1`):\n{}",
        diffs.join("\n")
    );
}
