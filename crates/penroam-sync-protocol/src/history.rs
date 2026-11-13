//! Attribution and checkpoint fallback (capability `history.v1`, protocol 1.10).
//!
//! **Attribution** says, for each operation in a stream, which account's session the server
//! accepted it from, through which device, and when. It is server-authoritative metadata from the
//! op row; there is no extra plaintext. Clients join it with what they verify themselves: the
//! envelope's signing device must equal the `device_id` reported here, otherwise the author is shown
//! as unverified. The document stream's attribution is at [`crate::routes::ATTRIBUTION`], the
//! comment stream's at [`crate::routes::COMMENT_ATTRIBUTION`].
//!
//! When an account is erased, its id in attribution (and in member lists) is replaced by a stable
//! **tombstone id** ([`ERASED_ACCOUNT_PREFIX`]): the same erased account always maps to the same
//! tombstone, and the tombstone can't be turned back into the original id.
//!
//! **Checkpoint fallback.** Servers keep the two newest checkpoints of a stream and compact only up
//! to the older one, so a client whose latest checkpoint is corrupt can recover from the previous
//! one ([`crate::routes::CHECKPOINT_PREVIOUS`]) plus the tail after it.
//!
//! The normative description is `docs/protocol/sync-protocol-v1.md` §12i.

use serde::{Deserialize, Serialize};

use crate::ids::{AccountId, DeviceId, OpId};

/// Prefix of a tombstone account id. The full form is `erased-` followed by 32 lowercase hex
/// digits; servers never assign ordinary account ids that start with it.
pub const ERASED_ACCOUNT_PREFIX: &str = "erased-";
/// Most entries in one [`AttributionPage`].
pub const ATTRIBUTION_PAGE_MAX: u32 = 1000;
/// Entries per page when `limit` is absent.
pub const ATTRIBUTION_PAGE_DEFAULT: u32 = 200;

/// Whether `id` is a tombstone of an erased account (`erased-` + 32 lowercase hex digits).
pub fn is_erased_account(id: &AccountId) -> bool {
    id.as_str()
        .strip_prefix(ERASED_ACCOUNT_PREFIX)
        .is_some_and(|h| {
            h.len() == 32
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// Who wrote one operation, according to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionEntry {
    /// The operation's position in the stream.
    pub server_seq: u64,
    /// The operation.
    pub op_id: OpId,
    /// The account whose session pushed it: in a space, the member who registered the device; for
    /// an ordinary account or vault, the account itself. A tombstone ([`is_erased_account`]) after
    /// that account was erased.
    pub account_id: AccountId,
    /// The device whose session pushed it (the envelope's `device_id`).
    pub device_id: DeviceId,
    /// When the server accepted it, Unix milliseconds. Comparing it with the operation's own time
    /// shows edits made offline. 0 when the server didn't record it.
    pub accepted_at_ms: u64,
}

/// `GET /v1/attribution?from_seq=&limit=` (and `/v1/comments/attribution`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionPage {
    /// Entries with `server_seq >= from_seq`, in order. `limit` bounds the operations scanned,
    /// not the entries: operations the server no longer has (compacted), or whose author it can't
    /// establish, have no entry, so a page may be empty while [`AttributionPage::has_more`] is
    /// true.
    pub items: Vec<AttributionEntry>,
    /// The `from_seq` of the next page: one past the last operation scanned. Keep paging while
    /// `has_more` is true and this advances; stop if it doesn't.
    pub next_from_seq: u64,
    /// Whether more entries existed when the query ran.
    pub has_more: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstone_ids_have_one_form() {
        let ok: AccountId = format!("erased-{}", "0a".repeat(16)).parse().unwrap();
        assert!(is_erased_account(&ok));
        for bad in [
            "erased-",
            "erased-0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A",
            "erased-0a0a",
            "acct-erased-0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
            "acct-1",
        ] {
            assert!(!is_erased_account(&bad.parse().unwrap()), "{bad}");
        }
    }
}
