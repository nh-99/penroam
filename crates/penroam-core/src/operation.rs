//! Sync-ready semantic operations (operation schema version 1).
//!
//! Every persisted change to a notebook is a [`LocalOperation`]: a stable
//! random id (retries reuse it), the device that made it, that device's
//! monotonically increasing `local_sequence` (never a wall clock), the causal
//! frontier the device had seen, and a typed [`DocumentOperation`] body.
//!
//! Two appliers share these types:
//!
//! * **Local** ([`apply_to_notebook`], [`apply_to_page`]): the single-writer
//!   path used by the live library and by journal replay. Page edits reuse
//!   the editor's page-operation semantics exactly, so replaying a journal yields
//!   bit-identical pages; notebook edits are idempotent "set" operations so
//!   re-applying a journal suffix converges to the same state.
//! * **Merge** ([`crate::replica::Replica`]): order-independent convergence
//!   of operations from several devices with retained conflict alternates.
//!
//! Only features that exist in the app are represented. Bodies from a newer
//! operation schema are never applied or dropped: they fail with
//! [`OpError::UnsupportedSchema`] so callers can pause (`upgrade_required`)
//! and keep the bytes.

use crate::model::*;
use crate::ops::{self, ApplyError, OpKind, OperationBatch};
use crate::validate::{validate_element, validate_page, validate_template, validate_title};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Newest [`DocumentOperation`] encoding and semantics this build can read.
/// Independent of the file format and model schema versions.
pub const OP_SCHEMA_VERSION: u16 = 2;

/// The schema an operation is written with: the oldest that means the same
/// thing, so that a device on an older version only stops syncing once it
/// receives something it can't honour.
///
/// Version 2 is a `notebookPurged` that syncs. Without its own version an older
/// client would take one for a forged payload and raise an integrity alert;
/// this way it parks the operation and asks to be updated.
pub fn op_schema_for(body: &DocumentOperation) -> u16 {
    match body {
        DocumentOperation::NotebookPurged { .. } => 2,
        _ => 1,
    }
}

/// Globally unique operation id (random UUID), stable across retries.
pub type OperationId = Uuid;
/// Id of the device (or sync device) that wrote an operation.
pub type DeviceId = Uuid;

/// "Device `device_id` up to and including `device_seq`."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRef {
    /// The device.
    pub device_id: DeviceId,
    /// Its sequence number (inclusive).
    pub device_seq: u64,
}

/// One journaled change: a typed body plus the identity and causal context
/// needed to deduplicate, order and merge it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalOperation {
    /// Stable id (retries reuse it).
    pub id: OperationId,
    /// The writing device.
    pub device_id: DeviceId,
    /// 1, 2, 3, … per device, persisted; single writer per device.
    pub local_sequence: u64,
    /// [`OP_SCHEMA_VERSION`] of the body encoding.
    pub schema_version: u16,
    /// Causal frontier when the operation was made: for each device whose
    /// operations had been applied locally, the highest contiguous sequence.
    /// Includes the own device (`local_sequence - 1`) unless that is 0.
    pub causal_dependencies: Vec<OperationRef>,
    /// Wall-clock milliseconds when made. Informational (`updatedAt`,
    /// `deletedAt` values) — never used for ordering or merging.
    pub at: Timestamp,
    /// What changed.
    pub body: DocumentOperation,
}

impl LocalOperation {
    /// This operation as a causal reference (device, sequence).
    pub fn op_ref(&self) -> OperationRef {
        OperationRef {
            device_id: self.device_id,
            device_seq: self.local_sequence,
        }
    }
}

/// Field groups of an element that merge independently (a concurrent move
/// and text edit of the same block both survive).
pub mod groups {
    /// z, position/size (ink: the points).
    pub const PLACEMENT: u8 = 1;
    /// text, asset, ink input metadata, unknown fields.
    pub const CONTENT: u8 = 2;
    /// colour, opacity, widths, tool, font size, alignment.
    pub const STYLE: u8 = 4;
    /// Every group.
    pub const ALL: u8 = PLACEMENT | CONTENT | STYLE;
}

