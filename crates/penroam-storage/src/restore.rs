//! Non-destructive restore: bring back a past version of a page (or notebook) without undoing
//! anyone's later work.
//!
//! [`crate::Library::restore_page_content`] makes a page *equal* a past version, which deletes
//! everything added since and reverts everyone's later edits. That is unacceptable in a shared
//! notebook. This module plans a three-way restore instead, between the **base** (the historical
//! revision), the **head** (the page now) and who wrote each element since the base:
//!
//! * elements present at the base but deleted now are offered for **re-adding** under the same id
//!   (selected by default: bringing deleted content back destroys nothing);
//! * elements changed since the base are listed with their writers since then, and are selected for
//!   reverting by default **only if every writer since the base is the current user**; an element
//!   with unknown writers counts as someone else's;
//! * text blocks changed by someone else can instead be restored as a **recovered copy**: a new
//!   block with a new id next to the current one, carrying its provenance, so nobody's text is
//!   overwritten;
//! * elements added since the base are **kept**; a restore never removes anything unless the
//!   caller explicitly lists additions in [`RestoreSelection::remove_additions`], which is only for
//!   notebooks whose only writer is the user (making a personal notebook *equal* a past version);
//! * a template change is listed separately and is never selected by default.
//!
//! A plan is a preview. It records a [`HeadFingerprint`] of the head it was computed on;
//! [`check_precondition`] refuses to commit when the page has changed since
//! ([`RestoreError::ChangedSincePreview`]), so the UI recomputes rather than applying a stale
//! plan. [`RestorePlan::to_batch`] turns a selection into one ordinary [`OperationBatch`] built on
//! the head revision: a forward edit that syncs, is attributed like any other, and can itself be
//! undone. The notebook-level counterpart, [`NotebookRestorePlan`], brings back trashed pages and
//! optionally the title, and keeps pages made since.
//!
//! This is pure logic: the caller loads the pages, supplies the writers (for shared notebooks,
//! from server attribution, `docs/protocol/sync-protocol-v1.md` §12i) and applies the batch with
//! [`crate::Library::apply_page_operations`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use penroam_core::model::{Element, Notebook, Page, Template};
use penroam_core::ops::{OpKind, OperationBatch, PageOperation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Who wrote an element: an account id from attribution (or any stable author key the caller
/// uses consistently, such as a local device id for unshared notebooks).
pub type AuthorId = String;

/// For each element id, every author who changed it (added, edited or deleted it) after the base
/// revision. Elements absent from the map have unknown writers.
pub type WritersSinceBase = HashMap<Uuid, BTreeSet<AuthorId>>;

/// How far below the original a recovered copy of a text block is placed, in points, so both are
/// visible.
pub const RECOVERED_COPY_OFFSET: f64 = 16.0;
/// Key in a recovered copy's `extra` fields recording where it came from:
/// `{"elementId": <original id>, "revision": <base revision>}`.
pub const RECOVERED_FROM_KEY: &str = "recoveredFrom";

/// What a plan was computed on: the head revision and a SHA-256 over the head's elements and
/// template. Any change to the page changes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadFingerprint {
    /// The head page's revision.
    pub revision: u64,
    /// Lowercase hex SHA-256 of the head's content.
    pub hash: String,
}

impl HeadFingerprint {
    /// The fingerprint of `page`: its revision, and a hash over its template and every element
    /// (sorted by id, as JSON). Computing it reads the whole page.
    pub fn of(page: &Page) -> Self {
        let mut h = Sha256::new();
        h.update(b"penroam/restore-head/v1\0");
        h.update(serde_json::to_vec(&page.template).unwrap_or_default());
        let mut elements: Vec<&Element> = page.elements.iter().collect();
        elements.sort_by_key(|e| e.id());
        for e in elements {
            let bytes = serde_json::to_vec(e).unwrap_or_default();
            h.update(e.id().as_bytes());
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(&bytes);
        }
        Self {
            revision: page.revision,
            hash: h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        }
    }
}

/// Why a restore can't be planned or committed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RestoreError {
    /// The page (or notebook) changed after the preview was computed. Recompute the plan.
    #[error("the page changed since the preview (revision {expected} → {actual})")]
    ChangedSincePreview {
        /// Revision the plan was computed on.
        expected: u64,
        /// Revision now.
        actual: u64,
    },
    /// Base and head are different pages.
    #[error("base and head are different pages")]
    WrongPage,
    /// A selected id isn't an item of the plan (or of that category).
    #[error("{0} is not offered by this plan")]
    NotOffered(Uuid),
    /// A recovered copy was asked for an element that isn't a text block changed by others.
    #[error("no recovered copy is offered for {0}")]
    NoRecoveredCopy(Uuid),
    /// The template was selected but the plan has no template change.
    #[error("the template didn't change")]
    NoTemplateChange,
    /// The title was selected but the plan has no title change.
    #[error("the title didn't change")]
    NoTitleChange,
    /// The notebook's title, page order or trash changed after the preview. Recompute the plan.
    #[error("the notebook changed since the preview")]
    NotebookChangedSincePreview,
}

