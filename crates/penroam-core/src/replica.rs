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
//! * The reserved library stream ([`crate::folder::LIBRARY_STREAM_ID`]) holds
//!   folders instead of a notebook. Their names, parents and deletions are
//!   registers too, and so is the folder a notebook is filed in. Concurrent
//!   writes to those show the highest operation id and raise no conflict: a
//!   folder's name isn't worth interrupting anyone for.
//! * Whether a notebook shows infinite canvases is a register that raises no
//!   conflict either: it only changes how the pages are shown.
//! * A text block's text, in a shared notebook, is also a sequence CRDT
//!   ([`crate::textcrdt`]): every content write of the block starts one (its
//!   *genesis*, built deterministically from that write), `textEdited` updates
//!   edit the genesis they name, and the block shows the CRDT of its content
//!   winner. Concurrent typing in one block therefore keeps everyone's text. An
//!   edit to a genesis the winner didn't see (a whole-content write from an older
//!   client, say) is kept as a content conflict alternate, and an edit the
//!   winning delete didn't see makes `editedWhileDeleted`, as other edits do.
//! * The shared notebook space a notebook moved to (`notebookShared`) is a
//!   register too, without a conflict: the highest operation id wins, so every
//!   device routes the notebook to the same space even if two devices shared it
//!   at once.

use crate::folder::{Folder, FolderSet};
use crate::model::*;
use crate::operation::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// Adds a write: every head the writer had seen is superseded (dropped);
    /// heads it had not seen are concurrent and stay as alternates.
    fn write(&mut self, w: Write<T>) {
        self.heads.retain(|h| !dominated(h.key, w.key, &w.frontier));
        let at = self.heads.partition_point(|h| h.id < w.id);
        self.heads.insert(at, w);
    }
    /// The visible value: the concurrent head with the highest operation id
    /// (an arbitrary but identical choice on every replica).
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

/// One applied `textEdited` (kept for conflicts and for rebuilding a text at an earlier state).
#[derive(Debug, Clone)]
struct TextEdit {
    id: OperationId,
    key: OpKey,
    frontier: Arc<Frontier>,
    base: OperationId,
    update: Arc<Vec<u8>>,
}

/// A text block's CRDT started by one content write.
#[derive(Debug, Clone)]
struct Genesis {
    key: OpKey,
    frontier: Arc<Frontier>,
    text: Arc<String>,
    doc: crate::textcrdt::TextDoc,
}

#[derive(Debug, Clone)]
struct ElementState {
    alive: Register<bool>,
    placement: Register<Arc<Element>>,
    content: Register<Arc<Element>>,
    style: Register<Arc<Element>>,
    /// Text blocks: one CRDT per content write, by that write's id.
    geneses: BTreeMap<OperationId, Genesis>,
    /// Text blocks: every `textEdited`, in the order applied.
    text_edits: Vec<TextEdit>,
}

impl ElementState {
    /// The element as shown: content from the content winner, overlaid with
    /// the placement and style winners (field groups merge independently).
    fn materialize(&self) -> Element {
        let w = self.content.winner();
        let mut e = (*w.value).clone();
        if let (Element::Text(t), Some(g)) = (&mut e, self.geneses.get(&w.id)) {
            t.text = g.doc.text();
        }
        take_group(&mut e, &self.placement.winner().value, groups::PLACEMENT);
        take_group(&mut e, &self.style.winner().value, groups::STYLE);
        e
    }

    /// Starts a text CRDT for a content write of a text block.
    fn start_text(&mut self, w: &WriteCtx, e: &Element) {
        if let Element::Text(t) = e {
            self.geneses.entry(w.id).or_insert_with(|| Genesis {
                key: w.key,
                frontier: w.frontier.clone(),
                text: Arc::new(t.text.clone()),
                doc: crate::textcrdt::TextDoc::genesis(w.id, &t.text),
            });
        }
    }