/// The typed change carried by a [`LocalOperation`]. JSON-tagged by `type`.
/// Notebook-level variants change the notebook document; element and
/// template variants change one page's content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DocumentOperation {
    /// Creates the notebook (first operation of every notebook).
    #[serde(rename_all = "camelCase")]
    NotebookCreated {
        /// Target notebook.
        notebook_id: Uuid,
        /// New title (trimmed when applied).
        title: String,
        /// Cover colour `#rrggbb`.
        cover: String,
        /// Defaults for new pages.
        defaults: PageDefaults,
    },
    /// Sets the title.
    #[serde(rename_all = "camelCase")]
    NotebookRenamed {
        /// Target notebook.
        notebook_id: Uuid,
        /// New title (trimmed when applied).
        title: String,
    },
    /// Sets the cover colour.
    #[serde(rename_all = "camelCase")]
    NotebookCoverChanged {
        /// Target notebook.
        notebook_id: Uuid,
        /// Cover colour `#rrggbb`.
        cover: String,
    },
    /// Moves the notebook to the trash (tombstone; nothing is erased).
    #[serde(rename_all = "camelCase")]
    NotebookDeleted {
        /// Target notebook.
        notebook_id: Uuid,
    },
    /// Takes the notebook out of the trash.
    #[serde(rename_all = "camelCase")]
    NotebookRestored {
        /// Target notebook.
        notebook_id: Uuid,
    },
    /// Physical erasure of a trashed notebook from a library. Every device of a
    /// synced account erases its copy, and the operations stay in the account,
    /// so a later `NotebookRestored` brings it back: not merged
    /// (hosted deletion/retention is a separate, later policy).
    #[serde(rename_all = "camelCase")]
    NotebookPurged {
        /// Target notebook.
        notebook_id: Uuid,
    },
    /// Adds a page at the position given by its order key.
    #[serde(rename_all = "camelCase")]
    PageCreated {
        /// Target notebook.
        notebook_id: Uuid,
        /// Target page.
        page_id: Uuid,
        /// Fractional order key (see [`crate::order_key`]).
        order_key: String,
        /// Paper size class.
        paper: Paper,
        /// Page size in points.
        size: Size,
        /// Background template.
        template: Template,
        /// PDF page shown under the content, for imported PDFs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<PdfBackgroundRef>,
    },
    /// Moves a page to the notebook's trash (tombstone).
    #[serde(rename_all = "camelCase")]
    PageDeleted {
        /// Target notebook.
        notebook_id: Uuid,
        /// Target page.
        page_id: Uuid,
    },
    /// Takes a page out of the trash.
    #[serde(rename_all = "camelCase")]
    PageRestored {
        /// Target notebook.
        notebook_id: Uuid,
        /// Target page.
        page_id: Uuid,
    },
    /// Moves a page by giving it a new order key.
    #[serde(rename_all = "camelCase")]
    PageReordered {
        /// Target notebook.
        notebook_id: Uuid,
        /// Target page.
        page_id: Uuid,
        /// Fractional order key (see [`crate::order_key`]).
        order_key: String,
    },
    /// Changes a page's background template.
    #[serde(rename_all = "camelCase")]
    PageTemplateSet {
        /// Target notebook.
        notebook_id: Uuid,
        /// Target page.
        page_id: Uuid,
        /// Background template.
        template: Template,
    },
    /// Adds elements; an id that was removed earlier is restored (undo).
    #[serde(rename_all = "camelCase")]
    ElementsAdded {
        /// Target notebook.
        notebook_id: Uuid,
        /// Target page.
        page_id: Uuid,
        /// The elements, in order.
        elements: Vec<Element>,
    },
    /// Removes elements (tombstones in the merge model).
    #[serde(rename_all = "camelCase")]
    ElementsRemoved {
        /// Target notebook.
        notebook_id: Uuid,
        /// Target page.
        page_id: Uuid,
        /// Ids of the elements to remove.
        ids: Vec<Uuid>,
    },
    /// Replaces elements with new versions. `changed[i]` is the
    /// [`groups`] mask of what differs from the version the device had, so
    /// merging takes only those field groups from `elements[i]`.
    #[serde(rename_all = "camelCase")]
    ElementsReplaced {
        /// Target notebook.
        notebook_id: Uuid,
        /// Target page.
        page_id: Uuid,
        /// The elements, in order.
        elements: Vec<Element>,
        /// Per element, the [`groups`] mask that differs from the old version.
        changed: Vec<u8>,
    },
    /// An immutable, hash-addressed attachment became part of the notebook.
    #[serde(rename_all = "camelCase")]
    AssetReferenced {
        /// Target notebook.
        notebook_id: Uuid,
        /// The asset (bytes are stored separately, by hash).
        asset: Asset,
    },
}

