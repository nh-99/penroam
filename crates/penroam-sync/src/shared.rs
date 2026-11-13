//! Shared notebooks: one notebook synced in a **sharing space** that several accounts belong to.
//!
//! A space is a hidden account like a vault (protocol §12f, capability `sharing.v1`), holding
//! exactly one notebook, with its own operation log, key log, blobs and checkpoints. Each device
//! runs one more [`SyncEngine`] per space it takes part in ([`Scope::Shared`]), with its own sync
//! folder and cursor, so two devices of one person and two people never mix account-level state.
//! The server keeps the space's root secret (like an ordinary account's), which members fetch with
//! their own session; shared notebooks are therefore **not** private from the server, and a vault
//! notebook can't be shared ([`share`] refuses).
//!
//! ## Where a notebook syncs
//!
//! [`NotebookOwners::shared`] maps a shared notebook to its space, this account's [`Role`] there
//! and how far this device is with it ([`SharedState`]). It is written under the library lock and
//! engines reread it under that lock, so a shared notebook is only ever seen by its space's engine:
//! the account engine neither ingests nor installs it. A space's engine installs only the one
//! notebook it is assigned, and only once joining finished.
//!
//! ## Sharing a notebook that already exists ([`share`])
//!
//! The notebook keeps its id and its history:
//!
//! 1. Create the space (`POST /v1/shared-notebooks`, idempotent by a key kept in a progress file),
//!    with its root generated and kept in the secret store *before* the request, so a retry seals
//!    the same title under the same root. Store the root on the server (`PUT …/key`).
//! 2. Start the space's engine (first device of the space's key log).
//! 3. **History transplant:** every operation of the notebook in the account's operation log is
//!    re-published into the space, in log order (a causal order), as an operation of this device,
//!    with the same body and `at` and the id [`republished_id`]`(space, original id)`. Fixed ids
//!    make every step safe to repeat after a crash. Filing operations stay behind: where a notebook
//!    is filed is personal. Assets are uploaded before the operations that reference them, as for
//!    any upload.
//! 4. Under the library lock: install whatever the account engine delivered, transplant the rest,
//!    add a **corrective diff** so the space's merged notebook equals the library's exactly, then
//!    write `notebookShared { space_id }` into the *account* log (fixed id) and assign the notebook
//!    to the space. Every other device of the account that applies that marker assigns the
//!    notebook too, before installing anything of it, and joins the space. A client too old to know
//!    the marker parks it as "needs an update" and loses nothing.
//!
//! Each step persists its result before the next one starts, so [`share`] can be called again
//! after any failure and picks up where it stopped.
//!
//! ## Operations concurrent with the marker
//!
//! The marker depends on everything the sharing device had installed, and the transplant covers
//! exactly that. An account-log operation on the notebook that is *not* in the marker's causal
//! past was made on another device that hadn't seen the marker yet. [`republish_concurrent`]
//! re-publishes such operations into the space, and only on the device that wrote them (it holds
//! them as local operations), so no two devices ever publish the same id. A device that joins
//! because of the marker does this before it installs anything from the space, so its unsent
//! edits are part of what it installs.
//!
//! ## Joining ([`join`], [`join_step`])
//!
//! Fetch the space's root, open the sealed title to learn the notebook id, assign the notebook
//! (state `joining`), add this device to the space's key log (which must verify against the root),
//! pull the whole log (or a checkpoint and its tail), and download every referenced asset. Only
//! then is the entry marked `active` and the notebook installed.
//!
//! ## Roles and access loss
//!
//! A viewer's engine never uploads. Losing access (`access_revoked`, or the index lists the
//! membership as revoked) stops the space's engine for good: [`mark_access_removed`] first copies
//! the remaining journal entries into the space's log, so unsent edits are kept, then marks the
//! entry `accessRemoved`. Nothing is deleted: [`keep_private_copy`] makes a personal copy of the
//! notebook as it is on this device (unsent edits included) and [`remove_local`] removes this
//! device's copy, both only when asked.
//!
//! See `docs/protocol/sync-protocol-v1.md` §12f and `docs/security/threat-model.md`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use penroam_core::operation::{changed_groups, validate_body, DocumentOperation};
use penroam_core::{Element, Notebook};
use penroam_storage::fsutil::write_atomic;
use penroam_storage::Library;
use penroam_sync_protocol::{
    base64url, AccountKeyDocument, CreateSpaceRequest, ErrorCode, Role, SharedNotebookRecord,
    SpaceId, UpdateSpaceMetaRequest,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::{self, RootSecret};
use crate::engine::{ReconcileReport, Result, Setup, SyncEngine, SyncError};
use crate::secrets::SecretStore;
use crate::store::{Origin, SyncStore};
use crate::transport::{SyncTransport, TransportError};
use crate::vault::{NotebookOwners, Scope};

/// Associated data of a sealed space title.
const META_AAD: &[u8] = b"penroam/space-meta/v1";
/// Base name of the secret holding a space's root on a member device
/// ([`Scope::secret_name`] adds the space's prefix).
pub const SPACE_ROOT_SECRET: &str = "space-root";

/// How far this device is with a shared notebook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SharedState {
    /// Assigned to its space, which is still being downloaded and checked. The notebook isn't
    /// shown (or isn't updated) until that finishes.
    Joining,
    /// Synced in its space.
    Active,
    /// This account's access was removed. The space's engine stopped; the notebook and its
    /// unsent changes stay on this device, read-only.
    AccessRemoved,
}

/// Where one shared notebook syncs (an entry of [`NotebookOwners::shared`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedEntry {
    /// The space's id.
    pub space_id: String,
    /// This account's role in it, as the server last said.
    pub role: Role,
    /// How far this device is with it.
    pub state: SharedState,
    /// Library journal sequence at which the notebook was assigned to the space: local edits
    /// after it belong to the space's engine.
    #[serde(default)]
    pub since_lib_seq: u64,
    /// The title last sealed into the space's metadata (so a rename can refresh it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The owner's verified email, from the invitation (display only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_display: Option<String>,
    /// Another device of this account shared the same notebook at the same time, and its space
    /// won: this is the space this device had made, whose edits still have to move to the
    /// winner before it is removed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
}

impl SharedEntry {
    /// A new entry.
    pub fn new(space_id: &str, role: Role, state: SharedState, since_lib_seq: u64) -> Self {
        Self {
            space_id: space_id.to_string(),
            role,
            state,
            since_lib_seq,
            title: None,
            owner_display: None,
            replaces: None,
        }
    }

    /// Whether this device may change the notebook: the role writes and access wasn't removed.
    pub fn writable(&self) -> bool {
        self.role.may_write() && self.state != SharedState::AccessRemoved
    }
}

/// The plaintext of a space's `sealed_meta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceMeta {
    /// The shared notebook's id.
    pub notebook_id: Uuid,
    /// Its title.
    pub title: String,
}

/// Seals a space's notebook id and title for the server (which can't read it without the root).
pub fn seal_meta(root: &RootSecret, meta: &SpaceMeta) -> String {
    let plain = serde_json::to_vec(meta).expect("meta serializes");
    base64url::encode(&crypto::seal(
        &root.space_meta_key(),
        b"space-meta",
        META_AAD,
        &plain,
    ))
}

/// Opens [`seal_meta`] output. Fails for the wrong root or an altered value.
pub fn open_meta(root: &RootSecret, sealed: &str) -> Result<SpaceMeta> {
    let bad = || SyncError::Integrity("the shared notebook's sealed title doesn't open".into());
    let bytes = base64url::decode(sealed).map_err(|_| bad())?;
    let plain = crypto::open(&root.space_meta_key(), META_AAD, &bytes).map_err(|_| bad())?;
    serde_json::from_slice(&plain).map_err(|_| bad())
}

/// A name-based id (SHA-256, RFC 9562 version 8 layout) from labelled parts.
fn name_id(label: &[u8], parts: &[&[u8]]) -> Uuid {
    let mut buf = Vec::with_capacity(64);
    buf.extend_from_slice(label);
    for p in parts {
        buf.extend_from_slice(&(p.len() as u64).to_le_bytes());
        buf.extend_from_slice(p);
    }
    let h = crypto::sha256(&buf);
    let mut b: [u8; 16] = h[..16].try_into().expect("16 bytes");
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    Uuid::from_bytes(b)
}

/// The id an operation gets when it is re-published into space `space_id`. Deterministic, so a
/// repeated transplant appends nothing twice, and distinct per space.
pub fn republished_id(space_id: &str, op: Uuid) -> Uuid {
    name_id(
        b"penroam/space-op/v1",
        &[space_id.as_bytes(), op.as_bytes()],
    )
}

/// The id of the `notebookShared` marker for a notebook and space.
pub fn marker_id(space_id: &str, notebook: Uuid) -> Uuid {
    name_id(
        b"penroam/space-marker/v1",
        &[space_id.as_bytes(), notebook.as_bytes()],
    )
}

/// Bodies that never move into a space: filing is personal, and markers belong to the account.
fn stays_behind(body: &DocumentOperation) -> bool {
    matches!(
        body,
        DocumentOperation::NotebookFiled { .. } | DocumentOperation::NotebookShared { .. }
    )
}

/// Why joining a space again waits: the copy from the earlier membership, with its unsent
/// edits, is still on this device. It is never merged into the new membership; the person keeps a
/// private copy of it or removes it first.
fn stale_copy() -> SyncError {
    SyncError::Invalid(
        "this device still has your earlier copy of this notebook, from when your access was \
         removed; keep a private copy of it or remove it from this device, then accept again"
            .into(),
    )
}

fn space_id(id: &str) -> Result<SpaceId> {
    id.parse()
        .map_err(|_| SyncError::Invalid("the server sent an invalid shared notebook id".into()))
}