    /// Whether some write on the genesis `base` (its content write or an edit of it) causally
    /// follows `h`.
    fn followed_on(&self, base: OperationId, h: OpKey) -> bool {
        self.geneses
            .get(&base)
            .is_some_and(|g| g.key == h || dominated(h, g.key, &g.frontier))
            || self
                .text_edits
                .iter()
                .filter(|e| e.base == base)
                .any(|e| e.key == h || dominated(h, e.key, &e.frontier))
    }

    /// The text of the genesis `base` with only the edits `visible` accepts, or `None` without
    /// that genesis.
    fn text_at(
        &self,
        base: OperationId,
        visible: &dyn Fn(DeviceId, u64) -> bool,
    ) -> Option<crate::textcrdt::TextDoc> {
        let g = self.geneses.get(&base)?;
        let doc = crate::textcrdt::TextDoc::genesis(base, &g.text);
        for e in self.text_edits.iter().filter(|e| e.base == base) {
            if visible(e.key.device, e.key.seq) {
                let _ = doc.apply(&e.update);
            }
        }
        Some(doc)
    }
}

#[derive(Debug, Clone)]
struct PageState {
    /// Create operation whose immutable fields (`created_at`, `paper`,
    /// `size`, `background`) are used: the highest id among the creates.
    creator: OperationId,
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
    /// Create operation whose `created_at`/`defaults` are used: the highest id
    /// among the creates.
    creator: OperationId,
    created_at: Timestamp,
    defaults: PageDefaults,
    title: Register<String>,
    cover: Register<String>,
    alive: Register<bool>,
    deleted_at: HashMap<OperationId, Timestamp>,
    /// The `alive` writes that were purges rather than moves to the trash.
    purges: HashSet<OperationId>,
    /// The folder it's filed in; `None` until it is first filed.
    folder: Option<Register<Option<Uuid>>>,
    /// Infinite canvases on or off; `None` until first set.
    canvas: Option<Register<bool>>,
    /// The shared notebook space it moved to; `None` until shared.
    shared: Option<Register<String>>,
}

#[derive(Debug, Clone)]
struct FolderState {
    /// Create operation whose `created_at` is used: the highest id among the
    /// creates.
    creator: OperationId,
    created_at: Timestamp,
    name: Register<String>,
    parent: Register<Option<Uuid>>,
    alive: Register<bool>,
    deleted_at: HashMap<OperationId, Timestamp>,
}

/// Where a conflict is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ConflictTarget {
    /// Concurrent renames.
    NotebookTitle,
    /// Concurrent cover changes.
    NotebookCover,
    /// Concurrent trash and restore of the notebook.
    NotebookTrash,
    /// Concurrent moves of one page.
    #[serde(rename_all = "camelCase")]
    PageOrder {
        /// The page.
        page_id: Uuid,
    },
    /// Concurrent template changes of one page.
    #[serde(rename_all = "camelCase")]
    PageTemplate {
        /// The page.
        page_id: Uuid,
    },
    /// Concurrent trash and restore of one page.
    #[serde(rename_all = "camelCase")]
    PageTrash {
        /// The page.
        page_id: Uuid,
    },
    /// Concurrent writes of one field group (see [`groups`]).
    #[serde(rename_all = "camelCase")]
    Element {
        /// The page.
        page_id: Uuid,
        /// The element.
        element_id: Uuid,
        /// The [`groups`] bit in conflict.
        group: u8,
    },
    /// Concurrent delete and restore of one element.
    #[serde(rename_all = "camelCase")]
    ElementTrash {
        /// The page.
        page_id: Uuid,
        /// The element.
        element_id: Uuid,
    },
    /// The element is deleted, but an edit the deleting device had not
    /// seen exists; the alternate holds the edited element.
    #[serde(rename_all = "camelCase")]
    EditedWhileDeleted {
        /// The page.
        page_id: Uuid,
        /// The element.
        element_id: Uuid,
    },
}

