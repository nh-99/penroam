//! Activity summaries: what happened in a notebook, by whom, from its operation log.
//!
//! [`summarize`] reads operations in log order, each with the caller's attribution (for shared
//! notebooks, from the server's attribution of §12i of `docs/protocol/sync-protocol-v1.md`), and
//! produces [`ActivityEntry`]s an activity list can show. It describes only what the operations
//! say:
//!
//! * added elements are counted by type (strokes, text blocks, images);
//! * removed elements are counted by the type they had when this log added them, and as `unknown`
//!   when the log never saw them added (a removal names ids only);
//! * a replaced text block whose content group changed, or a `textEdited` operation, is a text
//!   edit; any other replacement (moved, resized, restyled) is a change;
//! * page and notebook operations map one to one (`pageCreated` with a PDF background is a PDF page
//!   import).
//!
//! Library-level operations (folders, filing, assets, purge) are not notebook activity and are
//! skipped. Nothing is inferred beyond the operation kinds: there is no "restore" entry, because a
//! restore is an ordinary batch and indistinguishable from the edits it consists of.
//!
//! Consecutive-ish operations of the same author, page and kind whose times fall within
//! [`ActivityOptions::window_ms`] of the group's first one merge into one entry with summed counts
//! (looking back over the last few entries, so interleaved authors still group). An entry is
//! `edited_offline` when any of its operations was accepted by the server much later than it was
//! made ([`ActivityOptions::offline_after_ms`]).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{Element, Timestamp};
use crate::operation::{groups, DocumentOperation, LocalOperation};

/// What the caller knows about who made an operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpAttribution {
    /// The author (an account id from attribution), when known.
    pub author: Option<String>,
    /// When the server accepted it, Unix milliseconds, when known.
    pub accepted_at: Option<Timestamp>,
}

/// Tunables of [`summarize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityOptions {
    /// Operations within this long of a group's first one join it (default one minute).
    pub window_ms: i64,
    /// An operation accepted more than this long after it was made counts as made offline
    /// (default ten minutes).
    pub offline_after_ms: i64,
}

impl Default for ActivityOptions {
    fn default() -> Self {
        Self {
            window_ms: 60_000,
            offline_after_ms: 10 * 60_000,
        }
    }
}

/// What an entry is about. Counts add up when entries merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ActivityKind {
    /// Elements were added.
    #[serde(rename_all = "camelCase")]
    ElementsAdded {
        /// Ink strokes.
        strokes: u32,
        /// Text blocks.
        texts: u32,
        /// Images.
        images: u32,
    },
    /// Elements were erased or deleted.
    #[serde(rename_all = "camelCase")]
    ElementsRemoved {
        /// Ink strokes.
        strokes: u32,
        /// Text blocks.
        texts: u32,
        /// Images.
        images: u32,
        /// Elements this log never saw added.
        unknown: u32,
    },
    /// Text blocks' text was edited.
    TextEdited {
        /// Edits (operations), not distinct blocks.
        edits: u32,
    },
    /// Elements were moved, resized or restyled.
    ElementsChanged {
        /// Elements changed.
        count: u32,
    },
    /// Pages were added.
    PageAdded {
        /// Pages.
        count: u32,
    },
    /// Pages of a PDF were imported.
    PdfPageImported {
        /// Pages.
        count: u32,
    },
    /// Pages were moved.
    PageMoved {
        /// Moves.
        count: u32,
    },
    /// Pages were moved to the trash.
    PageDeleted {
        /// Pages.
        count: u32,
    },
    /// Pages were brought back from the trash.
    PageRestored {
        /// Pages.
        count: u32,
    },
    /// A page's template changed.
    TemplateChanged,
    /// The notebook was renamed.
    Renamed {
        /// The new title.
        title: String,
    },
    /// The notebook was created.
    NotebookCreated,
    /// The cover changed.
    CoverChanged,
    /// Infinite canvas was switched on or off.
    CanvasChanged {
        /// Whether pages are infinite canvases now.
        infinite: bool,
    },
    /// The notebook went to the trash.
    NotebookTrashed,
    /// The notebook came back from the trash.
    NotebookRestored,
    /// The notebook was shared.
    NotebookShared,
}