/// Keeps a space's root in the secret store (members need it to read and seal its title).
pub fn keep_root(secrets: &dyn SecretStore, space: &str, root: &RootSecret) -> Result<()> {
    let scope = Scope::shared(space)?;
    secrets.set(
        &scope.secret_name(SPACE_ROOT_SECRET),
        &base64url::encode(&root.to_bytes()),
    )?;
    Ok(())
}

/// A space's root from the secret store.
pub fn load_root(secrets: &dyn SecretStore, space: &str) -> Result<Option<RootSecret>> {
    let scope = Scope::shared(space)?;
    match secrets.get(&scope.secret_name(SPACE_ROOT_SECRET))? {
        Some(s) => Ok(Some(RootSecret::from_bytes(crypto::decode32(&s)?))),
        None => Ok(None),
    }
}

/// Runs `f` on the notebook owners under the library lock and saves them.
fn edit_owners<R>(
    lib: &Mutex<Library>,
    f: impl FnOnce(&mut NotebookOwners, &Library) -> Result<R>,
) -> Result<R> {
    let lib = lib.lock().unwrap_or_else(|p| p.into_inner());
    let root = lib.root().to_path_buf();
    let mut owners = NotebookOwners::load(&root)?;
    let r = f(&mut owners, &lib)?;
    owners.save(&root)?;
    Ok(r)
}

// ------------------------------------------------------------ transplant --

/// Re-publishes the notebook's account history into the space (step 3 of [`share`]). Operations
/// already there are skipped, as are bodies that stay behind or that no longer validate (the
/// corrective diff covers what they did). `hook` runs after each operation (tests use it to
/// simulate a crash). Returns how many were appended.
pub fn transplant<T: SyncTransport>(
    account: &SyncEngine<T>,
    space: &mut SyncEngine<T>,
    space_id: &str,
    nb: Uuid,
    hook: &mut dyn FnMut(ShareStep) -> Result<()>,
) -> Result<usize> {
    let mut n = 0;
    for (i, (_, op)) in account.logged_ops(nb)?.into_iter().enumerate() {
        if stays_behind(&op.body) || validate_body(&op.body).is_err() {
            continue;
        }
        if space.append_fixed(republished_id(space_id, op.id), op.at, op.body)? {
            n += 1;
            if n % 256 == 0 {
                space.save_state()?;
            }
        }
        hook(ShareStep::Transplanted(i))?;
    }
    space.save_state()?;
    Ok(n)
}

/// Appends operations to the space so that its merged notebook equals the library's notebook
/// `nb`: title, cover, canvas, trash state, assets, every page (order key, template, trash state,
/// elements). Creates the notebook when the space has none yet. Returns how many operations it
/// appended. Call with the library lock held.
pub fn correct_to<T: SyncTransport>(
    space: &mut SyncEngine<T>,
    lib: &mut Library,
    nb: Uuid,
) -> Result<usize> {
    let local = lib.notebook(nb)?.clone();
    let now = penroam_core::now_ms();
    let mut ops: Vec<(i64, DocumentOperation)> = Vec::new();
    let (merged, merged_pages) = match space.merged(nb)? {
        Some((n, p)) => (Some(n), p),
        None => (None, Vec::new()),
    };
    if merged.is_none() {
        ops.push((
            local.created_at,
            DocumentOperation::NotebookCreated {
                notebook_id: nb,
                title: local.title.clone(),
                cover: local.cover.clone(),
                defaults: local.defaults.clone(),
            },
        ));
    } else if let Some(m) = &merged {
        if m.title != local.title {
            ops.push((
                now,
                DocumentOperation::NotebookRenamed {
                    notebook_id: nb,
                    title: local.title.clone(),
                },
            ));
        }
        if m.cover != local.cover {
            ops.push((
                now,
                DocumentOperation::NotebookCoverChanged {
                    notebook_id: nb,
                    cover: local.cover.clone(),
                },
            ));
        }
    }
    let m_canvas = merged.as_ref().is_some_and(|m| m.infinite_canvas);
    if m_canvas != local.infinite_canvas {
        ops.push((
            now,
            DocumentOperation::NotebookCanvasSet {
                notebook_id: nb,
                infinite: local.infinite_canvas,
            },
        ));
    }
    for a in &local.assets {
        if !merged
            .as_ref()
            .is_some_and(|m| m.assets.iter().any(|x| x.sha256 == a.sha256))
        {
            ops.push((
                now,
                DocumentOperation::AssetReferenced {
                    notebook_id: nb,
                    asset: a.clone(),
                },
            ));
        }
    }
    let m_trashed = |id: Uuid| {
        merged
            .as_ref()
            .is_some_and(|m| m.trashed_pages.iter().any(|t| t.id == id))
    };
    let local_ids: Vec<Uuid> = local.all_page_ids().collect();
    for pid in &local_ids {
        let page = lib.load_page(nb, *pid)?;
        let key = local
            .key_of(*pid)
            .map(str::to_string)
            .unwrap_or_else(|| penroam_core::order_key::n_between(None, None, 1).remove(0));
        let local_trashed = local.trashed_pages.iter().any(|t| t.id == *pid);
        match merged_pages.iter().find(|p| p.id == *pid) {
            None => {
                ops.push((
                    page.created_at,
                    DocumentOperation::PageCreated {
                        notebook_id: nb,
                        page_id: *pid,
                        order_key: key,
                        paper: page.paper,
                        size: page.size,
                        template: page.template.clone(),
                        background: page.background.clone(),
                    },
                ));
                if !page.elements.is_empty() {
                    ops.push((
                        page.updated_at,
                        DocumentOperation::ElementsAdded {
                            notebook_id: nb,
                            page_id: *pid,
                            elements: page.elements.clone(),
                        },
                    ));
                }
                if local_trashed {
                    ops.push((
                        now,
                        DocumentOperation::PageDeleted {
                            notebook_id: nb,
                            page_id: *pid,
                        },
                    ));
                }
            }
            Some(mp) => {
                let m_key = merged.as_ref().and_then(|m| m.key_of(*pid)).unwrap_or("");
                if m_key != key {
                    ops.push((
                        now,
                        DocumentOperation::PageReordered {
                            notebook_id: nb,
                            page_id: *pid,
                            order_key: key,
                        },
                    ));
                }
                if mp.template != page.template {
                    ops.push((
                        now,
                        DocumentOperation::PageTemplateSet {
                            notebook_id: nb,
                            page_id: *pid,
                            template: page.template.clone(),
                        },
                    ));
                }
                ops.extend(
                    element_diff(nb, *pid, &mp.elements, &page.elements)
                        .into_iter()
                        .map(|b| (now, b)),
                );
                match (m_trashed(*pid), local_trashed) {
                    (false, true) => ops.push((
                        now,
                        DocumentOperation::PageDeleted {
                            notebook_id: nb,
                            page_id: *pid,
                        },
                    )),
                    (true, false) => ops.push((
                        now,
                        DocumentOperation::PageRestored {
                            notebook_id: nb,
                            page_id: *pid,
                        },
                    )),
                    _ => {}
                }
            }
        }
    }
    // pages the space has and this library doesn't (any more): to the trash
    if let Some(m) = &merged {
        for pid in &m.page_order {
            if !local_ids.contains(pid) {
                ops.push((
                    now,
                    DocumentOperation::PageDeleted {
                        notebook_id: nb,
                        page_id: *pid,
                    },
                ));
            }
        }
    }
    let m_deleted = merged.as_ref().is_some_and(|m| m.deleted_at.is_some());
    match (m_deleted, local.deleted_at) {
        (false, Some(at)) => ops.push((at, DocumentOperation::NotebookDeleted { notebook_id: nb })),
        (true, None) => ops.push((now, DocumentOperation::NotebookRestored { notebook_id: nb })),
        _ => {}
    }
    let n = ops.len();
    for (at, body) in ops {
        space.append_fixed(Uuid::new_v4(), at, body)?;
    }
    space.save_state()?;
    Ok(n)
}

/// The operations that turn one page's elements into another's.
fn element_diff(nb: Uuid, page: Uuid, from: &[Element], to: &[Element]) -> Vec<DocumentOperation> {
    let mut out = Vec::new();
    let removed: Vec<Uuid> = from
        .iter()
        .filter(|e| !to.iter().any(|t| t.id() == e.id()))
        .map(Element::id)
        .collect();
    if !removed.is_empty() {
        out.push(DocumentOperation::ElementsRemoved {
            notebook_id: nb,
            page_id: page,
            ids: removed,
        });
    }
    let added: Vec<Element> = to
        .iter()
        .filter(|e| !from.iter().any(|f| f.id() == e.id()))
        .cloned()
        .collect();
    if !added.is_empty() {
        out.push(DocumentOperation::ElementsAdded {
            notebook_id: nb,
            page_id: page,
            elements: added,
        });
    }
    let mut changed = Vec::new();
    let mut masks = Vec::new();
    for e in to {
        if let Some(old) = from.iter().find(|f| f.id() == e.id()) {
            if old != e {
                masks.push(changed_groups(old, e));
                changed.push(e.clone());
            }
        }
    }
    if !changed.is_empty() {
        out.push(DocumentOperation::ElementsReplaced {
            notebook_id: nb,
            page_id: page,
            elements: changed,
            changed: masks,
        });
    }
    out
}

