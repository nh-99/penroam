//! Library folders: named, nestable containers for notebooks.
//!
//! Folders belong to the library, not to any notebook, so their operations
//! travel on a stream of their own, addressed by the reserved
//! [`LIBRARY_STREAM_ID`]. Which folder a notebook is in is stored on the
//! notebook ([`crate::Notebook::folder`]) and changes with its own operation.
//!
//! A [`FolderSet`] is the raw state: every folder ever created, including
//! deleted ones, each with the parent it was last given. The single-writer
//! library updates it with [`apply_folder_op`]; sync builds one from
//! [`crate::replica::Replica::folders`]. Everything the user sees comes from
//! [`FolderSet::tree`], a pure function of that state.
//!
//! Invariants that make this converge without cascading writes:
//!
//! * **Deleted folders stay as tombstones.** Deleting never touches what is
//!   inside. A notebook or subfolder whose folder is gone shows at the nearest
//!   live ancestor, or at the top level. A device that files a notebook into a
//!   folder another device just deleted therefore loses nothing.
//! * **Cycles are cut when reading.** Two devices can each move one folder
//!   under the other. No single move is wrong, so neither is rejected; every
//!   cycle is cut at its highest folder id, which then sits at the top level.
//!   The choice depends only on the state, so every device makes the same one.
//!
//! See `docs/formats/model-and-archive.md` for the file and
//! `docs/protocol/sync-protocol-v1.md` for the operations.

use crate::model::Timestamp;
use crate::operation::{check_schema, DocumentOperation, LocalOperation, OpError};
use crate::validate::validate_title;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;
use uuid::Uuid;

/// Reserved stream id that folder operations are routed and keyed under. It
/// never names a notebook: validation refuses notebooks and operations that
/// use it as one.
pub const LIBRARY_STREAM_ID: Uuid = Uuid::from_u128(0x70656e72_6f61_4d00_8000_666f6c646572);

/// One folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Folder {
    /// Stable random id.
    pub id: Uuid,
    /// Name as shown.
    pub name: String,
    /// Containing folder, or `None` at the top level. In a [`FolderSet`] this
    /// is the stored value; in [`FolderSet::tree`] it is the folder to show it
    /// under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent: Option<Uuid>,
    /// Creation time (informational).
    #[ts(type = "number")]
    pub created_at: Timestamp,
    /// Set once the folder is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub deleted_at: Option<Timestamp>,
}

/// Every folder of a library, live and deleted, by id. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FolderSet(pub BTreeMap<Uuid, Folder>);

impl FolderSet {
    /// Whether `id` is a folder that hasn't been deleted.
    pub fn is_live(&self, id: Uuid) -> bool {
        self.0.get(&id).is_some_and(|f| f.deleted_at.is_none())
    }

    /// Stored parents with unknown parents removed and every cycle cut.
    fn parents(&self) -> BTreeMap<Uuid, Option<Uuid>> {
        let mut parents: BTreeMap<Uuid, Option<Uuid>> = self
            .0
            .values()
            .map(|f| (f.id, f.parent.filter(|p| self.0.contains_key(p))))
            .collect();
        let mut settled: BTreeSet<Uuid> = BTreeSet::new();
        for &start in self.0.keys() {
            let mut path: Vec<Uuid> = Vec::new();
            let mut at = Some(start);
            while let Some(x) = at {
                if settled.contains(&x) {
                    break;
                }
                if let Some(i) = path.iter().position(|p| *p == x) {
                    // cycles are disjoint here (each folder has one parent), so
                    // cutting each at its highest id doesn't depend on `start`
                    let top = *path[i..].iter().max().expect("a cycle has members");
                    parents.insert(top, None);
                    break;
                }
                path.push(x);
                at = parents[&x];
            }
            settled.extend(path);
        }
        parents
    }

    /// The nearest live folder at or above `from`, given [`Self::parents`].
    fn nearest_live(
        &self,
        parents: &BTreeMap<Uuid, Option<Uuid>>,
        from: Option<Uuid>,
    ) -> Option<Uuid> {
        let mut at = from;
        while let Some(x) = at {
            if self.is_live(x) {
                return Some(x);
            }
            at = parents.get(&x).copied().flatten();
        }
        None
    }

    /// The live folders as shown, each with the parent to show it under,
    /// sorted by name (ignoring case), then id.
    pub fn tree(&self) -> Vec<Folder> {
        let parents = self.parents();
        let mut out: Vec<Folder> = self
            .0
            .values()
            .filter(|f| f.deleted_at.is_none())
            .map(|f| Folder {
                parent: self.nearest_live(&parents, parents[&f.id]),
                ..f.clone()
            })
            .collect();
        out.sort_by_key(|f| (f.name.to_lowercase(), f.id));
        out
    }