/// An element present at the base and deleted from the head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReAdd {
    /// The element as it was at the base (re-added with this id).
    pub element: Element,
    /// Who changed it since the base (including whoever deleted it), if known.
    pub writers: Vec<AuthorId>,
    /// Whether it is selected by default (always: re-adding destroys nothing).
    pub default_selected: bool,
}

/// An element present in both, changed since the base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    /// The element at the base.
    pub base: Element,
    /// The element now.
    pub head: Element,
    /// Who changed it since the base; empty when unknown.
    pub writers: Vec<AuthorId>,
    /// Selected for reverting by default: only when every writer since the base is the current
    /// user (never when the writers are unknown).
    pub default_selected: bool,
    /// Whether a recovered copy is offered: a text block that someone else (or someone unknown)
    /// changed. Then the default is to leave it as it is.
    pub recovered_copy: bool,
}

/// An element added since the base. Always kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptAddition {
    /// The element's id.
    pub element_id: Uuid,
    /// Who wrote it, if known.
    pub writers: Vec<AuthorId>,
}

/// The page template differs between base and head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateChange {
    /// The template at the base.
    pub base: Template,
    /// The template now.
    pub head: Template,
}

/// What to do with a changed element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangedChoice {
    /// Replace the current element with the base version (same id).
    Revert,
    /// Add the base version as a new text block next to the current one (text blocks changed by
    /// others only).
    RecoveredCopy,
}

/// The user's choices for a [`RestorePlan`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSelection {
    /// Elements to re-add.
    pub re_add: BTreeSet<Uuid>,
    /// Changed elements and what to do with each; absent ones stay as they are.
    pub changed: BTreeMap<Uuid, ChangedChoice>,
    /// Restore the base template.
    pub template: bool,
    /// Elements added since the base to remove. Only for notebooks whose only writer is the
    /// current user: never offer it in a shared notebook, where additions are other people's work.
    #[serde(default)]
    pub remove_additions: BTreeSet<Uuid>,
}

/// A previewed restore of one page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    /// The page.
    pub page_id: Uuid,
    /// The revision being restored from.
    pub base_revision: u64,
    /// What the plan was computed on; checked again before committing.
    pub head: HeadFingerprint,
    /// Deleted since the base: offered back.
    pub re_add: Vec<ReAdd>,
    /// Changed since the base.
    pub changed: Vec<Changed>,
    /// Added since the base: kept.
    pub kept: Vec<KeptAddition>,
    /// The template change, if any.
    pub template: Option<TemplateChange>,
    /// Whether the PDF background differs. Backgrounds are not restorable by an operation, so this
    /// is informational.
    pub background_changed: bool,
    /// Highest `z` on the head, so new copies go on top.
    head_max_z: i64,
}

fn writers_of(writers: &WritersSinceBase, id: Uuid) -> Vec<AuthorId> {
    writers
        .get(&id)
        .map(|w| w.iter().cloned().collect())
        .unwrap_or_default()
}

impl RestorePlan {
    /// Plans restoring `base` (a past version of the page) onto `head` (the page now), for
    /// `current_user`. Items are listed in the base's (re-adds, changes) or head's (additions)
    /// element order.
    pub fn new(
        base: &Page,
        head: &Page,
        writers: &WritersSinceBase,
        current_user: &str,
    ) -> Result<Self, RestoreError> {
        if base.id != head.id {
            return Err(RestoreError::WrongPage);
        }
        let head_by_id: HashMap<Uuid, &Element> =
            head.elements.iter().map(|e| (e.id(), e)).collect();
        let base_ids: BTreeSet<Uuid> = base.elements.iter().map(Element::id).collect();
        let mut re_add = Vec::new();
        let mut changed = Vec::new();
        for b in &base.elements {
            let id = b.id();
            match head_by_id.get(&id) {
                None => re_add.push(ReAdd {
                    element: b.clone(),
                    writers: writers_of(writers, id),
                    default_selected: true,
                }),
                Some(h) if *h != b => {
                    let w = writers_of(writers, id);
                    let only_me = !w.is_empty() && w.iter().all(|a| a == current_user);
                    changed.push(Changed {
                        base: b.clone(),
                        head: (*h).clone(),
                        default_selected: only_me,
                        recovered_copy: !only_me
                            && matches!((b, h), (Element::Text(_), Element::Text(_))),
                        writers: w,
                    });
                }
                Some(_) => {}
            }
        }
        let kept = head
            .elements
            .iter()
            .filter(|e| !base_ids.contains(&e.id()))
            .map(|e| KeptAddition {
                element_id: e.id(),
                writers: writers_of(writers, e.id()),
            })
            .collect();
        Ok(Self {
            page_id: head.id,
            base_revision: base.revision,
            head: HeadFingerprint::of(head),
            re_add,
            changed,
            kept,
            template: (base.template != head.template).then(|| TemplateChange {
                base: base.template.clone(),
                head: head.template.clone(),
            }),
            background_changed: base.background != head.background,
            head_max_z: head.elements.iter().map(Element::z).max().unwrap_or(0),
        })
    }