impl ActivityKind {
    /// Whether two kinds merge (same variant).
    fn same(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// Adds `other`'s counts into `self` (same variant).
    fn absorb(&mut self, other: ActivityKind) {
        use ActivityKind::*;
        match (self, other) {
            (
                ElementsAdded {
                    strokes,
                    texts,
                    images,
                },
                ElementsAdded {
                    strokes: s,
                    texts: t,
                    images: i,
                },
            ) => {
                *strokes += s;
                *texts += t;
                *images += i;
            }
            (
                ElementsRemoved {
                    strokes,
                    texts,
                    images,
                    unknown,
                },
                ElementsRemoved {
                    strokes: s,
                    texts: t,
                    images: i,
                    unknown: u,
                },
            ) => {
                *strokes += s;
                *texts += t;
                *images += i;
                *unknown += u;
            }
            (TextEdited { edits }, TextEdited { edits: e }) => *edits += e,
            (ElementsChanged { count }, ElementsChanged { count: c })
            | (PageAdded { count }, PageAdded { count: c })
            | (PdfPageImported { count }, PdfPageImported { count: c })
            | (PageMoved { count }, PageMoved { count: c })
            | (PageDeleted { count }, PageDeleted { count: c })
            | (PageRestored { count }, PageRestored { count: c }) => *count += c,
            (Renamed { title }, Renamed { title: t }) => *title = t,
            (CanvasChanged { infinite }, CanvasChanged { infinite: i }) => *infinite = i,
            _ => {}
        }
    }
}

/// One line of an activity list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEntry {
    /// Who, when known.
    pub author: Option<String>,
    /// When the first of its operations was made (the operations' own clock).
    pub first_at: Timestamp,
    /// When the last of its operations was made.
    pub at: Timestamp,
    /// When the server accepted the last of them, when known.
    pub accepted_at: Option<Timestamp>,
    /// The page, for page-level activity.
    pub page_id: Option<Uuid>,
    /// What happened.
    pub kind: ActivityKind,
    /// The operations it summarizes, in log order.
    pub op_ids: Vec<Uuid>,
    /// Whether any of them was made offline (accepted much later than made).
    pub edited_offline: bool,
}

#[derive(Clone, Copy)]
enum ElemType {
    Ink,
    Text,
    Image,
}

fn elem_type(e: &Element) -> ElemType {
    match e {
        Element::Ink(_) => ElemType::Ink,
        Element::Text(_) => ElemType::Text,
        Element::Image(_) => ElemType::Image,
    }
}

/// How many recent entries a new operation may join.
const LOOKBACK: usize = 16;

/// Summarizes `ops` (in log order, each with its attribution) into activity entries, oldest
/// first. See the module docs for what each operation becomes.
pub fn summarize(
    ops: &[(LocalOperation, OpAttribution)],
    options: ActivityOptions,
) -> Vec<ActivityEntry> {
    let mut seen: HashMap<Uuid, ElemType> = HashMap::new();
    let mut out: Vec<ActivityEntry> = Vec::new();
    for (op, who) in ops {
        let Some((page_id, kind)) = describe(&op.body, &mut seen) else {
            continue;
        };
        let offline = who
            .accepted_at
            .is_some_and(|acc| acc.saturating_sub(op.at) > options.offline_after_ms);
        let start = out.len().saturating_sub(LOOKBACK);
        let joinable = out[start..].iter_mut().rev().find(|e| {
            e.author == who.author
                && e.page_id == page_id
                && e.kind.same(&kind)
                && op.at >= e.first_at
                && op.at - e.first_at <= options.window_ms
        });
        match joinable {
            Some(e) => {
                e.kind.absorb(kind);
                e.at = e.at.max(op.at);
                e.accepted_at = match (e.accepted_at, who.accepted_at) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                e.op_ids.push(op.id);
                e.edited_offline |= offline;
            }
            None => out.push(ActivityEntry {
                author: who.author.clone(),
                first_at: op.at,
                at: op.at,
                accepted_at: who.accepted_at,
                page_id,
                kind,
                op_ids: vec![op.id],
                edited_offline: offline,
            }),
        }
    }
    out
}

