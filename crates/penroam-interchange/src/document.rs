//! The result of an import: pages ready to be written into a new notebook,
//! and an honest account of what didn't survive.

use penroam_core::{Element, Size, Template, TemplateKind};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Why a file could not be imported at all. Anything less serious is a
/// note in [`ImportLoss`].
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The bytes are not a readable file of this format.
    #[error("{0}")]
    Malformed(String),
    /// The file exceeds a limit in [`crate::limits`].
    #[error("the file is too large: {0}")]
    TooLarge(String),
}

/// Blank pages to carry an attached PDF, one per PDF page. The sizes are replaced by the
/// PDF's own when the notebook is created. `None` when the bytes aren't a readable PDF.
pub(crate) fn pdf_attachment(bytes: Vec<u8>) -> Option<(ImportedAsset, Vec<ImportedPage>)> {
    let count = penroam_pdf::inspect(&bytes).ok()?.len() as u32;
    let asset = ImportedAsset::new(bytes, "application/pdf");
    let pages = (0..count)
        .map(|index| ImportedPage {
            size: Size {
                width: 612.0,
                height: 792.0,
            },
            template: Template {
                kind: TemplateKind::Blank,
                ..Template::default()
            },
            elements: Vec::new(),
            pdf: Some((asset.sha256.clone(), index)),
        })
        .collect();
    Some((asset, pages))
}

/// One page of an imported file.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedPage {
    /// Page size in points.
    pub size: Size,
    /// Background template.
    pub template: Template,
    /// Page content, with z values already assigned.
    pub elements: Vec<Element>,
    /// A page of a PDF in [`ImportedDocument::assets`] that sits behind this page:
    /// the PDF's SHA-256 and the zero-based page index.
    pub pdf: Option<(String, u32)>,
}

/// An attachment that elements refer to by [`ImportedAsset::sha256`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAsset {
    /// Lowercase hex SHA-256 of `bytes`.
    pub sha256: String,
    /// The bytes.
    pub bytes: Vec<u8>,
    /// MIME type.
    pub media_type: String,
}

impl ImportedAsset {
    /// Wraps bytes, computing their address.
    pub fn new(bytes: Vec<u8>, media_type: &str) -> Self {
        let sha256 = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Self {
            sha256,
            bytes,
            media_type: media_type.to_string(),
        }
    }
}

/// How much of a file came through.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Kept {
    /// Pages created.
    pub pages: u32,
    /// Ink strokes.
    pub strokes: u32,
    /// Text blocks.
    pub text_blocks: u32,
    /// Images.
    pub images: u32,
}

/// What an import kept and what it lost. Shown to the user after every
/// import, so entries are written for them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportLoss {
    /// Name of the source format.
    pub format: String,
    /// What came through.
    pub kept: Kept,
    /// Everything that was lost or changed, one sentence each.
    pub lost: Vec<String>,
}

/// Collects loss notes, counting repeats of the same kind.
#[derive(Debug, Default)]
pub(crate) struct LossLog(BTreeMap<&'static str, u32>);

impl LossLog {
    /// Records one occurrence of `what`, a sentence with `{n}` for the count.
    pub(crate) fn note(&mut self, what: &'static str) {
        *self.0.entry(what).or_default() += 1;
    }

    /// Same as [`LossLog::note`]; for sentences that take no count.
    pub(crate) fn note_once(&mut self, what: &'static str) {
        self.note(what);
    }

    pub(crate) fn sentences(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|(what, n)| what.replace("{n}", &n.to_string()))
            .collect()
    }
}

/// A file ready to become a notebook.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedDocument {
    /// Suggested notebook title.
    pub title: String,
    /// Pages in order. Never empty.
    pub pages: Vec<ImportedPage>,
    /// Attachments, each listed once.
    pub assets: Vec<ImportedAsset>,
    /// What was kept and lost.
    pub report: ImportLoss,
}