    /// Whether there is nothing to restore (the head already has everything of the base).
    pub fn is_empty(&self) -> bool {
        self.re_add.is_empty() && self.changed.is_empty() && self.template.is_none()
    }

    /// The default choices: every re-add, and reverting the changes made only by the current
    /// user. Nobody else's change and no template change is selected.
    pub fn default_selection(&self) -> RestoreSelection {
        RestoreSelection {
            re_add: self
                .re_add
                .iter()
                .filter(|r| r.default_selected)
                .map(|r| r.element.id())
                .collect(),
            changed: self
                .changed
                .iter()
                .filter(|c| c.default_selected)
                .map(|c| (c.base.id(), ChangedChoice::Revert))
                .collect(),
            template: false,
            remove_additions: BTreeSet::new(),
        }
    }

    /// Everything that makes the head equal the base again: every re-add, every revert, the
    /// template, and removing every addition. Only for notebooks whose only writer is the user.
    pub fn full_selection(&self) -> RestoreSelection {
        RestoreSelection {
            re_add: self.re_add.iter().map(|r| r.element.id()).collect(),
            changed: self
                .changed
                .iter()
                .map(|c| (c.base.id(), ChangedChoice::Revert))
                .collect(),
            template: self.template.is_some(),
            remove_additions: self.kept.iter().map(|k| k.element_id).collect(),
        }
    }

    /// The batch that applies `selection` on top of the head the plan was computed on
    /// (`base_revision` of the batch = the head revision). `None` when nothing is selected. Check
    /// [`check_precondition`] against the current page first; applying it to a moved page fails
    /// with a revision conflict anyway.
    ///
    /// Re-adds and recovered copies become one `addElements`, reverts one `replaceElements`, the
    /// template one `setTemplate`, and listed additions one `removeElements`. Nothing else is
    /// removed.
    pub fn to_batch(
        &self,
        selection: &RestoreSelection,
    ) -> Result<Option<OperationBatch>, RestoreError> {
        let mut add: Vec<Element> = Vec::new();
        for id in &selection.re_add {
            let r = self
                .re_add
                .iter()
                .find(|r| r.element.id() == *id)
                .ok_or(RestoreError::NotOffered(*id))?;
            add.push(r.element.clone());
        }
        let mut replace = Vec::new();
        let mut z = self.head_max_z;
        for (id, choice) in &selection.changed {
            let c = self
                .changed
                .iter()
                .find(|c| c.base.id() == *id)
                .ok_or(RestoreError::NotOffered(*id))?;
            match choice {
                ChangedChoice::Revert => replace.push(c.base.clone()),
                ChangedChoice::RecoveredCopy => {
                    let Element::Text(t) = &c.base else {
                        return Err(RestoreError::NoRecoveredCopy(*id));
                    };
                    if !c.recovered_copy {
                        return Err(RestoreError::NoRecoveredCopy(*id));
                    }
                    let mut copy = t.clone();
                    copy.id = Uuid::new_v4();
                    z = z.saturating_add(1);
                    copy.z = z;
                    copy.y += RECOVERED_COPY_OFFSET;
                    copy.extra.insert(
                        RECOVERED_FROM_KEY.into(),
                        serde_json::json!({"elementId": id, "revision": self.base_revision}),
                    );
                    add.push(Element::Text(copy));
                }
            }
        }
        let mut ops = Vec::new();
        if !selection.remove_additions.is_empty() {
            for id in &selection.remove_additions {
                if !self.kept.iter().any(|k| k.element_id == *id) {
                    return Err(RestoreError::NotOffered(*id));
                }
            }
            ops.push(OpKind::RemoveElements {
                ids: selection.remove_additions.iter().copied().collect(),
            });
        }
        if !add.is_empty() {
            // ascending z keeps the page's draw order cheap to maintain
            add.sort_by_key(|e| (e.z(), e.id()));
            ops.push(OpKind::AddElements { elements: add });
        }
        if !replace.is_empty() {
            ops.push(OpKind::ReplaceElements { elements: replace });
        }
        if selection.template {
            let t = self
                .template
                .as_ref()
                .ok_or(RestoreError::NoTemplateChange)?;
            ops.push(OpKind::SetTemplate {
                template: t.base.clone(),
            });
        }
        if ops.is_empty() {
            return Ok(None);
        }
        Ok(Some(OperationBatch {
            batch_id: Uuid::new_v4(),
            base_revision: self.head.revision,
            ops: ops
                .into_iter()
                .map(|kind| PageOperation {
                    op_id: Uuid::new_v4(),
                    kind,
                })
                .collect(),
        }))
    }
}