/// Re-publishes into the space the notebook's operations that this device wrote to the account
/// log without having seen the `notebookShared` marker (see the module docs). Safe to call
/// repeatedly; does nothing before the marker is delivered. Returns how many it appended.
pub fn republish_concurrent<T: SyncTransport>(
    account: &SyncEngine<T>,
    space: &mut SyncEngine<T>,
    nb: Uuid,
) -> Result<usize> {
    let Some(space_id) = space.scope().id().map(str::to_string) else {
        return Ok(0);
    };
    let ops = account.logged_ops(nb)?;
    let Some(marker) = ops
        .iter()
        .map(|(_, o)| o)
        .filter(|o| {
            matches!(&o.body, DocumentOperation::NotebookShared { space_id: s, .. } if *s == space_id)
        })
        .max_by_key(|o| o.id)
        .cloned()
    else {
        return Ok(0);
    };
    let Ok(me) = Uuid::parse_str(account.device_id()) else {
        return Ok(0);
    };
    let in_past = |dev: Uuid, seq: u64| {
        (dev == marker.device_id && seq < marker.local_sequence)
            || marker
                .causal_dependencies
                .iter()
                .any(|d| d.device_id == dev && d.device_seq >= seq)
    };
    let mut n = 0;
    for (meta, op) in ops {
        if meta.origin != Origin::Local
            || op.device_id != me
            || op.id == marker.id
            || stays_behind(&op.body)
            || in_past(op.device_id, op.local_sequence)
            || validate_body(&op.body).is_err()
        {
            continue;
        }
        if space.append_fixed(republished_id(&space_id, op.id), op.at, op.body)? {
            n += 1;
        }
    }
    if n > 0 {
        space.save_state()?;
    }
    Ok(n)
}

/// Moves this device's edits from a space that lost a concurrent share (`old`) into the winning
/// space: every local operation of `old` that isn't a transplanted copy of an account operation
/// (those reach the winner through its own transplant, or [`republish_concurrent`]). Ids are
/// [`republished_id`] in the winner, so repeating appends nothing twice. Returns how many it
/// appended.
pub fn republish_orphan<T: SyncTransport>(
    account: &SyncEngine<T>,
    old: &SyncEngine<T>,
    winner: &mut SyncEngine<T>,
    nb: Uuid,
) -> Result<usize> {
    let (Some(old_id), Some(win_id)) = (
        old.scope().id().map(str::to_string),
        winner.scope().id().map(str::to_string),
    ) else {
        return Ok(0);
    };
    let copies: std::collections::HashSet<Uuid> = account
        .logged_ops(nb)?
        .iter()
        .map(|(_, op)| republished_id(&old_id, op.id))
        .collect();
    let Ok(me) = Uuid::parse_str(old.device_id()) else {
        return Ok(0);
    };
    let mut n = 0;
    for (meta, op) in old.logged_ops(nb)? {
        if meta.origin != Origin::Local
            || op.device_id != me
            || copies.contains(&op.id)
            || stays_behind(&op.body)
            || validate_body(&op.body).is_err()
        {
            continue;
        }
        if winner.append_fixed(republished_id(&win_id, op.id), op.at, op.body)? {
            n += 1;
        }
    }
    if n > 0 {
        winner.save_state()?;
    }
    Ok(n)
}

// ---------------------------------------------------------------- sharing --

/// Points in [`share`] where a test can make it stop as if the app had crashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareStep {
    /// The space exists on the server.
    SpaceCreated,
    /// Its root is stored on the server.
    KeyStored,
    /// Its engine is set up on this device.
    EngineReady,
    /// Operation number `n` of the notebook's history was transplanted.
    Transplanted(usize),
    /// The marker is in the account log and the notebook is assigned.
    Marked,
}

/// What [`share`] remembers between attempts (`<sync>/shares/<notebook>.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareProgress {
    idempotency_key: String,
    #[serde(default)]
    space_id: Option<String>,
    #[serde(default)]
    key_stored: bool,
}

fn progress_path(library_root: &Path, nb: Uuid) -> PathBuf {
    SyncStore::dir_for(library_root)
        .join("shares")
        .join(format!("{nb}.json"))
}

fn load_progress(path: &Path) -> Result<Option<ShareProgress>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).map_err(|e| {
            SyncError::Io(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
        })?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save_progress(path: &Path, p: &ShareProgress) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    write_atomic(
        path,
        &serde_json::to_vec_pretty(p).map_err(io::Error::other)?,
    )
    .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}

/// Whether a share of `nb` was started on this device and not finished.
pub fn share_in_progress(library_root: &Path, nb: Uuid) -> bool {
    progress_path(library_root, nb).exists()
}

/// Makes a space transport with an account session on the space ([`SpaceAccess::connect`]).
pub type ConnectFn<'a, T> = dyn FnMut(&mut SyncEngine<T>, &SpaceId) -> Result<T> + 'a;
/// Gives a space engine a device session ([`SpaceAccess::upgrade`]).
pub type UpgradeFn<'a, T> = dyn FnMut(&mut SyncEngine<T>, &mut SyncEngine<T>) -> Result<()> + 'a;

/// What [`share`], [`join`] and [`join_step`] need from the app.
pub struct SpaceAccess<'a, T: SyncTransport> {
    /// A transport for the space, from a session the account engine's transport obtains
    /// (`POST /v1/shared-notebooks/{id}/session`).
    pub connect: &'a mut ConnectFn<'a, T>,
    /// A secret store for a new engine (the same store the account engine uses).
    pub secrets: &'a dyn Fn() -> Result<Box<dyn SecretStore>>,
    /// Gives a space engine (second argument) whose device is registered a **device session**
    /// on the space, replacing the account session `connect` gave it: the sync routes need one
    /// (protocol §12f). Called after every open, create or enroll and after a session expired.
    /// The account engine (first argument) can obtain the account session that is traded in.
    pub upgrade: &'a mut UpgradeFn<'a, T>,
}

impl<T: SyncTransport> SpaceAccess<'_, T> {
    /// A space engine opened from its sync folder, with a device session.
    fn open_existing(
        &mut self,
        account: &mut SyncEngine<T>,
        lib_root: &Path,
        space: &str,
    ) -> Result<SyncEngine<T>> {
        let t = (self.connect)(account, &space_id(space)?)?;
        let mut e = SyncEngine::open_scope(lib_root, Scope::shared(space)?, t, (self.secrets)()?)?;
        (self.upgrade)(account, &mut e)?;
        Ok(e)
    }

    /// A fresh session for a space engine whose session expired: account session, then device
    /// session.
    fn reconnect(&mut self, account: &mut SyncEngine<T>, space: &mut SyncEngine<T>) -> Result<()> {
        let id = space_id(space.account_id())?;
        *space.transport_mut() = (self.connect)(account, &id)?;
        (self.upgrade)(account, space)
    }
}

/// Whether device id `device` already appears in a space's key log (verified against `root`),
/// active or removed. A member removed from a space and admitted again needs a new device id
/// there: the old one stays revoked for good.
fn device_in_space_log<T: SyncTransport>(
    t: &mut T,
    root: &RootSecret,
    device: &str,
) -> Result<bool> {
    let mut raw: Vec<Vec<u8>> = Vec::new();
    loop {
        let page = t.keylog_get(raw.len() as u64)?;
        if page.entries.is_empty() {
            break;
        }
        for e in page.entries {
            raw.push(
                base64url::decode(&e.data)
                    .map_err(|_| SyncError::Integrity("key log entry encoding".into()))?,
            );
        }
        if raw.len() as u64 >= page.next_index {
            break;
        }
    }
    if raw.is_empty() {
        return Ok(false);
    }
    let acct = root.account_signing_key().verifying_key().to_bytes();
    let m = crate::keylog::verify_log_with(
        &raw,
        Some(&acct),
        None,
        crate::keylog::GrantRule::Recovery,
    )?;
    Ok(m.devices.contains_key(device))
}

fn pending_root_name(nb: Uuid) -> String {
    format!("share-{nb}-{SPACE_ROOT_SECRET}")
}

/// Opens the space's engine when this device has it, otherwise adds this device to it (or
/// starts its key log when nobody has yet, which only the owner does).
fn open_or_enroll<T: SyncTransport>(
    account: &mut SyncEngine<T>,
    lib_root: &Path,
    space: &str,
    root: &RootSecret,
    access: &mut SpaceAccess<'_, T>,
    may_create: bool,
) -> Result<SyncEngine<T>> {
    let scope = Scope::shared(space)?;
    if scope.dir(lib_root).join("state.json").exists() {
        return access.open_existing(account, lib_root, space);
    }
    let id = space_id(space)?;
    let mut t = (access.connect)(account, &id)?;
    // The device's id in the main account, as vaults do, so that removing the device there can
    // reach the space too. Not when that id was in this space before (this account was removed
    // and admitted again): then a new one.
    let main_id = account.device_id().to_string();
    let device_id = if device_in_space_log(&mut t, root, &main_id)? {
        None
    } else {
        Some(main_id.clone())
    };
    let setup = Setup {
        library_root: lib_root.to_path_buf(),
        scope,
        device_id,
        server_url: account.store.state.server_url.clone(),
        account_id: space.to_string(),
        label: account.store.state.label.clone(),
    };
    let fresh = t.keylog_get(0)?.next_index == 0;
    if fresh && !may_create {
        return Err(SyncError::AccountHasNoKeys);
    }
    let start = |setup: Setup, t: T, access: &mut SpaceAccess<'_, T>| -> Result<SyncEngine<T>> {
        if fresh {
            SyncEngine::create_with_root(setup, t, (access.secrets)()?, root)
        } else {
            SyncEngine::enroll_with_root(setup, t, (access.secrets)()?, root)
        }
    };
    let mut engine = match start(setup.clone(), t, access) {
        // Someone registered this device's main id in the space first, or the server refuses it:
        // a random one instead. Only when registering failed: a key-log append that lost a race
        // after this device registered is an ordinary retry later, under the same id.
        Err(SyncError::Transport(e))
            if setup.device_id.is_some()
                && matches!(e.code(), Some(ErrorCode::Conflict | ErrorCode::Forbidden))
                && !registered_as_us(account, access, &id, space, &main_id)? =>
        {
            let t = (access.connect)(account, &id)?;
            start(
                Setup {
                    device_id: None,
                    ..setup
                },
                t,
                access,
            )?
        }
        r => r?,
    };
    (access.upgrade)(account, &mut engine)?;
    Ok(engine)
}

