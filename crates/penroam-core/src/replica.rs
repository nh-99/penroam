//! Order-independent merge of [`LocalOperation`]s from several devices.
//!
//! A [`Replica`] holds one notebook as a set of *multi-value registers*:
//! each writable field (title, cover, trash state, page order key, page
//! template, and each element's placement / content / style / trash state)
//! keeps every write that no later write has superseded. "Later" is causal,
//! decided by the writing operation's frontier (version vector) — never by
//! wall-clock time. Under causal delivery (each operation is applied after
//! everything it depends on) applying the same set of operations in any
//! order yields the same registers, so materialized notebooks, pages and
//! conflict lists are identical on every replica.
//!
//! Rules (protocol v1 §3):
//! * Concurrent writes of the same register: the write with the highest
//!   operation id is shown, every other one is retained as an alternate in
//!   [`Replica::conflicts`] until a later write supersedes them all.
//! * Independent elements, pages and field groups never conflict.
//! * Delete is a tombstone. Restoring (re-adding an id, page/notebook
//!   restore) is an explicit write; a concurrent restore beats a delete.
//!   An edit the deleting device had not seen keeps the element deleted but
//!   is reported as an `editedWhileDeleted` conflict with its value.
//! * Page order = (order key, page id).
//! * Element order = (z, element id).
//! * Retried operations (same id, same body) are no-ops; an id reused with a
//!   different body is rejected. Newer schemas are refused, not dropped.

use crate::model::*;
use crate::operation::*;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use uuid::Uuid;

type Frontier = BTreeMap<DeviceId, u64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct OpKey {
    device: DeviceId,
    seq: u64,
}

#[derive(Debug, Clone)]
struct Write<T> {
    id: OperationId,
    key: OpKey,
    frontier: Arc<Frontier>,
    value: T,
}

/// `h` happened before the operation (`key`, `frontier`).
fn dominated(h: OpKey, key: OpKey, frontier: &Frontier) -> bool {
    if h.device == key.device {
        h.seq < key.seq
    } else {
        frontier.get(&h.device).is_some_and(|s| *s >= h.seq)
    }
}

#[derive(Debug, Clone)]
struct Register<T> {
    /// Concurrent writes, sorted by operation id.
    heads: Vec<Write<T>>,
}

impl<T: Clone> Register<T> {
    fn new(w: Write<T>) -> Self {
        Self { heads: vec![w] }
    }
    fn write(&mut self, w: Write<T>) {
        self.heads.retain(|h| !dominated(h.key, w.key, &w.frontier));
        let at = self.heads.partition_point(|h| h.id < w.id);
        self.heads.insert(at, w);
    }
    fn winner(&self) -> &Write<T> {
        self.heads.last().expect("a register always has a write")
    }
}

impl Register<bool> {
    /// Add-wins: alive if any concurrent write says alive.
    fn alive_winner(&self) -> &Write<bool> {
        self.heads
            .iter()
            .rev()
            .find(|h| h.value)
            .unwrap_or_else(|| self.winner())
    }
}

#[derive(Debug, Clone)]
struct ElementState {
    alive: Register<bool>,
    placement: Register<Arc<Element>>,
    content: Register<Arc<Element>>,
    style: Register<Arc<Element>>,
}

impl ElementState {
    fn materialize(&self) -> Element {
        let mut e = (*self.content.winner().value).clone();
        take_group(&mut e, &self.placement.winner().value, groups::PLACEMENT);
        take_group(&mut e, &self.style.winner().value, groups::STYLE);
        e
    }
}

#[derive(Debug, Clone)]
struct PageState {
    created_at: Timestamp,
    paper: Paper,
    size: Size,
    background: Option<PdfBackgroundRef>,
    alive: Register<bool>,
    key: Register<String>,
    template: Register<Template>,
    elements: BTreeMap<Uuid, ElementState>,
    ops: u64,
    updated_at: Timestamp,
}

#[derive(Debug, Clone)]
struct NotebookState {
    created_at: Timestamp,
    defaults: PageDefaults,
    title: Register<String>,
    cover: Register<String>,
    alive: Register<bool>,
    deleted_at: HashMap<OperationId, Timestamp>,
}

