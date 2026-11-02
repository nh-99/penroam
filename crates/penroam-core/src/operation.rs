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
//!   the M01 page-operation semantics exactly, so replaying a journal yields
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

/// Version of the [`DocumentOperation`] encoding and semantics. Independent
/// of the file format and model schema versions.
pub const OP_SCHEMA_VERSION: u16 = 1;

pub type OperationId = Uuid;
pub type DeviceId = Uuid;

/// "Device `device_id` up to and including `device_seq`."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRef {
    pub device_id: DeviceId,
    pub device_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalOperation {
    pub id: OperationId,
    pub device_id: DeviceId,
    /// 1, 2, 3, … per device, persisted; single writer per device.
    pub local_sequence: u64,
    pub schema_version: u16,
    /// Causal frontier when the operation was made: for each device whose
    /// operations had been applied locally, the highest contiguous sequence.
    /// Includes the own device (`local_sequence - 1`) unless that is 0.
    pub causal_dependencies: Vec<OperationRef>,
    /// Wall-clock milliseconds when made. Informational (`updatedAt`,
    /// `deletedAt` values) — never used for ordering or merging.
    pub at: Timestamp,
    pub body: DocumentOperation,
}

impl LocalOperation {
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
    pub const ALL: u8 = PLACEMENT | CONTENT | STYLE;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DocumentOperation {
    #[serde(rename_all = "camelCase")]
    NotebookCreated {
        notebook_id: Uuid,
        title: String,
        cover: String,
        defaults: PageDefaults,
    },
    #[serde(rename_all = "camelCase")]
    NotebookRenamed { notebook_id: Uuid, title: String },
    #[serde(rename_all = "camelCase")]
    NotebookCoverChanged { notebook_id: Uuid, cover: String },
    /// Moves the notebook to the trash (tombstone; nothing is erased).
    #[serde(rename_all = "camelCase")]
    NotebookDeleted { notebook_id: Uuid },
    #[serde(rename_all = "camelCase")]
    NotebookRestored { notebook_id: Uuid },
    /// Local physical erasure of a trashed notebook. Local-only: not merged
    /// (hosted deletion/retention is a separate, later policy).
    #[serde(rename_all = "camelCase")]
    NotebookPurged { notebook_id: Uuid },
    #[serde(rename_all = "camelCase")]
    PageCreated {
        notebook_id: Uuid,
        page_id: Uuid,
        order_key: String,
        paper: Paper,
        size: Size,
        template: Template,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<PdfBackgroundRef>,
    },
    /// Moves a page to the notebook's trash (tombstone).
    #[serde(rename_all = "camelCase")]
    PageDeleted { notebook_id: Uuid, page_id: Uuid },
    #[serde(rename_all = "camelCase")]
    PageRestored { notebook_id: Uuid, page_id: Uuid },
    #[serde(rename_all = "camelCase")]
    PageReordered {
        notebook_id: Uuid,
        page_id: Uuid,
        order_key: String,
    },
    #[serde(rename_all = "camelCase")]
    PageTemplateSet {
        notebook_id: Uuid,
        page_id: Uuid,
        template: Template,
    },
    /// Adds elements; an id that was removed earlier is restored (undo).
    #[serde(rename_all = "camelCase")]
    ElementsAdded {
        notebook_id: Uuid,
        page_id: Uuid,
        elements: Vec<Element>,
    },
    /// Removes elements (tombstones in the merge model).
    #[serde(rename_all = "camelCase")]
    ElementsRemoved {
        notebook_id: Uuid,
        page_id: Uuid,
        ids: Vec<Uuid>,
    },
    /// Replaces elements with new versions. `changed[i]` is the
    /// [`groups`] mask of what differs from the version the device had, so
    /// merging takes only those field groups from `elements[i]`.
    #[serde(rename_all = "camelCase")]
    ElementsReplaced {
        notebook_id: Uuid,
        page_id: Uuid,
        elements: Vec<Element>,
        changed: Vec<u8>,
    },
    /// An immutable, hash-addressed attachment became part of the notebook.
    #[serde(rename_all = "camelCase")]
    AssetReferenced { notebook_id: Uuid, asset: Asset },
}

impl DocumentOperation {
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

    pub fn elements(&self) -> &[Element] {
        use DocumentOperation::*;
        match self {
            ElementsAdded { elements, .. } | ElementsReplaced { elements, .. } => elements,
            _ => &[],
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OpError {
    #[error("operation schema {found} is newer than this build supports ({OP_SCHEMA_VERSION}); upgrade required")]
    UnsupportedSchema { found: u16 },
    #[error("operation targets notebook {0}, which does not exist")]
    NoNotebook(Uuid),
    #[error("page {0} does not exist in this notebook")]
    NoPage(Uuid),
    #[error("{0}")]
    Precondition(String),
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
/// applies them in order with exactly the M01 batch semantics. Returns
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
