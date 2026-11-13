//! Concurrent editing of one text block's text: a sequence CRDT (Yrs, pinned at `=0.28.1`, v1
//! update encoding, UTF-16 offsets as Yjs uses) for [`crate::model::TextBlock::text`].
//!
//! Only shared notebook spaces use it; the app and the library keep writing whole-element
//! replaces, and the sync client turns a text change into an update
//! ([`crate::operation::DocumentOperation::TextEdited`]). The merge model
//! ([`crate::replica::Replica`]) keeps one [`TextDoc`] per *genesis*: a content write of the block
//! (its creation, or a whole-content write such as an older client's), started deterministically
//! from that write's text under a client id derived from its operation id
//! ([`genesis_client`]), so every replica builds the same items and later updates refer to them
//! alike. Updates name the genesis they edit.
//!
//! Invariants:
//!
//! * The encoding is Yrs' v1 update format at the pinned version; `tests/vectors/text-crdt.json`
//!   checks the bytes, so an upgrade that changes them is caught.
//! * Offsets are UTF-16 code units, and [`diff`] never splits a Unicode scalar value.
//! * A [`TextDoc`] is only ever changed by applying updates; local edits are made on a fork with
//!   the editing device's own client id ([`edit_client`]) and travel as updates.

use sha2::{Digest, Sha256};
use uuid::Uuid;
use yrs::block::ClientID;
use yrs::updates::decoder::Decode;
use yrs::{Doc, GetString, OffsetKind, Options, ReadTxn, StateVector, Text, Transact, Update};

/// Name of the one text type inside every [`TextDoc`].
const FIELD: &str = "text";

/// Why an update can't be used.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TextCrdtError {
    /// The bytes are not a Yrs v1 update.
    #[error("not a text update: {0}")]
    Malformed(String),
    /// The update inserts text under another writer's client id ([`TextDoc::check_edit`]).
    #[error("text update written under another device's client id")]
    ForeignClient,
    /// The update is larger than the limits allow.
    #[error("text update too large")]
    TooLarge,
    /// The update depends on text this document doesn't have (it would stay pending).
    #[error("text update depends on missing text")]
    MissingDependency,
    /// The update adds anything but plain text (formatting, embeds, other types, subdocuments).
    #[error("text update with content other than plain text")]
    NotPlainText,
}

/// Most text one update may insert, in clock units (≈ UTF-16 code units).
pub const MAX_INSERTED_CLOCKS: u64 = 1 << 20;
/// Most separate deleted ranges one update may name.
pub const MAX_DELETE_RANGES: usize = 50_000;

/// Runs `f`, turning a panic inside Yrs into an error, so hostile bytes can't take the caller
/// (the sync worker) down.
fn contained<R>(f: impl FnOnce() -> Result<R, TextCrdtError>) -> Result<R, TextCrdtError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err(TextCrdtError::Malformed(
            "the update made the decoder fail".into(),
        ))
    })
}

/// 53 bits of SHA-256(`label` ‖ `id`): a client id that is also a safe JavaScript integer.
fn client_from(label: &[u8], id: &[u8]) -> u64 {
    let h = Sha256::new()
        .chain_update(label)
        .chain_update(id)
        .finalize();
    let mut b = [0u8; 8];
    b.copy_from_slice(&h[..8]);
    u64::from_le_bytes(b) & ((1u64 << 53) - 1)
}

/// The client id a genesis is written under: derived from the content write's operation id.
pub fn genesis_client(op: Uuid) -> u64 {
    client_from(b"penroam/text-genesis/v1", op.as_bytes())
}

/// The client id a device edits under: derived from its (space) device id.
pub fn edit_client(device: Uuid) -> u64 {
    client_from(b"penroam/text-client/v1", device.as_bytes())
}

fn new_doc(client: u64) -> Doc {
    Doc::with_options(Options {
        client_id: ClientID::new(client),
        offset_kind: OffsetKind::Utf16,
        skip_gc: false,
        ..Options::default()
    })
}