/// What one operation is, or `None` for operations that aren't notebook activity.
fn describe(
    body: &DocumentOperation,
    seen: &mut HashMap<Uuid, ElemType>,
) -> Option<(Option<Uuid>, ActivityKind)> {
    use ActivityKind as K;
    use DocumentOperation as D;
    Some(match body {
        D::ElementsAdded {
            page_id, elements, ..
        } => {
            let (mut strokes, mut texts, mut images) = (0, 0, 0);
            for e in elements {
                let t = elem_type(e);
                seen.insert(e.id(), t);
                match t {
                    ElemType::Ink => strokes += 1,
                    ElemType::Text => texts += 1,
                    ElemType::Image => images += 1,
                }
            }
            (
                Some(*page_id),
                K::ElementsAdded {
                    strokes,
                    texts,
                    images,
                },
            )
        }
        D::ElementsRemoved { page_id, ids, .. } => {
            let (mut strokes, mut texts, mut images, mut unknown) = (0, 0, 0, 0);
            for id in ids {
                match seen.get(id) {
                    Some(ElemType::Ink) => strokes += 1,
                    Some(ElemType::Text) => texts += 1,
                    Some(ElemType::Image) => images += 1,
                    None => unknown += 1,
                }
            }
            (
                Some(*page_id),
                K::ElementsRemoved {
                    strokes,
                    texts,
                    images,
                    unknown,
                },
            )
        }
        D::ElementsReplaced {
            page_id,
            elements,
            changed,
            ..
        } => {
            let text_edit = elements.iter().enumerate().any(|(i, e)| {
                matches!(e, Element::Text(_))
                    && changed.get(i).is_some_and(|m| m & groups::CONTENT != 0)
            });
            for e in elements {
                seen.insert(e.id(), elem_type(e));
            }
            if text_edit {
                (Some(*page_id), K::TextEdited { edits: 1 })
            } else {
                (
                    Some(*page_id),
                    K::ElementsChanged {
                        count: elements.len() as u32,
                    },
                )
            }
        }
        D::TextEdited { page_id, .. } => (Some(*page_id), K::TextEdited { edits: 1 }),
        D::PageCreated {
            page_id,
            background,
            ..
        } => {
            if background.is_some() {
                (Some(*page_id), K::PdfPageImported { count: 1 })
            } else {
                (Some(*page_id), K::PageAdded { count: 1 })
            }
        }
        D::PageDeleted { page_id, .. } => (Some(*page_id), K::PageDeleted { count: 1 }),
        D::PageRestored { page_id, .. } => (Some(*page_id), K::PageRestored { count: 1 }),
        D::PageReordered { page_id, .. } => (Some(*page_id), K::PageMoved { count: 1 }),
        D::PageTemplateSet { page_id, .. } => (Some(*page_id), K::TemplateChanged),
        D::NotebookCreated { .. } => (None, K::NotebookCreated),
        D::NotebookRenamed { title, .. } => (
            None,
            K::Renamed {
                title: title.clone(),
            },
        ),
        D::NotebookCoverChanged { .. } => (None, K::CoverChanged),
        D::NotebookCanvasSet { infinite, .. } => (
            None,
            K::CanvasChanged {
                infinite: *infinite,
            },
        ),
        D::NotebookDeleted { .. } => (None, K::NotebookTrashed),
        D::NotebookRestored { .. } => (None, K::NotebookRestored),
        D::NotebookShared { .. } => (None, K::NotebookShared),
        D::NotebookPurged { .. }
        | D::AssetReferenced { .. }
        | D::NotebookFiled { .. }
        | D::FolderCreated { .. }
        | D::FolderRenamed { .. }
        | D::FolderMoved { .. }
        | D::FolderDeleted { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Paper, Size, Template};
    use crate::operation::LocalOperation;

    const NB: Uuid = Uuid::from_u128(1);
    const P1: Uuid = Uuid::from_u128(11);
    const P2: Uuid = Uuid::from_u128(12);

    fn op(at: Timestamp, body: DocumentOperation) -> LocalOperation {
        LocalOperation {
            id: Uuid::new_v4(),
            device_id: Uuid::nil(),
            local_sequence: 1,
            schema_version: crate::operation::op_schema_for(&body),
            causal_dependencies: vec![],
            at,
            body,
        }
    }

    fn by(author: &str, accepted_at: Option<Timestamp>) -> OpAttribution {
        OpAttribution {
            author: Some(author.into()),
            accepted_at,
        }
    }

    fn ink() -> Element {
        serde_json::from_value(serde_json::json!({
            "type": "ink", "id": Uuid::new_v4(), "z": 1, "tool": "pen", "color": "#101010",
            "width": 2.0, "source": "stylus", "points": [{"x": 1.0, "y": 1.0, "pressure": 0.5}]
        }))
        .unwrap()
    }

    fn text() -> Element {
        serde_json::from_value(serde_json::json!({
            "type": "text", "id": Uuid::new_v4(), "z": 2, "x": 0.0, "y": 0.0, "width": 100.0,
            "height": 20.0, "text": "hi", "fontSize": 12.0, "color": "#000000"
        }))
        .unwrap()
    }

    fn added(page: Uuid, elements: Vec<Element>) -> DocumentOperation {
        DocumentOperation::ElementsAdded {
            notebook_id: NB,
            page_id: page,
            elements,
        }
    }

    #[test]
    fn strokes_group_by_author_page_and_window() {
        let (a, b, c) = (ink(), ink(), ink());
        let ops = vec![
            (
                op(1_000, added(P1, vec![a.clone()])),
                by("ada", Some(1_100)),
            ),
            (
                op(2_000, added(P1, vec![b.clone()])),
                by("bob", Some(2_100)),
            ),
            (
                op(3_000, added(P1, vec![c.clone()])),
                by("ada", Some(3_100)),
            ),
            (op(4_000, added(P2, vec![ink()])), by("ada", Some(4_100))),
            // outside the window of ada's first group on P1
            (op(70_000, added(P1, vec![ink()])), by("ada", Some(70_100))),
        ];
        let e = summarize(&ops, ActivityOptions::default());
        assert_eq!(e.len(), 4, "{e:#?}");
        assert_eq!(e[0].author.as_deref(), Some("ada"));
        assert_eq!(
            e[0].kind,
            ActivityKind::ElementsAdded {
                strokes: 2,
                texts: 0,
                images: 0
            }
        );
        assert_eq!(
            (e[0].first_at, e[0].at, e[0].accepted_at),
            (1_000, 3_000, Some(3_100))
        );
        assert_eq!(e[0].op_ids.len(), 2);
        assert_eq!(e[1].author.as_deref(), Some("bob"));
        assert_eq!(e[2].page_id, Some(P2));
        assert_eq!(e[3].first_at, 70_000);
        assert!(e.iter().all(|e| !e.edited_offline));
    }

    #[test]
    fn removals_are_typed_only_when_the_log_saw_the_element() {
        let (s, t) = (ink(), text());
        let ops = vec![
            (
                op(1, added(P1, vec![s.clone(), t.clone()])),
                by("ada", None),
            ),
            (
                op(
                    2,
                    DocumentOperation::ElementsRemoved {
                        notebook_id: NB,
                        page_id: P1,
                        ids: vec![s.id(), t.id(), Uuid::new_v4()],
                    },
                ),
                by("ada", None),
            ),
        ];
        let e = summarize(&ops, ActivityOptions::default());
        assert_eq!(
            e[0].kind,
            ActivityKind::ElementsAdded {
                strokes: 1,
                texts: 1,
                images: 0
            }
        );
        assert_eq!(
            e[1].kind,
            ActivityKind::ElementsRemoved {
                strokes: 1,
                texts: 1,
                images: 0,
                unknown: 1
            }
        );
    }

    #[test]
    fn text_edits_moves_pages_and_notebook_ops() {
        let t = text();
        let mut edited = t.clone();
        if let Element::Text(b) = &mut edited {
            b.text = "hello".into();
        }
        let mut moved = t.clone();
        moved.translate(3.0, 3.0);
        let page = |page_id, background| DocumentOperation::PageCreated {
            notebook_id: NB,
            page_id,
            order_key: "a".into(),
            paper: Paper::Letter,
            size: Size {
                width: 600.0,
                height: 800.0,
            },
            template: Template::default(),
            background,
        };
        let pdf: crate::model::PdfBackgroundRef = serde_json::from_value(serde_json::json!({
            "assetHash": "0".repeat(64), "pageIndex": 0,
            "mediaBox": [0.0, 0.0, 600.0, 800.0], "cropBox": [0.0, 0.0, 600.0, 800.0],
            "rotation": 0
        }))
        .unwrap();
        let ops = vec![
            (op(1, page(P1, None)), by("ada", None)),
            (op(2, page(P2, Some(pdf))), by("ada", None)),
            (
                op(
                    3,
                    DocumentOperation::ElementsReplaced {
                        notebook_id: NB,
                        page_id: P1,
                        elements: vec![edited],
                        changed: vec![groups::CONTENT],
                    },
                ),
                by("ada", None),
            ),
            (
                op(
                    4,
                    DocumentOperation::ElementsReplaced {
                        notebook_id: NB,
                        page_id: P1,
                        elements: vec![moved],
                        changed: vec![groups::PLACEMENT],
                    },
                ),
                by("ada", None),
            ),
            (
                op(
                    5,
                    DocumentOperation::TextEdited {
                        notebook_id: NB,
                        page_id: P1,
                        element_id: t.id(),
                        base: Uuid::new_v4(),
                        update: String::new(),
                    },
                ),
                by("ada", None),
            ),
            (
                op(
                    6,
                    DocumentOperation::NotebookRenamed {
                        notebook_id: NB,
                        title: "One".into(),
                    },
                ),
                by("ada", None),
            ),
            (
                op(
                    7,
                    DocumentOperation::NotebookRenamed {
                        notebook_id: NB,
                        title: "Two".into(),
                    },
                ),
                by("ada", None),
            ),
            (
                op(
                    8,
                    DocumentOperation::NotebookFiled {
                        notebook_id: NB,
                        folder: None,
                    },
                ),
                by("ada", None),
            ),
            (
                op(
                    9,
                    DocumentOperation::PageDeleted {
                        notebook_id: NB,
                        page_id: P2,
                    },
                ),
                OpAttribution::default(),
            ),
        ];
        let e = summarize(&ops, ActivityOptions::default());
        let kinds: Vec<&ActivityKind> = e.iter().map(|e| &e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                &ActivityKind::PageAdded { count: 1 },
                &ActivityKind::PdfPageImported { count: 1 },
                &ActivityKind::TextEdited { edits: 2 },
                &ActivityKind::ElementsChanged { count: 1 },
                &ActivityKind::Renamed {
                    title: "Two".into()
                },
                &ActivityKind::PageDeleted { count: 1 },
            ]
        );
        assert_eq!(e[4].page_id, None);
        assert_eq!(e[5].author, None, "unattributed stays unattributed");
    }

    #[test]
    fn late_acceptance_marks_offline_edits() {
        let ops = vec![
            (
                op(1_000, added(P1, vec![ink()])),
                by("ada", Some(1_000 + 11 * 60_000)),
            ),
            (op(1_500, added(P1, vec![ink()])), by("ada", Some(2_000))),
            (
                op(200_000, added(P1, vec![ink()])),
                by("ada", Some(201_000)),
            ),
        ];
        let e = summarize(&ops, ActivityOptions::default());
        assert!(e[0].edited_offline);
        assert_eq!(e[0].op_ids.len(), 2);
        assert!(!e[1].edited_offline);
        let strict = ActivityOptions {
            offline_after_ms: 100,
            ..Default::default()
        };
        assert!(summarize(&ops, strict)[1].edited_offline);
    }
}
