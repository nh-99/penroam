//! Semantic page operations. Every edit the UI makes (ink commit, erase, text
//! edit, move, undo, redo, template change) is expressed as a batch of these
//! operations and applied by [`apply_batch`]. Undo/redo are ordinary batches
//! carrying the inverse operations, so the persisted state is always the
//! deterministic result of applying batches in revision order.
//!
//! Each operation and batch has a stable UUID and each batch names the page
//! revision it was built on. A revision mismatch is rejected rather than
//! merged: this is the single-writer path of one device. Merging operations
//! from several devices happens in [`crate::replica`], on the
//! [`crate::operation::DocumentOperation`]s these batches are journaled as.

use crate::model::{Element, Page, Template, Timestamp};
use crate::validate::{validate_element, ValidationError};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
/// One atomic edit from the editor: applies completely or not at all.
pub struct OperationBatch {
    /// Stable id; a retried batch id returns the original outcome.
    pub batch_id: Uuid,
    /// The page revision this batch was built against.
    #[ts(type = "number")]
    pub base_revision: u64,
    /// The operations, applied in order.
    pub ops: Vec<PageOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
/// One operation inside a batch.
pub struct PageOperation {
    /// Stable id (becomes the journaled operation's id).
    pub op_id: Uuid,
    /// What it does.
    #[serde(flatten)]
    pub kind: OpKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "camelCase")]
#[ts(export)]
/// The kinds of page edit. JSON-tagged by `op`.
pub enum OpKind {
    /// Inserts new elements (ids must not exist yet).
    AddElements {
        /// Elements to append.
        elements: Vec<Element>,
    },
    /// Removes existing elements by id.
    RemoveElements {
        /// Ids to remove.
        ids: Vec<Uuid>,
    },
    /// Replaces existing elements (matched by id) with new versions.
    ReplaceElements {
        /// New versions; each id must exist.
        elements: Vec<Element>,
    },
    /// Changes the page background template.
    SetTemplate {
        /// The new template.
        template: Template,
    },
}

