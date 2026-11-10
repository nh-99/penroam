//! Full-library backups (`.penroam-backup`, ZIP). Format spec:
//! `docs/formats/portability.md`.
//!
//! ```text
//! mimetype                  "application/vnd.penroam.library-backup+zip" (first entry, stored)
//! backup.json               format marker, versions, one row per notebook
//! notebooks/<uuid>.penroam  a complete notebook archive, trash included (stored)
//! extras/<name>             optional app data, such as search corrections
//! ```
//!
//! A backup is a thin wrapper around ordinary `.penroam` archives, so the
//! defensive import in [`crate::archive`] is the only code that reads notebook
//! content, and any one notebook can be unzipped and imported by hand. Nested
//! archives are written with [`ExportOptions::deterministic`], so an unchanged
//! notebook has the same checksum in every backup.
//!
//! **Invariants.**
//! * A restore never overwrites. A notebook whose id is already in the library
//!   is skipped unless the caller asks for a copy with a new id.
//! * Every notebook is checked on its own. A damaged one is reported and the
//!   rest are still restored; the existing library is never touched by a failure.
//! * Exporting carries on past a notebook that cannot be read and lists it in
//!   [`BackupManifest::unreadable`], so a backup is never silently incomplete.
//! * App data outside any notebook (search corrections) sits under `extras/`.
//!   Readers ignore names they don't know.
//! * Nested archives are unpacked to files in the staging folder, with sizes
//!   enforced while copying, so a backup can be larger than memory.

use crate::archive::{ArchiveSummary, ExportOptions, ImportOptions};
use crate::error::{Result, StorageError};
use crate::library::Library;
use penroam_core::{now_ms, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use ts_rs::TS;
use uuid::Uuid;

/// Content of the `mimetype` entry.
pub const BACKUP_MIMETYPE: &str = "application/vnd.penroam.library-backup+zip";
/// `backup.json` `format` marker.
pub const BACKUP_FORMAT: &str = "penroam-backup";
/// Layout version of a backup. Newer versions are refused.
pub const BACKUP_FORMAT_VERSION: u32 = 1;
/// Most notebooks a backup may list.
pub const MAX_BACKUP_NOTEBOOKS: usize = 20_000;
/// Largest `backup.json`.
const MAX_BACKUP_MANIFEST: u64 = 64 << 20;
/// Largest nested archive: the most a notebook archive may expand to.
const MAX_NESTED_ARCHIVE: u64 = crate::archive::limits::MAX_TOTAL;

/// `backup.json`: what the backup holds.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupManifest {
    /// Always [`BACKUP_FORMAT`].
    pub format: String,
    /// See [`BACKUP_FORMAT_VERSION`].
    pub format_version: u32,
    /// Application that wrote the backup.
    pub generator: String,
    /// When the backup was made (ms since the epoch; informational).
    #[ts(type = "number")]
    pub created_at: Timestamp,
    /// One row per notebook in the backup.
    pub notebooks: Vec<BackupEntry>,
    /// Notebooks that could not be read when the backup was made. They are
    /// not in this backup.
    #[serde(default)]
    pub unreadable: Vec<BackupProblem>,
}

/// One notebook in a backup.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupEntry {
    /// Notebook id.
    pub id: Uuid,
    /// Title when backed up.
    pub title: String,
    /// Entry name (`notebooks/<id>.penroam`).
    pub path: String,
    /// SHA-256 of the nested archive.
    pub sha256: String,
    /// Size of the nested archive.
    #[ts(type = "number")]
    pub bytes: u64,
}

/// A notebook that a backup or restore could not handle.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupProblem {
    /// Notebook id.
    pub id: Uuid,
    /// Title, as far as it is known.
    pub title: String,
    /// What went wrong.
    pub error: String,
}

/// A checked backup, for the restore confirmation screen.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupPreview {
    /// Application that wrote the backup.
    pub generator: String,
    /// When the backup was made.
    #[ts(type = "number")]
    pub created_at: Timestamp,
    /// Notebooks that passed every check.
    pub notebooks: Vec<ArchiveSummary>,
    /// Notebooks in the backup that failed a check.
    pub damaged: Vec<BackupProblem>,
    /// Notebooks that were already unreadable when the backup was made.
    pub unreadable: Vec<BackupProblem>,
}

/// What to do about a notebook that is already in the library.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ConflictPolicy {
    /// Leave the library's notebook alone and skip this one.
    #[default]
    Skip,
    /// Restore it next to the existing one, with a new id.
    Copy,
}

/// Choices for [`Library::restore_backup`].
#[derive(Debug, Clone, Default, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestoreOptions {
    /// Handling of notebooks already in the library.
    #[serde(default)]
    pub on_conflict: ConflictPolicy,
    /// Restore only these notebooks. `None` restores everything.
    #[serde(default)]
    #[ts(optional)]
    pub only: Option<Vec<Uuid>>,
}