    /// Where a notebook filed under `folder` is shown: `folder` itself, or the
    /// nearest live ancestor when it has been deleted, or `None` for the top
    /// level.
    pub fn shown_folder(&self, folder: Option<Uuid>) -> Option<Uuid> {
        self.nearest_live(&self.parents(), folder)
    }

    /// Whether moving `id` under `parent` would put it inside itself, judged
    /// by the tree as shown.
    pub fn would_cycle(&self, id: Uuid, parent: Uuid) -> bool {
        let parents = self.parents();
        let mut at = Some(parent);
        while let Some(x) = at {
            if x == id {
                return true;
            }
            at = parents.get(&x).copied().flatten();
        }
        false
    }
}

/// Applies a folder operation to `set` (single writer: the live library and
/// journal replay). Idempotent, so a journal suffix can be re-applied. Bodies
/// that aren't folder operations are refused.
pub fn apply_folder_op(set: &mut FolderSet, op: &LocalOperation) -> Result<(), OpError> {
    check_schema(op)?;
    let missing = |id: &Uuid| OpError::Precondition(format!("folder {id} does not exist"));
    match &op.body {
        DocumentOperation::FolderCreated {
            folder_id,
            name,
            parent,
        } => {
            validate_title(name).map_err(|e| OpError::Apply(e.into()))?;
            set.0.entry(*folder_id).or_insert_with(|| Folder {
                id: *folder_id,
                name: name.trim().to_string(),
                parent: *parent,
                created_at: op.at,
                deleted_at: None,
            });
        }
        DocumentOperation::FolderRenamed { folder_id, name } => {
            validate_title(name).map_err(|e| OpError::Apply(e.into()))?;
            let f = set.0.get_mut(folder_id).ok_or_else(|| missing(folder_id))?;
            f.name = name.trim().to_string();
        }
        DocumentOperation::FolderMoved { folder_id, parent } => {
            let f = set.0.get_mut(folder_id).ok_or_else(|| missing(folder_id))?;
            f.parent = *parent;
        }
        DocumentOperation::FolderDeleted { folder_id } => {
            let f = set.0.get_mut(folder_id).ok_or_else(|| missing(folder_id))?;
            f.deleted_at.get_or_insert(op.at);
        }
        other => {
            return Err(OpError::Precondition(format!(
                "{} is not a folder operation",
                other.name()
            )))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn set(rows: &[(u128, Option<u128>, bool)]) -> FolderSet {
        FolderSet(
            rows.iter()
                .map(|(i, p, dead)| {
                    (
                        id(*i),
                        Folder {
                            id: id(*i),
                            name: format!("f{i}"),
                            parent: p.map(id),
                            created_at: 0,
                            deleted_at: dead.then_some(1),
                        },
                    )
                })
                .collect(),
        )
    }

    fn parent_of(tree: &[Folder], n: u128) -> Option<Uuid> {
        tree.iter().find(|f| f.id == id(n)).unwrap().parent
    }

    #[test]
    fn deleted_folders_hand_their_contents_to_the_nearest_live_ancestor() {
        let s = set(&[(1, None, false), (2, Some(1), true), (3, Some(2), false)]);
        let t = s.tree();
        assert_eq!(t.len(), 2);
        assert_eq!(parent_of(&t, 3), Some(id(1)));
        assert_eq!(s.shown_folder(Some(id(2))), Some(id(1)));
        assert_eq!(s.shown_folder(Some(id(99))), None);
    }

    #[test]
    fn a_cycle_is_cut_at_its_highest_id() {
        let t = set(&[
            (1, Some(2), false),
            (2, Some(1), false),
            (3, Some(1), false),
        ])
        .tree();
        assert_eq!(parent_of(&t, 2), None);
        assert_eq!(parent_of(&t, 1), Some(id(2)));
        assert_eq!(parent_of(&t, 3), Some(id(1)));
    }

    #[test]
    fn a_self_parent_is_a_cycle_of_one() {
        let t = set(&[(1, Some(1), false)]).tree();
        assert_eq!(parent_of(&t, 1), None);
    }

    #[test]
    fn a_cycle_of_deleted_folders_ends_at_the_top_level() {
        let s = set(&[(1, Some(2), true), (2, Some(1), true), (3, Some(1), false)]);
        assert_eq!(parent_of(&s.tree(), 3), None);
    }

    #[test]
    fn moving_a_folder_into_its_own_subtree_is_detected() {
        let s = set(&[(1, None, false), (2, Some(1), false), (3, None, false)]);
        assert!(s.would_cycle(id(1), id(2)));
        assert!(s.would_cycle(id(1), id(1)));
        assert!(!s.would_cycle(id(2), id(3)));
    }
}