/// Whether the space lists device `device` with the signing key this device just generated for
/// it (kept in the secret store before registering), i.e. registering it succeeded.
fn registered_as_us<T: SyncTransport>(
    account: &mut SyncEngine<T>,
    access: &mut SpaceAccess<'_, T>,
    id: &SpaceId,
    space: &str,
    device: &str,
) -> Result<bool> {
    let name = Scope::shared(space)?.secret_name(crate::engine::DEVICE_SECRET);
    let Some(raw) = (access.secrets)()?.get(&name)? else {
        return Ok(false);
    };
    let keys = crate::crypto::DeviceKeys::from_secret_string(&raw)?;
    if keys.device_id != device {
        return Ok(false);
    }
    let ours = base64url::encode(&keys.sign_pk());
    let mut t = (access.connect)(account, id)?;
    Ok(t.list_devices()?
        .devices
        .iter()
        .any(|d| d.device_id.as_str() == device && d.signing_public_key == ours))
}

/// Shares notebook `nb` (see the module docs). Idempotent: call it again after any failure and
/// it continues; for a notebook already shared it returns the existing space. Returns the space's
/// engine, ready to reconcile (reconcile it before the account engine, so the space has the
/// notebook before other devices see the marker). Runs network requests; don't hold the library
/// lock when calling it.
pub fn share<T: SyncTransport>(
    account: &mut SyncEngine<T>,
    lib: &Mutex<Library>,
    nb: Uuid,
    access: &mut SpaceAccess<'_, T>,
    hook: &mut dyn FnMut(ShareStep) -> Result<()>,
) -> Result<(SyncEngine<T>, String)> {
    let lib_root = lib
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .root()
        .to_path_buf();
    let title = {
        let l = lib.lock().unwrap_or_else(|p| p.into_inner());
        let owners = NotebookOwners::load(&lib_root)?;
        if owners.owner(&nb).is_some() {
            return Err(SyncError::Invalid(
                "notebooks in a vault can't be shared".into(),
            ));
        }
        let n = l.notebook(nb)?;
        if let Some(e) = owners.shared_entry(&nb) {
            if !e.role.may_write() || e.state == SharedState::AccessRemoved || e.role != Role::Owner
            {
                return Err(SyncError::Invalid(
                    "only the owner can share this notebook".into(),
                ));
            }
        }
        n.title.clone()
    };
    if *account.scope() != Scope::Account {
        return Err(SyncError::Invalid(
            "only account notebooks can be shared".into(),
        ));
    }
    let path = progress_path(&lib_root, nb);
    let mut progress = match load_progress(&path)? {
        Some(p) => p,
        None => {
            // already shared and finished: hand back the space's engine
            let owners = NotebookOwners::load(&lib_root)?;
            if let Some(e) = owners.shared_entry(&nb).cloned() {
                let root = load_root(account.secret_store(), &e.space_id)?.ok_or_else(|| {
                    SyncError::Invalid("this device doesn't hold the shared notebook's key".into())
                })?;
                let engine = open_or_enroll(account, &lib_root, &e.space_id, &root, access, false)?;
                return Ok((engine, e.space_id));
            }
            let p = ShareProgress {
                idempotency_key: Uuid::new_v4().hyphenated().to_string(),
                ..Default::default()
            };
            // the root is kept before anything reaches the server, so every retry uses it
            let root = RootSecret::generate()?;
            account
                .secret_store()
                .set(&pending_root_name(nb), &base64url::encode(&root.to_bytes()))?;
            save_progress(&path, &p)?;
            p
        }
    };
    let root = match account.secret_store().get(&pending_root_name(nb))? {
        Some(s) => RootSecret::from_bytes(crypto::decode32(&s)?),
        None => match &progress.space_id {
            Some(sp) => load_root(account.secret_store(), sp)?.ok_or_else(|| {
                SyncError::Integrity("the key of a notebook being shared is missing".into())
            })?,
            None => {
                return Err(SyncError::Integrity(
                    "the key of a notebook being shared is missing".into(),
                ))
            }
        },
    };
    // 1. the space
    if progress.space_id.is_none() {
        let rec = account.transport_mut().shared_create(&CreateSpaceRequest {
            idempotency_key: progress.idempotency_key.clone(),
            sealed_meta: seal_meta(
                &root,
                &SpaceMeta {
                    notebook_id: nb,
                    title: title.clone(),
                },
            ),
        })?;
        Scope::shared(rec.space_id.as_str())?;
        progress.space_id = Some(rec.space_id.to_string());
        save_progress(&path, &progress)?;
        hook(ShareStep::SpaceCreated)?;
    }
    let space = progress.space_id.clone().expect("set above");
    let sid = space_id(&space)?;
    keep_root(account.secret_store(), &space, &root)?;
    if !progress.key_stored {
        let doc = AccountKeyDocument {
            key: base64url::encode(&root.to_bytes()),
        };
        match account.transport_mut().shared_key_put(&sid, &doc) {
            Ok(()) => {}
            Err(e) if e.code() == Some(ErrorCode::Conflict) => {
                let held = account.transport_mut().shared_key_get(&sid)?;
                if held.as_ref().map(|d| d.key.as_str()) != Some(doc.key.as_str()) {
                    return Err(SyncError::Integrity(
                        "the server holds another key for this shared notebook".into(),
                    ));
                }
            }
            Err(e) => return Err(e.into()),
        }
        progress.key_stored = true;
        save_progress(&path, &progress)?;
        hook(ShareStep::KeyStored)?;
    }
    // 2. the engine
    let mut engine = open_or_enroll(account, &lib_root, &space, &root, access, true)?;
    engine.start_ingesting_at(0)?;
    hook(ShareStep::EngineReady)?;
    // 3. history, outside the lock
    transplant(account, &mut engine, &space, nb, hook)?;
    // 4. catch up, correct, mark and assign, all in one critical section
    {
        let mut l = lib.lock().unwrap_or_else(|p| p.into_inner());
        let mut report = ReconcileReport::default();
        // installs what the account delivered, so the marker depends on everything transplanted
        account.apply(&mut l, &mut report)?;
        transplant(account, &mut engine, &space, nb, &mut |_| Ok(()))?;
        correct_to(&mut engine, &mut l, nb)?;
        account.append_fixed(
            marker_id(&space, nb),
            penroam_core::now_ms(),
            DocumentOperation::NotebookShared {
                notebook_id: nb,
                space_id: space.clone(),
            },
        )?;
        account.save_state()?;
        let at = l.last_seq();
        engine.start_ingesting_at(at)?;
        let mut owners = NotebookOwners::load(&lib_root)?;
        let mut entry = SharedEntry::new(&space, Role::Owner, SharedState::Active, at);
        entry.title = Some(title);
        owners.shared.insert(nb, entry);
        owners.save(&lib_root)?;
    }
    hook(ShareStep::Marked)?;
    std::fs::remove_file(&path)?;
    let _ = account.secret_store().delete(&pending_root_name(nb));
    Ok((engine, space))
}

/// Refreshes a space's sealed title after the notebook was renamed. Only on the owner's devices
/// (the server refuses anyone else). Does nothing when the title didn't change since it was last
/// sealed.
pub fn refresh_meta<T: SyncTransport>(
    account: &mut SyncEngine<T>,
    lib: &Mutex<Library>,
    nb: Uuid,
) -> Result<bool> {
    let (entry, title, root_dir) = {
        let l = lib.lock().unwrap_or_else(|p| p.into_inner());
        let owners = NotebookOwners::load(l.root())?;
        let Some(e) = owners.shared_entry(&nb).cloned() else {
            return Ok(false);
        };
        let Ok(n) = l.notebook(nb) else {
            return Ok(false);
        };
        (e, n.title.clone(), l.root().to_path_buf())
    };
    // the server lets only the owner change the sealed title
    if entry.role != Role::Owner
        || entry.state != SharedState::Active
        || entry.title.as_deref() == Some(&title)
    {
        return Ok(false);
    }
    let Some(root) = load_root(account.secret_store(), &entry.space_id)? else {
        return Ok(false);
    };
    let sealed = seal_meta(
        &root,
        &SpaceMeta {
            notebook_id: nb,
            title: title.clone(),
        },
    );
    account.transport_mut().shared_set_meta(
        &space_id(&entry.space_id)?,
        &UpdateSpaceMetaRequest {
            sealed_meta: sealed,
        },
    )?;
    let _ = root_dir;
    edit_owners(lib, |o, _| {
        if let Some(e) = o.shared.get_mut(&nb) {
            e.title = Some(title);
        }
        Ok(())
    })?;
    Ok(true)
}

// ---------------------------------------------------------------- joining --

