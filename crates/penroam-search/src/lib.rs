//! Penroam local search: find typed text, handwriting and PDF text across the whole library,
//! offline, and point back at the exact ink.
//!
//! * [`segment`]: groups pen strokes into handwriting regions (lines) using vector geometry
//!   only, with stable identities so edits re-recognize only what changed.
//! * [`recognize`]: the [`Recognizer`] provider interface, a deterministic test provider, the
//!   platform-first [`ChainRecognizer`], and the platform providers: (feature `windows-ink`) the
//!   Windows handwriting recognizers and, on macOS / iOS, Apple Vision (`vision`).
//! * [`model`]: the optional Penroam handwriting model (bundle format, verification, CTC
//!   decoding; ONNX Runtime inference with feature `onnx-model`), the fallback where the
//!   platform has no recognizer.
//! * [`raster`]: normalised line rendering for image-based recognizers.
//! * [`index`]: the derived SQLite FTS5 index ([`SearchIndex`]): per-page rows with their
//!   source ids, bounds and recognizer versions; ranking; user corrections.
//! * [`query`]: safe parsing of user input into FTS5 queries; fuzzy matching helpers.
//!
//! Everything here is derived, local metadata. Notes are never modified, nothing is sent over
//! the network, and the index can be deleted and rebuilt from the notes at any time. The app
//! decides when pages are indexed (`apps/desktop-mobile/src-tauri/src/search.rs`).
//! Design and privacy: `docs/architecture/overview.md` and `docs/security/search-index.md`.

#![warn(missing_docs)]

pub mod index;
pub mod model;
pub mod query;
pub mod raster;
pub mod recognize;
pub mod segment;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod vision;
#[cfg(all(windows, feature = "windows-ink"))]
pub mod windows_ink;

pub use index::{
    EntryKind, IndexStats, PageOutcome, PageState, Recognition, SearchError, SearchHit,
    SearchIndex, SnippetPart,
};
pub use recognize::{
    ChainRecognizer, RecognitionCandidate, RecognizeError, Recognizer, RecognizerInfo,
    ScriptedRecognizer,
};
pub use segment::{Rect, Region};
