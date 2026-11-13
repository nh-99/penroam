//! History commands: non-destructive restore previews and commits, version comparison and the
//! activity list.
//!
//! Restores always go through [`penroam_storage::restore`]: a preview computes a plan between the
//! chosen past version (the base) and the page as it is now (the head), the UI picks items, and
//! the commit re-checks that the head is unchanged before applying one ordinary forward batch.
//! Nothing a restore does removes anyone's later work, so these are the only restore paths shared
//! notebooks may use ([`refuse_destructive_restore`] guards the older, page-replacing commands).
//!
//! Plans live in memory for [`PLAN_TTL`] under a random `planId`; a commit after that, or after
//! the page changed, fails with a message starting with [`CHANGED_SINCE_PREVIEW`] so the UI
//! re-runs the preview.
//!
//! Who wrote what comes from a [`HistorySource`]. For personal notebooks the local user is the
//! only writer. Shared notebooks use [`AttributedHistory`], which reads what the sync worker
//! provides through [`HistoryData`] (the space's logged operations, its stored attribution from
//! `penroam_sync::attribution`, the member list) once the worker [`install`]s it. Until then
//! [`NoAttribution`] reports every writer as unknown, which makes the planner select nothing but
//! re-adds by default (the safe choice) and the activity list empty.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use penroam_core::activity::{self, ActivityKind, ActivityOptions, OpAttribution};
use penroam_core::model::{Element, Notebook, Page};
use penroam_core::operation::LocalOperation;
use penroam_storage::restore::{
    check_notebook_precondition, check_precondition, ChangedChoice, NotebookRestorePlan,
    NotebookRestoreSelection, NotebookRestoreStep, RestorePlan, RestoreSelection, WritersSinceBase,
};
use penroam_storage::Library;
use penroam_sync::attribution::{
    self, AttributionStore, AuthorContext, AuthorKind as SyncAuthorKind,
};
use penroam_sync::shared::{self, Access};
use penroam_sync::store::OpMeta;
use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;
use uuid::Uuid;

use crate::commands::{invalid, CmdError};
use crate::AppState;

/// How long a preview stays committable.
pub const PLAN_TTL: Duration = Duration::from_secs(15 * 60);
/// Prefix of the error a stale commit returns; the UI then runs the preview again.
pub const CHANGED_SINCE_PREVIEW: &str = "changed_since_preview:";
/// Author key of the local user in personal notebooks.
const ME: &str = "local-user";
/// Activity entries per page of `history_activity`.
const ACTIVITY_PAGE: usize = 200;

type R<T> = Result<T, CmdError>;

// ------------------------------------------------------------------ source --

/// What the history commands need to know about authors, beyond the library. The sync side
/// implements this for shared notebooks; [`NoAttribution`] stands in until then.
pub trait HistorySource: Send + Sync {
    /// For each element of `page` changed since the version at log `position`, the account ids
    /// that changed it (`penroam-sync`: `writers_since(notebook, page, position)`). `None` when
    /// unknown.
    fn writers_since(&self, notebook: Uuid, page: Uuid, position: u64) -> Option<WritersSinceBase>;
    /// The notebook's operations in log order, with attribution (`penroam-sync`: the op log
    /// joined with `attribution_for(notebook, op ids)`). `None` when there is no log.
    fn notebook_ops(&self, notebook: Uuid) -> Option<Vec<(LocalOperation, OpAttribution)>>;
    /// The current user's account id in the notebook's space, if known.
    fn me(&self, notebook: Uuid) -> Option<String>;
    /// How to show `account` in `notebook` (member list, former member, erased account).
    fn author(&self, notebook: Uuid, account: &str) -> AuthorView;
    /// A resolver for many authors of one notebook at once (activity lists). The default asks
    /// [`HistorySource::author`] each time.
    fn authors(&self, notebook: Uuid) -> Box<dyn Fn(&str) -> AuthorView + '_> {
        Box::new(move |a| self.author(notebook, a))
    }
}

/// What the sync worker hands the history commands for one shared notebook: its space's logged
/// operations, the space's stored attribution, who is who, and this user's account id.
pub struct NotebookHistory {
    /// `SyncEngine::logged_ops(notebook)` of the space engine, in log order.
    pub entries: Vec<(OpMeta, LocalOperation)>,
    /// The space's [`AttributionStore`] (fetched with `attribution::fetch`).
    pub attribution: AttributionStore,
    /// Members, transplanted ids and the space's creation time.
    pub ctx: AuthorContext,
    /// This user's account id.
    pub me: Option<String>,
}

/// Implemented by the sync worker: the data of one shared notebook, or `None` (not shared, not
/// synced yet, worker not running).
pub trait HistoryData: Send + Sync {
    /// The notebook's history data.
    fn notebook(&self, notebook: Uuid) -> Option<NotebookHistory>;
}

/// The [`HistorySource`] backed by the worker's [`HistoryData`]. Install it with [`install`].
pub struct AttributedHistory(pub Arc<dyn HistoryData>);

/// How an author key is shown in a notebook with context `ctx`; `me` is shown as "You".
pub(crate) fn author_view(key: &str, ctx: &AuthorContext, me: Option<&str>) -> AuthorView {
    let (account_id, display, kind) = attribution::describe_key(key, ctx);
    if kind == SyncAuthorKind::Member && me.is_some_and(|m| m == key) {
        return AuthorView {
            account_id,
            ..AuthorView::you()
        };
    }
    AuthorView {
        account_id,
        display,
        kind: match kind {
            SyncAuthorKind::Member => AuthorKind::Member,
            SyncAuthorKind::Former => AuthorKind::Former,
            SyncAuthorKind::Erased => AuthorKind::Erased,
            SyncAuthorKind::BeforeSharing => AuthorKind::BeforeSharing,
            SyncAuthorKind::Unknown => AuthorKind::Unknown,
            SyncAuthorKind::Unverified => AuthorKind::Unverified,
        },
    }
}

impl HistorySource for AttributedHistory {
    fn writers_since(&self, notebook: Uuid, page: Uuid, position: u64) -> Option<WritersSinceBase> {
        let h = self.0.notebook(notebook)?;
        Some(attribution::writers_since(
            &h.entries,
            &h.attribution,
            &h.ctx,
            page,
            position,
        ))
    }
    fn notebook_ops(&self, notebook: Uuid) -> Option<Vec<(LocalOperation, OpAttribution)>> {
        let h = self.0.notebook(notebook)?;
        Some(attribution::notebook_ops(
            &h.entries,
            &h.attribution,
            &h.ctx,
        ))
    }
    fn me(&self, notebook: Uuid) -> Option<String> {
        self.0.notebook(notebook)?.me
    }
    fn author(&self, notebook: Uuid, key: &str) -> AuthorView {
        match self.0.notebook(notebook) {
            Some(h) => author_view(key, &h.ctx, h.me.as_deref()),
            None => AuthorView::unknown(Some(key)),
        }
    }
    fn authors(&self, notebook: Uuid) -> Box<dyn Fn(&str) -> AuthorView + '_> {
        match self.0.notebook(notebook) {
            Some(h) => Box::new(move |k| author_view(k, &h.ctx, h.me.as_deref())),
            None => Box::new(|k| AuthorView::unknown(Some(k))),
        }
    }
}

static SOURCE: RwLock<Option<Arc<dyn HistorySource>>> = RwLock::new(None);

/// Makes the history commands use `source` (the worker calls this once it runs).
pub fn install(source: Arc<dyn HistorySource>) {
    *SOURCE.write().unwrap_or_else(|e| e.into_inner()) = Some(source);
}

/// The stand-in [`HistorySource`]: no attribution, every author unknown.
pub struct NoAttribution;

impl HistorySource for NoAttribution {
    fn writers_since(&self, _: Uuid, _: Uuid, _: u64) -> Option<WritersSinceBase> {
        None
    }
    fn notebook_ops(&self, _: Uuid) -> Option<Vec<(LocalOperation, OpAttribution)>> {
        None
    }
    fn me(&self, _: Uuid) -> Option<String> {
        None
    }
    fn author(&self, _: Uuid, account: &str) -> AuthorView {
        AuthorView::unknown(Some(account))
    }
}

/// The source in use: the installed one, else [`NoAttribution`].
fn source() -> Arc<dyn HistorySource> {
    SOURCE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(NoAttribution))
}

// ------------------------------------------------------------------- views --

/// How an author is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum AuthorKind {
    /// A current member (or the local user).
    Member,
    /// Someone who left or was removed.
    Former,
    /// An erased account.
    Erased,
    /// Made before the notebook was shared.
    BeforeSharing,
    /// Not known.
    Unknown,
    /// The server named someone the signature doesn't confirm.
    Unverified,
}

/// An author as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AuthorView {
    /// The account, when known.
    #[ts(optional)]
    pub account_id: Option<String>,
    /// Name to show; empty means a generic label.
    pub display: String,
    /// Which kind of author.
    pub kind: AuthorKind,
}

impl AuthorView {
    fn you() -> Self {
        Self {
            account_id: None,
            display: "You".into(),
            kind: AuthorKind::Member,
        }
    }
    fn unknown(account: Option<&str>) -> Self {
        Self {
            account_id: account.map(str::to_string),
            display: String::new(),
            kind: AuthorKind::Unknown,
        }
    }
}