/// Starts joining the space of `record` (after accepting its invitation, or because another
/// device of the owner's account shared it): fetches its root, learns the notebook from the
/// sealed title, assigns it (state `joining`) and adds this device to the space. Continue with
/// [`join_step`] until it answers `true`. Safe to repeat. Runs network requests; don't hold the
/// library lock.
pub fn join<T: SyncTransport>(
    account: &mut SyncEngine<T>,
    lib: &Mutex<Library>,
    record: &SharedNotebookRecord,
    access: &mut SpaceAccess<'_, T>,
    expected: Option<Uuid>,
) -> Result<(SyncEngine<T>, Uuid)> {
    let space = record.space_id.to_string();
    Scope::shared(&space)?;
    let doc = account
        .transport_mut()
        .shared_key_get(&record.space_id)?
        .ok_or(SyncError::AccountHasNoKeys)?;
    let root = RootSecret::from_bytes(crypto::decode32(&doc.key)?);
    let meta = open_meta(&root, &record.sealed_meta)?;
    // The sealed title names the notebook only for a first join that nothing else vouches for
    // (an invitee's). On the owner's devices the share marker decides, and once a notebook is
    // assigned to a space its id is pinned: a member who rewrites the title can change neither.
    let nb = expected.unwrap_or(meta.notebook_id);
    let title = (meta.notebook_id == nb).then(|| meta.title.clone());
    if nb.is_nil() {
        return Err(SyncError::Invalid(
            "the shared notebook names no notebook".into(),
        ));
    }
    let lib_root = edit_owners(lib, |owners, l| {
        if owners.owner(&nb).is_some() {
            return Err(SyncError::Invalid(
                "this notebook is in a vault on this device".into(),
            ));
        }
        // one space, one notebook
        if owners
            .shared
            .iter()
            .any(|(n, e)| e.space_id == space && *n != nb)
        {
            return Err(SyncError::Invalid(
                "this shared notebook is already assigned to another notebook here".into(),
            ));
        }
        match owners.shared.get_mut(&nb) {
            Some(e) if e.space_id != space => {
                return Err(SyncError::Invalid(
                    "this notebook is already shared in another space".into(),
                ))
            }
            Some(e) if e.state == SharedState::AccessRemoved => return Err(stale_copy()),
            Some(e) => {
                e.role = record.role;
                if title.is_some() {
                    e.title = title.clone();
                }
            }
            None => {
                // only the share marker assigns a notebook on the owner's devices
                if record.role == Role::Owner {
                    return Err(SyncError::Invalid(
                        "this device hasn't received the share of this notebook yet".into(),
                    ));
                }
                // a space must never take over a notebook that is already here (a personal one,
                // say, whose id someone put in the sealed title)
                // (erased here is fine only if this device erased it as an earlier copy of this
                // same shared notebook)
                if l.notebook(nb).is_ok() || (l.is_purged(nb) && !owners.retired.contains(&nb)) {
                    return Err(SyncError::Invalid(
                        "a notebook with this id is already in this library".into(),
                    ));
                }
                owners.retired.remove(&nb);
                let mut e =
                    SharedEntry::new(&space, record.role, SharedState::Joining, l.last_seq());
                e.title = title.clone();
                owners.shared.insert(nb, e);
            }
        }
        Ok(l.root().to_path_buf())
    })?;
    keep_root(account.secret_store(), &space, &root)?;
    let mut engine = open_or_enroll(account, &lib_root, &space, &root, access, false)?;
    let since = NotebookOwners::load(&lib_root)?
        .shared_entry(&nb)
        .map_or(0, |e| e.since_lib_seq);
    engine.start_ingesting_at(since)?;
    Ok((engine, nb))
}

/// One step of joining: pulls the space, checks every asset is downloaded, and only then marks the
/// entry `active` and installs the notebook. `account` is the account engine on the owner's own
/// devices, whose unsent edits are re-published into the space first ([`republish_concurrent`]).
/// Returns whether joining is finished. Already-active entries return `true` at once.
pub fn join_step<T: SyncTransport>(
    space: &mut SyncEngine<T>,
    account: Option<&SyncEngine<T>>,
    lib: &Mutex<Library>,
) -> Result<(bool, ReconcileReport)> {
    join_step_with(space, account, None, lib)
}

/// [`join_step`], also moving the edits of a space this device lost a concurrent share with
/// (`orphan`, see [`republish_orphan`]). Re-published operations are added only once the space is
/// pulled: added earlier, they would refer to pages the space's merge doesn't have yet.
fn join_step_with<T: SyncTransport>(
    space: &mut SyncEngine<T>,
    account: Option<&SyncEngine<T>>,
    orphan: Option<&SyncEngine<T>>,
    lib: &Mutex<Library>,
) -> Result<(bool, ReconcileReport)> {
    let mut report = ReconcileReport::default();
    let Some(id) = space.scope().id().map(str::to_string) else {
        return Err(SyncError::Invalid("not a shared notebook engine".into()));
    };
    let lib_root = lib
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .root()
        .to_path_buf();
    let Some((nb, entry)) = NotebookOwners::load(&lib_root)?
        .space(&id)
        .map(|(n, e)| (n, e.clone()))
    else {
        return Err(SyncError::Invalid(
            "this shared notebook isn't assigned here".into(),
        ));
    };
    if entry.state == SharedState::Active {
        return Ok((true, report));
    }
    if entry.state == SharedState::AccessRemoved {
        return Err(SyncError::Transport(TransportError::Api(
            penroam_sync_protocol::ErrorBody::new(
                ErrorCode::AccessRevoked,
                "access to this shared notebook was removed",
                "",
            ),
        )));
    }
    space.exchange(&mut report)?;
    if !space.pull_complete() {
        return Ok((false, report));
    }
    let mut l = lib.lock().unwrap_or_else(|p| p.into_inner());
    if space.merged(nb)?.is_none() || !space.missing_assets(&l, nb)?.is_empty() {
        return Ok((false, report));
    }
    if let Some(acc) = account {
        republish_concurrent(acc, space, nb)?;
        if let Some(old) = orphan {
            republish_orphan(acc, old, space, nb)?;
        }
    }
    let mut owners = NotebookOwners::load(&lib_root)?;
    if let Some(e) = owners.shared.get_mut(&nb) {
        e.state = SharedState::Active;
    }
    owners.save(&lib_root)?;
    // the notebook may have been purged here earlier; joining brings it back
    l.forget_purge(nb);
    space.apply(&mut l, &mut report)?;
    Ok((true, report))
}

// ------------------------------------------------------------ access loss --

/// Stops a space for good on this device after access was removed: copies the journal entries
/// not yet in its log there (so unsent edits are kept with it), then marks the entry
/// `accessRemoved`. Nothing is uploaded or deleted. The caller drops the engine afterwards.
pub fn mark_access_removed<T: SyncTransport>(
    space: &mut SyncEngine<T>,
    lib: &Mutex<Library>,
) -> Result<()> {
    let Some(id) = space.scope().id().map(str::to_string) else {
        return Ok(());
    };
    let mut l = lib.lock().unwrap_or_else(|p| p.into_inner());
    if space.store.state.enrolled {
        space.ingest_now(&mut l)?;
    }
    let root = l.root().to_path_buf();
    let mut owners = NotebookOwners::load(&root)?;
    if let Some(nb) = owners.space(&id).map(|(n, _)| n) {
        if let Some(e) = owners.shared.get_mut(&nb) {
            e.state = SharedState::AccessRemoved;
        }
        owners.save(&root)?;
    }
    space.set_access_removed();
    Ok(())
}

/// Records a new role from the server (role changes take effect for uploads at once).
pub fn set_role(lib: &Mutex<Library>, space: &str, role: Role) -> Result<bool> {
    edit_owners(lib, |o, _| {
        let Some(nb) = o.space(space).map(|(n, _)| n) else {
            return Ok(false);
        };
        let e = o.shared.get_mut(&nb).expect("found");
        let changed = e.role != role;
        e.role = role;
        Ok(changed)
    })
}

/// Makes a personal copy of a shared notebook as it is on this device, unsent edits included,
/// under a new id. The copy syncs with the account like any notebook.
pub fn keep_private_copy(lib: &mut Library, nb: Uuid) -> Result<Notebook> {
    Ok(lib.duplicate_notebook(nb)?)
}

/// Removes this device's copy of a shared notebook and its space's sync folder and keys. Only
/// for a notebook whose access was removed (an active member would simply join again); the
/// server-side space is untouched. Call with the library lock held; nothing else of the notebook is kept.
pub fn remove_local(lib: &mut Library, secrets: &dyn SecretStore, space: &str) -> Result<()> {
    let root = lib.root().to_path_buf();
    let mut owners = NotebookOwners::load(&root)?;
    let Some((nb, entry)) = owners.space(space).map(|(n, e)| (n, e.clone())) else {
        return Err(SyncError::Invalid(
            "this shared notebook isn't on this device".into(),
        ));
    };
    if entry.state != SharedState::AccessRemoved {
        // a member would just join again; leaving comes first
        return Err(SyncError::Invalid(
            "only a notebook you no longer have access to can be removed from this device".into(),
        ));
    }
    // the notebook goes first: once the entry is gone, the account engine would take it as its own
    if lib.notebook(nb).is_ok() {
        lib.erase_from_sync(nb)?;
    }
    owners.shared.remove(&nb);
    owners.retired.insert(nb);
    owners.save(&root)?;
    let scope = Scope::shared(space)?;
    let _ = secrets.delete(&scope.secret_name(crate::engine::DEVICE_SECRET));
    let _ = secrets.delete(&scope.secret_name(SPACE_ROOT_SECRET));
    match std::fs::remove_dir_all(scope.dir(&root)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    Ok(())
}

/// What [`notebook_access`] answers: whether a notebook may be changed on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    /// An ordinary notebook.
    Personal,
    /// A notebook in a vault.
    Vault,
    /// A shared notebook, with this account's role, and whether access was removed.
    Shared {
        /// The role.
        role: Role,
        /// The entry's state.
        state: SharedState,
    },
}

impl Access {
    /// Why a change would be refused: `"viewer"` or `"accessRemoved"`, `None` when allowed.
    pub fn read_only_reason(&self) -> Option<&'static str> {
        match self {
            Access::Shared {
                state: SharedState::AccessRemoved,
                ..
            } => Some("accessRemoved"),
            Access::Shared { role, .. } if !role.may_write() => Some("viewer"),
            _ => None,
        }
    }
}

/// How notebook `nb` may be used on this device, from the owners file in `library_root`.
pub fn notebook_access(library_root: &Path, nb: Uuid) -> io::Result<Access> {
    let owners = NotebookOwners::load(library_root)?;
    if owners.owner(&nb).is_some() {
        return Ok(Access::Vault);
    }
    Ok(match owners.shared_entry(&nb) {
        Some(e) => Access::Shared {
            role: e.role,
            state: e.state,
        },
        None => Access::Personal,
    })
}

// ------------------------------------------------------------- the set --