/// Refuses to commit `plan` when `current` (the page as it is now) differs from the head the plan
/// was computed on, by revision or content.
pub fn check_precondition(plan: &RestorePlan, current: &Page) -> Result<(), RestoreError> {
    if current.id != plan.page_id {
        return Err(RestoreError::WrongPage);
    }
    if HeadFingerprint::of(current) != plan.head {
        return Err(RestoreError::ChangedSincePreview {
            expected: plan.head.revision,
            actual: current.revision,
        });
    }
    Ok(())
}

// ------------------------------------------------------------------ notebook --

/// The title differs between the past state and now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleChange {
    /// The title then.
    pub base: String,
    /// The title now.
    pub head: String,
}

/// One step of a notebook restore, applied by the caller with the library's ordinary operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "step")]
pub enum NotebookRestoreStep {
    /// Bring a page back from the trash (`Library::restore_page`).
    RestoreTrashedPage {
        /// The page.
        page_id: Uuid,
    },
    /// Rename the notebook (`Library::rename_notebook`).
    Rename {
        /// The title to restore.
        title: String,
    },
}

/// The user's choices for a [`NotebookRestorePlan`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookRestoreSelection {
    /// Trashed pages to bring back.
    pub pages: BTreeSet<Uuid>,
    /// Restore the title.
    pub title: bool,
}

/// A previewed restore of a notebook's page list and title. Page content is restored per page
/// with [`RestorePlan`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotebookRestorePlan {
    /// The notebook.
    pub notebook_id: Uuid,
    /// Hash of the notebook's title, page order and trash when planned.
    pub head_hash: String,
    /// Pages live then and in the trash now, in the past page order: offered back (selected by
    /// default).
    pub restore_pages: Vec<Uuid>,
    /// Pages live then that are gone entirely (purged): can't be restored.
    pub unavailable_pages: Vec<Uuid>,
    /// Pages live now that weren't then: kept, never trashed.
    pub kept_pages: Vec<Uuid>,
    /// The title change, if any (never selected by default).
    pub title: Option<TitleChange>,
}

