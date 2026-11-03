//! Migration of legacy notebooks (notebook schema 1, per-page JSON-lines
//! journals `pages/<id>.journal`, pre-binary `pages/<id>.json` snapshots).
//!
//! Order (each step is atomic; a crash anywhere leaves a state the next
//! open migrates again, idempotently):
//!
//! 1. Copy `notebook.json` and every legacy file (plus the `.prp` it will
//!    overwrite) to `<library>/migration-backups/<ts>-legacy/<notebook>/`.
//! 2. Fold each legacy journal into its snapshot (records at or below the
//!    snapshot revision are skipped, exactly like the legacy loader) and write `.prp`.
//! 3. Write `notebook.json` at schema 2 — the commit point.
//! 4. Remove the legacy journals and JSON snapshots.
//!
//! Failure before step 3 leaves the notebook at schema 1 with all original
//! bytes in place (and backed up).

use crate::error::{Result, StorageError};
use crate::fsutil::{remove_durable, write_atomic};
use penroam_core::migrate::{notebook_from_slice, schema_version_of};
use penroam_core::validate::validate_page;
use penroam_core::*;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use uuid::Uuid;

#[derive(Deserialize)]
struct LegacyEntry {
    revision: u64,
    at: Timestamp,
    batch: OperationBatch,
}

#[derive(Default)]
pub(crate) struct Outcome {
    pub replayed: u32,
    pub truncated: u32,
    pub warnings: Vec<String>,
}

fn legacy_files(pages_dir: &Path, page: Uuid) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        pages_dir.join(format!("{page}.journal")),
        pages_dir.join(format!("{page}.json")),
    )
}

/// Whether the notebook directory still needs the legacy migration.
pub(crate) fn needed(nb_dir: &Path, raw: &[u8]) -> bool {
    let v1 = serde_json::from_slice::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| schema_version_of(&v))
        == Some(1);
    v1 || fs::read_dir(nb_dir.join("pages")).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.ends_with(".journal") || (n.ends_with(".json") && !n.contains(".tmp-"))
        })
    })
}

pub(crate) fn migrate(root: &Path, nb_dir: &Path, raw: &[u8]) -> Result<(Notebook, Outcome)> {
    let mut out = Outcome::default();
    let mut nb = notebook_from_slice(raw).map_err(|e| StorageError::Corrupt(e.to_string()))?;
    let pages_dir = nb_dir.join("pages");

    // 1. backups
    let backup = root
        .join("migration-backups")
        .join(format!("{}-legacy", now_ms()))
        .join(nb.id.to_string());
    write_atomic(&backup.join("notebook.json"), raw)?;
    let mut work = Vec::new();
    for page in nb.all_page_ids().collect::<Vec<_>>() {
        let (journal, json) = legacy_files(&pages_dir, page);
        let prp = pages_dir.join(format!("{page}.prp"));
        if !journal.exists() && !json.exists() {
            continue;
        }
        for p in [&journal, &json, &prp] {
            if let Ok(bytes) = fs::read(p) {
                write_atomic(&backup.join(p.file_name().unwrap()), &bytes)?;
            }
        }
        work.push((page, journal, json, prp));
    }

    // 2. fold legacy journals into binary snapshots
    for (page_id, journal, json, prp) in &work {
        let snap = match fs::read(prp) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::read(json)?,
            Err(e) => return Err(e.into()),
        };
        let mut page = crate::library::decode_page_bytes(&snap)
            .map_err(|e| StorageError::Corrupt(format!("page {page_id}: {e}")))?;
        validate_page(&page)
            .map_err(|e| StorageError::Corrupt(format!("page {page_id}: {}", e.0)))?;
        let before = page.revision;
        if let Ok(bytes) = fs::read(journal) {
            for line in bytes.split_inclusive(|b| *b == b'\n') {
                let entry: Option<LegacyEntry> = line
                    .ends_with(b"\n")
                    .then(|| serde_json::from_slice(line).ok())
                    .flatten();
                let Some(entry) = entry else {
                    out.truncated += 1;
                    out.warnings.push(format!(
                        "page {page_id}: discarded an incomplete legacy journal record (crash during write)"
                    ));
                    break;
                };
                if entry.revision <= page.revision {
                    continue;
                }
                if entry.revision != page.revision + 1 {
                    out.warnings.push(format!(
                        "page {page_id}: legacy journal gap at revision {}",
                        entry.revision
                    ));
                    break;
                }
                if let Err(e) = ops::replay_batch(&mut page, &entry.batch, entry.revision, entry.at)
                {
                    out.warnings.push(format!(
                        "page {page_id}: legacy journal record {} could not be replayed: {e}",
                        entry.revision
                    ));
                    break;
                }
                out.replayed += 1;
            }
        }
        if page.revision != before || !prp.exists() {
            write_atomic(prp, &penroam_core::binary::encode_page(&page))?;
        }
    }

    // 3. commit: notebook.json at the current schema
    if !nb.order_is_consistent() {
        nb.rebuild_order_keys();
    }
    write_atomic(
        &nb_dir.join("notebook.json"),
        &serde_json::to_vec_pretty(&nb).map_err(|e| StorageError::Io(e.to_string()))?,
    )?;

    // 4. remove legacy files (their bytes are in the backup)
    for (_, journal, json, _) in &work {
        remove_durable(journal)?;
        remove_durable(json)?;
    }
    if !work.is_empty() || out.replayed > 0 {
        out.warnings.push(format!(
            "notebook {}: migrated from the legacy storage layout (backup in {})",
            nb.id,
            backup.display()
        ));
    }
    Ok((nb, out))
}
