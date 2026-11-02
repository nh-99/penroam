//! Semantic page operations. Every edit the UI makes (ink commit, erase, text
//! edit, move, undo, redo, template change) is expressed as a batch of these
//! operations and applied by [`apply_batch`]. Undo/redo are ordinary batches
//! carrying the inverse operations, so the persisted state is always the
//! deterministic result of applying batches in revision order.
//!
//! Each operation and batch has a stable UUID and each batch names the page
//! revision it was built on. This is not a sync/CRDT mechanism: a revision
//! mismatch is rejected rather than merged.

use crate::model::{Element, Page, Template, Timestamp};
use crate::validate::{validate_element, ValidationError};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OperationBatch {
    pub batch_id: Uuid,
    /// The page revision this batch was built against.
    #[ts(type = "number")]
    pub base_revision: u64,
    pub ops: Vec<PageOperation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PageOperation {
    pub op_id: Uuid,
    #[serde(flatten)]
    pub kind: OpKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "camelCase")]
#[ts(export)]
pub enum OpKind {
    /// Inserts new elements (ids must not exist yet).
    AddElements { elements: Vec<Element> },
    /// Removes existing elements by id.
    RemoveElements { ids: Vec<Uuid> },
    /// Replaces existing elements (matched by id) with new versions.
    ReplaceElements { elements: Vec<Element> },
    /// Changes the page background template.
    SetTemplate { template: Template },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ApplyError {
    #[error("revision conflict: page is at {actual}, batch was built on {expected}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("element {0} already exists")]
    DuplicateElement(Uuid),
    #[error("element {0} does not exist")]
    MissingElement(Uuid),
    #[error("invalid element: {0}")]
    Invalid(#[from] ValidationError),
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

/// Stable sort: equal z keeps insertion order.
fn sort_by_z(elements: &mut [Element]) {
    elements.sort_by_key(Element::z);
}
