//! Moving notes between Penroam and other formats.
//!
//! Nothing here touches the library or the file system. Importers turn the
//! bytes of one file into an [`ImportedDocument`], which the caller writes
//! into a **new** notebook, and exporters turn pages into text or SVG. The
//! native, lossless format is the `.penroam` archive in `penroam-storage`;
//! everything in this crate may lose something and says exactly what.
//!
//! * [`xournal`]: Xournal++ `.xopp` and classic Xournal `.xoj` documents.
//! * [`jex`]: Joplin `.jex` exports.
//! * [`enex`]: Evernote `.enex` exports, as Markdown text blocks and images.
//! * [`text_import`]: Markdown and plain text, paginated into text blocks.
//! * [`text_export`]: Markdown and plain text, with a handwriting-loss report.
//! * [`svg`]: one page as SVG, using the same stroke geometry as the canvas.
//!
//! **Hostile input.** Imported files are treated as attacker-controlled.
//! Decompression and every count are bounded ([`limits`]), XML is parsed
//! without DTDs or entity expansion, paths inside a file are never followed,
//! image bytes must match a known signature and sane dimensions, and all
//! output passes the model's own validation. Text stays inert: Penroam
//! doesn't run scripts or follow links in notes.
//!
//! Format notes and the loss table: `docs/formats/portability.md`.

#![warn(missing_docs)]

pub mod enex;
pub mod jex;
pub mod svg;
pub mod text_export;
pub mod text_import;
pub mod xournal;

mod document;
mod images;

pub use document::{ImportError, ImportLoss, ImportedAsset, ImportedDocument, ImportedPage, Kept};

/// Bounds on what an import will accept.
pub mod limits {
    /// Largest text or Markdown file.
    pub const MAX_TEXT_FILE: usize = 64 << 20;
    /// Largest Xournal document after decompression.
    pub const MAX_XML_BYTES: u64 = 256 << 20;
    /// Largest `.enex` file. These aren't compressed, but they carry every
    /// attachment of every note as base64.
    pub const MAX_ENEX_BYTES: u64 = 1 << 30;
    /// Most ink samples in one imported document. Parsed samples are about
    /// 70 bytes each, so this bounds memory at about 1 GiB.
    pub const MAX_TOTAL_POINTS: usize = 16_000_000;
    /// Largest Joplin export. It holds the note bytes directly, with no compression.
    pub const MAX_JEX_BYTES: u64 = 1 << 30;
    /// Largest embedded image.
    pub const MAX_IMAGE_BYTES: usize = 64 << 20;
    /// Most pixels in an embedded image. Decoded by the webview, so this
    /// is what stops a tiny PNG expanding to gigabytes of bitmap.
    pub const MAX_IMAGE_PIXELS: u64 = 100_000_000;
}