/// A conflict: the visible winner and every retained alternative.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    /// What is in conflict.
    pub target: ConflictTarget,
    /// Operation whose value is shown (highest id, or add-wins for trash state).
    pub winner: OperationId,
    /// The value that is shown (same JSON shapes as [`Alternate::value`]), so
    /// a resolution UI can present both sides without reading the page.
    pub winner_value: serde_json::Value,
    /// Every other concurrent value, kept until a later write supersedes them.
    pub alternates: Vec<Alternate>,
}

/// One retained value of a conflict.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alternate {
    /// The operation that wrote it.
    pub op_id: OperationId,
    /// The value (title string, template, element JSON, `true`/`false` for trash state).
    pub value: serde_json::Value,
}

/// Result of applying an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The operation was new and is now part of the replica.
    New,
    /// Same id and identical body seen before (retry): nothing changed.
    Duplicate,
}

/// Why an operation was not merged (the replica is unchanged).
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum MergeError {
    /// Invalid body, failed precondition or newer schema.
    #[error(transparent)]
    Op(#[from] OpError),
    /// The id was already applied with a different body (corruption or a bug).
    #[error("operation id {0} was already used for a different operation")]
    OpIdReuse(OperationId),
    /// Another operation already used this (device, sequence).
    #[error("device {device} sequence {seq} was already used by another operation")]
    SequenceReuse {
        /// The device.
        device: DeviceId,
        /// The reused sequence.
        seq: u64,
    },
    /// Not applicable yet: something it depends on is missing (keep it).
    #[error("waiting for {0:?}")]
    MissingDependency(OperationRef),
    /// The operation belongs to another notebook.
    #[error("operation is for notebook {0}, not this replica")]
    WrongNotebook(Uuid),
}

/// One notebook, merged from operations of any number of devices.
#[derive(Debug, Clone)]
pub struct Replica {
    notebook_id: Uuid,
    /// Highest applied sequence per device.
    frontier: Frontier,
    /// Applied ids → (device, sequence) and content fingerprint (dedup).
    applied: HashMap<OperationId, (OpKey, u32, usize)>,
    /// (device, sequence) → id, to reject sequence reuse.
    by_key: HashMap<OpKey, OperationId>,
    /// `None` until `notebookCreated` is applied.
    notebook: Option<NotebookState>,
    /// Referenced assets (a grow-only set; references are never removed).
    assets: BTreeMap<String, Asset>,
    /// Every page ever created (trashed pages stay, with `alive = false`).
    pages: BTreeMap<Uuid, PageState>,
    /// Every folder ever created; only the library stream has any.
    folders: BTreeMap<Uuid, FolderState>,
    /// Latest `at` of any applied operation (informational).
    updated_at: Timestamp,
}

/// Cheap identity of an operation's full content, kept per applied id so a
/// retry (same id, same content) is recognised without storing every body.
fn fingerprint(op: &LocalOperation) -> (u32, usize) {
    let bytes = serde_json::to_vec(op).expect("operation serialises");
    (crc32fast::hash(&bytes), bytes.len())
}

impl Replica {
    /// An empty replica; the notebook exists once its `notebookCreated` is applied.
    pub fn new(notebook_id: Uuid) -> Self {
        Self {
            notebook_id,
            frontier: Frontier::new(),
            applied: HashMap::new(),
            by_key: HashMap::new(),
            notebook: None,
            assets: BTreeMap::new(),
            pages: BTreeMap::new(),
            folders: BTreeMap::new(),
            updated_at: 0,
        }
    }

    /// Highest applied sequence per device (the frontier a new local operation
    /// would depend on). With [`Replica::apply`] it is contiguous; with
    /// [`Replica::apply_delivered`] a device's sequences may have gaps here
    /// (they belong to other notebooks).
    pub fn frontier(&self) -> Vec<OperationRef> {
        self.frontier
            .iter()
            .map(|(d, s)| OperationRef {
                device_id: *d,
                device_seq: *s,
            })
            .collect()
    }

    /// Whether an operation id has been applied.
    pub fn contains(&self, id: OperationId) -> bool {
        self.applied.contains_key(&id)
    }

    /// Applies one operation. On error nothing changed.
    ///
    /// Checks causal delivery itself: every operation of the writing device
    /// must arrive in sequence and its dependencies must already be applied
    /// *to this replica*. That only holds when one replica sees all of a
    /// device's operations; use [`Replica::apply_delivered`] when the caller
    /// delivers causally across several notebooks.
    pub fn apply(&mut self, op: &LocalOperation) -> Result<Applied, MergeError> {
        self.apply_inner(op, true)
    }

    /// Applies an operation the caller has already delivered causally across
    /// the whole account (all of its dependencies, in every notebook, were
    /// applied before it). Device sequences may then have gaps within one
    /// notebook. Concurrency is still decided from the operation's own
    /// frontier (`causal_dependencies`), which must be the full version
    /// vector its device had seen.
    pub fn apply_delivered(&mut self, op: &LocalOperation) -> Result<Applied, MergeError> {
        self.apply_inner(op, false)
    }

    fn apply_inner(
        &mut self,
        op: &LocalOperation,
        check_delivery: bool,
    ) -> Result<Applied, MergeError> {
        check_schema(op)?;
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
        if check_delivery && have + 1 != op.local_sequence {
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
            if check_delivery
                && self.frontier.get(&d.device_id).copied().unwrap_or(0) < d.device_seq
            {
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
        self.check(&op.body, op.device_id)?;
        self.mutate(&op.body, &w, op.at);
        let f = self.frontier.entry(op.device_id).or_insert(0);
        *f = (*f).max(op.local_sequence);
        self.applied.insert(op.id, (key, fp.0, fp.1));
        self.by_key.insert(key, op.id);
        // filing and sharing aren't changes to the notebook, as on the local path
        if !matches!(
            op.body,
            DocumentOperation::NotebookFiled { .. } | DocumentOperation::NotebookShared { .. }
        ) {
            self.updated_at = self.updated_at.max(op.at);
        }
        Ok(Applied::New)
    }

    fn page_state(&self, id: &Uuid) -> Result<&PageState, OpError> {
        self.pages.get(id).ok_or(OpError::NoPage(*id))
    }

    fn check(&self, body: &DocumentOperation, author: DeviceId) -> Result<(), OpError> {
        use DocumentOperation::*;
        let pre = |m: String| Err(OpError::Precondition(m));
        match body {
            // A second create of the same id (a re-joining device seeding its
            // copy, or one archive imported on two devices) is merged, not
            // rejected: rejecting would make the result depend on which create
            // a replica happened to apply first.
            NotebookCreated { .. } | FolderCreated { .. } => return Ok(()),
            FolderRenamed { folder_id, .. }
            | FolderMoved { folder_id, .. }
            | FolderDeleted { folder_id } => {
                if !self.folders.contains_key(folder_id) {
                    return pre(format!("folder {folder_id} does not exist"));
                }
                return Ok(());
            }
            _ if self.notebook.is_none() => return Err(OpError::NoNotebook(self.notebook_id)),
            PageCreated { background, .. } => {
                // a repeated create merges (see NotebookCreated above)
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
            TextEdited {
                page_id,
                element_id,
                base,
                update,
                ..
            } => {
                let p = self.page_state(page_id)?;
                let Some(e) = p.elements.get(element_id) else {
                    return pre(format!("element {element_id} does not exist"));
                };
                if !matches!(&*e.content.winner().value, Element::Text(_)) {
                    return pre(format!("element {element_id} is not a text block"));
                }
                if !e.geneses.contains_key(base) {
                    return pre(format!(
                        "text of element {element_id} has no genesis {base}"
                    ));
                }
                let bytes = crate::textcrdt::b64::decode(update)
                    .ok_or_else(|| OpError::Precondition("invalid text update".into()))?;
                // written under its author's own client id, complete, bounded and plain text
                // (checked on a copy, panics contained), before anything changes
                if let Err(err) = e.geneses[base]
                    .doc
                    .check_edit(&bytes, crate::textcrdt::edit_client(author))
                {
                    return pre(format!("text update refused: {err}"));
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
            | NotebookFiled { .. }
            | NotebookShared { .. }
            | NotebookCanvasSet { .. }
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
                if let Some(nb) = self.notebook.as_mut() {
                    // repeated create: its values are concurrent register writes
                    // (identical values produce no conflict); immutable fields
                    // come from the highest create id, whatever the order
                    nb.title.write(w.of(title.trim().to_string()));
                    nb.cover.write(w.of(cover.to_lowercase()));
                    nb.alive.write(w.of(true));
                    if w.id > nb.creator {
                        nb.creator = w.id;
                        nb.created_at = at;
                        nb.defaults = defaults.clone();
                    }
                    return;
                }
                self.notebook = Some(NotebookState {
                    creator: w.id,
                    created_at: at,
                    defaults: defaults.clone(),
                    title: Register::new(w.of(title.trim().to_string())),
                    cover: Register::new(w.of(cover.to_lowercase())),
                    alive: Register::new(w.of(true)),
                    deleted_at: HashMap::new(),
                    purges: HashSet::new(),
                    folder: None,
                    canvas: None,
                    shared: None,
                });
            }
            NotebookFiled { folder, .. } => {
                let nb = self.nb();
                match nb.folder.as_mut() {
                    Some(r) => r.write(w.of(*folder)),
                    None => nb.folder = Some(Register::new(w.of(*folder))),
                }
            }
            NotebookCanvasSet { infinite, .. } => {
                let nb = self.nb();
                match nb.canvas.as_mut() {
                    Some(r) => r.write(w.of(*infinite)),
                    None => nb.canvas = Some(Register::new(w.of(*infinite))),
                }
            }
            NotebookShared { space_id, .. } => {
                let nb = self.nb();
                match nb.shared.as_mut() {
                    Some(r) => r.write(w.of(space_id.clone())),
                    None => nb.shared = Some(Register::new(w.of(space_id.clone()))),
                }
            }
            FolderCreated {
                folder_id,
                name,
                parent,
            } => match self.folders.get_mut(folder_id) {
                // repeated create (a device seeding its copy again): merged,
                // like a repeated notebook create
                Some(f) => {
                    f.name.write(w.of(name.trim().to_string()));
                    f.parent.write(w.of(*parent));
                    f.alive.write(w.of(true));
                    if w.id > f.creator {
                        f.creator = w.id;
                        f.created_at = at;
                    }
                }
                None => {
                    self.folders.insert(
                        *folder_id,
                        FolderState {
                            creator: w.id,
                            created_at: at,
                            name: Register::new(w.of(name.trim().to_string())),
                            parent: Register::new(w.of(*parent)),
                            alive: Register::new(w.of(true)),
                            deleted_at: HashMap::new(),
                        },
                    );
                }
            },
            FolderRenamed { folder_id, name } => {
                if let Some(f) = self.folders.get_mut(folder_id) {
                    f.name.write(w.of(name.trim().to_string()));
                }
            }
            FolderMoved { folder_id, parent } => {
                if let Some(f) = self.folders.get_mut(folder_id) {
                    f.parent.write(w.of(*parent));
                }
            }
            FolderDeleted { folder_id } => {
                if let Some(f) = self.folders.get_mut(folder_id) {
                    f.alive.write(w.of(false));
                    f.deleted_at.insert(w.id, at);
                }
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
            NotebookPurged { .. } => {
                let nb = self.nb();
                nb.alive.write(w.of(false));
                nb.deleted_at.insert(w.id, at);
                nb.purges.insert(w.id);
            }
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
                if let Some(p) = self.pages.get_mut(page_id) {
                    // repeated create: merge like NotebookCreated
                    p.key.write(w.of(order_key.clone()));
                    p.template.write(w.of(template.clone()));
                    p.alive.write(w.of(true));
                    if w.id > p.creator {
                        p.creator = w.id;
                        p.created_at = at;
                        p.paper = *paper;
                        p.size = *size;
                        p.background = background.clone();
                    }
                    p.updated_at = p.updated_at.max(at);
                    return;
                }
                self.pages.insert(
                    *page_id,
                    PageState {
                        creator: w.id,
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
                            s.start_text(w, e);
                        }
                        None => {
                            let mut s = ElementState {
                                alive: Register::new(w.of(true)),
                                placement: Register::new(w.of(v.clone())),
                                content: Register::new(w.of(v.clone())),
                                style: Register::new(w.of(v)),
                                geneses: BTreeMap::new(),
                                text_edits: Vec::new(),
                            };
                            s.start_text(w, e);
                            p.elements.insert(e.id(), s);
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
            TextEdited {
                page_id,
                element_id,
                base,
                update,
                ..
            } => {
                let p = self.pg(page_id, at);
                p.ops += 1;
                let s = p.elements.get_mut(element_id).expect("checked");
                let bytes = crate::textcrdt::b64::decode(update).expect("validated");
                let g = s.geneses.get(base).expect("checked");
                if g.doc.apply(&bytes).is_err() {
                    // Checked, yet Yrs failed (a contained panic): its document may be
                    // half-changed. Rebuild it from the genesis and the edits applied before,
                    // and leave this one out, as every replica that rebuilds does.
                    let doc = crate::textcrdt::TextDoc::genesis(*base, &g.text);
                    for e in s.text_edits.iter().filter(|e| e.base == *base) {
                        let _ = doc.apply(&e.update);
                    }
                    s.geneses.get_mut(base).expect("checked").doc = doc;
                    return;
                }
                s.text_edits.push(TextEdit {
                    id: w.id,
                    key: w.key,
                    frontier: w.frontier.clone(),
                    base: *base,
                    update: Arc::new(bytes),
                });
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
                        s.start_text(w, e);
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
        nb.folder = s.folder.as_ref().and_then(|r| r.winner().value);
        nb.infinite_canvas = s.canvas.as_ref().is_some_and(|r| r.winner().value);
        Some(nb)
    }

    /// The merged folders, deleted ones included. Empty on any stream but the
    /// library's.
    pub fn folders(&self) -> FolderSet {
        FolderSet(
            self.folders
                .iter()
                .map(|(id, f)| {
                    let alive = f.alive.alive_winner();
                    let folder = Folder {
                        id: *id,
                        name: f.name.winner().value.clone(),
                        parent: f.parent.winner().value,
                        created_at: f.created_at,
                        deleted_at: (!alive.value)
                            .then(|| f.deleted_at.get(&alive.id).copied().unwrap_or(0)),
                    };
                    (*id, folder)
                })
                .collect(),
        )
    }

    /// The genesis a text block's shown text comes from, and that text. `None` for other
    /// elements and for blocks without a CRDT (none outside shared notebooks have one).
    pub fn text_of(&self, page: Uuid, element: Uuid) -> Option<(OperationId, String)> {
        let e = self.pages.get(&page)?.elements.get(&element)?;
        let w = e.content.winner();
        let g = e.geneses.get(&w.id)?;
        Some((w.id, g.doc.text()))
    }

    /// A stable position (Yrs sticky index bytes) at UTF-16 offset `offset` in a text block's
    /// shown text ([`crate::textcrdt::TextDoc::sticky`]); `after` for a range's start. `None` for
    /// other elements, blocks without a text CRDT, or an offset past the end.
    pub fn text_sticky(
        &self,
        page: Uuid,
        element: Uuid,
        offset: u32,
        after: bool,
    ) -> Option<Vec<u8>> {
        let e = self.pages.get(&page)?.elements.get(&element)?;
        e.geneses
            .get(&e.content.winner().id)?
            .doc
            .sticky(offset, after)
    }

    /// Where a [`Replica::text_sticky`] position is now in the block's shown text, as a UTF-16
    /// offset; `None` when the position belongs to text that is no longer the block's (another
    /// whole-content write replaced it) or the bytes are bad.
    pub fn text_offset(&self, page: Uuid, element: Uuid, sticky: &[u8]) -> Option<usize> {
        let e = self.pages.get(&page)?.elements.get(&element)?;
        e.geneses
            .get(&e.content.winner().id)?
            .doc
            .resolve_sticky(sticky)
            .map(|o| o as usize)
    }

    /// The `textEdited` update that turns a text block's text, **as of the state `visible`
    /// selects** (which edits count), into `new_text`, written under `client`
    /// ([`crate::textcrdt::edit_client`]). Returns the genesis it edits and the base64url update,
    /// or `None` when there is nothing to change or the element has no text CRDT.
    ///
    /// Use the state the person saw when they typed (the sync client passes "everything this
    /// device wrote, plus what it had installed"), so text that arrived later isn't deleted by a
    /// change computed without it; the update refers to items by id, so that text stays.
    pub fn text_edit(
        &self,
        page: Uuid,
        element: Uuid,
        client: u64,
        new_text: &str,
        visible: &dyn Fn(DeviceId, u64) -> bool,
    ) -> Option<(OperationId, String)> {
        let e = self.pages.get(&page)?.elements.get(&element)?;
        let base = e.content.winner().id;
        let doc = e.text_at(base, visible)?;
        let update = doc.edit_update(client, new_text)?;
        Some((base, crate::textcrdt::b64::encode(&update)))
    }

    /// The shared notebook space this notebook moved to (`notebookShared`), if any. When two
    /// devices shared it concurrently, the operation with the highest id decides, on every
    /// replica alike.
    pub fn shared_space(&self) -> Option<String> {
        let s = self.notebook.as_ref()?;
        s.shared.as_ref().map(|r| r.winner().value.clone())
    }

    /// Whether the notebook is permanently deleted: no device should keep a
    /// copy. The operations remain, so a restore undoes it, and restoring is
    /// add-wins like every other `alive` write: a restore concurrent with a
    /// purge keeps the notebook.
    pub fn is_purged(&self) -> bool {
        self.notebook.as_ref().is_some_and(|s| {
            let alive = s.alive.alive_winner();
            !alive.value && s.purges.contains(&alive.id)
        })
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
            reg_conflict(
                &mut out,
                ConflictTarget::NotebookTitle,
                &s.title,
                |v| serde_json::json!(v),
                |a, b| a == b,
            );
            reg_conflict(
                &mut out,
                ConflictTarget::NotebookCover,
                &s.cover,
                |v| serde_json::json!(v),
                |a, b| a == b,
            );
            alive_conflict(&mut out, ConflictTarget::NotebookTrash, &s.alive);
        }
        for (pid, p) in &self.pages {
            reg_conflict(
                &mut out,
                ConflictTarget::PageOrder { page_id: *pid },
                &p.key,
                |v| serde_json::json!(v),
                |a, b| a == b,
            );
            reg_conflict(
                &mut out,
                ConflictTarget::PageTemplate { page_id: *pid },
                &p.template,
                |v| serde_json::to_value(v).expect("json"),
                |a, b| a == b,
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
                        // e.g. two writes of the same position are not a
                        // position conflict even if their text differs
                        |a, b| changed_groups(a, b) & g == 0,
                    );
                }
                text_conflict(&mut out, *pid, *eid, e);
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
                    let seen = |id: OperationId, key: OpKey| {
                        deletes
                            .iter()
                            .any(|d| d.id == id || dominated(key, d.key, &d.frontier))
                    };
                    let unseen: Vec<OperationId> = [&e.placement, &e.content, &e.style]
                        .iter()
                        .flat_map(|r| r.heads.iter())
                        .filter(|h| !seen(h.id, h.key))
                        .map(|h| h.id)
                        // typing the deleting device hadn't seen is an edit too
                        .chain(
                            e.text_edits
                                .iter()
                                .filter(|t| !seen(t.id, t.key))
                                .map(|t| t.id),
                        )
                        .collect();
                    if let Some(latest) = unseen.iter().max() {
                        out.push(Conflict {
                            target: ConflictTarget::EditedWhileDeleted {
                                page_id: *pid,
                                element_id: *eid,
                            },
                            winner: alive.id,
                            // the element is shown as deleted
                            winner_value: serde_json::json!(false),
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

/// Reports a register's concurrent heads as a conflict. `same` decides
/// whether two values are equal *for this register*: element field-group
/// registers hold whole elements but only their group's fields matter.
fn reg_conflict<T: Clone>(
    out: &mut Vec<Conflict>,
    target: ConflictTarget,
    r: &Register<T>,
    json: impl Fn(&T) -> serde_json::Value,
    same: impl Fn(&T, &T) -> bool,
) {
    // concurrent writes of the same value are not a conflict (for example a
    // re-seeded copy of content every device already has)
    if r.heads.len() < 2 || r.heads.iter().all(|h| same(&h.value, &r.winner().value)) {
        return;
    }
    let w = r.winner();
    out.push(Conflict {
        target,
        winner: w.id,
        winner_value: json(&w.value),
        alternates: r
            .heads
            .iter()
            .filter(|h| h.id != w.id && !same(&h.value, &w.value))
            .map(|h| Alternate {
                op_id: h.id,
                value: json(&h.value),
            })
            .collect(),
    });
}

/// For a text block: shows the content conflict's values with their CRDT text, and adds the
/// texts of geneses that were edited without the winner seeing those edits (an older client's
/// whole-content write concurrent with typing) as alternates, so that text stays recoverable.
fn text_conflict(out: &mut Vec<Conflict>, page_id: Uuid, element_id: Uuid, e: &ElementState) {
    if e.geneses.is_empty() || !matches!(&*e.content.winner().value, Element::Text(_)) {
        return;
    }
    let winner = e.content.winner();
    let with_text = |id: OperationId, v: &Element| -> serde_json::Value {
        let mut v = v.clone();
        if let (Element::Text(t), Some(g)) = (&mut v, e.geneses.get(&id)) {
            t.text = g.doc.text();
        }
        serde_json::to_value(v).expect("json")
    };
    let target = ConflictTarget::Element {
        page_id,
        element_id,
        group: groups::CONTENT,
    };
    // the shown value and register alternates carry the text as edited, not as first written
    if let Some(c) = out.iter_mut().find(|c| c.target == target) {
        c.winner_value = with_text(winner.id, &winner.value);
        for a in &mut c.alternates {
            if let Some(h) = e.content.heads.iter().find(|h| h.id == a.op_id) {
                a.value = with_text(h.id, &h.value);
            }
        }
    }
    let mut extra: Vec<Alternate> = Vec::new();
    for (base, g) in &e.geneses {
        if *base == winner.id {
            continue;
        }
        let missed = e
            .text_edits
            .iter()
            .filter(|t| t.base == *base)
            .any(|t| !e.followed_on(winner.id, t.key));
        if missed
            && g.doc.text()
                != e.geneses
                    .get(&winner.id)
                    .map(|w| w.doc.text())
                    .unwrap_or_default()
        {
            extra.push(Alternate {
                op_id: *base,
                value: with_text(*base, &winner.value),
            });
        }
    }
    if extra.is_empty() {
        return;
    }
    match out.iter_mut().find(|c| c.target == target) {
        Some(c) => {
            for a in extra {
                if !c.alternates.iter().any(|x| x.op_id == a.op_id) {
                    c.alternates.push(a);
                }
            }
        }
        None => out.push(Conflict {
            target,
            winner: winner.id,
            winner_value: with_text(winner.id, &winner.value),
            alternates: extra,
        }),
    }
}

fn alive_conflict(out: &mut Vec<Conflict>, target: ConflictTarget, r: &Register<bool>) {
    let w = r.alive_winner();
    if r.heads.iter().all(|h| h.value == w.value) {
        return;
    }
    out.push(Conflict {
        target,
        winner: w.id,
        winner_value: serde_json::json!(w.value),
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