fn notebook_hash(nb: &Notebook) -> String {
    let mut h = Sha256::new();
    h.update(b"penroam/restore-notebook/v1\0");
    h.update((nb.title.len() as u64).to_le_bytes());
    h.update(nb.title.as_bytes());
    for p in &nb.page_order {
        h.update(b"L");
        h.update(p.as_bytes());
    }
    for t in &nb.trashed_pages {
        h.update(b"T");
        h.update(t.id.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

impl NotebookRestorePlan {
    /// Plans restoring a notebook to a past `base_title` and `base_page_order` (from its history)
    /// onto `head` (the notebook now).
    pub fn new(base_title: &str, base_page_order: &[Uuid], head: &Notebook) -> Self {
        let trashed: BTreeSet<Uuid> = head.trashed_pages.iter().map(|t| t.id).collect();
        let mut restore_pages = Vec::new();
        let mut unavailable_pages = Vec::new();
        for p in base_page_order {
            if head.page_order.contains(p) {
                continue;
            }
            if trashed.contains(p) {
                restore_pages.push(*p);
            } else {
                unavailable_pages.push(*p);
            }
        }
        Self {
            notebook_id: head.id,
            head_hash: notebook_hash(head),
            restore_pages,
            unavailable_pages,
            kept_pages: head
                .page_order
                .iter()
                .filter(|p| !base_page_order.contains(p))
                .copied()
                .collect(),
            title: (base_title != head.title).then(|| TitleChange {
                base: base_title.to_string(),
                head: head.title.clone(),
            }),
        }
    }

    /// Every restorable page; not the title.
    pub fn default_selection(&self) -> NotebookRestoreSelection {
        NotebookRestoreSelection {
            pages: self.restore_pages.iter().copied().collect(),
            title: false,
        }
    }

    /// The steps for `selection`, pages in the past page order, then the title.
    pub fn steps(
        &self,
        selection: &NotebookRestoreSelection,
    ) -> Result<Vec<NotebookRestoreStep>, RestoreError> {
        if let Some(p) = selection
            .pages
            .iter()
            .find(|p| !self.restore_pages.contains(p))
        {
            return Err(RestoreError::NotOffered(*p));
        }
        let mut steps: Vec<NotebookRestoreStep> = self
            .restore_pages
            .iter()
            .filter(|p| selection.pages.contains(p))
            .map(|p| NotebookRestoreStep::RestoreTrashedPage { page_id: *p })
            .collect();
        if selection.title {
            let t = self.title.as_ref().ok_or(RestoreError::NoTitleChange)?;
            steps.push(NotebookRestoreStep::Rename {
                title: t.base.clone(),
            });
        }
        Ok(steps)
    }
}

/// Refuses to commit a notebook plan when the notebook's title, page order or trash changed
/// since it was computed.
pub fn check_notebook_precondition(
    plan: &NotebookRestorePlan,
    current: &Notebook,
) -> Result<(), RestoreError> {
    if current.id != plan.notebook_id {
        return Err(RestoreError::WrongPage);
    }
    if notebook_hash(current) != plan.head_hash {
        return Err(RestoreError::NotebookChangedSincePreview);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Library;
    use penroam_core::model::{PageDefaults, TrashedPage};
    use tempfile::TempDir;

    const ME: &str = "acct-me";
    const BOB: &str = "acct-bob";

    fn ink(z: i64) -> Element {
        serde_json::from_value(serde_json::json!({
            "type": "ink", "id": Uuid::new_v4(), "z": z, "tool": "pen", "color": "#101010",
            "width": 2.0, "source": "stylus",
            "points": [{"x": 10.0, "y": 10.0 + z as f64, "pressure": 0.5},
                       {"x": 20.0, "y": 12.0 + z as f64, "pressure": 0.5}]
        }))
        .unwrap()
    }

    fn text(z: i64, body: &str) -> Element {
        serde_json::from_value(serde_json::json!({
            "type": "text", "id": Uuid::new_v4(), "z": z, "x": 10.0, "y": 100.0,
            "width": 200.0, "height": 24.0, "text": body, "fontSize": 14.0, "color": "#000000"
        }))
        .unwrap()
    }

    fn with_text(e: &Element, body: &str) -> Element {
        let mut e = e.clone();
        if let Element::Text(t) = &mut e {
            t.text = body.into();
        }
        e
    }

    fn moved(e: &Element) -> Element {
        let mut e = e.clone();
        e.translate(5.0, 5.0);
        e
    }

    fn writers(pairs: &[(&Element, &[&str])]) -> WritersSinceBase {
        pairs
            .iter()
            .map(|(e, w)| (e.id(), w.iter().map(|a| a.to_string()).collect()))
            .collect()
    }

    fn page_with(elements: Vec<Element>, revision: u64) -> Page {
        let mut p = Page::new(
            Uuid::nil(),
            penroam_core::model::Paper::Letter,
            penroam_core::model::Size {
                width: 600.0,
                height: 800.0,
            },
            Default::default(),
            0,
        );
        p.elements = elements;
        p.revision = revision;
        p
    }

    #[test]
    fn plan_categorizes_and_defaults_protect_others() {
        let kept_stroke = ink(1);
        let deleted_stroke = ink(2);
        let my_text = text(3, "mine");
        let bobs_text = text(4, "bob's");
        let bobs_stroke = ink(5);
        let unknown_stroke = ink(6);
        let base = page_with(
            vec![
                kept_stroke.clone(),
                deleted_stroke.clone(),
                my_text.clone(),
                bobs_text.clone(),
                bobs_stroke.clone(),
                unknown_stroke.clone(),
            ],
            3,
        );
        let new_stroke = ink(7);
        let head = page_with(
            vec![
                kept_stroke.clone(),
                with_text(&my_text, "mine, edited"),
                with_text(&bobs_text, "bob's, edited"),
                moved(&bobs_stroke),
                moved(&unknown_stroke),
                new_stroke.clone(),
            ],
            9,
        );
        let w = writers(&[
            (&deleted_stroke, &[BOB]),
            (&my_text, &[ME]),
            (&bobs_text, &[ME, BOB]),
            (&bobs_stroke, &[BOB]),
            (&new_stroke, &[BOB]),
        ]);
        let plan = RestorePlan::new(&base, &head, &w, ME).unwrap();
        assert_eq!(plan.base_revision, 3);
        assert_eq!(plan.head.revision, 9);
        assert_eq!(plan.re_add.len(), 1);
        assert_eq!(plan.re_add[0].element, deleted_stroke);
        assert!(plan.re_add[0].default_selected);
        assert_eq!(plan.re_add[0].writers, vec![BOB.to_string()]);
        let c = |e: &Element| plan.changed.iter().find(|c| c.base.id() == e.id()).unwrap();
        assert!(c(&my_text).default_selected && !c(&my_text).recovered_copy);
        assert!(!c(&bobs_text).default_selected && c(&bobs_text).recovered_copy);
        assert!(!c(&bobs_stroke).default_selected && !c(&bobs_stroke).recovered_copy);
        assert!(
            !c(&unknown_stroke).default_selected,
            "unknown writers count as others"
        );
        assert!(c(&unknown_stroke).writers.is_empty());
        assert_eq!(plan.kept.len(), 1);
        assert_eq!(plan.kept[0].element_id, new_stroke.id());
        assert!(plan.template.is_none() && !plan.background_changed);

        // The default batch re-adds the deleted stroke and reverts only my text.
        let sel = plan.default_selection();
        let batch = plan.to_batch(&sel).unwrap().unwrap();
        assert_eq!(batch.base_revision, 9);
        let mut after = head.clone();
        penroam_core::ops::apply_batch(&mut after, &batch, 1).unwrap();
        let get = |id: Uuid| after.elements.iter().find(|e| e.id() == id).cloned();
        assert_eq!(get(deleted_stroke.id()), Some(deleted_stroke));
        assert_eq!(get(my_text.id()), Some(my_text));
        assert_eq!(
            get(bobs_text.id()),
            Some(with_text(&bobs_text, "bob's, edited"))
        );
        assert_eq!(get(bobs_stroke.id()), Some(moved(&bobs_stroke)));
        assert_eq!(get(new_stroke.id()), Some(new_stroke), "additions are kept");
        assert_eq!(after.elements.len(), 7);
    }

    #[test]
    fn the_full_selection_makes_a_personal_page_equal_the_base() {
        let (keep, gone, changed) = (ink(1), ink(2), text(3, "old"));
        let base = page_with(vec![keep.clone(), gone.clone(), changed.clone()], 1);
        let added = ink(4);
        let head = page_with(
            vec![keep.clone(), with_text(&changed, "new"), added.clone()],
            4,
        );
        let plan = RestorePlan::new(&base, &head, &WritersSinceBase::new(), ME).unwrap();
        let mut after = head.clone();
        let batch = plan.to_batch(&plan.full_selection()).unwrap().unwrap();
        penroam_core::ops::apply_batch(&mut after, &batch, 1).unwrap();
        let ids = |p: &Page| -> BTreeSet<Uuid> { p.elements.iter().map(Element::id).collect() };
        assert_eq!(ids(&after), ids(&base));
        assert!(after.elements.contains(&changed));
        // Additions can only be removed when listed, and only real additions.
        let mut sel = RestoreSelection::default();
        sel.remove_additions.insert(keep.id());
        assert_eq!(
            plan.to_batch(&sel),
            Err(RestoreError::NotOffered(keep.id()))
        );
        assert!(plan.default_selection().remove_additions.is_empty());
    }

    #[test]
    fn recovered_copy_keeps_both_texts() {
        let block = text(1, "original");
        let base = page_with(vec![block.clone()], 1);
        let head = page_with(vec![with_text(&block, "bob rewrote it")], 2);
        let plan = RestorePlan::new(&base, &head, &writers(&[(&block, &[BOB])]), ME).unwrap();
        let mut sel = RestoreSelection::default();
        sel.changed.insert(block.id(), ChangedChoice::RecoveredCopy);
        let batch = plan.to_batch(&sel).unwrap().unwrap();
        let mut after = head.clone();
        penroam_core::ops::apply_batch(&mut after, &batch, 1).unwrap();
        assert_eq!(after.elements.len(), 2);
        assert_eq!(after.elements[0], with_text(&block, "bob rewrote it"));
        let Element::Text(copy) = &after.elements[1] else {
            panic!("not text")
        };
        assert_ne!(copy.id, block.id());
        assert_eq!(copy.text, "original");
        assert!(copy.z > block.z());
        assert_eq!(
            copy.extra[RECOVERED_FROM_KEY],
            serde_json::json!({"elementId": block.id(), "revision": 1})
        );
        // Strokes and text changed only by me have no recovered copy.
        let stroke = ink(2);
        let base = page_with(vec![stroke.clone()], 1);
        let head = page_with(vec![moved(&stroke)], 2);
        let plan = RestorePlan::new(&base, &head, &writers(&[(&stroke, &[BOB])]), ME).unwrap();
        let mut sel = RestoreSelection::default();
        sel.changed
            .insert(stroke.id(), ChangedChoice::RecoveredCopy);
        assert_eq!(
            plan.to_batch(&sel),
            Err(RestoreError::NoRecoveredCopy(stroke.id()))
        );
    }

    #[test]
    fn selections_are_checked_and_template_is_opt_in() {
        let s = ink(1);
        let mut base = page_with(vec![s.clone()], 1);
        base.template = Template {
            kind: penroam_core::model::TemplateKind::Grid,
            spacing: 20.0,
        };
        let head = page_with(vec![s.clone()], 2);
        let plan = RestorePlan::new(&base, &head, &WritersSinceBase::new(), ME).unwrap();
        // Only the template differs, and it isn't selected by default.
        assert!(plan.template.is_some());
        assert!(!plan.default_selection().template);
        assert_eq!(plan.to_batch(&plan.default_selection()), Ok(None));
        let mut sel = RestoreSelection::default();
        sel.re_add.insert(s.id());
        assert_eq!(plan.to_batch(&sel), Err(RestoreError::NotOffered(s.id())));
        let sel = RestoreSelection {
            template: true,
            ..Default::default()
        };
        let batch = plan.to_batch(&sel).unwrap().unwrap();
        assert_eq!(
            batch.ops[0].kind,
            OpKind::SetTemplate {
                template: base.template.clone()
            }
        );
        let same = RestorePlan::new(&head, &head, &WritersSinceBase::new(), ME).unwrap();
        assert!(same.is_empty());
        assert_eq!(same.to_batch(&sel), Err(RestoreError::NoTemplateChange));
        let mut other = head.clone();
        other.id = Uuid::new_v4();
        assert_eq!(
            RestorePlan::new(&base, &other, &WritersSinceBase::new(), ME).unwrap_err(),
            RestoreError::WrongPage
        );
    }

    #[test]
    fn precondition_fails_after_a_concurrent_change() {
        let s = ink(1);
        let gone = ink(2);
        let base = page_with(vec![s.clone(), gone.clone()], 1);
        let head = page_with(vec![s.clone()], 2);
        let plan = RestorePlan::new(&base, &head, &WritersSinceBase::new(), ME).unwrap();
        assert_eq!(check_precondition(&plan, &head), Ok(()));
        // Same revision but different content (e.g. an install that kept the number) also fails.
        let mut tampered = head.clone();
        tampered.elements[0] = moved(&s);
        assert!(matches!(
            check_precondition(&plan, &tampered),
            Err(RestoreError::ChangedSincePreview {
                expected: 2,
                actual: 2
            })
        ));
        let mut newer = head.clone();
        newer.elements.push(ink(3));
        newer.revision = 3;
        assert_eq!(
            check_precondition(&plan, &newer),
            Err(RestoreError::ChangedSincePreview {
                expected: 2,
                actual: 3
            })
        );
        // And the batch itself is pinned to the previewed revision.
        let batch = plan.to_batch(&plan.default_selection()).unwrap().unwrap();
        assert!(penroam_core::ops::apply_batch(&mut newer, &batch, 1).is_err());
    }

    #[test]
    fn round_trip_through_the_library_keeps_the_other_persons_stroke() {
        let d = TempDir::new().unwrap();
        let mut lib = Library::open(d.path().join("lib")).unwrap();
        let (nb, page) = lib
            .create_notebook("Shared", PageDefaults::default(), None)
            .unwrap();
        let apply = |lib: &mut Library, kind: OpKind| {
            let rev = lib.load_page(nb.id, page.id).unwrap().revision;
            lib.apply_page_operations(
                nb.id,
                page.id,
                &OperationBatch {
                    batch_id: Uuid::new_v4(),
                    base_revision: rev,
                    ops: vec![PageOperation {
                        op_id: Uuid::new_v4(),
                        kind,
                    }],
                },
            )
            .unwrap();
        };
        let mine = ink(1);
        let my_text = text(2, "draft");
        apply(
            &mut lib,
            OpKind::AddElements {
                elements: vec![mine.clone(), my_text.clone()],
            },
        );
        let base = lib.load_page(nb.id, page.id).unwrap();
        // Later: I erase my stroke and rewrite my text; Bob adds a stroke.
        apply(
            &mut lib,
            OpKind::RemoveElements {
                ids: vec![mine.id()],
            },
        );
        apply(
            &mut lib,
            OpKind::ReplaceElements {
                elements: vec![with_text(&my_text, "rewritten")],
            },
        );
        let bobs = ink(3);
        apply(
            &mut lib,
            OpKind::AddElements {
                elements: vec![bobs.clone()],
            },
        );
        let head = lib.load_page(nb.id, page.id).unwrap();
        let w = writers(&[(&mine, &[ME]), (&my_text, &[ME]), (&bobs, &[BOB])]);
        let plan = RestorePlan::new(&base, &head, &w, ME).unwrap();
        check_precondition(&plan, &lib.load_page(nb.id, page.id).unwrap()).unwrap();
        let batch = plan.to_batch(&plan.default_selection()).unwrap().unwrap();
        lib.apply_page_operations(nb.id, page.id, &batch).unwrap();
        let after = lib.load_page(nb.id, page.id).unwrap();
        let ids: BTreeSet<Uuid> = after.elements.iter().map(Element::id).collect();
        assert_eq!(
            ids,
            [mine.id(), my_text.id(), bobs.id()].into_iter().collect()
        );
        assert!(after.elements.contains(&my_text));
        assert!(after.elements.contains(&bobs));
        // Undo the restore is just another plan: from the head before it.
        let undo = RestorePlan::new(&head, &after, &WritersSinceBase::new(), ME).unwrap();
        assert!(
            undo.re_add.is_empty(),
            "a restore never removed anything to bring back"
        );
        // A stale plan is refused after the commit moved the page.
        assert!(check_precondition(&plan, &after).is_err());
    }

    #[test]
    fn notebook_restore_brings_back_trash_and_keeps_new_pages() {
        let d = TempDir::new().unwrap();
        let mut lib = Library::open(d.path().join("lib")).unwrap();
        let (nb, p1) = lib
            .create_notebook("Then", PageDefaults::default(), None)
            .unwrap();
        let mut head = lib.notebook(nb.id).unwrap().clone();
        let (p2, p3, purged, new) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        head.title = "Now".into();
        head.page_order = vec![p1.id, new];
        head.trashed_pages = [p2, p3]
            .iter()
            .map(|id| TrashedPage {
                id: *id,
                deleted_at: 5,
                former_index: 1,
            })
            .collect();
        let base_order = [p1.id, p2, purged, p3];
        let plan = NotebookRestorePlan::new("Then", &base_order, &head);
        assert_eq!(plan.restore_pages, vec![p2, p3]);
        assert_eq!(plan.unavailable_pages, vec![purged]);
        assert_eq!(plan.kept_pages, vec![new]);
        assert_eq!(
            plan.title,
            Some(TitleChange {
                base: "Then".into(),
                head: "Now".into()
            })
        );
        let sel = plan.default_selection();
        assert!(!sel.title);
        assert_eq!(
            plan.steps(&sel).unwrap(),
            vec![
                NotebookRestoreStep::RestoreTrashedPage { page_id: p2 },
                NotebookRestoreStep::RestoreTrashedPage { page_id: p3 },
            ]
        );
        let all = NotebookRestoreSelection {
            pages: [p3].into_iter().collect(),
            title: true,
        };
        assert_eq!(
            plan.steps(&all).unwrap(),
            vec![
                NotebookRestoreStep::RestoreTrashedPage { page_id: p3 },
                NotebookRestoreStep::Rename {
                    title: "Then".into()
                },
            ]
        );
        let bad = NotebookRestoreSelection {
            pages: [new].into_iter().collect(),
            title: false,
        };
        assert_eq!(plan.steps(&bad), Err(RestoreError::NotOffered(new)));
        assert_eq!(check_notebook_precondition(&plan, &head), Ok(()));
        let mut moved = head.clone();
        moved.page_order.reverse();
        assert_eq!(
            check_notebook_precondition(&plan, &moved),
            Err(RestoreError::NotebookChangedSincePreview)
        );
        let same = NotebookRestorePlan::new("Now", &head.page_order, &head);
        assert!(same.title.is_none() && same.restore_pages.is_empty());
        assert_eq!(
            same.steps(&NotebookRestoreSelection {
                pages: Default::default(),
                title: true
            }),
            Err(RestoreError::NoTitleChange)
        );
    }
}