/// Per-notebook result of a restore.
#[derive(Debug, Clone, Default, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestoreReport {
    /// Ids of the notebooks now in the library, new copies included.
    pub restored: Vec<Uuid>,
    /// Backup id and library id of each restored notebook. They differ for copies.
    pub mapped: Vec<(Uuid, Uuid)>,
    /// Restored next to an existing notebook, under a new id.
    pub copied: u32,
    /// Titles left out because they are already in the library.
    pub skipped: Vec<String>,
    /// Notebooks that could not be restored. The rest were.
    pub failed: Vec<BackupProblem>,
    /// Non-fatal findings from the notebooks that were restored.
    pub warnings: Vec<String>,
}

fn zerr(e: impl std::fmt::Display) -> StorageError {
    StorageError::Archive(e.to_string())
}

fn io_json(e: serde_json::Error) -> StorageError {
    StorageError::Io(e.to_string())
}

/// Deletes a staging file when dropped, on every exit path.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Hashes everything that passes through it.
struct Hashing<W> {
    inner: W,
    hash: Sha256,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn open_temp(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?)
}

/// Copies one zip entry to a new staging file, hashing it and enforcing the
/// size on the bytes actually read.
fn unpack_entry<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    entry: &BackupEntry,
    dest: &Path,
) -> Result<(File, String, u64)> {
    let f = zip
        .by_name(&entry.path)
        .map_err(|_| StorageError::Archive(format!("missing entry {}", entry.path)))?;
    let mut out = Hashing {
        inner: open_temp(dest)?,
        hash: Sha256::new(),
    };
    let n = io::copy(&mut f.take(MAX_NESTED_ARCHIVE + 1), &mut out)
        .map_err(|e| StorageError::Archive(format!("{}: {e}", entry.path)))?;
    if n > MAX_NESTED_ARCHIVE {
        return Err(StorageError::TooLarge(format!(
            "{} is too large",
            entry.path
        )));
    }
    let digest = hex(&out.hash.finalize());
    let mut file = out.inner;
    file.seek(SeekFrom::Start(0))?;
    Ok((file, digest, n))
}

/// Opens a backup and validates its outer structure.
fn open_backup<R: Read + Seek>(input: R) -> Result<(zip::ZipArchive<R>, BackupManifest)> {
    let mut zip = zip::ZipArchive::new(input).map_err(zerr)?;
    if zip.len() > MAX_BACKUP_NOTEBOOKS + 2 {
        return Err(StorageError::TooLarge(format!(
            "backup has {} entries",
            zip.len()
        )));
    }
    let mime = read_small(&mut zip, "mimetype", 256)?;
    if mime != BACKUP_MIMETYPE.as_bytes() {
        return Err(StorageError::Archive("this is not a Penroam backup".into()));
    }
    let manifest: BackupManifest =
        serde_json::from_slice(&read_small(&mut zip, "backup.json", MAX_BACKUP_MANIFEST)?)
            .map_err(|e| StorageError::Archive(format!("backup.json: {e}")))?;
    if manifest.format != BACKUP_FORMAT {
        return Err(StorageError::Archive(format!(
            "unknown format {:?}",
            manifest.format
        )));
    }
    if manifest.format_version > BACKUP_FORMAT_VERSION {
        return Err(StorageError::Archive(format!(
            "backup format version {} is newer than this app supports ({BACKUP_FORMAT_VERSION}); please update Penroam",
            manifest.format_version
        )));
    }
    if manifest.notebooks.len() > MAX_BACKUP_NOTEBOOKS {
        return Err(StorageError::TooLarge(
            "backup lists too many notebooks".into(),
        ));
    }
    let mut ids = HashSet::new();
    for e in &manifest.notebooks {
        if e.path != format!("notebooks/{}.penroam", e.id) || !ids.insert(e.id) {
            return Err(StorageError::Archive(format!(
                "bad notebook entry {:?}",
                e.path
            )));
        }
    }
    Ok((zip, manifest))
}

fn read_small<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let f = zip
        .by_name(name)
        .map_err(|_| StorageError::Archive(format!("missing entry {name}")))?;
    let mut buf = Vec::new();
    f.take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| StorageError::Archive(format!("{name}: {e}")))?;
    if buf.len() as u64 > limit {
        return Err(StorageError::TooLarge(format!("{name} is too large")));
    }
    Ok(buf)
}

/// Writes a backup one notebook at a time. Callers can release the library
/// between notebooks, so a long backup doesn't stop the user writing, and can
/// report progress. Each notebook is a consistent snapshot of its own moment.
pub struct BackupWriter<W: Write + Seek> {
    zip: zip::ZipWriter<W>,
    generator: String,
    notebooks: Vec<BackupEntry>,
    unreadable: Vec<BackupProblem>,
}

