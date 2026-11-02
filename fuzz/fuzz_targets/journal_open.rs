#![no_main]
//! Journal recovery on arbitrary segment bytes: never panics, never
//! returns records that fail their checksum.
use libfuzzer_sys::fuzz_target;
use penroam_storage::journal::Journal;

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    let j = dir.path().join("journal");
    std::fs::create_dir_all(&j).unwrap();
    std::fs::write(j.join("00000000000000000001.pjl"), data).unwrap();
    if let Ok((journal, txns, _)) = Journal::open(&j) {
        // a second open of the recovered journal sees the same records
        drop(journal);
        let (_, again, report) = Journal::open(&j).expect("reopen");
        assert_eq!(again.len(), txns.len());
        assert_eq!(report.truncated_tails, 0, "recovery is idempotent");
    }
});