/// What [`SpaceSet::sync_state`] reports for one shared notebook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NotebookSyncState {
    /// Saved on this device; not uploaded (no session, joining, offline, or a viewer's edits
    /// from before a role change, which are kept but never sent).
    LocalSaved,
    /// Changes are waiting to be uploaded.
    WaitingToUpload,
    /// Nothing waiting, and up to date with the server.
    Synced,
    /// This account's access was removed.
    AccessRemoved,
    /// Concurrent edits left a conflict to resolve.
    NeedsResolution,
    /// Catching up from a checkpoint after a long time away; unsent changes are kept.
    NeedsCatchUp,
}

/// Where a space's last member list is saved (in its sync folder).
const MEMBERS_FILE: &str = "members.json";

/// The engines of every shared notebook space this device takes part in, and the server's
/// latest list of spaces and invitations. The app's sync worker owns one and calls
/// [`SpaceSet::refresh`] and [`SpaceSet::reconcile`] after each account reconcile; a space that
/// fails never stops the others.
pub struct SpaceSet<T: SyncTransport> {
    /// One engine per space with an engine running here.
    pub engines: Vec<SyncEngine<T>>,
    /// The spaces the server listed last (owned, joined, and revoked once).
    pub records: Vec<SharedNotebookRecord>,
    /// Invitations waiting for this account, as last listed.
    pub invitations: Vec<penroam_sync_protocol::IncomingInvitation>,
    /// Space id → the last failure of that space (user-facing text).
    pub errors: std::collections::BTreeMap<String, String>,
    /// Whether the server offers sharing (`sharing.v1`), as of the last refresh.
    pub available: bool,
    /// The server wants a recent sign-in before it lists this account's invitations
    /// (`reauth_required`: the verified email is older than its window). The invitations are
    /// then unknown; shared notebooks keep syncing.
    pub invitations_need_sign_in: bool,
    /// Whether the server has comment streams (`comments.v1`), as of the last refresh.
    pub comments_available: bool,
    /// Each space's comment stream on this device, opened on first use.
    pub comments: BTreeMap<String, crate::comments::CommentSync>,
    /// Each space's member roster, from its member list at the last comment sync.
    pub rosters: BTreeMap<String, crate::comments::SpaceRoster>,
    /// This account's notifications as last fetched (which threads have unread ones).
    pub notifications: Vec<penroam_sync_protocol::comments::NotificationRecord>,
    /// Whether the server records who pushed what (`history.v1`), as of the last refresh.
    pub history_available: bool,
    /// Each space's member list as last read (for rosters and author names).
    pub members: BTreeMap<String, penroam_sync_protocol::MemberList>,
    /// The active members each space's attribution was last fetched with. When one of them is
    /// no longer active (left, removed or erased), attribution is fetched again from the start,
    /// because the server may have replaced their id with a tombstone.
    attributed_members: BTreeMap<String, std::collections::BTreeSet<String>>,
}

impl<T: SyncTransport> Default for SpaceSet<T> {
    fn default() -> Self {
        Self {
            engines: Vec::new(),
            records: Vec::new(),
            invitations: Vec::new(),
            errors: Default::default(),
            available: false,
            invitations_need_sign_in: false,
            comments_available: false,
            comments: BTreeMap::new(),
            rosters: BTreeMap::new(),
            notifications: Vec::new(),
            history_available: false,
            members: BTreeMap::new(),
            attributed_members: BTreeMap::new(),
        }
    }
}

fn is_code(e: &SyncError, code: ErrorCode) -> bool {
    matches!(e, SyncError::Transport(t) if t.code() == Some(code))
}