/// Walks a v1 update without decoding it, accepting only what a plain-text edit contains: at
/// most [`MAX_UPDATE_CLIENTS`] clients of blocks that are garbage-collected ranges, skips,
/// deleted ranges or strings (in the one text field or after an origin), then a delete set, and
/// nothing after it. Every count must fit in the bytes that remain. Yrs's decoder reserves
/// memory up front for the counts an update declares, so a few hostile bytes could otherwise
/// make it allocate and clear gigabytes; this runs first and keeps such bytes away from it.
fn prescan_plain_v1(b: &[u8]) -> Result<(), TextCrdtError> {
    let bad = |m: &str| TextCrdtError::Malformed(m.to_string());
    let mut at = 0usize;
    let var = |at: &mut usize| -> Result<u64, TextCrdtError> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = *b.get(*at).ok_or_else(|| bad("truncated"))?;
            *at += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(bad("varint too long"))
    };
    let left = |at: usize| (b.len() - at) as u64;
    let buf = |at: &mut usize| -> Result<(), TextCrdtError> {
        let n = var(at)?;
        if n > left(*at) {
            return Err(bad("string past the end"));
        }
        *at += n as usize;
        Ok(())
    };
    let clients = var(&mut at)?;
    if clients > MAX_UPDATE_CLIENTS || clients > left(at) {
        return Err(TextCrdtError::ForeignClient);
    }
    for _ in 0..clients {
        let structs = var(&mut at)?;
        if structs > left(at) {
            return Err(bad("more blocks than bytes"));
        }
        var(&mut at)?; // client
        var(&mut at)?; // first clock
        for _ in 0..structs {
            let info = *b.get(at).ok_or_else(|| bad("truncated"))?;
            at += 1;
            match info {
                0 | 10 => {
                    var(&mut at)?; // length of a GC range or a skip
                    continue;
                }
                _ => {}
            }
            let has_origin = info & 0x80 != 0;
            let has_right = info & 0x40 != 0;
            if has_origin {
                var(&mut at)?;
                var(&mut at)?;
            }
            if has_right {
                var(&mut at)?;
                var(&mut at)?;
            }
            if !has_origin && !has_right {
                if var(&mut at)? == 1 {
                    buf(&mut at)?; // the root type's name
                } else {
                    var(&mut at)?;
                    var(&mut at)?;
                }
                if info & 0x20 != 0 {
                    return Err(TextCrdtError::NotPlainText); // a map key
                }
            }
            match info & 0x1f {
                1 => {
                    var(&mut at)?; // deleted content's length
                }
                4 => buf(&mut at)?,
                _ => return Err(TextCrdtError::NotPlainText),
            }
        }
    }
    // the delete set
    let ds_clients = var(&mut at)?;
    if ds_clients > left(at) {
        return Err(bad("more deleting clients than bytes"));
    }
    for _ in 0..ds_clients {
        var(&mut at)?;
        let ranges = var(&mut at)?;
        if ranges > left(at) / 2 || ranges as usize > MAX_DELETE_RANGES {
            return Err(TextCrdtError::TooLarge);
        }
        for _ in 0..ranges {
            var(&mut at)?;
            var(&mut at)?;
        }
    }
    if at != b.len() {
        return Err(bad("bytes after the update"));
    }
    Ok(())
}

/// Most clients one edit's blocks may belong to: an edit writes only under its author.
pub const MAX_UPDATE_CLIENTS: u64 = 1;

thread_local! {
    static PANIC_AFTER: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// One text block's text as a CRDT.
pub struct TextDoc {
    doc: Doc,
}

impl std::fmt::Debug for TextDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextDoc")
            .field("text", &self.text())
            .finish()
    }
}

impl Clone for TextDoc {
    /// A deep copy (a Yrs `Doc` clone would share its state).
    fn clone(&self) -> Self {
        let d = new_doc(0);
        let copy = TextDoc { doc: d };
        copy.apply(&self.state())
            .expect("a document's own state is a valid update");
        copy
    }
}

impl TextDoc {
    /// The update that starts a genesis: `text` inserted under [`genesis_client`]`(op)`.
    /// Deterministic: the same op and text give the same bytes everywhere.
    pub fn genesis_update(op: Uuid, text: &str) -> Vec<u8> {
        let d = new_doc(genesis_client(op));
        let t = d.get_or_insert_text(FIELD);
        let mut tx = d.transact_mut();
        if !text.is_empty() {
            t.insert(&mut tx, 0, text);
        }
        tx.encode_update_v1()
    }

    /// A document holding the genesis of `op` with `text`.
    pub fn genesis(op: Uuid, text: &str) -> Self {
        let d = TextDoc { doc: new_doc(0) };
        d.apply(&Self::genesis_update(op, text))
            .expect("a genesis update is valid");
        d
    }