/// What a plan item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum RestoreItemKind {
    /// Bring back an element (or, in a notebook plan, a page from the trash).
    Readd,
    /// Put a changed element back as it was.
    Revert,
    /// Added since; always kept (information only).
    KeptAddition,
    /// Add the old text as a new block next to the current one.
    RecoveredCopy,
    /// Restore the page template.
    Template,
    /// Restore the notebook title (notebook plans).
    Title,
    /// A page that existed then but can't be brought back (information only).
    Unavailable,
    /// Bring a page's content back as it was (notebook plans; `count` elements). In a personal
    /// notebook that is every deleted and changed element and the template; in a shared one the
    /// deleted elements and the current user's own changes.
    PageContent,
    /// Remove what was added to a page since then (personal notebooks only; `count` elements).
    RemoveAdditions,
    /// Move a page created since then to the trash, where it stays recoverable (personal
    /// notebooks only).
    TrashNewer,
    /// Put the pages back in the order they had then (personal notebooks only).
    PageOrder,
}

/// One item of a restore preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestoreItemView {
    /// Unique within the plan; send it back in the selection.
    pub item_id: String,
    /// What it does.
    pub kind: RestoreItemKind,
    /// The element, for element items.
    #[ts(optional)]
    pub element_id: Option<Uuid>,
    /// The page, for notebook plan items.
    #[ts(optional)]
    pub page_id: Option<Uuid>,
    /// Who changed it since the version, when known.
    pub authors: Vec<AuthorView>,
    /// Selected by default.
    pub selected: bool,
    /// Whether the user may change the selection.
    pub editable: bool,
    /// How many elements a `pageContent` or `removeAdditions` item affects.
    #[ts(optional)]
    pub count: Option<u32>,
}

/// A restore preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestorePreview {
    /// Pass to the commit.
    pub plan_id: String,
    /// The items. A page plan lists no `keptAddition` items: see `keptAdditions`.
    pub items: Vec<RestoreItemView>,
    /// How many elements (page plans) or pages (notebook plans) were added since the version and
    /// are kept. Page plans send only this count, since a large page can have thousands.
    pub kept_additions: u32,
}

/// One side of a comparison: a log position, a local revision, or the page now.
///
/// The UI sends a bare number (the version's log position when it has one, else its local
/// revision, as `page_history` lists them) or `"head"`. A bare number is tried as a log position
/// first and then as a revision; the object forms say which explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", untagged)]
#[ts(export)]
pub enum VersionRef {
    /// A version with a log position (from `page_history`).
    Position {
        /// The position.
        #[ts(type = "number")]
        position: u64,
    },
    /// A version this device recorded (from `page_history`).
    Revision {
        /// The revision.
        #[ts(type = "number")]
        revision: u64,
    },
    /// The page as it is now (`"head"`).
    Head(HeadMarker),
    /// A log position or, failing that, a local revision.
    Number(#[ts(type = "number")] u64),
}

/// The string `"head"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum HeadMarker {
    /// The page now.
    Head,
}

/// One run of a text diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DiffRun {
    /// `"equal"`, `"insert"` or `"delete"`.
    pub op: DiffOp,
    /// The text of the run.
    pub text: String,
}

/// A diff run's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum DiffOp {
    /// In both.
    Equal,
    /// Only in the newer version.
    Insert,
    /// Only in the older version.
    Delete,
}

/// The text change of one text block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TextDiff {
    /// The block.
    pub element_id: Uuid,
    /// Runs in order; deletions before insertions at the same place.
    pub diff: Vec<DiffRun>,
}

/// `version_compare`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VersionComparison {
    /// Elements only in `to`.
    pub added: Vec<Uuid>,
    /// Elements only in `from`.
    pub removed: Vec<Uuid>,
    /// Elements in both that differ.
    pub changed: Vec<Uuid>,
    /// Text source changes of changed text blocks.
    pub text: Vec<TextDiff>,
    /// Whether the template differs.
    pub template_changed: bool,
}

/// What an activity entry is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ActivityKindView {
    /// Strokes were drawn.
    StrokesAdded,
    /// Strokes (or other elements of unknown type) were erased.
    StrokesErased,
    /// Text was typed or edited (including text blocks added).
    TextEdited,
    /// Images were added.
    ImagesAdded,
    /// Elements were moved, resized or restyled.
    ElementsChanged,
    /// Pages were added.
    PageAdded,
    /// Pages were moved.
    PageMoved,
    /// Pages went to the trash.
    PageDeleted,
    /// Pages came back from the trash.
    PageRestored,
    /// A page template changed.
    Template,
    /// The notebook was renamed.
    Renamed,
    /// PDF pages were imported.
    PdfImported,
    /// The notebook was created, trashed, restored, shared or its cover or canvas changed.
    Notebook,
}

/// One activity entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ActivityEntryView {
    /// Stable within one listing.
    pub entry_id: String,
    /// Who.
    pub author: AuthorView,
    /// When made (the writing device's clock).
    #[ts(type = "number")]
    pub at: i64,
    /// When the server accepted it, if known.
    #[ts(optional, type = "number")]
    pub accepted_at: Option<i64>,
    /// Accepted much later than made.
    pub edited_offline: bool,
    /// The page, for page activity.
    #[ts(optional)]
    pub page_id: Option<Uuid>,
    /// What happened.
    pub kind: ActivityKindView,
    /// How many (strokes, pages, edits…).
    pub count: u32,
}

/// `history_activity`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ActivityPage {
    /// Entries, oldest first.
    pub entries: Vec<ActivityEntryView>,
    /// Pass back for the next page.
    #[ts(optional)]
    pub next_cursor: Option<String>,
}

// ------------------------------------------------------------------- plans --

enum Stored {
    Page {
        notebook: Uuid,
        plan: RestorePlan,
        /// The notebook's only writer is the user (additions may be removed).
        personal: bool,
    },
    Notebook(WholeRestore),
}

/// One page's content inside a notebook restore.
struct PagePart {
    plan: RestorePlan,
    /// The notebook's only writer is the user: the whole page may be made equal to then.
    personal: bool,
}

impl PagePart {
    /// What `pageContent:<page>` applies (without removing additions).
    fn content_selection(&self) -> RestoreSelection {
        if self.personal {
            RestoreSelection {
                remove_additions: BTreeSet::new(),
                ..self.plan.full_selection()
            }
        } else {
            self.plan.default_selection()
        }
    }
}

/// A previewed notebook restore: its page list and title, and each page's content.
struct WholeRestore {
    plan: NotebookRestorePlan,
    pages: Vec<PagePart>,
    /// The user is the notebook's only writer: newer pages may be trashed and the old order put
    /// back, so the default result is the notebook as it was.
    personal: bool,
    /// The page order then.
    then_order: Vec<Uuid>,
}

struct Entry {
    made: Instant,
    stored: Stored,
}

static PLANS: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);

/// Most previews held at once; storing another evicts the oldest. A preview can hold two whole
/// pages, so this bounds the memory they take.
pub const MAX_PLANS: usize = 8;

/// The plan map, with expired previews dropped (on every access).
fn plans() -> std::sync::MutexGuard<'static, Option<HashMap<String, Entry>>> {
    let mut g = PLANS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .retain(|_, e| e.made.elapsed() < PLAN_TTL);
    g
}

fn store(stored: Stored) -> String {
    let id = Uuid::new_v4().simple().to_string();
    let mut g = plans();
    let map = g.get_or_insert_with(HashMap::new);
    while map.len() >= MAX_PLANS {
        let Some(oldest) = map
            .iter()
            .min_by_key(|(_, e)| e.made)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        map.remove(&oldest);
    }
    map.insert(
        id.clone(),
        Entry {
            made: Instant::now(),
            stored,
        },
    );
    id
}

fn take(id: &str) -> R<Stored> {
    let mut g = plans();
    match g.as_mut().and_then(|m| m.remove(id)) {
        Some(e) if e.made.elapsed() < PLAN_TTL => Ok(e.stored),
        _ => Err(invalid(format!(
            "{CHANGED_SINCE_PREVIEW} the preview expired; preview again"
        ))),
    }
}

fn lock<'a>(state: &'a State<'_, AppState>) -> R<std::sync::MutexGuard<'a, Library>> {
    state
        .lib
        .lock()
        .map_err(|_| invalid("library lock poisoned"))
}

/// What a person whose role can't change the notebook (a viewer or commenter) is told.
pub const RESTORE_ROLE_REFUSAL: &str = "You can't restore versions in this notebook with your role";

/// The check every restore makes, with the library lock held: like
/// [`crate::sharing::require_writable`], but a role that can't edit gets
/// [`RESTORE_ROLE_REFUSAL`] instead of the general view-only wording.
fn require_restorable(lib: &Library, nb: Uuid) -> R<()> {
    let access = shared::notebook_access(lib.root(), nb).map_err(|e| invalid(e.to_string()))?;
    if access.read_only_reason() == Some("viewer") {
        return Err(invalid(RESTORE_ROLE_REFUSAL));
    }
    crate::sharing::require_writable(lib, nb)
}

fn is_shared(lib: &Library, nb: Uuid) -> R<bool> {
    Ok(matches!(
        shared::notebook_access(lib.root(), nb).map_err(|e| invalid(e.to_string()))?,
        Access::Shared { .. }
    ))
}