/// Why a batch was rejected (the page is unchanged in every case).
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ApplyError {
    /// The batch was built on another revision (the editor must reload).
    #[error("revision conflict: page is at {actual}, batch was built on {expected}")]
    RevisionConflict {
        /// Revision named by the batch.
        expected: u64,
        /// Revision of the page.
        actual: u64,
    },
    /// An added element id already exists on the page.
    #[error("element {0} already exists")]
    DuplicateElement(Uuid),
    /// A removed or replaced element id does not exist.
    #[error("element {0} does not exist")]
    MissingElement(Uuid),
    /// Element data failed validation.
    #[error("invalid element: {0}")]
    Invalid(#[from] ValidationError),
    /// The batch has no operations.
    #[error("empty operation batch")]
    Empty,
}

/// Applies a batch atomically: either every operation applies or the page is
/// left untouched. Returns the new revision.
pub fn apply_batch(
    page: &mut Page,
    batch: &OperationBatch,
    now: Timestamp,
) -> Result<u64, ApplyError> {
    check_batch(page, batch)?;
    Ok(apply_checked(page, batch, None, now))
}

/// Validates a batch against the page without modifying it (revision, ids,
/// element data). After `Ok`, [`apply_checked`] cannot fail. This avoids
/// copying the whole page per batch (a 10k-stroke page is ~20 MB).
pub fn check_batch(page: &Page, batch: &OperationBatch) -> Result<(), ApplyError> {
    if batch.base_revision != page.revision {
        return Err(ApplyError::RevisionConflict {
            expected: batch.base_revision,
            actual: page.revision,
        });
    }
    if batch.ops.is_empty() {
        return Err(ApplyError::Empty);
    }
    check_ops(page, &batch.ops)
}

pub(crate) fn check_ops(page: &Page, ops: &[PageOperation]) -> Result<(), ApplyError> {
    let mut ids: HashSet<Uuid> = page.elements.iter().map(Element::id).collect();
    for op in ops {
        match &op.kind {
            OpKind::AddElements { elements } => {
                for e in elements {
                    validate_element(e)?;
                    if !ids.insert(e.id()) {
                        return Err(ApplyError::DuplicateElement(e.id()));
                    }
                }
            }
            OpKind::RemoveElements { ids: rm } => {
                for id in rm {
                    if !ids.remove(id) {
                        // removing the same id twice in one op is also missing
                        return Err(ApplyError::MissingElement(*id));
                    }
                }
            }
            OpKind::ReplaceElements { elements } => {
                for e in elements {
                    validate_element(e)?;
                    if !ids.contains(&e.id()) {
                        return Err(ApplyError::MissingElement(e.id()));
                    }
                }
            }
            OpKind::SetTemplate { template } => crate::validate::validate_template(template)?,
        }
    }
    Ok(())
}

/// Applies a batch that passed [`check_batch`] in place. `revision`
/// overrides the new revision (journal replay); otherwise it is +1.
pub fn apply_checked(
    page: &mut Page,
    batch: &OperationBatch,
    revision: Option<u64>,
    now: Timestamp,
) -> u64 {
    for op in &batch.ops {
        apply_op_unchecked(&mut page.elements, &mut page.template, &op.kind);
    }
    page.revision = revision.unwrap_or(page.revision + 1);
    page.updated_at = now;
    page.revision
}

/// Applies a batch that is already known to be valid (journal replay). Skips
/// the revision check but still refuses inconsistent operations.
pub fn replay_batch(
    page: &mut Page,
    batch: &OperationBatch,
    revision: u64,
    now: Timestamp,
) -> Result<(), ApplyError> {
    check_ops(page, &batch.ops)?;
    apply_checked(page, batch, Some(revision), now);
    Ok(())
}

pub(crate) fn apply_op_unchecked(
    elements: &mut Vec<Element>,
    template: &mut Template,
    op: &OpKind,
) {
    match op {
        OpKind::AddElements { elements: add } => {
            let max_z = elements.last().map(Element::z).unwrap_or(i64::MIN);
            let sorted = add.windows(2).all(|w| w[0].z() <= w[1].z());
            elements.extend(add.iter().cloned());
            if !(sorted && add.first().is_none_or(|e| e.z() >= max_z)) {
                sort_by_z(elements);
            }
        }
        OpKind::RemoveElements { ids } => {
            if ids.len() == 1 {
                if let Some(i) = elements.iter().position(|e| e.id() == ids[0]) {
                    elements.remove(i);
                }
            } else {
                let remove: HashSet<Uuid> = ids.iter().copied().collect();
                elements.retain(|e| !remove.contains(&e.id()));
            }
        }
        OpKind::ReplaceElements { elements: repl } => {
            let mut resort = false;
            for e in repl {
                if let Some(slot) = elements.iter_mut().find(|x| x.id() == e.id()) {
                    resort |= slot.z() != e.z();
                    *slot = e.clone();
                }
            }
            if resort {
                sort_by_z(elements);
            }
        }
        OpKind::SetTemplate { template: t } => *template = t.clone(),
    }
}

/// The operations that turn `from` into `to`: removals, then additions, then
/// replacements, then the template. Empty when the pages have the same content.
/// Used to restore an old version as an ordinary forward edit.
pub fn page_diff(from: &Page, to: &Page) -> Vec<OpKind> {
    let old: HashMap<Uuid, &Element> = from.elements.iter().map(|e| (e.id(), e)).collect();
    let new: HashSet<Uuid> = to.elements.iter().map(Element::id).collect();
    let ids: Vec<Uuid> = from
        .elements
        .iter()
        .map(Element::id)
        .filter(|id| !new.contains(id))
        .collect();
    let (mut added, mut replaced) = (Vec::new(), Vec::new());
    for e in &to.elements {
        match old.get(&e.id()) {
            None => added.push(e.clone()),
            Some(o) if *o != e => replaced.push(e.clone()),
            Some(_) => {}
        }
    }
    let mut ops = Vec::new();
    if !ids.is_empty() {
        ops.push(OpKind::RemoveElements { ids });
    }
    if !added.is_empty() {
        ops.push(OpKind::AddElements { elements: added });
    }
    if !replaced.is_empty() {
        ops.push(OpKind::ReplaceElements { elements: replaced });
    }
    if from.template != to.template {
        ops.push(OpKind::SetTemplate {
            template: to.template.clone(),
        });
    }
    ops
}

/// Stable sort: equal z keeps insertion order.
fn sort_by_z(elements: &mut [Element]) {
    elements.sort_by_key(Element::z);
}