    /// Applies an update (idempotent; parts that depend on missing ones wait inside the
    /// document). A panic inside the decoder is contained and reported as an error. Updates from
    /// other devices go through [`TextDoc::check_edit`] first.
    ///
    /// After an error the document may be half-changed (a panic can stop Yrs mid-transaction):
    /// callers rebuild it from the genesis and the updates applied before, without this one.
    pub fn apply(&self, update: &[u8]) -> Result<(), TextCrdtError> {
        contained(|| {
            let u =
                Update::decode_v1(update).map_err(|e| TextCrdtError::Malformed(e.to_string()))?;
            let mut tx = self.doc.transact_mut();
            tx.apply_update(u)
                .map_err(|e| TextCrdtError::Malformed(e.to_string()))?;
            drop(tx);
            if PANIC_AFTER.with(|p| p.borrow().as_deref() == Some(update)) {
                panic!("test hook: panic after applying this update");
            }
            Ok(())
        })
    }

    /// Test hook: make [`TextDoc::apply`] panic right after applying exactly `update` (on this
    /// thread), leaving the document changed, to test recovery from a panic inside Yrs.
    #[doc(hidden)]
    pub fn panic_after_applying_for_tests(update: Option<Vec<u8>>) {
        PANIC_AFTER.with(|p| *p.borrow_mut() = update);
    }

    /// Checks an edit before it is applied, without changing this document:
    ///
    /// * everything it inserts is written under `author` (the writing device's
    ///   [`edit_client`]), continuing exactly where that client's text ends here, so nobody can
    ///   write items in another device's name or leave gaps (deletes may name any text);
    /// * it stays within [`MAX_INSERTED_CLOCKS`] and [`MAX_DELETE_RANGES`];
    /// * applied here, it leaves nothing pending: everything it builds on is present;
    /// * the result is still plain text in the one text field (no formatting, embeds, other
    ///   types or subdocuments).
    ///
    /// Decoding and applying run on a copy, with panics contained.
    pub fn check_edit(&self, update: &[u8], author: u64) -> Result<(), TextCrdtError> {
        // before Yrs sees the bytes: its decoder reserves memory for counts the bytes declare
        prescan_plain_v1(update)?;
        contained(|| {
            let u =
                Update::decode_v1(update).map_err(|e| TextCrdtError::Malformed(e.to_string()))?;
            // Every client the update writes blocks for, from two views: the lowest clock per
            // client and the inserted id ranges (a crafted update can leave the upper state
            // vector empty while still carrying blocks, so that alone isn't trusted).
            let lower = u.state_vector_lower();
            let insertions = u.insertions(true);
            let mut clients: Vec<yrs::block::ClientID> = lower.iter().map(|(c, _)| *c).collect();
            clients.extend(insertions.iter().map(|(c, _)| *c));
            if clients.iter().any(|c| c.get() != author) {
                return Err(TextCrdtError::ForeignClient);
            }
            let mut inserted = 0u64;
            for (_, ranges) in insertions.iter() {
                for r in ranges.iter() {
                    inserted += u64::from(r.end.saturating_sub(r.start));
                }
            }
            if inserted > MAX_INSERTED_CLOCKS {
                return Err(TextCrdtError::TooLarge);
            }
            let ranges: usize = u.delete_set().iter().map(|(_, r)| r.len()).sum();
            if ranges > MAX_DELETE_RANGES {
                return Err(TextCrdtError::TooLarge);
            }
            // contiguous with what the author already wrote here
            let have = self.doc.transact().state_vector();
            for c in &clients {
                if lower.get(c) != have.get(c) {
                    return Err(TextCrdtError::MissingDependency);
                }
            }
            let fork = self.clone();
            {
                let mut tx = fork.doc.transact_mut();
                tx.apply_update(u)
                    .map_err(|e| TextCrdtError::Malformed(e.to_string()))?;
            }
            let tx = fork.doc.transact();
            if tx.has_missing_updates() {
                return Err(TextCrdtError::MissingDependency);
            }
            if tx.root_refs().any(|(name, _)| name != FIELD) || tx.subdoc_guids().next().is_some() {
                return Err(TextCrdtError::NotPlainText);
            }
            drop(tx);
            let t = fork.doc.get_or_insert_text(FIELD);
            let tx = fork.doc.transact();
            let plain = t
                .diff(&tx, yrs::types::text::YChange::identity)
                .iter()
                .all(|d| {
                    d.attributes.is_none()
                        && matches!(&d.insert, yrs::Out::Any(yrs::Any::String(_)))
                });
            if !plain {
                return Err(TextCrdtError::NotPlainText);
            }
            Ok(())
        })
    }