impl DocumentOperation {
    /// The notebook this operation belongs to.
    pub fn notebook_id(&self) -> Uuid {
        use DocumentOperation::*;
        match self {
            NotebookCreated { notebook_id, .. }
            | NotebookRenamed { notebook_id, .. }
            | NotebookCoverChanged { notebook_id, .. }
            | NotebookDeleted { notebook_id }
            | NotebookRestored { notebook_id }
            | NotebookPurged { notebook_id }
            | PageCreated { notebook_id, .. }
            | PageDeleted { notebook_id, .. }
            | PageRestored { notebook_id, .. }
            | PageReordered { notebook_id, .. }
            | PageTemplateSet { notebook_id, .. }
            | ElementsAdded { notebook_id, .. }
            | ElementsRemoved { notebook_id, .. }
            | ElementsReplaced { notebook_id, .. }
            | AssetReferenced { notebook_id, .. } => *notebook_id,
        }
    }

    /// The page whose *content* this operation edits (element and template
    /// operations). Page list operations return `None`.
    pub fn content_page(&self) -> Option<Uuid> {
        use DocumentOperation::*;
        match self {
            PageTemplateSet { page_id, .. }
            | ElementsAdded { page_id, .. }
            | ElementsRemoved { page_id, .. }
            | ElementsReplaced { page_id, .. } => Some(*page_id),
            _ => None,
        }
    }