/// Guard for the older restore commands, which make a page equal a past version and so delete
/// later work: refused for shared notebooks, whose restores go through [`restore_preview`]. Call
/// it with the same library guard that performs the restore, so a notebook can't become shared
/// (a share marker being applied) between the check and the restore.
pub fn refuse_destructive_restore(lib: &Library, nb: Uuid) -> R<()> {
    if is_shared(lib, nb)? {
        return Err(invalid(
            "shared notebooks are restored with a preview, so nobody's later work is lost",
        ));
    }
    Ok(())
}

/// Writers since `base` when the local user is the only writer: every element that differs.
fn all_mine(base: &Page, head: &Page) -> WritersSinceBase {
    let base_by: HashMap<Uuid, &Element> = base.elements.iter().map(|e| (e.id(), e)).collect();
    let head_by: HashMap<Uuid, &Element> = head.elements.iter().map(|e| (e.id(), e)).collect();
    let me: BTreeSet<String> = [ME.to_string()].into();
    base_by
        .keys()
        .chain(head_by.keys())
        .filter(|id| base_by.get(id) != head_by.get(id))
        .map(|id| (*id, me.clone()))
        .collect()
}

fn version(state: &State<'_, AppState>, nb: Uuid, page: Uuid, v: VersionRef) -> R<Page> {
    match v {
        VersionRef::Revision { revision } => Ok(lock(state)?.page_version(nb, page, revision)?),
        VersionRef::Position { position } => crate::sync::page_at(state, nb, page, position)?
            .ok_or_else(|| invalid("that version isn't available")),
        VersionRef::Head(_) => Ok(lock(state)?.load_page(nb, page)?),
        VersionRef::Number(n) => {
            let logged = if state.sync.running() {
                crate::sync::page_at(state, nb, page, n)?
            } else {
                None
            };
            match logged {
                Some(p) => Ok(p),
                None => Ok(lock(state)?.page_version(nb, page, n)?),
            }
        }
    }
}

fn authors(
    resolve: &dyn Fn(&str) -> AuthorView,
    writers: &[String],
    shared_nb: bool,
) -> Vec<AuthorView> {
    writers
        .iter()
        .map(|w| {
            if !shared_nb && w == ME {
                AuthorView::you()
            } else {
                resolve(w)
            }
        })
        .collect()
}

/// The items of a page plan.
fn page_items(nb: Uuid, plan: &RestorePlan, shared_nb: bool) -> Vec<RestoreItemView> {
    let mut items = Vec::new();
    let src = source();
    let resolve = src.authors(nb);
    let item =
        |kind, id: Uuid, prefix: &str, writers: &[String], selected, editable| RestoreItemView {
            item_id: format!("{prefix}:{id}"),
            kind,
            element_id: Some(id),
            page_id: None,
            authors: authors(&*resolve, writers, shared_nb),
            selected,
            editable,
            count: None,
        };
    for r in &plan.re_add {
        items.push(item(
            RestoreItemKind::Readd,
            r.element.id(),
            "readd",
            &r.writers,
            r.default_selected,
            true,
        ));
    }
    for c in &plan.changed {
        items.push(item(
            RestoreItemKind::Revert,
            c.base.id(),
            "revert",
            &c.writers,
            c.default_selected,
            true,
        ));
        if c.recovered_copy {
            items.push(item(
                RestoreItemKind::RecoveredCopy,
                c.base.id(),
                "copy",
                &c.writers,
                false,
                true,
            ));
        }
    }
    // Kept additions are reported as a count (`keptAdditions`), not one item each.
    if plan.template.is_some() {
        items.push(RestoreItemView {
            item_id: "template".into(),
            kind: RestoreItemKind::Template,
            element_id: None,
            page_id: None,
            authors: Vec::new(),
            // in a personal notebook the default makes the page equal the version
            selected: !shared_nb,
            editable: true,
            count: None,
        });
    }
    // Personal notebooks only: removing what was added since, selected by default, so the default
    // result is the page exactly as it was. In a shared notebook additions are always kept.
    if !shared_nb && !plan.kept.is_empty() {
        items.push(RestoreItemView {
            item_id: "removeAdditions".into(),
            kind: RestoreItemKind::RemoveAdditions,
            element_id: None,
            page_id: None,
            authors: Vec::new(),
            selected: true,
            editable: true,
            count: Some(plan.kept.len() as u32),
        });
    }
    items
}

/// Turns selected item ids into a [`RestoreSelection`]. `removeAdditions` is only accepted for
/// personal notebooks.
fn page_selection(plan: &RestorePlan, ids: &[String], personal: bool) -> R<RestoreSelection> {
    let mut sel = RestoreSelection::default();
    for id in ids {
        if id == "template" {
            sel.template = true;
            continue;
        }
        if id == "removeAdditions" {
            if !personal {
                return Err(invalid("additions are never removed in a shared notebook"));
            }
            sel.remove_additions = plan.kept.iter().map(|k| k.element_id).collect();
            continue;
        }
        let (kind, el) = id
            .split_once(':')
            .and_then(|(k, e)| Some((k, Uuid::parse_str(e).ok()?)))
            .ok_or_else(|| invalid(format!("unknown item {id}")))?;
        match kind {
            "readd" => {
                sel.re_add.insert(el);
            }
            "revert" | "copy" => {
                let choice = if kind == "revert" {
                    ChangedChoice::Revert
                } else {
                    ChangedChoice::RecoveredCopy
                };
                if sel.changed.insert(el, choice).is_some_and(|c| c != choice) {
                    return Err(invalid(
                        "choose either reverting the block or a recovered copy, not both",
                    ));
                }
            }
            "kept" => {} // information only
            _ => return Err(invalid(format!("unknown item {id}"))),
        }
    }
    Ok(sel)
}

/// Plans restoring `base` onto the current page for the right writers.
fn plan_page(
    state: &State<'_, AppState>,
    nb: Uuid,
    page: Uuid,
    base: &Page,
    position: Option<u64>,
    element_ids: Option<&[Uuid]>,
) -> R<(RestorePlan, bool)> {
    let mut lib = lock(state)?;
    require_restorable(&lib, nb)?;
    let shared_nb = is_shared(&lib, nb)?;
    let head = lib.load_page(nb, page)?;
    drop(lib);
    let (writers, me) = if shared_nb {
        let w = position
            .and_then(|p| source().writers_since(nb, page, p))
            .unwrap_or_default();
        (w, source().me(nb).unwrap_or_default())
    } else {
        (all_mine(base, &head), ME.to_string())
    };
    let mut plan =
        RestorePlan::new(base, &head, &writers, &me).map_err(|e| invalid(e.to_string()))?;
    if let Some(only) = element_ids {
        plan.re_add.retain(|r| only.contains(&r.element.id()));
        plan.changed.retain(|c| only.contains(&c.base.id()));
        plan.kept.clear();
        plan.template = None;
    }
    Ok((plan, shared_nb))
}

// ---------------------------------------------------------------- commands --

/// Previews restoring a page to a past version (`position` from the log, or a local `revision`),
/// optionally only for `element_ids`. Changes nothing.
#[tauri::command(async)]
pub fn restore_preview(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    position: Option<u64>,
    revision: Option<u64>,
    element_ids: Option<Vec<Uuid>>,
) -> R<RestorePreview> {
    let v = match (position, revision) {
        (Some(position), _) => VersionRef::Position { position },
        (None, Some(revision)) => VersionRef::Revision { revision },
        (None, None) => return Err(invalid("no version given")),
    };
    let base = version(&state, notebook_id, page_id, v)?;
    let (plan, shared_nb) = plan_page(
        &state,
        notebook_id,
        page_id,
        &base,
        position,
        element_ids.as_deref(),
    )?;
    let items = page_items(notebook_id, &plan, shared_nb);
    let kept_additions = plan.kept.len() as u32;
    let plan_id = store(Stored::Page {
        notebook: notebook_id,
        plan,
        personal: !shared_nb,
    });
    Ok(RestorePreview {
        plan_id,
        items,
        kept_additions,
    })
}

/// Commits a page restore preview with the chosen items. Fails with a message starting with
/// `changed_since_preview:` when the page changed since the preview (or it expired).
#[tauri::command(async)]
pub fn restore_commit(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    plan_id: String,
    selected_item_ids: Vec<String>,
) -> R<()> {
    let Stored::Page {
        notebook,
        plan,
        personal,
    } = take(&plan_id)?
    else {
        return Err(invalid("not a page restore"));
    };
    if notebook != notebook_id {
        return Err(invalid("that preview is for another notebook"));
    }
    let sel = page_selection(&plan, &selected_item_ids, personal)?;
    state.sync.nudge();
    let mut lib = lock(&state)?;
    require_restorable(&lib, notebook_id)?;
    let head = lib.load_page(notebook_id, plan.page_id)?;
    check_precondition(&plan, &head)
        .map_err(|e| invalid(format!("{CHANGED_SINCE_PREVIEW} {e}")))?;
    if let Some(batch) = plan.to_batch(&sel).map_err(|e| invalid(e.to_string()))? {
        lib.apply_page_operations(notebook_id, plan.page_id, &batch)?;
        drop(lib);
        state.search.page_changed(notebook_id, plan.page_id);
    }
    Ok(())
}