    /// The current text.
    pub fn text(&self) -> String {
        let t = self.doc.get_or_insert_text(FIELD);
        let tx = self.doc.transact();
        t.get_string(&tx)
    }

    /// A stable position in this text at UTF-16 offset `offset` (Yrs sticky index, v1 bytes),
    /// for anchoring a comment to text: it keeps pointing at the same place while others edit
    /// around it. `after` attaches it to the character after the offset (a range's start), not
    /// the one before (its end). `None` when the offset is past the end.
    pub fn sticky(&self, offset: u32, after: bool) -> Option<Vec<u8>> {
        use yrs::updates::encoder::Encode;
        use yrs::IndexedSequence;
        let t = self.doc.get_or_insert_text(FIELD);
        let tx = self.doc.transact();
        let assoc = if after {
            yrs::Assoc::After
        } else {
            yrs::Assoc::Before
        };
        t.sticky_index(&tx, offset, assoc).map(|s| s.encode_v1())
    }

    /// Where a [`TextDoc::sticky`] position is now, as a UTF-16 offset. `None` when the bytes
    /// don't decode or refer to text that isn't in this document.
    pub fn resolve_sticky(&self, sticky: &[u8]) -> Option<u32> {
        contained(|| {
            let s = yrs::StickyIndex::decode_v1(sticky)
                .map_err(|e| TextCrdtError::Malformed(e.to_string()))?;
            let tx = self.doc.transact();
            Ok(s.get_offset(&tx).map(|o| o.index))
        })
        .ok()
        .flatten()
    }

    /// The whole document as one update.
    pub fn state(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    /// The update that turns this document's text into `new_text`, made under `client` (the
    /// editing device's [`edit_client`]). `None` when the text is already `new_text`. The
    /// document itself is left unchanged: apply the returned update to record the edit, and do
    /// so before making the next one. Two updates made from the same unapplied state reuse the
    /// same clocks of `client`, so only the first of them survives.
    pub fn edit_update(&self, client: u64, new_text: &str) -> Option<Vec<u8>> {
        let old = self.text();
        if old == new_text {
            return None;
        }
        let (at, del, ins) = diff(&old, new_text);
        let fork = new_doc(client);
        {
            let mut tx = fork.transact_mut();
            let u = Update::decode_v1(&self.state()).expect("own state");
            tx.apply_update(u).expect("own state applies");
        }
        let before = fork.transact().state_vector();
        let t = fork.get_or_insert_text(FIELD);
        {
            let mut tx = fork.transact_mut();
            if del > 0 {
                t.remove_range(&mut tx, at, del);
            }
            if !ins.is_empty() {
                t.insert(&mut tx, at, &ins);
            }
        }
        let update = fork.transact().encode_diff_v1(&before);
        Some(update)
    }
}

/// The single changed range between `old` and `new`: (start, removed length, inserted text),
/// start and length in UTF-16 code units.
///
/// It starts from the longest common prefix and suffix, then keeps only what is *meaningfully*
/// shared, because the update anchors the new text to the kept characters: if someone else
/// deletes those concurrently, whatever relied on them goes too.
///
/// * Boundaries always fall between grapheme clusters, so an emoji sequence, a combining mark or a
///   skin tone is never split between "kept" and "new".
/// * When the change replaces text (something removed and something inserted), a common prefix or
///   suffix is kept only if it ends on a word boundary (Unicode word segmentation, so in CJK
///   between ideographs too) in both texts. Rewriting "first draft" as "agenda: budget" is then
///   a deletion of all the old text and an insertion of all the new, instead of reusing the
///   final "t", and two people rewriting the same block concurrently both keep every character.
/// * A pure insertion or deletion (typing, backspace) stays minimal, which is what lets people
///   type into the same block at once.
pub fn diff(old: &str, new: &str) -> (u32, u32, String) {
    use unicode_segmentation::UnicodeSegmentation;
    let o: Vec<char> = old.chars().collect();
    let n: Vec<char> = new.chars().collect();
    let mut p = 0;
    while p < o.len() && p < n.len() && o[p] == n[p] {
        p += 1;
    }
    let mut s = 0;
    while s < o.len() - p && s < n.len() - p && o[o.len() - 1 - s] == n[n.len() - 1 - s] {
        s += 1;
    }
    // char offsets where a grapheme cluster (or a word) starts, plus the end
    let bounds = |t: &str, words: bool| -> std::collections::BTreeSet<usize> {
        let starts: Vec<usize> = if words {
            t.split_word_bound_indices().map(|(i, _)| i).collect()
        } else {
            t.grapheme_indices(true).map(|(i, _)| i).collect()
        };
        let mut out: std::collections::BTreeSet<usize> =
            starts.into_iter().map(|b| t[..b].chars().count()).collect();
        out.insert(0);
        out.insert(t.chars().count());
        out
    };
    let replaces = o.len() - p - s > 0 && n.len() - p - s > 0;
    let (bo, bn) = (bounds(old, replaces), bounds(new, replaces));
    let gro = bounds(old, false);
    let grn = bounds(new, false);
    // the prefix ends where both texts have a boundary (graphemes always; words for a replace)
    while p > 0 && !(bo.contains(&p) && bn.contains(&p) && gro.contains(&p) && grn.contains(&p)) {
        p -= 1;
    }
    while s > 0 && {
        let (eo, en) = (o.len() - s, n.len() - s);
        !(bo.contains(&eo) && bn.contains(&en) && gro.contains(&eo) && grn.contains(&en))
    } {
        s -= 1;
    }
    let u16len = |cs: &[char]| cs.iter().map(|c| c.len_utf16() as u32).sum::<u32>();
    (
        u16len(&o[..p]),
        u16len(&o[p..o.len() - s]),
        n[p..n.len() - s].iter().collect(),
    )
}

/// Unpadded base64url, for carrying updates in JSON operation bodies.
pub mod b64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    /// Encodes without padding.
    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            let chars = chunk.len() + 1;
            for i in 0..chars {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    }