impl<W: Write + Seek> BackupWriter<W> {
    /// Starts a backup in `out`.
    pub fn new(out: W, generator: &str) -> Result<Self> {
        let mut zip = zip::ZipWriter::new(out);
        let small = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("mimetype", small).map_err(zerr)?;
        zip.write_all(BACKUP_MIMETYPE.as_bytes())?;
        Ok(Self {
            zip,
            generator: generator.into(),
            notebooks: Vec::new(),
            unreadable: Vec::new(),
        })
    }

    /// Adds one notebook, trash included. A notebook that can't be read is
    /// listed in [`BackupManifest::unreadable`] and a notebook that no longer
    /// exists is skipped. Errors only for failures of the backup file itself.
    pub fn add_notebook(&mut self, lib: &mut Library, id: Uuid) -> Result<()> {
        let title = match lib.notebook(id) {
            Ok(n) => n.title.clone(),
            Err(StorageError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        let tmp = TempFile(
            lib.staging_dir()
                .join(format!("backup-{}.penroam", Uuid::new_v4())),
        );
        let mut file = open_temp(&tmp.0)?;
        let opts = ExportOptions {
            include_trash: true,
            deterministic: true,
            ..Default::default()
        };
        match lib.export_archive_with(id, &mut file, &self.generator, &opts) {
            Ok(_) => {}
            // the disk filling up is a failure of the backup, not of this notebook
            Err(e @ (StorageError::StorageFull(_) | StorageError::Io(_))) => return Err(e),
            Err(e) => {
                lib.recovery
                    .warnings
                    .push(format!("notebook {id} left out of the backup: {e}"));
                self.unreadable.push(BackupProblem {
                    id,
                    title,
                    error: e.to_string(),
                });
                return Ok(());
            }
        }
        file.seek(SeekFrom::Start(0))?;
        let path = format!("notebooks/{id}.penroam");
        // nested archives are already deflated
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        self.zip.start_file(&path, stored).map_err(zerr)?;
        let mut w = Hashing {
            inner: &mut self.zip,
            hash: Sha256::new(),
        };
        let bytes = io::copy(&mut file, &mut w)?;
        let sha256 = hex(&w.hash.finalize());
        self.notebooks.push(BackupEntry {
            id,
            title,
            path,
            sha256,
            bytes,
        });
        Ok(())
    }

    /// Adds app data that isn't part of any notebook, such as search corrections.
    /// `name` must start with `extras/`. Read it back with [`BackupReader::extra`].
    pub fn add_extra(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        debug_assert!(name.starts_with("extras/"));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        self.zip.start_file(name, opts).map_err(zerr)?;
        self.zip.write_all(bytes)?;
        Ok(())
    }

    /// Writes the manifest and closes the backup.
    pub fn finish(mut self) -> Result<BackupManifest> {
        let manifest = BackupManifest {
            format: BACKUP_FORMAT.into(),
            format_version: BACKUP_FORMAT_VERSION,
            generator: self.generator,
            created_at: now_ms(),
            notebooks: self.notebooks,
            unreadable: self.unreadable,
        };
        let small = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        self.zip.start_file("backup.json", small).map_err(zerr)?;
        self.zip
            .write_all(&serde_json::to_vec_pretty(&manifest).map_err(io_json)?)?;
        self.zip.finish().map_err(zerr)?.flush()?;
        Ok(manifest)
    }
}

/// An opened backup, read one notebook at a time so that callers can release
/// the library between notebooks and report progress.
pub struct BackupReader<R: Read + Seek> {
    zip: zip::ZipArchive<R>,
    manifest: BackupManifest,
}

impl<R: Read + Seek> BackupReader<R> {
    /// Opens a backup. Errors when it can't be used at all: not a backup, cut
    /// short, from a newer app, or with entries that break the format.
    pub fn open(input: R) -> Result<Self> {
        let (zip, manifest) = open_backup(input)?;
        Ok(Self { zip, manifest })
    }

    /// What the backup says it holds.
    pub fn manifest(&self) -> &BackupManifest {
        &self.manifest
    }

    /// Reads extra data stored with [`BackupWriter::add_extra`], if present.
    pub fn extra(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        if self.zip.index_for_name(name).is_none() {
            return Ok(None);
        }
        read_small(&mut self.zip, name, MAX_BACKUP_MANIFEST).map(Some)
    }

    /// Fully checks the notebook at `index` of [`BackupManifest::notebooks`].
    pub fn check(&mut self, lib: &Library, index: usize) -> Result<ArchiveSummary> {
        let entry = self.manifest.notebooks[index].clone();
        let (file, _tmp) = unpack_checked(lib, &mut self.zip, &entry)?;
        let summary = lib.verify_archive(file)?;
        if summary.notebook_id != entry.id {
            return Err(StorageError::Archive(format!(
                "{} contains notebook {}",
                entry.path, summary.notebook_id
            )));
        }
        Ok(summary)
    }

    /// Restores the notebook at `index`, recording the outcome in `report`.
    /// Never overwrites: see [`ConflictPolicy`].
    pub fn restore(
        &mut self,
        lib: &mut Library,
        index: usize,
        options: &RestoreOptions,
        report: &mut RestoreReport,
    ) {
        let entry = self.manifest.notebooks[index].clone();
        if options
            .only
            .as_ref()
            .is_some_and(|o| !o.contains(&entry.id))
        {
            return;
        }
        let exists = lib.contains_notebook(entry.id);
        if exists && options.on_conflict == ConflictPolicy::Skip {
            report.skipped.push(entry.title.clone());
            return;
        }
        let res = unpack_checked(lib, &mut self.zip, &entry).and_then(|(file, _tmp)| {
            lib.import_archive_with(
                file,
                ImportOptions {
                    keep_trash: true,
                    keep_folder: true,
                },
            )
        });
        match res {
            Ok(r) => {
                report.mapped.push((entry.id, r.notebook.id));
                report.restored.push(r.notebook.id);
                report.copied += r.renamed_copy as u32;
                report.warnings.extend(r.warnings);
            }
            Err(e) => report.failed.push(BackupProblem {
                id: entry.id,
                title: entry.title.clone(),
                error: e.to_string(),
            }),
        }
    }
}

fn unpack_checked<R: Read + Seek>(
    lib: &Library,
    zip: &mut zip::ZipArchive<R>,
    entry: &BackupEntry,
) -> Result<(File, TempFile)> {
    let tmp = TempFile(
        lib.staging_dir()
            .join(format!("restore-{}.penroam", Uuid::new_v4())),
    );
    let (file, digest, bytes) = unpack_entry(zip, entry, &tmp.0)?;
    if digest != entry.sha256 || bytes != entry.bytes {
        return Err(StorageError::Archive(format!(
            "{} is corrupt (checksum mismatch)",
            entry.path
        )));
    }
    Ok((file, tmp))
}

impl Library {
    /// Writes every notebook, trash included, to a backup. See [`BackupWriter`]
    /// to release the library between notebooks.
    pub fn export_backup<W: Write + Seek>(
        &mut self,
        out: W,
        generator: &str,
    ) -> Result<BackupManifest> {
        let mut w = BackupWriter::new(out, generator)?;
        let mut ids = self.notebook_ids();
        ids.sort();
        for id in ids {
            w.add_notebook(self, id)?;
        }
        w.finish()
    }

    /// Checks every notebook in a backup and describes the result. The
    /// library is not changed. Errors only when the backup as a whole can't
    /// be opened; damage to single notebooks is reported in the preview.
    pub fn preview_backup<R: Read + Seek>(&self, input: R) -> Result<BackupPreview> {
        let mut r = BackupReader::open(input)?;
        let mut p = r.empty_preview();
        for i in 0..r.manifest.notebooks.len() {
            r.preview_one(self, i, &mut p);
        }
        Ok(p)
    }

    /// Restores notebooks from a backup. Never overwrites: see
    /// [`ConflictPolicy`]. Each notebook is verified and installed on its
    /// own, so a failure leaves the others restored and the existing library
    /// as it was. Errors only when the backup as a whole can't be opened.
    pub fn restore_backup<R: Read + Seek>(
        &mut self,
        input: R,
        options: &RestoreOptions,
    ) -> Result<RestoreReport> {
        let mut r = BackupReader::open(input)?;
        let mut report = RestoreReport::default();
        for i in 0..r.manifest.notebooks.len() {
            r.restore(self, i, options, &mut report);
        }
        Ok(report)
    }
}

impl<R: Read + Seek> BackupReader<R> {
    /// A preview with nothing checked yet; fill it with [`Self::preview_one`].
    pub fn empty_preview(&self) -> BackupPreview {
        BackupPreview {
            generator: self.manifest.generator.clone(),
            created_at: self.manifest.created_at,
            notebooks: Vec::new(),
            damaged: Vec::new(),
            unreadable: self.manifest.unreadable.clone(),
        }
    }

    /// Checks one notebook and adds the result to `preview`.
    pub fn preview_one(&mut self, lib: &Library, index: usize, preview: &mut BackupPreview) {
        match self.check(lib, index) {
            Ok(summary) => preview.notebooks.push(summary),
            Err(e) => {
                let entry = &self.manifest.notebooks[index];
                preview.damaged.push(BackupProblem {
                    id: entry.id,
                    title: entry.title.clone(),
                    error: e.to_string(),
                });
            }
        }
    }
}