/// Previews restoring a notebook's pages and title to time `at`, from the account's operation
/// log: trashed pages come back, newer pages are kept, the title is optional.
#[tauri::command(async)]
pub fn notebook_restore_preview(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    at: i64,
) -> R<RestorePreview> {
    let then = then_state(&state, notebook_id, at)?;
    let src = source();
    let mut lib = lock(&state)?;
    require_restorable(&lib, notebook_id)?;
    let shared_nb = is_shared(&lib, notebook_id)?;
    let (writers, me): (Box<dyn Fn(Uuid) -> Option<WritersSinceBase>>, String) = if shared_nb {
        let pos = then.position;
        let s2 = src.clone();
        (
            Box::new(move |page| pos.and_then(|p| s2.writers_since(notebook_id, page, p))),
            src.me(notebook_id).unwrap_or_default(),
        )
    } else {
        (Box::new(|_| None), ME.to_string())
    };
    let whole = build_whole(&mut lib, notebook_id, &then, !shared_nb, &*writers, &me)?;
    drop(lib);
    let items = whole_items(&whole);
    let kept_additions = whole.plan.kept_pages.len() as u32;
    let plan_id = store(Stored::Notebook(whole));
    Ok(RestorePreview {
        plan_id,
        items,
        kept_additions,
    })
}

/// A notebook as it was at a time: title, page order, each page's content (where known) and,
/// from an operation log, the log position of that time.
struct Then {
    title: String,
    page_order: Vec<Uuid>,
    pages: HashMap<Uuid, Page>,
    position: Option<u64>,
}

/// The notebook at time `at`. Shared notebooks come from their space's operation log. Others come
/// from the account's log when sync runs and reaches that far back, and otherwise from this
/// device's own history, so restoring works without sync.
fn then_state(state: &State<'_, AppState>, nb: Uuid, at: i64) -> R<Then> {
    let shared_nb = is_shared(&*lock(state)?, nb)?;
    let from_log = |then: Notebook, pages: Vec<Page>| {
        // the position of `at` in the log, as `notebook_at` counts it
        let position = source().notebook_ops(nb).map(|ops| {
            ops.iter()
                .rposition(|(o, _)| o.at <= at)
                .map_or(0, |i| i as u64 + 1)
        });
        Then {
            title: then.title,
            page_order: then.page_order,
            pages: pages.into_iter().map(|p| (p.id, p)).collect(),
            position,
        }
    };
    if shared_nb {
        let (then, pages) = crate::sync::notebook_at(state, nb, at)?
            .ok_or_else(|| invalid("that version isn't available on this device"))?;
        return Ok(from_log(then, pages));
    }
    if state.sync.running() {
        if let Ok(Some((then, pages))) = crate::sync::notebook_at(state, nb, at) {
            return Ok(from_log(then, pages));
        }
    }
    local_then(&*lock(state)?, nb, at)
}

/// The notebook at time `at` from this device's own history.
fn local_then(lib: &Library, nb: Uuid, at: i64) -> R<Then> {
    let (title, page_order) = lib.notebook_structure_at(nb, at)?;
    let mut pages = HashMap::new();
    for page in &page_order {
        let version = lib
            .page_history(nb, *page)?
            .into_iter()
            .take_while(|p| p.at <= at)
            .last();
        if let Some(v) = version {
            pages.insert(*page, lib.page_version(nb, *page, v.revision)?);
        }
    }
    Ok(Then {
        title,
        page_order,
        pages,
        position: None,
    })
}

/// Plans a notebook restore: the page list and title, and a content plan for every page that
/// existed then and is still on this device (live or in the trash). `personal` means the user is
/// the notebook's only writer; otherwise `writers` gives each page's writers since then.
fn build_whole(
    lib: &mut Library,
    nb: Uuid,
    then: &Then,
    personal: bool,
    writers: &dyn Fn(Uuid) -> Option<WritersSinceBase>,
    me: &str,
) -> R<WholeRestore> {
    let head = lib.notebook(nb)?.clone();
    let plan = NotebookRestorePlan::new(&then.title, &then.page_order, &head);
    let on_device: BTreeSet<Uuid> = head
        .page_order
        .iter()
        .copied()
        .chain(head.trashed_pages.iter().map(|t| t.id))
        .collect();
    let mut pages = Vec::new();
    for page in &then.page_order {
        let (Some(base), true) = (then.pages.get(page), on_device.contains(page)) else {
            continue;
        };
        let now = lib.load_page(nb, *page)?;
        let w = if personal {
            all_mine(base, &now)
        } else {
            writers(*page).unwrap_or_default()
        };
        let p = RestorePlan::new(base, &now, &w, me).map_err(|e| invalid(e.to_string()))?;
        if !p.is_empty() || (personal && !p.kept.is_empty()) {
            pages.push(PagePart { plan: p, personal });
        }
    }
    Ok(WholeRestore {
        plan,
        pages,
        personal,
        then_order: then.page_order.clone(),
    })
}

/// The items of a notebook restore: the page list items, then per page its content and (personal
/// notebooks) the removal of what was added since, as counts.
fn whole_items(whole: &WholeRestore) -> Vec<RestoreItemView> {
    let mut items = notebook_items(&whole.plan);
    if whole.personal {
        // newer pages: a choice to move them to the trash instead of a kept-page note
        items.retain(|i| i.kind != RestoreItemKind::KeptAddition);
        for p in &whole.plan.kept_pages {
            items.push(RestoreItemView {
                item_id: format!("trashNewer:{p}"),
                kind: RestoreItemKind::TrashNewer,
                element_id: None,
                page_id: Some(*p),
                authors: Vec::new(),
                selected: true,
                editable: true,
                count: None,
            });
        }
        if whole.then_order.len() > 1 {
            items.push(RestoreItemView {
                item_id: "pageOrder".into(),
                kind: RestoreItemKind::PageOrder,
                element_id: None,
                page_id: None,
                authors: Vec::new(),
                selected: true,
                editable: true,
                count: None,
            });
        }
    }
    for part in &whole.pages {
        let page = part.plan.page_id;
        let sel = part.content_selection();
        let count = (sel.re_add.len() + sel.changed.len() + usize::from(sel.template)) as u32;
        if count > 0 {
            items.push(RestoreItemView {
                item_id: format!("pageContent:{page}"),
                kind: RestoreItemKind::PageContent,
                element_id: None,
                page_id: Some(page),
                authors: Vec::new(),
                selected: true,
                editable: true,
                count: Some(count),
            });
        }
        if part.personal && !part.plan.kept.is_empty() {
            items.push(RestoreItemView {
                item_id: format!("removeAdditions:{page}"),
                kind: RestoreItemKind::RemoveAdditions,
                element_id: None,
                page_id: Some(page),
                authors: Vec::new(),
                selected: true,
                editable: true,
                count: Some(part.plan.kept.len() as u32),
            });
        }
    }
    items
}

/// The items of a notebook plan: pages to bring back, pages kept, pages gone, the title.
fn notebook_items(plan: &NotebookRestorePlan) -> Vec<RestoreItemView> {
    let mut items = Vec::new();
    let page_item = |kind, page: Uuid, selected, editable| RestoreItemView {
        item_id: format!("page:{page}"),
        kind,
        element_id: None,
        page_id: Some(page),
        authors: Vec::new(),
        selected,
        editable,
        count: None,
    };
    for p in &plan.restore_pages {
        items.push(page_item(RestoreItemKind::Readd, *p, true, true));
    }
    for p in &plan.kept_pages {
        items.push(page_item(RestoreItemKind::KeptAddition, *p, true, false));
    }
    for p in &plan.unavailable_pages {
        items.push(page_item(RestoreItemKind::Unavailable, *p, false, false));
    }
    if plan.title.is_some() {
        items.push(RestoreItemView {
            item_id: "title".into(),
            kind: RestoreItemKind::Title,
            element_id: None,
            page_id: None,
            authors: Vec::new(),
            selected: false,
            editable: true,
            count: None,
        });
    }
    items
}

/// Commits a notebook restore preview. Same staleness rule as [`restore_commit`].
#[tauri::command(async)]
pub fn notebook_restore_commit(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    plan_id: String,
    selected: Vec<String>,
) -> R<()> {
    let Stored::Notebook(whole) = take(&plan_id)? else {
        return Err(invalid("not a notebook restore"));
    };
    if whole.plan.notebook_id != notebook_id {
        return Err(invalid("that preview is for another notebook"));
    }
    state.sync.nudge();
    let mut lib = lock(&state)?;
    require_restorable(&lib, notebook_id)?;
    apply_whole(&mut lib, &whole, &selected)?;
    drop(lib);
    state.search.notebook_changed(notebook_id);
    Ok(())
}