/// Where a conflict is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ConflictTarget {
    NotebookTitle,
    NotebookCover,
    NotebookTrash,
    #[serde(rename_all = "camelCase")]
    PageOrder {
        page_id: Uuid,
    },
    #[serde(rename_all = "camelCase")]
    PageTemplate {
        page_id: Uuid,
    },
    #[serde(rename_all = "camelCase")]
    PageTrash {
        page_id: Uuid,
    },
    /// Concurrent writes of one field group (see [`groups`]).
    #[serde(rename_all = "camelCase")]
    Element {
        page_id: Uuid,
        element_id: Uuid,
        group: u8,
    },
    #[serde(rename_all = "camelCase")]
    ElementTrash {
        page_id: Uuid,
        element_id: Uuid,
    },
    /// The element is deleted, but an edit the deleting device had not
    /// seen exists; the alternate holds the edited element.
    #[serde(rename_all = "camelCase")]
    EditedWhileDeleted {
        page_id: Uuid,
        element_id: Uuid,
    },
}

/// A conflict: the visible winner and every retained alternative.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub target: ConflictTarget,
    pub winner: OperationId,
    pub alternates: Vec<Alternate>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alternate {
    pub op_id: OperationId,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    New,
    /// Same id and identical body seen before (retry): nothing changed.
    Duplicate,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MergeError {
    #[error(transparent)]
    Op(#[from] OpError),
    #[error("operation id {0} was already used for a different operation")]
    OpIdReuse(OperationId),
    #[error("device {device} sequence {seq} was already used by another operation")]
    SequenceReuse { device: DeviceId, seq: u64 },
    /// Not applicable yet: something it depends on is missing (keep it).
    #[error("waiting for {0:?}")]
    MissingDependency(OperationRef),
    #[error("operation is for notebook {0}, not this replica")]
    WrongNotebook(Uuid),
    #[error("{0} is local-only and is not merged")]
    LocalOnly(&'static str),
}

/// One notebook, merged from operations of any number of devices.
#[derive(Debug, Clone)]
pub struct Replica {
    notebook_id: Uuid,
    frontier: Frontier,
    applied: HashMap<OperationId, (OpKey, u32, usize)>,
    by_key: HashMap<OpKey, OperationId>,
    notebook: Option<NotebookState>,
    assets: BTreeMap<String, Asset>,
    pages: BTreeMap<Uuid, PageState>,
    updated_at: Timestamp,
}

fn fingerprint(op: &LocalOperation) -> (u32, usize) {
    let bytes = serde_json::to_vec(op).expect("operation serialises");
    (crc32fast::hash(&bytes), bytes.len())
}

impl Replica {
    pub fn new(notebook_id: Uuid) -> Self {
        Self {
            notebook_id,
            frontier: Frontier::new(),
            applied: HashMap::new(),
            by_key: HashMap::new(),
            notebook: None,
            assets: BTreeMap::new(),
            pages: BTreeMap::new(),
            updated_at: 0,
        }
    }

    /// Highest contiguous sequence applied per device (the frontier a new
    /// local operation would depend on).
    pub fn frontier(&self) -> Vec<OperationRef> {
        self.frontier
            .iter()
            .map(|(d, s)| OperationRef {
                device_id: *d,
                device_seq: *s,
            })
            .collect()
    }

    pub fn contains(&self, id: OperationId) -> bool {
        self.applied.contains_key(&id)
    }

    /// Applies one operation. On error nothing changed.
    pub fn apply(&mut self, op: &LocalOperation) -> Result<Applied, MergeError> {
        check_schema(op)?;
        if let DocumentOperation::NotebookPurged { .. } = op.body {
            return Err(MergeError::LocalOnly("notebookPurged"));
        }
        if op.body.notebook_id() != self.notebook_id {
            return Err(MergeError::WrongNotebook(op.body.notebook_id()));
        }
        let key = OpKey {
            device: op.device_id,
            seq: op.local_sequence,
        };
        let fp = fingerprint(op);
        if let Some((k, crc, len)) = self.applied.get(&op.id) {
            return if *k == key && (*crc, *len) == fp {
                Ok(Applied::Duplicate)
            } else {
                Err(MergeError::OpIdReuse(op.id))
            };
        }
        if self.by_key.contains_key(&key) || op.local_sequence == 0 {
            return Err(MergeError::SequenceReuse {
                device: op.device_id,
                seq: op.local_sequence,
            });
        }
        // causal delivery: contiguous per device, dependencies applied
        let have = self.frontier.get(&op.device_id).copied().unwrap_or(0);
        if have + 1 != op.local_sequence {
            return Err(MergeError::MissingDependency(OperationRef {
                device_id: op.device_id,
                device_seq: op.local_sequence - 1,
            }));
        }
        let mut frontier = Frontier::new();
        for d in &op.causal_dependencies {
            if d.device_id == op.device_id {
                if d.device_seq >= op.local_sequence {
                    return Err(MergeError::Op(OpError::Precondition(
                        "operation depends on itself or a later operation".into(),
                    )));
                }
                continue;
            }
            if self.frontier.get(&d.device_id).copied().unwrap_or(0) < d.device_seq {
                return Err(MergeError::MissingDependency(*d));
            }
            let e = frontier.entry(d.device_id).or_insert(0);
            *e = (*e).max(d.device_seq);
        }
        let w = WriteCtx {
            id: op.id,
            key,
            frontier: Arc::new(frontier),
        };
        // validate everything before mutating anything
        validate_body(&op.body)?;
        self.check(&op.body)?;
        self.mutate(&op.body, &w, op.at);
        self.frontier.insert(op.device_id, op.local_sequence);
        self.applied.insert(op.id, (key, fp.0, fp.1));
        self.by_key.insert(key, op.id);
        self.updated_at = self.updated_at.max(op.at);
        Ok(Applied::New)
    }

    fn page_state(&self, id: &Uuid) -> Result<&PageState, OpError> {
        self.pages.get(id).ok_or(OpError::NoPage(*id))
    }

    fn check(&self, body: &DocumentOperation) -> Result<(), OpError> {
        use DocumentOperation::*;
        let pre = |m: String| Err(OpError::Precondition(m));
        match body {
            NotebookCreated { .. } => {
                if self.notebook.is_some() {
                    return pre("notebook was already created".into());
                }
                return Ok(());
            }
            _ if self.notebook.is_none() => return Err(OpError::NoNotebook(self.notebook_id)),
            PageCreated {
                page_id,
                background,
                ..
            } => {
                if self.pages.contains_key(page_id) {
                    return pre(format!("page {page_id} was already created"));
                }
                if let Some(bg) = background {
                    if !self.assets.contains_key(&bg.asset_hash) {
                        return pre(format!("asset {} is not referenced", bg.asset_hash));
                    }
                }
            }
            PageDeleted { page_id, .. }
            | PageRestored { page_id, .. }
            | PageReordered { page_id, .. }
            | PageTemplateSet { page_id, .. } => {
                self.page_state(page_id)?;
            }
            ElementsAdded {
                page_id, elements, ..
            } => {
                let p = self.page_state(page_id)?;
                for e in elements {
                    if let Some(old) = p.elements.get(&e.id()) {
                        if std::mem::discriminant(&*old.content.winner().value)
                            != std::mem::discriminant(e)
                        {
                            return pre(format!("element {} changes kind", e.id()));
                        }
                    }
                }
            }
            ElementsRemoved { page_id, ids, .. } => {
                let p = self.page_state(page_id)?;
                if let Some(id) = ids.iter().find(|id| !p.elements.contains_key(id)) {
                    return pre(format!("element {id} does not exist"));
                }
            }
            ElementsReplaced {
                page_id, elements, ..
            } => {
                let p = self.page_state(page_id)?;
                for e in elements {
                    match p.elements.get(&e.id()) {
                        None => return pre(format!("element {} does not exist", e.id())),
                        Some(old)
                            if std::mem::discriminant(&*old.content.winner().value)
                                != std::mem::discriminant(e) =>
                        {
                            return pre(format!("element {} changes kind", e.id()))
                        }
                        _ => {}
                    }
                }
            }
            NotebookRenamed { .. }
            | NotebookCoverChanged { .. }
            | NotebookDeleted { .. }
            | NotebookRestored { .. }
            | NotebookPurged { .. }
            | AssetReferenced { .. } => {}
        }
        Ok(())
    }

    fn mutate(&mut self, body: &DocumentOperation, w: &WriteCtx, at: Timestamp) {
        use DocumentOperation::*;
        match body {
            NotebookCreated {
                title,
                cover,
                defaults,
                ..
            } => {
                self.notebook = Some(NotebookState {
                    created_at: at,
                    defaults: defaults.clone(),
                    title: Register::new(w.of(title.trim().to_string())),
                    cover: Register::new(w.of(cover.to_lowercase())),
                    alive: Register::new(w.of(true)),
                    deleted_at: HashMap::new(),
                });
            }
            NotebookRenamed { title, .. } => {
                self.nb().title.write(w.of(title.trim().to_string()));
            }
            NotebookCoverChanged { cover, .. } => {
                self.nb().cover.write(w.of(cover.to_lowercase()));
            }
            NotebookDeleted { .. } => {
                let nb = self.nb();
                nb.alive.write(w.of(false));
                nb.deleted_at.insert(w.id, at);
            }
            NotebookRestored { .. } => self.nb().alive.write(w.of(true)),
            NotebookPurged { .. } => unreachable!("rejected earlier"),
            AssetReferenced { asset, .. } => {
                self.assets
                    .entry(asset.sha256.clone())
                    .or_insert_with(|| asset.clone());
            }
            PageCreated {
                page_id,
                order_key,
                paper,
                size,
                template,
                background,
                ..
            } => {
                self.pages.insert(
                    *page_id,
                    PageState {
                        created_at: at,
                        paper: *paper,
                        size: *size,
                        background: background.clone(),
                        alive: Register::new(w.of(true)),
                        key: Register::new(w.of(order_key.clone())),
                        template: Register::new(w.of(template.clone())),
                        elements: BTreeMap::new(),
                        ops: 0,
                        updated_at: at,
                    },
                );
            }
            PageDeleted { page_id, .. } => self.pg(page_id, at).alive.write(w.of(false)),
            PageRestored { page_id, .. } => self.pg(page_id, at).alive.write(w.of(true)),
            PageReordered {
                page_id, order_key, ..
            } => self.pg(page_id, at).key.write(w.of(order_key.clone())),
            PageTemplateSet {
                page_id, template, ..
            } => {
                let p = self.pg(page_id, at);
                p.template.write(w.of(template.clone()));
                p.ops += 1;
            }
            ElementsAdded {
                page_id, elements, ..
            } => {
                let p = self.pg(page_id, at);
                p.ops += 1;
                for e in elements {
                    let v = Arc::new(e.clone());
                    match p.elements.get_mut(&e.id()) {
                        Some(s) => {
                            // re-adding a removed id is an explicit restore
                            s.alive.write(w.of(true));
                            s.placement.write(w.of(v.clone()));
                            s.content.write(w.of(v.clone()));
                            s.style.write(w.of(v));
                        }
                        None => {
                            p.elements.insert(
                                e.id(),
                                ElementState {
                                    alive: Register::new(w.of(true)),
                                    placement: Register::new(w.of(v.clone())),
                                    content: Register::new(w.of(v.clone())),
                                    style: Register::new(w.of(v)),
                                },
                            );
                        }
                    }
                }
            }
            ElementsRemoved { page_id, ids, .. } => {
                let p = self.pg(page_id, at);
                p.ops += 1;
                for id in ids {
                    p.elements
                        .get_mut(id)
                        .expect("checked")
                        .alive
                        .write(w.of(false));
                }
            }
            ElementsReplaced {
                page_id,
                elements,
                changed,
                ..
            } => {
                let p = self.pg(page_id, at);
                p.ops += 1;
                for (e, mask) in elements.iter().zip(changed) {
                    let v = Arc::new(e.clone());
                    let s = p.elements.get_mut(&e.id()).expect("checked");
                    if mask & groups::PLACEMENT != 0 {
                        s.placement.write(w.of(v.clone()));
                    }
                    if mask & groups::CONTENT != 0 {
                        s.content.write(w.of(v.clone()));
                    }
                    if mask & groups::STYLE != 0 {
                        s.style.write(w.of(v));
                    }
                }
            }
        }
    }

    fn nb(&mut self) -> &mut NotebookState {
        self.notebook.as_mut().expect("checked")
    }

    fn pg(&mut self, id: &Uuid, at: Timestamp) -> &mut PageState {
        let p = self.pages.get_mut(id).expect("checked");
        p.updated_at = p.updated_at.max(at);
        p
    }

    /// The merged notebook, or `None` before its creation was applied.
    pub fn notebook(&self) -> Option<Notebook> {
        let s = self.notebook.as_ref()?;
        let mut nb = Notebook::new(
            self.notebook_id,
            s.title.winner().value.clone(),
            s.defaults.clone(),
            s.cover.winner().value.clone(),
            s.created_at,
        );
        nb.updated_at = self.updated_at;
        let alive = s.alive.alive_winner();
        if !alive.value {
            nb.deleted_at = Some(s.deleted_at.get(&alive.id).copied().unwrap_or(0));
        }
        let mut all: Vec<(&String, &Uuid, &PageState)> = self
            .pages
            .iter()
            .map(|(id, p)| (&p.key.winner().value, id, p))
            .collect();
        all.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        for (i, (key, id, p)) in all.iter().enumerate() {
            nb.order_keys.insert(**id, (*key).clone());
            if p.alive.alive_winner().value {
                nb.page_order.push(**id);
            } else {
                nb.trashed_pages.push(TrashedPage {
                    id: **id,
                    deleted_at: p.updated_at,
                    former_index: i as u32,
                });
            }
        }
        nb.assets = self.assets.values().cloned().collect();
        Some(nb)
    }

    /// A merged page (live or trashed). `revision` counts applied content
    /// operations, which is the same on every replica.
    pub fn page(&self, id: Uuid) -> Option<Page> {
        let p = self.pages.get(&id)?;
        let mut page = Page::new(
            id,
            p.paper,
            p.size,
            p.template.winner().value.clone(),
            p.created_at,
        );
        page.background = p.background.clone();
        page.revision = p.ops;
        page.updated_at = p.updated_at;
        page.elements = p
            .elements
            .values()
            .filter(|e| e.alive.alive_winner().value)
            .map(ElementState::materialize)
            .collect();
        page.elements
            .sort_by(|a, b| a.z().cmp(&b.z()).then(a.id().cmp(&b.id())));
        Some(page)
    }

    /// Every unresolved conflict, in a deterministic order.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut out = Vec::new();
        if let Some(s) = &self.notebook {
            reg_conflict(&mut out, ConflictTarget::NotebookTitle, &s.title, |v| {
                serde_json::json!(v)
            });
            reg_conflict(&mut out, ConflictTarget::NotebookCover, &s.cover, |v| {
                serde_json::json!(v)
            });
            alive_conflict(&mut out, ConflictTarget::NotebookTrash, &s.alive);
        }
        for (pid, p) in &self.pages {
            reg_conflict(
                &mut out,
                ConflictTarget::PageOrder { page_id: *pid },
                &p.key,
                |v| serde_json::json!(v),
            );
            reg_conflict(
                &mut out,
                ConflictTarget::PageTemplate { page_id: *pid },
                &p.template,
                |v| serde_json::to_value(v).expect("json"),
            );
            alive_conflict(
                &mut out,
                ConflictTarget::PageTrash { page_id: *pid },
                &p.alive,
            );
            for (eid, e) in &p.elements {
                for (g, r) in [
                    (groups::PLACEMENT, &e.placement),
                    (groups::CONTENT, &e.content),
                    (groups::STYLE, &e.style),
                ] {
                    reg_conflict(
                        &mut out,
                        ConflictTarget::Element {
                            page_id: *pid,
                            element_id: *eid,
                            group: g,
                        },
                        r,
                        |v| serde_json::to_value(&**v).expect("json"),
                    );
                }
                alive_conflict(
                    &mut out,
                    ConflictTarget::ElementTrash {
                        page_id: *pid,
                        element_id: *eid,
                    },
                    &e.alive,
                );
                let alive = e.alive.alive_winner();
                if !alive.value {
                    let deletes: Vec<&Write<bool>> = e.alive.heads.iter().collect();
                    let unseen: Vec<OperationId> = [&e.placement, &e.content, &e.style]
                        .iter()
                        .flat_map(|r| r.heads.iter())
                        .filter(|h| {
                            !deletes
                                .iter()
                                .any(|d| d.id == h.id || dominated(h.key, d.key, &d.frontier))
                        })
                        .map(|h| h.id)
                        .collect();
                    if let Some(latest) = unseen.iter().max() {
                        out.push(Conflict {
                            target: ConflictTarget::EditedWhileDeleted {
                                page_id: *pid,
                                element_id: *eid,
                            },
                            winner: alive.id,
                            alternates: vec![Alternate {
                                op_id: *latest,
                                value: serde_json::to_value(e.materialize()).expect("json"),
                            }],
                        });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.target.cmp(&b.target));
        out
    }
}

struct WriteCtx {
    id: OperationId,
    key: OpKey,
    frontier: Arc<Frontier>,
}

impl WriteCtx {
    fn of<T>(&self, value: T) -> Write<T> {
        Write {
            id: self.id,
            key: self.key,
            frontier: self.frontier.clone(),
            value,
        }
    }
}

fn reg_conflict<T: Clone>(
    out: &mut Vec<Conflict>,
    target: ConflictTarget,
    r: &Register<T>,
    json: impl Fn(&T) -> serde_json::Value,
) {
    if r.heads.len() < 2 {
        return;
    }
    let w = r.winner();
    out.push(Conflict {
        target,
        winner: w.id,
        alternates: r
            .heads
            .iter()
            .filter(|h| h.id != w.id)
            .map(|h| Alternate {
                op_id: h.id,
                value: json(&h.value),
            })
            .collect(),
    });
}

fn alive_conflict(out: &mut Vec<Conflict>, target: ConflictTarget, r: &Register<bool>) {
    let w = r.alive_winner();
    if r.heads.iter().all(|h| h.value == w.value) {
        return;
    }
    out.push(Conflict {
        target,
        winner: w.id,
        alternates: r
            .heads
            .iter()
            .filter(|h| h.value != w.value)
            .map(|h| Alternate {
                op_id: h.id,
                value: serde_json::json!(h.value),
            })
            .collect(),
    });
}