    /// Decodes unpadded base64url; anything else is `None`.
    pub fn decode(s: &str) -> Option<Vec<u8>> {
        let val = |c: u8| ALPHABET.iter().position(|a| *a == c).map(|p| p as u32);
        let bytes = s.as_bytes();
        if bytes.len() % 4 == 1 {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
        for chunk in bytes.chunks(4) {
            let mut n = 0u32;
            for (i, c) in chunk.iter().enumerate() {
                n |= val(*c)? << (18 - 6 * i);
            }
            for i in 0..chunk.len() - 1 {
                out.push((n >> (16 - 8 * i)) as u8);
            }
        }
        // canonical: re-encoding gives the same string (no stray low bits)
        (encode(&out) == s).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trips_and_is_canonical() {
        for n in 0..40u8 {
            let v: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(b64::decode(&b64::encode(&v)).unwrap(), v);
        }
        assert!(b64::decode("A").is_none());
        assert!(b64::decode("AB==").is_none());
        assert!(b64::decode("AA").is_some());
        assert!(b64::decode("AF").is_none(), "stray bits are refused");
    }

    #[test]
    fn the_diff_never_splits_a_scalar() {
        let (at, del, ins) = diff("a👩🏽‍🔬b", "a👩🏽‍🔬🎉b");
        assert_eq!((at, del, ins.as_str()), (8, 0, "🎉"));
        assert_eq!(diff("same", "same"), (4, 0, String::new()));
        assert_eq!(diff("abc", ""), (0, 3, String::new()));
    }

    #[test]
    fn genesis_is_deterministic_and_edits_converge() {
        let op = Uuid::from_u128(7);
        assert_eq!(
            TextDoc::genesis_update(op, "héllo"),
            TextDoc::genesis_update(op, "héllo")
        );
        let a = TextDoc::genesis(op, "hello world");
        let b = TextDoc::genesis(op, "hello world");
        let ua = a
            .edit_update(edit_client(Uuid::from_u128(1)), "hello brave world")
            .unwrap();
        let ub = b
            .edit_update(edit_client(Uuid::from_u128(2)), "hello world!")
            .unwrap();
        a.apply(&ua).unwrap();
        a.apply(&ub).unwrap();
        b.apply(&ub).unwrap();
        b.apply(&ua).unwrap();
        b.apply(&ua).unwrap();
        assert_eq!(a.text(), b.text());
        assert_eq!(a.text(), "hello brave world!");
        assert!(a.edit_update(1, &a.text()).is_none());
        assert!(a.apply(b"junk").is_err());
        let c = a.clone();
        assert_eq!(c.text(), a.text());
        assert!(genesis_client(op) < (1 << 53) && edit_client(op) < (1 << 53));
    }
}