/// Applies a notebook restore with the chosen item ids: re-checks the notebook and every page
/// with content to restore against the preview (nothing is written if any changed), then brings
/// pages back from the trash, renames, moves newer pages to the trash (personal notebooks),
/// applies each page's batch, and puts the old page order back (personal notebooks).
fn apply_whole(lib: &mut Library, whole: &WholeRestore, selected: &[String]) -> R<()> {
    let nb = whole.plan.notebook_id;
    let mut content: BTreeSet<Uuid> = BTreeSet::new();
    let mut removals: BTreeSet<Uuid> = BTreeSet::new();
    let mut trash: Vec<Uuid> = Vec::new();
    let mut reorder = false;
    let mut rest = Vec::new();
    let personal_only = || -> R<()> {
        if whole.personal {
            Ok(())
        } else {
            Err(invalid(
                "a shared notebook keeps newer pages and its current order",
            ))
        }
    };
    for id in selected {
        if id == "pageOrder" {
            personal_only()?;
            reorder = true;
            continue;
        }
        if let Some(p) = id.strip_prefix("trashNewer:") {
            personal_only()?;
            let p = Uuid::parse_str(p)
                .ok()
                .filter(|p| whole.plan.kept_pages.contains(p))
                .ok_or_else(|| invalid(format!("unknown item {id}")))?;
            trash.push(p);
            continue;
        }
        let page = |prefix: &str| {
            id.strip_prefix(prefix)
                .and_then(|p| Uuid::parse_str(p).ok())
                .filter(|p| whole.pages.iter().any(|part| part.plan.page_id == *p))
        };
        if let Some(p) = page("pageContent:") {
            content.insert(p);
        } else if let Some(p) = page("removeAdditions:") {
            removals.insert(p);
        } else if id.starts_with("pageContent:") || id.starts_with("removeAdditions:") {
            return Err(invalid(format!("unknown item {id}")));
        } else {
            rest.push(id.clone());
        }
    }
    let sel = notebook_selection(&whole.plan, &rest)?;
    let steps = whole.plan.steps(&sel).map_err(|e| invalid(e.to_string()))?;
    let mut batches = Vec::new();
    for part in &whole.pages {
        let page = part.plan.page_id;
        let mut s = if content.contains(&page) {
            part.content_selection()
        } else {
            RestoreSelection::default()
        };
        if removals.contains(&page) {
            if !part.personal {
                return Err(invalid("additions are never removed in a shared notebook"));
            }
            s.remove_additions = part.plan.kept.iter().map(|k| k.element_id).collect();
        }
        if let Some(b) = part.plan.to_batch(&s).map_err(|e| invalid(e.to_string()))? {
            batches.push((part, b));
        }
    }
    // every precondition first, so a stale preview changes nothing
    check_notebook_precondition(&whole.plan, lib.notebook(nb)?)
        .map_err(|e| invalid(format!("{CHANGED_SINCE_PREVIEW} {e}")))?;
    for (part, _) in &batches {
        let now = lib.load_page(nb, part.plan.page_id)?;
        check_precondition(&part.plan, &now)
            .map_err(|e| invalid(format!("{CHANGED_SINCE_PREVIEW} {e}")))?;
    }
    for step in steps {
        match step {
            NotebookRestoreStep::RestoreTrashedPage { page_id } => {
                lib.restore_page(nb, page_id)?;
            }
            NotebookRestoreStep::Rename { title } => {
                lib.rename_notebook(nb, &title)?;
            }
        }
    }
    for p in &trash {
        lib.delete_page(nb, *p)?;
    }
    for (part, batch) in batches {
        lib.apply_page_operations(nb, part.plan.page_id, &batch)?;
    }
    if reorder {
        // the pages of then in their old order, then any newer page that stayed, as it is now
        let live = lib.notebook(nb)?.page_order.clone();
        let mut order: Vec<Uuid> = whole
            .then_order
            .iter()
            .filter(|p| live.contains(p))
            .copied()
            .collect();
        let newer: Vec<Uuid> = live
            .iter()
            .filter(|p| !order.contains(p))
            .copied()
            .collect();
        order.extend(newer);
        if order != live {
            lib.update_page_order(nb, order)?;
        }
    }
    Ok(())
}

/// Turns selected item ids of a notebook plan into its selection.
fn notebook_selection(
    plan: &NotebookRestorePlan,
    selected: &[String],
) -> R<NotebookRestoreSelection> {
    let mut sel = NotebookRestoreSelection::default();
    for id in selected {
        if id == "title" {
            sel.title = true;
        } else if let Some(p) = id
            .strip_prefix("page:")
            .and_then(|p| Uuid::parse_str(p).ok())
        {
            // kept and unavailable pages are information only
            if plan.restore_pages.contains(&p) {
                sel.pages.insert(p);
            }
        } else {
            return Err(invalid(format!("unknown item {id}")));
        }
    }
    Ok(sel)
}

/// Compares two versions of a page (`to` may be `"head"`).
#[tauri::command(async)]
pub fn version_compare(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    from: VersionRef,
    to: VersionRef,
) -> R<VersionComparison> {
    let a = version(&state, notebook_id, page_id, from)?;
    let b = version(&state, notebook_id, page_id, to)?;
    Ok(compare(&a, &b))
}

/// The notebook's activity, oldest first, [`ACTIVITY_PAGE`] entries at a time. Empty when no
/// attributed operation log is available.
#[tauri::command(async)]
pub fn history_activity(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    cursor: Option<String>,
) -> R<ActivityPage> {
    let shared_nb = is_shared(&*lock(&state)?, notebook_id)?;
    let Some(ops) = source().notebook_ops(notebook_id) else {
        return Ok(ActivityPage {
            entries: Vec::new(),
            next_cursor: None,
        });
    };
    let entries = activity_views(notebook_id, &ops, shared_nb);
    let start: usize = match cursor {
        Some(c) => c.parse().map_err(|_| invalid("bad cursor"))?,
        None => 0,
    };
    let end = (start + ACTIVITY_PAGE).min(entries.len());
    Ok(ActivityPage {
        entries: entries.get(start..end).unwrap_or_default().to_vec(),
        next_cursor: (end < entries.len()).then(|| end.to_string()),
    })
}

// ------------------------------------------------------------------ helpers --

fn activity_views(
    nb: Uuid,
    ops: &[(LocalOperation, OpAttribution)],
    shared_nb: bool,
) -> Vec<ActivityEntryView> {
    let mut out = Vec::new();
    let src = source();
    let resolve = src.authors(nb);
    for (i, e) in activity::summarize(ops, ActivityOptions::default())
        .into_iter()
        .enumerate()
    {
        let author = match &e.author {
            Some(a) if !shared_nb && a == ME => AuthorView::you(),
            Some(a) => resolve(a),
            None => AuthorView::unknown(None),
        };
        let rows: Vec<(ActivityKindView, u32)> = match e.kind {
            ActivityKind::ElementsAdded {
                strokes,
                texts,
                images,
            } => [
                (ActivityKindView::StrokesAdded, strokes),
                (ActivityKindView::TextEdited, texts),
                (ActivityKindView::ImagesAdded, images),
            ]
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .collect(),
            ActivityKind::ElementsRemoved {
                strokes,
                texts,
                images,
                unknown,
            } => vec![(
                ActivityKindView::StrokesErased,
                strokes + texts + images + unknown,
            )],
            ActivityKind::TextEdited { edits } => vec![(ActivityKindView::TextEdited, edits)],
            ActivityKind::ElementsChanged { count } => {
                vec![(ActivityKindView::ElementsChanged, count)]
            }
            ActivityKind::PageAdded { count } => vec![(ActivityKindView::PageAdded, count)],
            ActivityKind::PdfPageImported { count } => {
                vec![(ActivityKindView::PdfImported, count)]
            }
            ActivityKind::PageMoved { count } => vec![(ActivityKindView::PageMoved, count)],
            ActivityKind::PageDeleted { count } => vec![(ActivityKindView::PageDeleted, count)],
            ActivityKind::PageRestored { count } => {
                vec![(ActivityKindView::PageRestored, count)]
            }
            ActivityKind::TemplateChanged => vec![(ActivityKindView::Template, 1)],
            ActivityKind::Renamed { .. } => vec![(ActivityKindView::Renamed, 1)],
            _ => vec![(ActivityKindView::Notebook, 1)],
        };
        for (j, (kind, count)) in rows.into_iter().enumerate() {
            out.push(ActivityEntryView {
                entry_id: format!("{i}.{j}"),
                author: author.clone(),
                at: e.at,
                accepted_at: e.accepted_at,
                edited_offline: e.edited_offline,
                page_id: e.page_id,
                kind,
                count,
            });
        }
    }
    out
}

/// What differs from `a` to `b`.
fn compare(a: &Page, b: &Page) -> VersionComparison {
    let a_by: HashMap<Uuid, &Element> = a.elements.iter().map(|e| (e.id(), e)).collect();
    let b_by: HashMap<Uuid, &Element> = b.elements.iter().map(|e| (e.id(), e)).collect();
    let mut out = VersionComparison {
        added: b
            .elements
            .iter()
            .map(Element::id)
            .filter(|id| !a_by.contains_key(id))
            .collect(),
        removed: a
            .elements
            .iter()
            .map(Element::id)
            .filter(|id| !b_by.contains_key(id))
            .collect(),
        changed: Vec::new(),
        text: Vec::new(),
        template_changed: a.template != b.template,
    };
    for e in &a.elements {
        let Some(n) = b_by.get(&e.id()) else {
            continue;
        };
        if *n == e {
            continue;
        }
        out.changed.push(e.id());
        if let (Element::Text(x), Element::Text(y)) = (e, *n) {
            if x.text != y.text {
                out.text.push(TextDiff {
                    element_id: e.id(),
                    diff: text_diff(&x.text, &y.text),
                });
            }
        }
    }
    out
}