    /// Wire name of the variant (diagnostics).
    pub fn name(&self) -> &'static str {
        use DocumentOperation::*;
        match self {
            NotebookCreated { .. } => "notebookCreated",
            NotebookRenamed { .. } => "notebookRenamed",
            NotebookCoverChanged { .. } => "notebookCoverChanged",
            NotebookDeleted { .. } => "notebookDeleted",
            NotebookRestored { .. } => "notebookRestored",
            NotebookPurged { .. } => "notebookPurged",
            PageCreated { .. } => "pageCreated",
            PageDeleted { .. } => "pageDeleted",
            PageRestored { .. } => "pageRestored",
            PageReordered { .. } => "pageReordered",
            PageTemplateSet { .. } => "pageTemplateSet",
            ElementsAdded { .. } => "elementsAdded",
            ElementsRemoved { .. } => "elementsRemoved",
            ElementsReplaced { .. } => "elementsReplaced",
            AssetReferenced { .. } => "assetReferenced",
        }
    }

    /// The page operation this body corresponds to, for page content edits.
    pub fn as_page_op_kind(&self) -> Option<OpKind> {
        use DocumentOperation::*;
        Some(match self {
            ElementsAdded { elements, .. } => OpKind::AddElements {
                elements: elements.clone(),
            },
            ElementsRemoved { ids, .. } => OpKind::RemoveElements { ids: ids.clone() },
            ElementsReplaced { elements, .. } => OpKind::ReplaceElements {
                elements: elements.clone(),
            },
            PageTemplateSet { template, .. } => OpKind::SetTemplate {
                template: template.clone(),
            },
            _ => return None,
        })
    }

    /// Ink strokes carried by this operation, in encoding order (the journal
    /// stores their points in the compact column format).
    pub fn ink_strokes(&self) -> Vec<&InkStroke> {
        self.elements()
            .iter()
            .filter_map(|e| match e {
                Element::Ink(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    /// Mutable access to the ink strokes carried by this operation.
    pub fn ink_strokes_mut(&mut self) -> Vec<&mut InkStroke> {
        use DocumentOperation::*;
        match self {
            ElementsAdded { elements, .. } | ElementsReplaced { elements, .. } => elements
                .iter_mut()
                .filter_map(|e| match e {
                    Element::Ink(s) => Some(s),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Elements carried by `elementsAdded` / `elementsReplaced` (empty otherwise).
    pub fn elements(&self) -> &[Element] {
        use DocumentOperation::*;
        match self {
            ElementsAdded { elements, .. } | ElementsReplaced { elements, .. } => elements,
            _ => &[],
        }
    }
}

/// Why an operation could not be validated or applied.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OpError {
    /// The body comes from a newer operation schema; keep it, do not apply.
    #[error("operation schema {found} is newer than this build supports ({OP_SCHEMA_VERSION}); upgrade required")]
    UnsupportedSchema {
        /// The schema version found.
        found: u16,
    },
    /// The target notebook does not exist.
    #[error("operation targets notebook {0}, which does not exist")]
    NoNotebook(Uuid),
    /// The target page does not exist in the notebook.
    #[error("page {0} does not exist in this notebook")]
    NoPage(Uuid),
    /// A precondition failed (invalid value, wrong target, …).
    #[error("{0}")]
    Precondition(String),
    /// The page operation itself failed (see [`ApplyError`]).
    #[error(transparent)]
    Apply(#[from] ApplyError),
}

/// Rejects bodies from a newer schema before anything is applied.
pub fn check_schema(op: &LocalOperation) -> Result<(), OpError> {
    if op.schema_version > OP_SCHEMA_VERSION || op.schema_version == 0 {
        return Err(OpError::UnsupportedSchema {
            found: op.schema_version,
        });
    }
    Ok(())
}

/// Translates an editor batch into operation bodies, one per page
/// operation, preserving order. `page` is the state the batch applies to
/// (used to compute the changed field groups of replacements).
pub fn bodies_from_batch(
    notebook_id: Uuid,
    page: &Page,
    batch: &OperationBatch,
) -> Vec<(OperationId, DocumentOperation)> {
    let page_id = page.id;
    batch
        .ops
        .iter()
        .map(|op| {
            let body = match &op.kind {
                OpKind::AddElements { elements } => DocumentOperation::ElementsAdded {
                    notebook_id,
                    page_id,
                    elements: elements.clone(),
                },
                OpKind::RemoveElements { ids } => DocumentOperation::ElementsRemoved {
                    notebook_id,
                    page_id,
                    ids: ids.clone(),
                },
                OpKind::ReplaceElements { elements } => DocumentOperation::ElementsReplaced {
                    notebook_id,
                    page_id,
                    changed: elements
                        .iter()
                        .map(|e| {
                            page.elements
                                .iter()
                                .find(|x| x.id() == e.id())
                                .map_or(groups::ALL, |old| changed_groups(old, e))
                        })
                        .collect(),
                    elements: elements.clone(),
                },
                OpKind::SetTemplate { template } => DocumentOperation::PageTemplateSet {
                    notebook_id,
                    page_id,
                    template: template.clone(),
                },
            };
            (op.op_id, body)
        })
        .collect()
}

/// Validates page content operations against a page (all or nothing) and
/// applies them in order with exactly the editor's batch semantics. Returns
/// without modifying the page if any operation is invalid.
pub fn apply_to_page(page: &mut Page, bodies: &[&DocumentOperation]) -> Result<(), OpError> {
    let kinds: Vec<ops::PageOperation> = bodies
        .iter()
        .map(|b| {
            if b.content_page() != Some(page.id) {
                return Err(OpError::Precondition(format!(
                    "{} does not target page {}",
                    b.name(),
                    page.id
                )));
            }
            Ok(ops::PageOperation {
                op_id: Uuid::nil(),
                kind: b.as_page_op_kind().expect("content op"),
            })
        })
        .collect::<Result<_, _>>()?;
    if let Some(DocumentOperation::ElementsReplaced { elements, changed, .. }) = bodies
        .iter()
        .find(|b| matches!(b, DocumentOperation::ElementsReplaced { elements, changed, .. } if elements.len() != changed.len()))
    {
        return Err(OpError::Precondition(format!(
            "replacement of {} elements carries {} change masks",
            elements.len(),
            changed.len()
        )));
    }
    ops::check_ops(page, &kinds)?;
    for k in &kinds {
        ops::apply_op_unchecked(&mut page.elements, &mut page.template, &k.kind);
    }
    Ok(())
}

/// What a notebook-level operation did locally.
#[derive(Debug, Clone, PartialEq)]
pub enum NotebookEffect {
    /// Already reflected (idempotent replay) — nothing to write.
    Unchanged,
    /// The notebook changed and its snapshot must be rewritten.
    Changed,
    /// A page must be created on disk with this initial content.
    PageCreated(Box<Page>),
}

/// Validation of a body in isolation (live path, before journaling).
pub fn validate_body(body: &DocumentOperation) -> Result<(), OpError> {
    use DocumentOperation::*;
    let v = |r: Result<(), crate::validate::ValidationError>| {
        r.map_err(|e| OpError::Apply(ApplyError::Invalid(e)))
    };
    match body {
        NotebookCreated {
            title,
            cover,
            defaults,
            ..
        } => {
            v(validate_title(title))?;
            v(validate_template(&defaults.template))?;
            if !crate::validate::is_color(cover) {
                return Err(OpError::Precondition("invalid cover colour".into()));
            }
        }
        NotebookRenamed { title, .. } => v(validate_title(title))?,
        NotebookCoverChanged { cover, .. } => {
            if !crate::validate::is_color(cover) {
                return Err(OpError::Precondition("invalid cover colour".into()));
            }
        }
        PageCreated {
            order_key,
            size,
            template,
            background,
            page_id,
            paper,
            ..
        } => {
            if !crate::order_key::is_valid(order_key) {
                return Err(OpError::Precondition("invalid page order key".into()));
            }
            let mut p = Page::new(*page_id, *paper, *size, template.clone(), 0);
            p.background = background.clone();
            v(validate_page(&p))?;
        }
        PageReordered { order_key, .. } => {
            if !crate::order_key::is_valid(order_key) {
                return Err(OpError::Precondition("invalid page order key".into()));
            }
        }
        PageTemplateSet { template, .. } => v(validate_template(template))?,
        ElementsAdded { elements, .. } => {
            for e in elements {
                v(validate_element(e))?;
            }
        }
        ElementsReplaced {
            elements, changed, ..
        } => {
            if elements.len() != changed.len() {
                return Err(OpError::Precondition("change mask count mismatch".into()));
            }
            for e in elements {
                v(validate_element(e))?;
            }
        }
        AssetReferenced { asset, .. } => {
            if !crate::validate::is_sha256_hex(&asset.sha256) {
                return Err(OpError::Precondition("invalid asset hash".into()));
            }
        }
        NotebookDeleted { .. }
        | NotebookRestored { .. }
        | NotebookPurged { .. }
        | PageDeleted { .. }
        | PageRestored { .. }
        | ElementsRemoved { .. } => {}
    }
    Ok(())
}

/// Applies a notebook-level operation to a notebook (single writer).
/// Idempotent: re-applying a suffix of the journal in order converges to
/// the same notebook. `NotebookCreated` is handled by [`notebook_from_create`];
/// `NotebookPurged` by storage.
pub fn apply_to_notebook(
    nb: &mut Notebook,
    op: &LocalOperation,
) -> Result<NotebookEffect, OpError> {
    use DocumentOperation::*;
    check_schema(op)?;
    if op.body.notebook_id() != nb.id {
        return Err(OpError::Precondition(
            "operation targets another notebook".into(),
        ));
    }
    let at = op.at;
    let changed = match &op.body {
        NotebookCreated { .. } | NotebookPurged { .. } => false,
        NotebookRenamed { title, .. } => {
            validate_title(title).map_err(|e| OpError::Apply(e.into()))?;
            set(&mut nb.title, title.trim().to_string())
        }
        NotebookCoverChanged { cover, .. } => set(&mut nb.cover, cover.to_lowercase()),
        NotebookDeleted { .. } => set(&mut nb.deleted_at, Some(at)),
        NotebookRestored { .. } => set(&mut nb.deleted_at, None),
        PageCreated {
            page_id,
            order_key,
            paper,
            size,
            template,
            background,
            ..
        } => {
            if nb.order_keys.contains_key(page_id) {
                return Ok(NotebookEffect::Unchanged);
            }
            if let Some(bg) = background {
                if !nb.assets.iter().any(|a| a.sha256 == bg.asset_hash) {
                    return Err(OpError::Precondition(format!(
                        "asset {} is not part of this notebook",
                        bg.asset_hash
                    )));
                }
            }
            let mut page = Page::new(*page_id, *paper, *size, template.clone(), at);
            page.background = background.clone();
            validate_page(&page).map_err(|e| OpError::Apply(e.into()))?;
            nb.order_keys.insert(*page_id, order_key.clone());
            nb.page_order.push(*page_id);
            nb.sort_page_order();
            nb.updated_at = nb.updated_at.max(at);
            return Ok(NotebookEffect::PageCreated(Box::new(page)));
        }
        PageDeleted { page_id, .. } => match nb.page_order.iter().position(|p| p == page_id) {
            Some(idx) => {
                nb.page_order.remove(idx);
                nb.trashed_pages.push(TrashedPage {
                    id: *page_id,
                    deleted_at: at,
                    former_index: idx as u32,
                });
                true
            }
            None if nb.trashed_pages.iter().any(|t| t.id == *page_id) => false,
            None => return Err(OpError::NoPage(*page_id)),
        },
        PageRestored { page_id, .. } => {
            match nb.trashed_pages.iter().position(|t| t.id == *page_id) {
                Some(i) => {
                    nb.trashed_pages.remove(i);
                    nb.page_order.push(*page_id);
                    nb.sort_page_order();
                    true
                }
                None if nb.page_order.contains(page_id) => false,
                None => return Err(OpError::NoPage(*page_id)),
            }
        }
        PageReordered {
            page_id, order_key, ..
        } => {
            if !nb.order_keys.contains_key(page_id) {
                return Err(OpError::NoPage(*page_id));
            }
            let c = set(nb.order_keys.get_mut(page_id).unwrap(), order_key.clone());
            nb.sort_page_order();
            c
        }
        AssetReferenced { asset, .. } => {
            if nb.assets.iter().any(|a| a.sha256 == asset.sha256) {
                false
            } else {
                nb.assets.push(asset.clone());
                true
            }
        }
        PageTemplateSet { .. }
        | ElementsAdded { .. }
        | ElementsRemoved { .. }
        | ElementsReplaced { .. } => {
            return Err(OpError::Precondition(
                "page content operations are applied to pages".into(),
            ))
        }
    };
    if changed {
        // monotone, so re-applying an older operation never rewinds it
        nb.updated_at = nb.updated_at.max(at);
        Ok(NotebookEffect::Changed)
    } else {
        Ok(NotebookEffect::Unchanged)
    }
}

fn set<T: PartialEq>(slot: &mut T, v: T) -> bool {
    if *slot == v {
        false
    } else {
        *slot = v;
        true
    }
}

/// The notebook a `NotebookCreated` operation describes (no pages yet).
pub fn notebook_from_create(op: &LocalOperation) -> Result<Notebook, OpError> {
    check_schema(op)?;
    match &op.body {
        DocumentOperation::NotebookCreated {
            notebook_id,
            title,
            cover,
            defaults,
        } => {
            validate_body(&op.body)?;
            Ok(Notebook::new(
                *notebook_id,
                title.trim().to_string(),
                defaults.clone(),
                cover.to_lowercase(),
                op.at,
            ))
        }
        _ => Err(OpError::Precondition(
            "not a notebookCreated operation".into(),
        )),
    }
}

// ------------------------------------------------------------ field groups --

/// Which [`groups`] differ between two versions of an element.
pub fn changed_groups(old: &Element, new: &Element) -> u8 {
    if std::mem::discriminant(old) != std::mem::discriminant(new) {
        return groups::ALL;
    }
    let mut m = 0;
    for g in [groups::PLACEMENT, groups::CONTENT, groups::STYLE] {
        let mut probe = old.clone();
        take_group(&mut probe, new, g);
        if probe != *old {
            m |= g;
        }
    }
    m
}

/// Copies one field group from `src` into `dst` (same element kind; a kind
/// mismatch leaves `dst` unchanged).
pub fn take_group(dst: &mut Element, src: &Element, group: u8) {
    match (dst, src) {
        (Element::Ink(d), Element::Ink(s)) => match group {
            groups::PLACEMENT => {
                d.z = s.z;
                d.points = s.points.clone();
            }
            groups::STYLE => {
                d.tool = s.tool;
                d.color = s.color.clone();
                d.opacity = s.opacity;
                d.width = s.width;
                d.pressure = s.pressure;
            }
            groups::CONTENT => {
                d.source = s.source.clone();
                d.input = s.input.clone();
                d.extra = s.extra.clone();
            }
            _ => {}
        },
        (Element::Text(d), Element::Text(s)) => match group {
            groups::PLACEMENT => {
                d.z = s.z;
                d.x = s.x;
                d.y = s.y;
                d.width = s.width;
                d.height = s.height;
            }
            groups::STYLE => {
                d.font_size = s.font_size;
                d.color = s.color.clone();
                d.align = s.align;
            }
            groups::CONTENT => {
                d.text = s.text.clone();
                d.extra = s.extra.clone();
            }
            _ => {}
        },
        (Element::Image(d), Element::Image(s)) => match group {
            groups::PLACEMENT => {
                d.z = s.z;
                d.x = s.x;
                d.y = s.y;
                d.width = s.width;
                d.height = s.height;
            }
            groups::CONTENT => {
                d.asset_hash = s.asset_hash.clone();
                d.extra = s.extra.clone();
            }
            _ => {}
        },
        _ => {}
    }
}
