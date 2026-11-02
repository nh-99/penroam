//! The public sync protocol crate duplicates the local format versions it
//! advertises (it must not depend on the notebook engine). Keep them equal.

use penroam_sync_protocol::version as v;

#[test]
fn protocol_crate_advertises_the_real_format_versions() {
    let pairs: [(u64, u64, &str); 6] = [
        (
            v::FILE_FORMAT_VERSION.into(),
            penroam_core::binary::FILE_FORMAT_VERSION.into(),
            "file format",
        ),
        (
            v::JOURNAL_FORMAT_VERSION.into(),
            penroam_storage::journal::JOURNAL_FORMAT_VERSION.into(),
            "journal",
        ),
        (
            v::ARCHIVE_FORMAT_VERSION.into(),
            penroam_storage::archive::FORMAT_VERSION.into(),
            "archive",
        ),
        (
            v::NOTEBOOK_SCHEMA_VERSION.into(),
            penroam_core::NOTEBOOK_SCHEMA_VERSION.into(),
            "notebook schema",
        ),
        (
            v::PAGE_SCHEMA_VERSION.into(),
            penroam_core::PAGE_SCHEMA_VERSION.into(),
            "page schema",
        ),
        (
            v::OP_SCHEMA_VERSION.into(),
            penroam_core::operation::OP_SCHEMA_VERSION.into(),
            "op schema",
        ),
    ];
    for (protocol, local, what) in pairs {
        assert_eq!(protocol, local, "{what}");
    }
}