/// Largest `old × new` character product diffed character by character; beyond it lines are
/// compared, and beyond that the texts are shown as one deletion and one insertion.
const DIFF_CELLS: usize = 4_000_000;

/// A minimal-edit diff of `old` into `new` (longest common subsequence), by characters when
/// small enough, else by lines.
fn text_diff(old: &str, new: &str) -> Vec<DiffRun> {
    let (oc, nc): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    if oc.len().saturating_mul(nc.len()) <= DIFF_CELLS {
        let units = lcs(&oc, &nc);
        return merge(units.into_iter().map(|(op, c)| (op, c.to_string())));
    }
    let (ol, nl): (Vec<&str>, Vec<&str>) = (
        old.split_inclusive('\n').collect(),
        new.split_inclusive('\n').collect(),
    );
    if ol.len().saturating_mul(nl.len()) <= DIFF_CELLS {
        let units = lcs(&ol, &nl);
        return merge(units.into_iter().map(|(op, l)| (op, l.to_string())));
    }
    merge(
        [
            (DiffOp::Delete, old.to_string()),
            (DiffOp::Insert, new.to_string()),
        ]
        .into_iter(),
    )
}

fn lcs<T: PartialEq + Copy>(a: &[T], b: &[T]) -> Vec<(DiffOp, T)> {
    let (n, m) = (a.len(), b.len());
    let mut t = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[at(i, j)] = if a[i] == b[j] {
                t[at(i + 1, j + 1)] + 1
            } else {
                t[at(i + 1, j)].max(t[at(i, j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(n + m);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            out.push((DiffOp::Equal, a[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || t[at(i + 1, j)] >= t[at(i, j + 1)]) {
            out.push((DiffOp::Delete, a[i]));
            i += 1;
        } else {
            out.push((DiffOp::Insert, b[j]));
            j += 1;
        }
    }
    out
}

fn merge(units: impl Iterator<Item = (DiffOp, String)>) -> Vec<DiffRun> {
    let mut out: Vec<DiffRun> = Vec::new();
    for (op, s) in units {
        if s.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(r) if r.op == op => r.text.push_str(&s),
            _ => out.push(DiffRun { op, text: s }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(runs: &[DiffRun], keep: &[DiffOp]) -> String {
        runs.iter()
            .filter(|r| keep.contains(&r.op))
            .map(|r| r.text.as_str())
            .collect()
    }

    #[test]
    fn text_diff_reconstructs_both_sides() {
        for (a, b) in [
            ("hello world", "hello brave world"),
            ("", "new"),
            ("old", ""),
            ("same", "same"),
            ("emoji 👋 here", "emoji 🎉 here"),
            ("line one\nline two\n", "line one\nline 2\nline three\n"),
        ] {
            let d = text_diff(a, b);
            assert_eq!(joined(&d, &[DiffOp::Equal, DiffOp::Delete]), a);
            assert_eq!(joined(&d, &[DiffOp::Equal, DiffOp::Insert]), b);
            assert!(d.windows(2).all(|w| w[0].op != w[1].op), "runs are merged");
        }
        let d = text_diff("hello world", "hello brave world");
        assert_eq!(
            d,
            vec![
                DiffRun {
                    op: DiffOp::Equal,
                    text: "hello ".into()
                },
                DiffRun {
                    op: DiffOp::Insert,
                    text: "brave ".into()
                },
                DiffRun {
                    op: DiffOp::Equal,
                    text: "world".into()
                },
            ]
        );
    }

    #[test]
    fn huge_texts_fall_back_to_lines_then_whole() {
        let a = "x".repeat(3000);
        let b = "y".repeat(3000);
        let d = text_diff(&a, &b);
        assert_eq!(joined(&d, &[DiffOp::Equal, DiffOp::Delete]), a);
        assert_eq!(joined(&d, &[DiffOp::Equal, DiffOp::Insert]), b);
    }

    #[test]
    fn version_refs_parse_from_the_ui_shapes() {
        assert_eq!(
            serde_json::from_str::<VersionRef>(r#"{"position":4}"#).unwrap(),
            VersionRef::Position { position: 4 }
        );
        assert_eq!(
            serde_json::from_str::<VersionRef>(r#"{"revision":2}"#).unwrap(),
            VersionRef::Revision { revision: 2 }
        );
        assert_eq!(
            serde_json::from_str::<VersionRef>(r#""head""#).unwrap(),
            VersionRef::Head(HeadMarker::Head)
        );
        assert_eq!(
            serde_json::from_str::<VersionRef>("7").unwrap(),
            VersionRef::Number(7)
        );
    }

    fn page(elements: Vec<Element>) -> Page {
        let mut p = Page::new(
            Uuid::from_u128(1),
            penroam_core::model::Paper::Letter,
            penroam_core::model::Size {
                width: 600.0,
                height: 800.0,
            },
            Default::default(),
            0,
        );
        p.elements = elements;
        p
    }

    fn text(id: u128, body: &str) -> Element {
        serde_json::from_value(serde_json::json!({
            "type": "text", "id": Uuid::from_u128(id), "z": 1, "x": 0.0, "y": 0.0,
            "width": 100.0, "height": 20.0, "text": body, "fontSize": 12.0, "color": "#000000"
        }))
        .unwrap()
    }

    #[test]
    fn personal_plans_select_my_changes_and_items_round_trip() {
        let (a, b, c) = (text(10, "keep"), text(11, "old"), text(12, "gone"));
        let base = page(vec![a.clone(), b.clone(), c.clone()]);
        let mut head = page(vec![a.clone(), text(11, "new"), text(13, "added")]);
        head.revision = 5;
        let plan = RestorePlan::new(&base, &head, &all_mine(&base, &head), ME).unwrap();
        let items = page_items(Uuid::nil(), &plan, false);
        let by = |id: &str| items.iter().find(|i| i.item_id == id).unwrap();
        let readd = by(&format!("readd:{}", c.id()));
        assert!(readd.selected && readd.editable);
        assert_eq!(readd.authors, vec![AuthorView::you()]);
        assert!(by(&format!("revert:{}", b.id())).selected, "my own change");
        assert!(!items
            .iter()
            .any(|i| i.kind == RestoreItemKind::RecoveredCopy));
        // Kept additions are a count on the preview, not items.
        assert!(!items
            .iter()
            .any(|i| i.kind == RestoreItemKind::KeptAddition));
        assert_eq!(plan.kept.len(), 1);
        // The default selection round-trips into the planner's selection.
        let ids: Vec<String> = items
            .iter()
            .filter(|i| i.selected)
            .map(|i| i.item_id.clone())
            .collect();
        let sel = page_selection(&plan, &ids, true).unwrap();
        // personal: the defaults make the page equal the version, additions removed
        assert_eq!(sel, plan.full_selection());
        let remove = by("removeAdditions");
        assert_eq!(
            (remove.kind, remove.count, remove.selected),
            (RestoreItemKind::RemoveAdditions, Some(1), true)
        );
        let mut after = head.clone();
        penroam_core::ops::apply_batch(&mut after, &plan.to_batch(&sel).unwrap().unwrap(), 1)
            .unwrap();
        let ids_of = |p: &Page| p.elements.iter().map(Element::id).collect::<BTreeSet<_>>();
        assert_eq!(ids_of(&after), ids_of(&base));
        assert!(after.elements.contains(&b));
        assert!(page_selection(&plan, &["bogus".into()], true).is_err());
    }

    #[test]
    fn shared_plans_without_attribution_offer_copies_and_select_only_readds() {
        let b = text(11, "old");
        let gone = text(12, "gone");
        let base = page(vec![b.clone(), gone.clone()]);
        let head = page(vec![text(11, "bob's")]);
        let plan = RestorePlan::new(&base, &head, &WritersSinceBase::new(), "").unwrap();
        let items = page_items(Uuid::nil(), &plan, true);
        let selected: Vec<&str> = items
            .iter()
            .filter(|i| i.selected)
            .map(|i| i.item_id.as_str())
            .collect();
        assert_eq!(selected, vec![format!("readd:{}", gone.id()).as_str()]);
        assert!(items
            .iter()
            .any(|i| i.kind == RestoreItemKind::RecoveredCopy && i.editable));
        // Both a revert and a copy of the same block is refused.
        let both = vec![format!("revert:{}", b.id()), format!("copy:{}", b.id())];
        assert!(page_selection(&plan, &both, false).is_err());
        let copy = page_selection(&plan, &[format!("copy:{}", b.id())], false).unwrap();
        assert_eq!(copy.changed[&b.id()], ChangedChoice::RecoveredCopy);
        // Additions are kept in a shared notebook: no item, and asking anyway is refused.
        let mut head2 = head.clone();
        head2.elements.push(text(14, "bob's new"));
        let plan2 = RestorePlan::new(&base, &head2, &WritersSinceBase::new(), "").unwrap();
        assert_eq!(plan2.kept.len(), 1);
        let items2 = page_items(Uuid::nil(), &plan2, true);
        assert!(!items2
            .iter()
            .any(|i| i.kind == RestoreItemKind::RemoveAdditions));
        assert!(page_selection(&plan2, &["removeAdditions".into()], false).is_err());
    }

    #[test]
    fn compare_lists_changes_and_text_diffs() {
        let a = page(vec![text(1, "one"), text(2, "two")]);
        let b = page(vec![text(1, "one!"), text(3, "three")]);
        let c = compare(&a, &b);
        assert_eq!(c.added, vec![Uuid::from_u128(3)]);
        assert_eq!(c.removed, vec![Uuid::from_u128(2)]);
        assert_eq!(c.changed, vec![Uuid::from_u128(1)]);
        assert_eq!(c.text.len(), 1);
        assert!(!c.template_changed);
    }

    #[test]
    fn plans_expire_and_are_taken_once() {
        let _serial = PLAN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let id = store(Stored::Notebook(WholeRestore {
            plan: NotebookRestorePlan::new(
                "t",
                &[],
                &serde_json::from_value(serde_json::json!({
                    "schemaVersion": 2, "id": Uuid::nil(), "title": "t", "createdAt": 0,
                    "updatedAt": 0, "pageOrder": [], "defaults": {"paper": "letter", "template":
                    {"kind": "blank", "spacing": 24.0}}, "cover": "#000000"
                }))
                .unwrap(),
            ),
            pages: Vec::new(),
            personal: false,
            then_order: Vec::new(),
        }));
        assert!(take(&id).is_ok());
        let e = take(&id).err().unwrap();
        assert!(e.message.starts_with(CHANGED_SINCE_PREVIEW));
    }

    struct Fake(tempfile::TempDir);

    impl HistoryData for Fake {
        fn notebook(&self, _: Uuid) -> Option<NotebookHistory> {
            let mut store = AttributionStore::open(self.0.path()).unwrap();
            let rec = |op: u128, account: &str, dev: u128| attribution::AttributionRecord {
                stream: attribution::Stream::Ops,
                seq: op as u64,
                op: Uuid::from_u128(op).hyphenated().to_string(),
                account: account.into(),
                device: Uuid::from_u128(dev).hyphenated().to_string(),
                accepted_at_ms: 100,
                pin_mismatch: false,
            };
            store
                .append(vec![
                    rec(1, "acct-me", 7),
                    rec(2, "acct-bob", 8),
                    rec(3, "acct-bob", 9),
                ])
                .unwrap();
            let op = |id: u128, dev: u128, el: u128| {
                let body = penroam_core::operation::DocumentOperation::ElementsAdded {
                    notebook_id: Uuid::nil(),
                    page_id: Uuid::from_u128(1),
                    elements: vec![text(el, "x")],
                };
                (
                    OpMeta {
                        origin: penroam_sync::store::Origin::Remote,
                        nb: Uuid::nil(),
                        op: Uuid::from_u128(id),
                        dev: Uuid::from_u128(dev),
                        seq: id as u64,
                        ct: None,
                        lib: None,
                        epoch: None,
                    },
                    LocalOperation {
                        id: Uuid::from_u128(id),
                        device_id: Uuid::from_u128(dev),
                        local_sequence: id as u64,
                        schema_version: 1,
                        causal_dependencies: vec![],
                        at: 50,
                        body,
                    },
                )
            };
            let members = penroam_sync_protocol::MemberList {
                members: vec![],
                invitations: vec![],
            };
            let mut ctx = AuthorContext::for_space(&members, None, Default::default());
            for (a, d) in [
                ("acct-me", "me@example.org"),
                ("acct-bob", "bob@example.org"),
            ] {
                ctx.members.insert(
                    a.into(),
                    attribution::MemberInfo {
                        display: d.into(),
                        active: true,
                    },
                );
            }
            Some(NotebookHistory {
                // op 3 claims bob but was signed by device 10: unverified
                entries: vec![op(1, 7, 101), op(2, 8, 102), op(3, 10, 103)],
                attribution: store,
                ctx,
                me: Some("acct-me".into()),
            })
        }
    }

    #[test]
    fn attributed_history_feeds_the_planner_and_the_activity_list() {
        let src = AttributedHistory(Arc::new(Fake(tempfile::tempdir().unwrap())));
        let w = src
            .writers_since(Uuid::nil(), Uuid::from_u128(1), 0)
            .unwrap();
        assert_eq!(w[&Uuid::from_u128(101)], ["acct-me".to_string()].into());
        assert_eq!(w[&Uuid::from_u128(102)], ["acct-bob".to_string()].into());
        assert!(w[&Uuid::from_u128(103)]
            .iter()
            .all(|k| k.starts_with(attribution::UNVERIFIED_PREFIX)));
        assert_eq!(src.me(Uuid::nil()).as_deref(), Some("acct-me"));
        let resolve = src.authors(Uuid::nil());
        assert_eq!(resolve("acct-me").display, "You");
        let bob = resolve("acct-bob");
        assert_eq!(
            (bob.display.as_str(), bob.kind),
            ("bob@example.org", AuthorKind::Member)
        );
        assert_eq!(resolve("~unverified:acct-bob").kind, AuthorKind::Unverified);
        assert_eq!(resolve("~before-sharing").kind, AuthorKind::BeforeSharing);
        assert_eq!(resolve("acct-gone").kind, AuthorKind::Unknown);
        let ops = src.notebook_ops(Uuid::nil()).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].1.accepted_at, Some(100));
    }

    /// The plan store is global; tests that fill it run one at a time.
    static PLAN_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn at_most_max_plans_are_held_and_the_oldest_go_first() {
        let _serial = PLAN_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let nb_plan = || {
            Stored::Notebook(WholeRestore {
                plan: NotebookRestorePlan::new(
                    "t",
                    &[],
                    &serde_json::from_value(serde_json::json!({
                        "schemaVersion": 2, "id": Uuid::nil(), "title": "t", "createdAt": 0,
                        "updatedAt": 0, "pageOrder": [], "defaults": {"paper": "letter", "template":
                        {"kind": "blank", "spacing": 24.0}}, "cover": "#000000"
                    }))
                    .unwrap(),
                ),
                pages: Vec::new(),
                personal: false,
                then_order: Vec::new(),
            })
        };
        let ids: Vec<String> = (0..MAX_PLANS + 3).map(|_| store(nb_plan())).collect();
        let held = plans().as_ref().map_or(0, |m| m.len());
        assert!(held <= MAX_PLANS, "{held}");
        // the newest are still there, the first ones were evicted
        assert!(take(ids.last().unwrap()).is_ok());
        assert!(take(&ids[0]).is_err());
    }

    fn ink(n: u128) -> Element {
        serde_json::from_value(serde_json::json!({
            "type": "ink", "id": Uuid::from_u128(n), "z": n as i64, "tool": "pen",
            "color": "#000000", "width": 2.0, "source": "test",
            "points": [{"x": 1.0, "y": n as f64, "pressure": 0.5}]
        }))
        .unwrap()
    }

    fn edit_page(lib: &mut Library, nb: Uuid, page: Uuid, kind: penroam_core::ops::OpKind) {
        let rev = lib.load_page(nb, page).unwrap().revision;
        lib.apply_page_operations(
            nb,
            page,
            &penroam_core::ops::OperationBatch {
                batch_id: Uuid::new_v4(),
                base_revision: rev,
                ops: vec![penroam_core::ops::PageOperation {
                    op_id: Uuid::new_v4(),
                    kind,
                }],
            },
        )
        .unwrap();
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// A notebook with two pages of content at time `at`, then changes after it: a page and its
    /// content go to the trash, strokes are erased, changed and added, a page is added and the
    /// title changes. Returns (lib, dir, notebook, p1, p2, p3, at).
    fn timeline() -> (Library, tempfile::TempDir, Uuid, Uuid, Uuid, Uuid, i64) {
        use penroam_core::ops::OpKind;
        let d = tempfile::tempdir().unwrap();
        let mut lib = Library::open(d.path().join("lib")).unwrap();
        let (nb, p1) = lib
            .create_notebook("First", penroam_core::model::PageDefaults::default(), None)
            .unwrap();
        let (nb, p1) = (nb.id, p1.id);
        let p2 = lib.create_page(nb, None, None, None).unwrap().1.id;
        edit_page(
            &mut lib,
            nb,
            p1,
            OpKind::AddElements {
                elements: vec![ink(1), ink(2), text(3, "then")],
            },
        );
        edit_page(
            &mut lib,
            nb,
            p2,
            OpKind::AddElements {
                elements: vec![ink(10)],
            },
        );
        lib.rename_notebook(nb, "Second").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let at = now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        edit_page(
            &mut lib,
            nb,
            p1,
            OpKind::RemoveElements {
                ids: vec![Uuid::from_u128(1)],
            },
        );
        edit_page(
            &mut lib,
            nb,
            p1,
            OpKind::ReplaceElements {
                elements: vec![text(3, "now")],
            },
        );
        edit_page(
            &mut lib,
            nb,
            p1,
            OpKind::AddElements {
                elements: vec![ink(4)],
            },
        );
        edit_page(
            &mut lib,
            nb,
            p2,
            OpKind::AddElements {
                elements: vec![ink(11)],
            },
        );
        lib.delete_page(nb, p2).unwrap();
        let p3 = lib.create_page(nb, None, None, None).unwrap().1.id;
        lib.rename_notebook(nb, "Third").unwrap();
        (lib, d, nb, p1, p2, p3, at)
    }

    fn ids_of(lib: &mut Library, nb: Uuid, page: Uuid) -> BTreeSet<Uuid> {
        lib.load_page(nb, page)
            .unwrap()
            .elements
            .iter()
            .map(Element::id)
            .collect()
    }

    #[test]
    fn a_personal_notebook_gets_fully_back_to_its_old_content_without_sync() {
        let (mut lib, _d, nb, p1, p2, p3, at) = timeline();
        let then = local_then(&lib, nb, at).unwrap();
        assert_eq!(
            (then.title.as_str(), then.page_order.clone()),
            ("Second", vec![p1, p2])
        );
        let whole = build_whole(&mut lib, nb, &then, true, &|_| None, ME).unwrap();
        let items = whole_items(&whole);
        let item = |id: String| items.iter().find(|i| i.item_id == id).unwrap().clone();
        let c1 = item(format!("pageContent:{p1}"));
        assert_eq!(
            (c1.kind, c1.count, c1.selected),
            (RestoreItemKind::PageContent, Some(2), true)
        );
        let r1 = item(format!("removeAdditions:{p1}"));
        assert_eq!(
            (r1.kind, r1.count, r1.selected),
            (RestoreItemKind::RemoveAdditions, Some(1), true)
        );
        assert_eq!(item(format!("removeAdditions:{p2}")).count, Some(1));
        assert_eq!(
            item(format!("trashNewer:{p3}")).kind,
            RestoreItemKind::TrashNewer
        );
        assert!(item(format!("trashNewer:{p3}")).selected);
        assert!(item("pageOrder".into()).selected);
        assert!(!items
            .iter()
            .any(|i| i.kind == RestoreItemKind::KeptAddition));
        // the dialog's defaults, plus the title
        let mut chosen: Vec<String> = items
            .iter()
            .filter(|i| i.selected && i.editable)
            .map(|i| i.item_id.clone())
            .collect();
        chosen.push("title".into());
        apply_whole(&mut lib, &whole, &chosen).unwrap();
        // Exactly the old content, later strokes removed; the newer page went to the trash.
        assert_eq!(
            ids_of(&mut lib, nb, p1),
            [1, 2, 3].map(Uuid::from_u128).into()
        );
        assert_eq!(ids_of(&mut lib, nb, p2), [10].map(Uuid::from_u128).into());
        let page1 = lib.load_page(nb, p1).unwrap();
        assert!(page1.elements.contains(&text(3, "then")));
        let after = lib.notebook(nb).unwrap();
        assert_eq!(after.title, "Second");
        assert_eq!(after.page_order, vec![p1, p2], "the notebook as it was");
        assert_eq!(
            after.trashed_pages.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![p3],
            "recoverable from the trash"
        );
        // A stale preview changes nothing.
        let e = apply_whole(&mut lib, &whole, &chosen).unwrap_err();
        assert!(e.message.starts_with(CHANGED_SINCE_PREVIEW));
    }

    #[test]
    fn personal_page_order_comes_back_and_kept_newer_pages_go_last() {
        let d = tempfile::tempdir().unwrap();
        let mut lib = Library::open(d.path().join("lib")).unwrap();
        let (nb, p1) = lib
            .create_notebook("Order", penroam_core::model::PageDefaults::default(), None)
            .unwrap();
        let (nb, p1) = (nb.id, p1.id);
        let p2 = lib.create_page(nb, None, None, None).unwrap().1.id;
        let p3 = lib.create_page(nb, None, None, None).unwrap().1.id;
        std::thread::sleep(std::time::Duration::from_millis(5));
        let at = now();
        std::thread::sleep(std::time::Duration::from_millis(5));
        lib.update_page_order(nb, vec![p3, p1, p2]).unwrap();
        let p4 = lib.create_page(nb, None, None, None).unwrap().1.id;
        let then = local_then(&lib, nb, at).unwrap();
        assert_eq!(then.page_order, vec![p1, p2, p3]);
        let whole = build_whole(&mut lib, nb, &then, true, &|_| None, ME).unwrap();
        // Keep the newer page (unselect trashNewer) but restore the order.
        let chosen = vec!["pageOrder".to_string()];
        apply_whole(&mut lib, &whole, &chosen).unwrap();
        assert_eq!(lib.notebook(nb).unwrap().page_order, vec![p1, p2, p3, p4]);
        // Shared notebooks get neither item, and asking is refused.
        let then = local_then(&lib, nb, at).unwrap();
        let whole = build_whole(&mut lib, nb, &then, false, &|_| None, "acct-me").unwrap();
        let items = whole_items(&whole);
        assert!(!items.iter().any(|i| matches!(
            i.kind,
            RestoreItemKind::TrashNewer | RestoreItemKind::PageOrder
        )));
        assert!(items
            .iter()
            .any(|i| i.kind == RestoreItemKind::KeptAddition && i.page_id == Some(p4)));
        assert!(apply_whole(&mut lib, &whole, &["pageOrder".into()]).is_err());
        assert!(apply_whole(&mut lib, &whole, &[format!("trashNewer:{p4}")]).is_err());
        assert_eq!(lib.notebook(nb).unwrap().page_order, vec![p1, p2, p3, p4]);
    }

    #[test]
    fn a_page_change_after_the_preview_stops_the_whole_restore() {
        let (mut lib, _d, nb, p1, _p2, _p3, at) = timeline();
        let then = local_then(&lib, nb, at).unwrap();
        let whole = build_whole(&mut lib, nb, &then, true, &|_| None, ME).unwrap();
        let chosen: Vec<String> = whole_items(&whole)
            .into_iter()
            .filter(|i| i.selected && i.editable)
            .map(|i| i.item_id)
            .collect();
        edit_page(
            &mut lib,
            nb,
            p1,
            penroam_core::ops::OpKind::AddElements {
                elements: vec![ink(5)],
            },
        );
        let title = lib.notebook(nb).unwrap().title.clone();
        let e = apply_whole(&mut lib, &whole, &chosen).unwrap_err();
        assert!(e.message.starts_with(CHANGED_SINCE_PREVIEW));
        // nothing was written: not even the trashed page came back
        assert_eq!(lib.notebook(nb).unwrap().title, title);
        assert!(!lib.notebook(nb).unwrap().trashed_pages.is_empty());
        assert!(ids_of(&mut lib, nb, p1).contains(&Uuid::from_u128(5)));
    }

    #[test]
    fn a_shared_notebook_keeps_the_other_members_later_strokes() {
        let (mut lib, _d, nb, p1, p2, _p3, at) = timeline();
        let then = local_then(&lib, nb, at).unwrap();
        // Since then: I erased stroke 1 and rewrote the text; Bob drew strokes 4 and 11.
        let writers = move |page: Uuid| -> Option<WritersSinceBase> {
            let w = |n: u128, who: &str| (Uuid::from_u128(n), [who.to_string()].into());
            Some(if page == p1 {
                [w(1, "acct-me"), w(3, "acct-me"), w(4, "acct-bob")]
                    .into_iter()
                    .collect()
            } else {
                [w(11, "acct-bob")].into_iter().collect()
            })
        };
        let whole = build_whole(&mut lib, nb, &then, false, &writers, "acct-me").unwrap();
        let items = whole_items(&whole);
        assert!(!items
            .iter()
            .any(|i| i.kind == RestoreItemKind::RemoveAdditions));
        let chosen: Vec<String> = items
            .iter()
            .filter(|i| i.selected && i.editable)
            .map(|i| i.item_id.clone())
            .collect();
        apply_whole(&mut lib, &whole, &chosen).unwrap();
        // My erased stroke and my text came back; Bob's strokes stayed.
        assert_eq!(
            ids_of(&mut lib, nb, p1),
            [1, 2, 3, 4].map(Uuid::from_u128).into()
        );
        assert!(lib
            .load_page(nb, p1)
            .unwrap()
            .elements
            .contains(&text(3, "then")));
        assert_eq!(
            ids_of(&mut lib, nb, p2),
            [10, 11].map(Uuid::from_u128).into()
        );
        // Asking to remove additions anyway is refused.
        let (mut lib, _d, nb, p1, _p2, _p3, at) = timeline();
        let then = local_then(&lib, nb, at).unwrap();
        let whole = build_whole(&mut lib, nb, &then, false, &writers, "acct-me").unwrap();
        let e = apply_whole(&mut lib, &whole, &[format!("removeAdditions:{p1}")]).unwrap_err();
        assert!(e.message.contains("never removed"));
    }

    #[test]
    fn activity_maps_to_view_kinds() {
        let op = |body| LocalOperation {
            id: Uuid::new_v4(),
            device_id: Uuid::nil(),
            local_sequence: 1,
            schema_version: 1,
            causal_dependencies: vec![],
            at: 5,
            body,
        };
        let ops = vec![(
            op(penroam_core::operation::DocumentOperation::ElementsAdded {
                notebook_id: Uuid::nil(),
                page_id: Uuid::from_u128(1),
                elements: vec![text(1, "a")],
            }),
            OpAttribution {
                author: Some(ME.into()),
                accepted_at: None,
            },
        )];
        let v = activity_views(Uuid::nil(), &ops, false);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ActivityKindView::TextEdited);
        assert_eq!(v[0].author, AuthorView::you());
        let shared = activity_views(Uuid::nil(), &ops, true);
        assert_eq!(shared[0].author.kind, AuthorKind::Unknown);
    }
}
