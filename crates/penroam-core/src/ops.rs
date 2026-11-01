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
pub fn apply_batch(page: &mut Page, batch: &OperationBatch, now: Timestamp) -> Result<u64, ApplyError> {
    if batch.base_revision != page.revision {
        return Err(ApplyError::RevisionConflict { expected: batch.base_revision, actual: page.revision });
    }
    if batch.ops.is_empty() {
        return Err(ApplyError::Empty);
    }
    let mut next = page.elements.clone();
    let mut template = page.template.clone();
    for op in &batch.ops {
        apply_op(&mut next, &mut template, &op.kind)?;
    }
    page.elements = next;
    page.template = template;
    page.revision += 1;
    page.updated_at = now;
    Ok(page.revision)
}

/// Applies a batch that is already known to be valid (journal replay). Skips
/// the revision check but still refuses inconsistent operations.
pub fn replay_batch(page: &mut Page, batch: &OperationBatch, revision: u64, now: Timestamp) -> Result<(), ApplyError> {
    let mut next = page.elements.clone();
    let mut template = page.template.clone();
    for op in &batch.ops {
        apply_op(&mut next, &mut template, &op.kind)?;
    }
    page.elements = next;
    page.template = template;
    page.revision = revision;
    page.updated_at = now;
    Ok(())
}

fn apply_op(elements: &mut Vec<Element>, template: &mut Template, op: &OpKind) -> Result<(), ApplyError> {
    match op {
        OpKind::AddElements { elements: add } => {
            let mut ids: HashSet<Uuid> = elements.iter().map(Element::id).collect();
            for e in add {
                validate_element(e)?;
                if !ids.insert(e.id()) {
                    return Err(ApplyError::DuplicateElement(e.id()));
                }
            }
            elements.extend(add.iter().cloned());
            sort_by_z(elements);
        }
        OpKind::RemoveElements { ids } => {
            let remove: HashSet<Uuid> = ids.iter().copied().collect();
            for id in &remove {
                if !elements.iter().any(|e| e.id() == *id) {
                    return Err(ApplyError::MissingElement(*id));
                }
            }
            elements.retain(|e| !remove.contains(&e.id()));
        }
        OpKind::ReplaceElements { elements: repl } => {
            for e in repl {
                validate_element(e)?;
                let slot = elements
                    .iter_mut()
                    .find(|x| x.id() == e.id())
                    .ok_or(ApplyError::MissingElement(e.id()))?;
                *slot = e.clone();
            }
            sort_by_z(elements);
        }
        OpKind::SetTemplate { template: t } => {
            crate::validate::validate_template(t)?;
            *template = t.clone();
        }
    }
    Ok(())
}

/// Stable sort: equal z keeps insertion order.
fn sort_by_z(elements: &mut [Element]) {
    elements.sort_by_key(Element::z);
}