impl<
        T: SyncTransport
            + crate::comments::CommentTransport
            + crate::attribution::AttributionTransport,
    > SpaceSet<T>
{
    /// An empty set; engines are opened by [`SpaceSet::reconcile`] as needed.
    pub fn new() -> Self {
        Self::default()
    }

    /// The engine of a space, if running here.
    pub fn engine(&mut self, space: &str) -> Option<&mut SyncEngine<T>> {
        self.engines
            .iter_mut()
            .find(|e| e.scope().id() == Some(space))
    }

    /// Takes a space's engine out of the set.
    pub fn take(&mut self, space: &str) -> Option<SyncEngine<T>> {
        let i = self
            .engines
            .iter()
            .position(|e| e.scope().id() == Some(space))?;
        Some(self.engines.remove(i))
    }

    /// Adds (or replaces) a space's engine.
    pub fn insert(&mut self, engine: SyncEngine<T>) {
        if let Some(id) = engine.scope().id().map(str::to_string) {
            self.take(&id);
        }
        self.engines.push(engine);
    }

    /// Rereads the server's lists of spaces and invitations with the account engine's session.
    /// A membership listed as revoked, or a space no longer listed, stops that space here
    /// ([`mark_access_removed`]); a changed role is recorded. Returns whether anything a UI shows
    /// changed. Without `sharing.v1` it clears the lists and returns.
    pub fn refresh(&mut self, account: &mut SyncEngine<T>, lib: &Mutex<Library>) -> Result<bool> {
        let meta = account.transport_mut().meta()?;
        let available = meta.has_capability(penroam_sync_protocol::dto::capabilities::SHARING_V1);
        self.comments_available =
            meta.has_capability(penroam_sync_protocol::dto::capabilities::COMMENTS_V1);
        self.history_available =
            meta.has_capability(penroam_sync_protocol::dto::capabilities::HISTORY_V1);
        if !available {
            let changed =
                self.available || !self.records.is_empty() || !self.invitations.is_empty();
            self.available = false;
            self.records.clear();
            self.invitations.clear();
            self.invitations_need_sign_in = false;
            return Ok(changed);
        }
        let list = account.transport_mut().shared_list()?.spaces;
        // a stale sign-in only hides the invitations; it never stops the list or the spaces
        // (the list says so with its `reauth_required` flag; a server may also answer 403). Any
        // other failure keeps the invitations shown last: memberships and roles below must be
        // applied whatever happens to the invitation list.
        let (invitations, need_sign_in) = match account.transport_mut().invitations_list() {
            Ok(l) if l.reauth_required => (Vec::new(), true),
            Ok(l) => (l.invitations, false),
            Err(e) if e.code() == Some(ErrorCode::ReauthRequired) => (Vec::new(), true),
            Err(_) => (self.invitations.clone(), self.invitations_need_sign_in),
        };
        let mut changed = !self.available
            || list != self.records
            || invitations != self.invitations
            || need_sign_in != self.invitations_need_sign_in;
        self.invitations_need_sign_in = need_sign_in;
        self.available = true;
        let root = lib
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .root()
            .to_path_buf();
        let owners = NotebookOwners::load(&root)?;
        for e in owners.shared.values() {
            if e.state == SharedState::AccessRemoved {
                continue;
            }
            let rec = list.iter().find(|r| r.space_id.as_str() == e.space_id);
            match rec {
                Some(r) if r.state == penroam_sync_protocol::MembershipState::Active => {
                    if r.role != e.role {
                        set_role(lib, &e.space_id, r.role)?;
                        changed = true;
                    }
                }
                // Revoked, or gone (the space was erased). A space this device is still creating
                // isn't listed until it exists, and has no entry until then either.
                _ => {
                    self.stop(&e.space_id, lib)?;
                    changed = true;
                }
            }
        }
        self.records = list;
        self.invitations = invitations;
        Ok(changed)
    }

    /// Checks over HTTP whether this account still has access to `space`, after a cue that it
    /// might not (a live channel's `bye access_revoked` or membership notice, which aren't proof):
    /// rereads the shared notebook list ([`SpaceSet::refresh`], which stops the space only if the
    /// server lists the membership as revoked or not at all), then syncs the space (an
    /// authenticated `access_revoked` answer stops it too). Returns whether the space is still
    /// active here.
    pub fn confirm_access(
        &mut self,
        account: &mut SyncEngine<T>,
        lib: &Mutex<Library>,
        access: &mut SpaceAccess<'_, T>,
        space: &str,
    ) -> Result<bool> {
        self.refresh(account, lib)?;
        let _ = self.reconcile_space(account, lib, access, space);
        let root = lib
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .root()
            .to_path_buf();
        Ok(NotebookOwners::load(&root)?
            .space(space)
            .is_some_and(|(_, e)| e.state != SharedState::AccessRemoved))
    }

    /// Stops a space after access was removed: its engine keeps local data
    /// ([`mark_access_removed`]) and leaves the set.
    pub fn stop(&mut self, space: &str, lib: &Mutex<Library>) -> Result<()> {
        // the comment stream stays on disk (drafts and unsent comments are kept); it just stops
        self.comments.remove(space);
        self.rosters.remove(space);
        self.members.remove(space);
        self.attributed_members.remove(space);
        match self.take(space) {
            Some(mut e) => mark_access_removed(&mut e, lib)?,
            None => {
                edit_owners(lib, |o, _| {
                    if let Some(nb) = o.space(space).map(|(n, _)| n) {
                        o.shared.get_mut(&nb).expect("found").state = SharedState::AccessRemoved;
                    }
                    Ok(())
                })?;
            }
        }
        self.errors.remove(space);
        Ok(())
    }

    /// Syncs every space assigned on this device: opens or joins engines as needed, continues
    /// joins, re-publishes this device's edits that crossed a share, and reconciles the rest.
    /// Call after the account engine reconciled. Returns the reports of spaces that did
    /// something; failures are kept in [`SpaceSet::errors`].
    pub fn reconcile(
        &mut self,
        account: &mut SyncEngine<T>,
        lib: &Mutex<Library>,
        access: &mut SpaceAccess<'_, T>,
    ) -> Vec<ReconcileReport> {
        let mut reports = Vec::new();
        let root = lib
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .root()
            .to_path_buf();
        let Ok(mut owners) = NotebookOwners::load(&root) else {
            return reports;
        };
        // Spaces this account joined on another device: join here too. The owner's devices are
        // assigned through the share marker instead (which also re-publishes their edits).
        let joined_elsewhere: Vec<SharedNotebookRecord> = self
            .records
            .iter()
            .filter(|r| {
                r.state == penroam_sync_protocol::MembershipState::Active
                    && r.role != Role::Owner
                    && owners.space(r.space_id.as_str()).is_none()
            })
            .cloned()
            .collect();
        for rec in joined_elsewhere {
            match join(account, lib, &rec, access, None) {
                Ok((engine, _)) => self.insert(engine),
                Err(e) => {
                    self.errors.insert(rec.space_id.to_string(), e.to_string());
                }
            }
        }
        if let Ok(o) = NotebookOwners::load(&root) {
            owners = o;
        }
        for (nb, entry) in owners.shared.clone() {
            if let Some(r) = self.reconcile_entry(account, lib, access, nb, &entry, &root) {
                reports.push(r);
            }
        }
        reports
    }

    /// Syncs one space now (a live commit hint asked for it). `None` when this device has no
    /// such space, it stopped, or the attempt failed (kept in [`SpaceSet::errors`]).
    pub fn reconcile_space(
        &mut self,
        account: &mut SyncEngine<T>,
        lib: &Mutex<Library>,
        access: &mut SpaceAccess<'_, T>,
        space: &str,
    ) -> Option<ReconcileReport> {
        let root = lib
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .root()
            .to_path_buf();
        let owners = NotebookOwners::load(&root).ok()?;
        let (nb, entry) = owners.space(space).map(|(n, e)| (n, e.clone()))?;
        self.reconcile_entry(account, lib, access, nb, &entry, &root)
    }

    fn reconcile_entry(
        &mut self,
        account: &mut SyncEngine<T>,
        lib: &Mutex<Library>,
        access: &mut SpaceAccess<'_, T>,
        nb: Uuid,
        entry: &SharedEntry,
        root: &Path,
    ) -> Option<ReconcileReport> {
        let space = entry.space_id.clone();
        if entry.state == SharedState::AccessRemoved {
            self.take(&space);
            return None;
        }
        match self.reconcile_one(account, lib, access, nb, entry, root) {
            Ok(Some(r)) => {
                self.errors.remove(&space);
                Some(r)
            }
            Ok(None) => None,
            Err(e) if is_code(&e, ErrorCode::AccessRevoked) => {
                let _ = self.stop(&space, lib);
                None
            }
            Err(e) => {
                if is_code(&e, ErrorCode::Unauthenticated) {
                    // the space session expired: the next pass asks for a new one
                    if let Some(eng) = self.engine(&space) {
                        let _ = access.reconnect(account, eng);
                    }
                }
                self.errors.insert(space, e.to_string());
                None
            }
        }
    }

    fn reconcile_one(
        &mut self,
        account: &mut SyncEngine<T>,
        lib: &Mutex<Library>,
        access: &mut SpaceAccess<'_, T>,
        nb: Uuid,
        entry: &SharedEntry,
        root: &Path,
    ) -> Result<Option<ReconcileReport>> {
        let space = entry.space_id.clone();
        if self.engine(&space).is_none() {
            let scope = Scope::shared(&space)?;
            let engine = if scope.dir(root).join("state.json").exists() {
                access.open_existing(account, root, &space)?
            } else {
                let Some(rec) = self
                    .records
                    .iter()
                    .find(|r| r.space_id.as_str() == space)
                    .cloned()
                else {
                    return Ok(None);
                };
                // the notebook this device already assigned to the space
                join(account, lib, &rec, access, Some(nb))?.0
            };
            self.insert(engine);
        }
        let owner = entry.role == Role::Owner;
        // A concurrent share lost to this space: this device's edits in its own space move here
        // before anything of the winner is installed, then that space goes.
        let orphan = match &entry.replaces {
            Some(old) => self.open_orphan(account, access, old, root)?,
            None => None,
        };
        let engine = self.engine(&space).expect("inserted");
        if entry.state != SharedState::Active {
            let (done, report) =
                join_step_with(engine, owner.then_some(&*account), orphan.as_ref(), lib)?;
            if done {
                if let Some(old) = orphan {
                    self.drop_orphan(old, lib, nb)?;
                }
            } else if let Some(old) = orphan {
                self.insert(old);
            }
            return Ok(Some(report));
        }
        if let Some(old) = orphan {
            // already joined (a crash came between joining and removing it): the space is
            // pulled, so its operations apply on top of what is there
            republish_orphan(account, &old, self.engine(&space).expect("inserted"), nb)?;
            self.drop_orphan(old, lib, nb)?;
        }
        let engine = self.engine(&space).expect("inserted");
        if owner {
            republish_concurrent(account, engine, nb)?;
        }
        let report = engine.reconcile(lib)?;
        // the comment stream after the document stream (its keys are fresh now); a failure here
        // never fails the document sync
        if self.comments_available {
            if let Err(e) = self.sync_comments(account, lib, &space) {
                self.errors.insert(space.clone(), format!("comments: {e}"));
            }
        }
        if self.history_available {
            if let Err(e) = self.sync_attribution(account, &space) {
                if !e.is_retryable() {
                    self.errors.insert(space.clone(), format!("history: {e}"));
                }
            }
        }
        let engine = self.engine(&space).expect("inserted");
        if owner {
            // members who were removed or left: rotate their devices out of the key log
            if let Err(e) = engine.remove_revoked_devices() {
                if !e.is_retryable() {
                    self.errors.insert(space.clone(), e.to_string());
                }
            }
        }
        if owner {
            if let Err(e) = refresh_meta(account, lib, nb) {
                if !e.is_retryable() {
                    self.errors.insert(space, e.to_string());
                }
            }
        }
        Ok(Some(report))
    }

    /// The engine of a space this device made and then lost to a concurrent share: from the set,
    /// or opened from its sync folder. `None` when nothing of it is left here.
    fn open_orphan(
        &mut self,
        account: &mut SyncEngine<T>,
        access: &mut SpaceAccess<'_, T>,
        old: &str,
        root: &Path,
    ) -> Result<Option<SyncEngine<T>>> {
        if let Some(e) = self.take(old) {
            return Ok(Some(e));
        }
        let scope = Scope::shared(old)?;
        if !scope.dir(root).join("state.json").exists() {
            return Ok(None);
        }
        Ok(Some(access.open_existing(account, root, old)?))
    }

    /// Removes a lost space's engine and sync folder here, once its edits moved to the winner,
    /// and forgets it in the entry. The space itself stays on the server (no route removes one);
    /// it holds nothing the winner lacks.
    fn drop_orphan(&mut self, old: SyncEngine<T>, lib: &Mutex<Library>, nb: Uuid) -> Result<()> {
        old.disconnect()?;
        edit_owners(lib, |o, _| {
            if let Some(e) = o.shared.get_mut(&nb) {
                e.replaces = None;
            }
            Ok(())
        })
    }

    /// The comment stream of a space, opened (from `<space folder>/comments`) on first use.
    /// `None` when the space has no engine here or isn't assigned to a notebook.
    pub fn comment_stream(
        &mut self,
        account: &SyncEngine<T>,
        lib: &Mutex<Library>,
        space: &str,
    ) -> Result<Option<&mut crate::comments::CommentSync>> {
        if !self.comments.contains_key(space) {
            let root = lib
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .root()
                .to_path_buf();
            let Some(engine) = self.engine(space) else {
                return Ok(None);
            };
            let device = Uuid::parse_str(engine.device_id())
                .map_err(|_| SyncError::Integrity("space device id is not a UUID".into()))?;
            let me: penroam_sync_protocol::AccountId = account
                .account_id()
                .parse()
                .map_err(|_| SyncError::Invalid("invalid account id".into()))?;
            let dir = Scope::shared(space)?.dir(&root).join("comments");
            let cs = crate::comments::CommentSync::open(&dir, device, &me)
                .map_err(|e| SyncError::Invalid(e.to_string()))?;
            self.comments.insert(space.to_string(), cs);
        }
        Ok(self.comments.get_mut(space))
    }

    /// Rereads a space's member list (who may comment, who moderates, who can be mentioned) and
    /// saves it next to the space's sync data (`members.json`), so [`Self::cached_roster`] works
    /// offline after a restart.
    pub fn refresh_roster(
        &mut self,
        account: &mut SyncEngine<T>,
        space: &str,
    ) -> Result<Option<crate::comments::SpaceRoster>> {
        let list = account.transport_mut().shared_members(&space_id(space)?)?;
        let roster = crate::comments::SpaceRoster::from_member_list(&list);
        if let Some(e) = self.engine(space) {
            if let Ok(b) = serde_json::to_vec(&list) {
                // a cache only: failing to write it costs nothing but offline names
                let _ = penroam_storage::fsutil::write_atomic(&e.store.dir.join(MEMBERS_FILE), &b);
            }
        }
        self.members.insert(space.to_string(), list);
        match &roster {
            Some(r) => {
                self.rosters.insert(space.to_string(), r.clone());
            }
            None => {
                self.rosters.remove(space);
            }
        }
        Ok(roster)
    }

    /// The space's member list as last read: in memory, else as saved by
    /// [`Self::refresh_roster`]. `None` when it was never read on this device.
    pub fn cached_members(&mut self, space: &str) -> Option<&penroam_sync_protocol::MemberList> {
        if !self.members.contains_key(space) {
            let dir = self.engine(space)?.store.dir.clone();
            let b = std::fs::read(dir.join(MEMBERS_FILE)).ok()?;
            let list: penroam_sync_protocol::MemberList = serde_json::from_slice(&b).ok()?;
            self.members.insert(space.to_string(), list);
        }
        self.members.get(space)
    }

    /// The roster from [`Self::cached_members`].
    pub fn cached_roster(&mut self, space: &str) -> Option<crate::comments::SpaceRoster> {
        crate::comments::SpaceRoster::from_member_list(self.cached_members(space)?)
    }

    /// [`Self::refresh_roster`], falling back to [`Self::cached_roster`] when the server can't
    /// be reached (a network or other retryable failure). Other errors are returned.
    pub fn roster(
        &mut self,
        account: &mut SyncEngine<T>,
        space: &str,
    ) -> Result<Option<crate::comments::SpaceRoster>> {
        match self.refresh_roster(account, space) {
            Err(e)
                if e.is_retryable()
                    || matches!(
                        e,
                        SyncError::Transport(
                            crate::transport::TransportError::Network(_)
                                | crate::transport::TransportError::ResponseLost
                        )
                    ) =>
            {
                Ok(self.cached_roster(space))
            }
            r => r,
        }
    }

    /// Fetches who pushed each new operation and comment of a space (`history.v1`) into the
    /// space's [`AttributionStore`](crate::attribution::AttributionStore). Uses the member list
    /// read for comments when there is one, else reads it. When someone active at the last fetch
    /// no longer is, everything is fetched again ([`crate::attribution::refresh`]) so an erased
    /// account's tombstone replaces its id. Returns how many entries arrived.
    pub fn sync_attribution(&mut self, account: &mut SyncEngine<T>, space: &str) -> Result<usize> {
        use crate::attribution::{self as attr, AttributionStore, Stream};
        if !self.members.contains_key(space) {
            self.refresh_roster(account, space)?;
        }
        let active: std::collections::BTreeSet<String> = self
            .members
            .get(space)
            .map(|l| {
                l.members
                    .iter()
                    .filter(|m| m.state == penroam_sync_protocol::MembershipState::Active)
                    .map(|m| m.account_id.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let again = self
            .attributed_members
            .get(space)
            .is_some_and(|before| before.iter().any(|m| !active.contains(m)));
        let comments = self.comments_available;
        let engine = self
            .engine(space)
            .ok_or_else(|| SyncError::Invalid("no engine for this shared notebook".into()))?;
        let mut store = AttributionStore::open(&engine.store.dir)?;
        // what opening found was logged when it first arrived
        let known = store.pin_violations().len();
        let mut n = 0;
        let t = engine.transport_mut() as &mut dyn attr::AttributionTransport;
        for stream in [Stream::Ops, Stream::Comments] {
            if stream == Stream::Comments && !comments {
                continue;
            }
            n += if again {
                attr::refresh(&mut store, t, stream)?
            } else {
                attr::fetch(&mut store, t, stream)?
            };
        }
        // a device the server named with an account other than the one it was first seen with:
        // its operations show as unverified; record why (ids only, no content)
        let violations: Vec<String> = store.pin_violations()[known..]
            .iter()
            .map(|v| v.event())
            .collect();
        let engine = self.engine(space).expect("checked above");
        for v in violations {
            engine.event(v);
        }
        self.attributed_members.insert(space.to_string(), active);
        Ok(n)
    }

    /// Syncs a space's comment stream: rereads its roster, then uploads what is queued and pulls,
    /// verifies and merges what is new (`CommentSync::sync`), with the space engine's session and
    /// keys. Call after the space's document sync, so its key log is fresh.
    pub fn sync_comments(
        &mut self,
        account: &mut SyncEngine<T>,
        lib: &Mutex<Library>,
        space: &str,
    ) -> Result<crate::comments::CommentReport> {
        let root = lib
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .root()
            .to_path_buf();
        let nb = NotebookOwners::load(&root)?
            .space(space)
            .map(|(n, _)| n)
            .ok_or_else(|| SyncError::Invalid("this shared notebook isn't assigned here".into()))?;
        let roster = self.refresh_roster(account, space)?.ok_or_else(|| {
            SyncError::Invalid("the member list has no owner or no caller".into())
        })?;
        if self.comment_stream(account, lib, space)?.is_none() {
            return Err(SyncError::Invalid(
                "no engine for this shared notebook".into(),
            ));
        }
        let mut cs = self.comments.remove(space).expect("opened");
        let engine = self.engine(space).expect("checked by comment_stream");
        let res = engine.comment_session(nb, |t, keys, limits| {
            cs.sync(
                t as &mut dyn crate::comments::CommentTransport,
                keys,
                &roster,
                limits,
            )
        });
        self.comments.insert(space.to_string(), cs);
        match res {
            Some(r) => r.map_err(|e| match e {
                crate::comments::CommentSyncError::Transport(t) => SyncError::Transport(t),
                other => SyncError::Invalid(other.to_string()),
            }),
            None => Err(SyncError::NotEnrolled),
        }
    }

    /// Accepts an invitation and joins its space as far as it goes now (the rest continues in
    /// later [`SpaceSet::reconcile`] calls). Returns the record and the notebook id.
    pub fn accept(
        &mut self,
        account: &mut SyncEngine<T>,
        lib: &Mutex<Library>,
        access: &mut SpaceAccess<'_, T>,
        invitation: &penroam_sync_protocol::InvitationId,
    ) -> Result<(SharedNotebookRecord, Uuid, ReconcileReport)> {
        let listed = self
            .invitations
            .iter()
            .find(|i| i.invitation_id == *invitation)
            .cloned();
        if let Some(i) = &listed {
            let root = lib
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .root()
                .to_path_buf();
            if NotebookOwners::load(&root)?
                .space(i.space_id.as_str())
                .is_some_and(|(_, e)| e.state == SharedState::AccessRemoved)
            {
                return Err(stale_copy());
            }
        }
        let invited_by = listed.map(|i| i.invited_by);
        let rec = account.transport_mut().invitation_accept(invitation)?;
        self.invitations.retain(|i| i.invitation_id != *invitation);
        let (engine, nb) = join(account, lib, &rec, access, None)?;
        if let Some(by) = invited_by.filter(|b| !b.is_empty()) {
            edit_owners(lib, |o, _| {
                if let Some(e) = o.shared.get_mut(&nb) {
                    e.owner_display = Some(by);
                }
                Ok(())
            })?;
        }
        let space = rec.space_id.to_string();
        self.insert(engine);
        if !self.records.iter().any(|r| r.space_id == rec.space_id) {
            self.records.push(rec.clone());
        }
        let engine = self.engine(&space).expect("inserted");
        let report = match join_step(engine, None, lib) {
            Ok((_, r)) => r,
            Err(e) if e.is_retryable() => ReconcileReport::default(),
            Err(e) => {
                self.errors.insert(space.clone(), e.to_string());
                ReconcileReport::default()
            }
        };
        Ok((rec, nb, report))
    }

    /// Shares a notebook ([`share`]) and adds the space's engine to the set.
    pub fn share(
        &mut self,
        account: &mut SyncEngine<T>,
        lib: &Mutex<Library>,
        access: &mut SpaceAccess<'_, T>,
        nb: Uuid,
    ) -> Result<String> {
        let (engine, space) = share(account, lib, nb, access, &mut |_| Ok(()))?;
        self.insert(engine);
        Ok(space)
    }

    /// The sync state of shared notebook `nb` for the UI.
    pub fn sync_state(
        &self,
        lib: &Library,
        owners: &NotebookOwners,
        nb: Uuid,
    ) -> NotebookSyncState {
        let Some(entry) = owners.shared_entry(&nb) else {
            return NotebookSyncState::LocalSaved;
        };
        if entry.state == SharedState::AccessRemoved {
            return NotebookSyncState::AccessRemoved;
        }
        let Some(e) = self
            .engines
            .iter()
            .find(|e| e.scope().id() == Some(entry.space_id.as_str()))
        else {
            return NotebookSyncState::LocalSaved;
        };
        if entry.state != SharedState::Active {
            return NotebookSyncState::LocalSaved;
        }
        if e.conflict_count(nb) > 0 {
            return NotebookSyncState::NeedsResolution;
        }
        if e.phase() == crate::engine::Phase::NeedsCatchUp {
            return NotebookSyncState::NeedsCatchUp;
        }
        let queued = e.unacked() + e.not_ingested(lib, nb);
        if queued > 0 {
            return if entry.role.may_write() {
                NotebookSyncState::WaitingToUpload
            } else {
                NotebookSyncState::LocalSaved
            };
        }
        if e.phase() == crate::engine::Phase::Idle && !self.errors.contains_key(&entry.space_id) {
            NotebookSyncState::Synced
        } else {
            NotebookSyncState::LocalSaved
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_open_only_with_the_space_root() {
        let root = RootSecret::generate().unwrap();
        let other = RootSecret::generate().unwrap();
        let m = SpaceMeta {
            notebook_id: Uuid::from_u128(7),
            title: "Field notes".into(),
        };
        let sealed = seal_meta(&root, &m);
        assert!(!sealed.contains("Field"));
        assert!(penroam_sync_protocol::sharing::validate_sealed_meta(&sealed).is_ok());
        assert_eq!(open_meta(&root, &sealed).unwrap(), m);
        assert!(open_meta(&other, &sealed).is_err());
    }

    #[test]
    fn republished_ids_are_stable_and_per_space() {
        let op = Uuid::from_u128(42);
        assert_eq!(republished_id("s1", op), republished_id("s1", op));
        assert_ne!(republished_id("s1", op), republished_id("s2", op));
        assert_ne!(republished_id("s1", op), op);
        assert_ne!(marker_id("s1", op), republished_id("s1", op));
    }

    #[test]
    fn viewers_and_removed_members_are_read_only() {
        let a = |role, state| Access::Shared { role, state };
        assert_eq!(
            a(Role::Viewer, SharedState::Active).read_only_reason(),
            Some("viewer")
        );
        assert_eq!(
            a(Role::Editor, SharedState::AccessRemoved).read_only_reason(),
            Some("accessRemoved")
        );
        assert_eq!(
            a(Role::Editor, SharedState::Active).read_only_reason(),
            None
        );
        assert_eq!(Access::Personal.read_only_reason(), None);
    }
}
