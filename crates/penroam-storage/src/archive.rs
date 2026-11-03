//! Portable `.penroam` notebook archive (ZIP). Format spec:
//! `docs/formats/model-and-archive.md`.
//!
//! ```text
//! mimetype                 "application/vnd.penroam.notebook+zip" (first entry, stored)
//! manifest.json            format marker, versions, notebook, page + asset index
//! pages/<uuid>.prp         one canonical page per file, binary (journal folded in;
//!                          format 1 archives used pages/<uuid>.json and still import)
//! assets/<sha256>          original attachment bytes (PDFs, images)
//! ```
//!
//! Export and import use the same serialisation code on every platform.
//! Import is defensive: entry names are whitelisted (no traversal), sizes are
//! enforced while decompressing (not trusted from headers), compression ratios
//! are bounded, every checksum and cross-reference is verified, and the result
//! is staged and moved into the library atomically.

use crate::error::{Result, StorageError};
use crate::library::{sha256_hex, Library};
use penroam_core::migrate::notebook_from_value;
use penroam_core::validate::{is_sha256_hex, validate_notebook, validate_page};
use penroam_core::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, Write};
use ts_rs::TS;
use uuid::Uuid;

/// Content of the `mimetype` entry (first entry, uncompressed, so the file
/// type can be sniffed).
pub const MIMETYPE: &str = "application/vnd.penroam.notebook+zip";
/// `manifest.json` `format` marker.
pub const FORMAT: &str = "penroam-notebook";
/// 1: pages as `pages/<uuid>.json`. 2: pages as binary `pages/<uuid>.prp`
/// (`penroam_core::binary`). Import reads both.
pub const FORMAT_VERSION: u32 = 2;

/// Import limits (enforced while decompressing, not trusted from headers).
pub mod limits {
    /// Most entries an archive may contain.
    pub const MAX_ENTRIES: usize = 25_000;
    /// Largest `manifest.json`.
    pub const MAX_MANIFEST: u64 = 32 << 20;
    /// Largest single page entry.
    pub const MAX_PAGE: u64 = 512 << 20;
    /// Largest single asset entry.
    pub const MAX_ASSET: u64 = 1 << 30;
    /// Largest total uncompressed size.
    pub const MAX_TOTAL: u64 = 4 << 30;
    /// Max uncompressed/compressed ratio for entries larger than 1 MiB.
    pub const MAX_RATIO: u64 = 200;
}

/// `manifest.json`: everything needed to verify and rebuild the notebook.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Manifest {
    /// Always [`FORMAT`].
    pub format: String,
    /// Archive layout version (see [`FORMAT_VERSION`]).
    pub format_version: u32,
    /// Notebook model schema of `notebook`.
    pub schema_version: u32,
    /// Free-form producer string (app name and version).
    pub generator: String,
    /// Export time (ms since the epoch; informational).
    #[ts(type = "number")]
    pub exported_at: Timestamp,
    /// The notebook metadata (title, cover, page order, assets, …), kept as
    /// raw JSON so older schemas can be migrated on import.
    #[ts(type = "unknown")]
    pub notebook: serde_json::Value,
    /// Every page entry with its checksum.
    pub pages: Vec<ManifestPage>,
    /// Every asset entry with its checksum.
    pub assets: Vec<ManifestAsset>,
}

/// One page entry of the archive.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ManifestPage {
    /// Page id.
    pub id: Uuid,
    /// Entry name (`pages/<id>.prp`, or `.json` in format 1).
    pub path: String,
    /// SHA-256 of the entry bytes (verified on import).
    pub sha256: String,
    /// Entry size in bytes.
    #[ts(type = "number")]
    pub bytes: u64,
}

/// One asset entry of the archive.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ManifestAsset {
    /// SHA-256 of the asset bytes (also its address).
    pub sha256: String,
    /// Entry name (`assets/<sha256>`).
    pub path: String,
    /// MIME type, e.g. `application/pdf`.
    pub media_type: String,
    /// Size in bytes.
    #[ts(type = "number")]
    pub byte_size: u64,
}

/// Result of [`Library::import_archive`].
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportReport {
    /// The notebook as it now exists in the library.
    pub notebook: Notebook,
    /// True when the archive's notebook id already existed and a copy was made.
    pub renamed_copy: bool,
    /// Non-fatal findings (unknown entries ignored, trashed pages skipped, …).
    pub warnings: Vec<String>,
}

fn zerr(e: impl std::fmt::Display) -> StorageError {
    StorageError::Archive(e.to_string())
}

impl Library {
    /// Writes a snapshot of a notebook (live pages only) as a `.penroam` archive.
    pub fn export_archive<W: Write + Seek>(
        &mut self,
        nb_id: Uuid,
        out: W,
        generator: &str,
    ) -> Result<Manifest> {
        let nb = self.notebook(nb_id)?.clone();
        let mut zip = zip::ZipWriter::new(out);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file("mimetype", stored).map_err(zerr)?;
        zip.write_all(MIMETYPE.as_bytes())?;

        let mut pages = Vec::new();
        let mut used_assets: Vec<String> = Vec::new();
        for pid in nb.page_order.clone() {
            let bytes = self.with_page(nb_id, pid, |p| {
                for a in p.asset_refs() {
                    if !used_assets.iter().any(|u| u == a) {
                        used_assets.push(a.to_string());
                    }
                }
                penroam_core::binary::encode_page(p)
            })?;
            let path = format!("pages/{pid}.prp");
            zip.start_file(&path, deflated).map_err(zerr)?;
            zip.write_all(&bytes)?;
            pages.push(ManifestPage {
                id: pid,
                path,
                sha256: sha256_hex(&bytes),
                bytes: bytes.len() as u64,
            });
        }
        let mut assets = Vec::new();
        for hash in &used_assets {
            let meta = nb
                .assets
                .iter()
                .find(|a| &a.sha256 == hash)
                .ok_or_else(|| {
                    StorageError::Corrupt(format!("page references unknown asset {hash}"))
                })?;
            let bytes = self.read_asset(nb_id, hash)?;
            let path = format!("assets/{hash}");
            zip.start_file(&path, deflated).map_err(zerr)?;
            zip.write_all(&bytes)?;
            assets.push(ManifestAsset {
                sha256: hash.clone(),
                path,
                media_type: meta.media_type.clone(),
                byte_size: bytes.len() as u64,
            });
        }
        let mut snapshot = nb.clone();
        snapshot.trashed_pages.clear();
        let live: HashSet<Uuid> = snapshot.page_order.iter().copied().collect();
        snapshot.order_keys.retain(|p, _| live.contains(p));
        snapshot.deleted_at = None;
        snapshot.assets.retain(|a| used_assets.contains(&a.sha256));
        let manifest = Manifest {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            schema_version: NOTEBOOK_SCHEMA_VERSION,
            generator: generator.into(),
            exported_at: now_ms(),
            notebook: serde_json::to_value(&snapshot)
                .map_err(|e| StorageError::Io(e.to_string()))?,
            pages,
            assets,
        };
        zip.start_file("manifest.json", deflated).map_err(zerr)?;
        zip.write_all(
            &serde_json::to_vec_pretty(&manifest).map_err(|e| StorageError::Io(e.to_string()))?,
        )?;
        let mut w = zip.finish().map_err(zerr)?;
        w.flush()?;
        Ok(manifest)
    }

    /// Imports a `.penroam` archive as a new notebook.
    ///
    /// Everything is verified first (names, sizes, ratios, checksums,
    /// page/asset cross-references) and written to a staging folder; the
    /// finished folder is then moved into the library in one rename, so a
    /// failed or interrupted import leaves nothing behind. If the notebook id
    /// already exists, the import becomes a copy with a new id.
    pub fn import_archive<R: Read + Seek>(&mut self, input: R) -> Result<ImportReport> {
        let mut zip = zip::ZipArchive::new(input).map_err(zerr)?;
        if zip.len() > limits::MAX_ENTRIES {
            return Err(StorageError::TooLarge(format!(
                "archive has {} entries",
                zip.len()
            )));
        }
        let mut warnings = Vec::new();
        // ---- inventory and name whitelist
        let mut names: HashSet<String> = HashSet::new();
        let mut declared_total: u64 = 0;
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i).map_err(zerr)?;
            let name = f.name().to_string();
            if !names.insert(name.clone()) {
                return Err(StorageError::Archive(format!("duplicate entry {name}")));
            }
            if f.is_dir() {
                if !matches!(name.as_str(), "pages/" | "assets/") {
                    return Err(StorageError::Archive(format!(
                        "unexpected directory {name}"
                    )));
                }
                continue;
            }
            if !entry_name_ok(&name) {
                if safe_name(&name) {
                    warnings.push(format!("ignored unknown entry {name}"));
                    continue;
                }
                return Err(StorageError::Archive(format!("unsafe entry name {name:?}")));
            }
            let (size, comp) = (f.size(), f.compressed_size());
            if size > 1 << 20 && size / comp.max(1) > limits::MAX_RATIO {
                return Err(StorageError::Archive(format!(
                    "entry {name} has a suspicious compression ratio"
                )));
            }
            declared_total = declared_total.saturating_add(size);
        }
        if declared_total > limits::MAX_TOTAL {
            return Err(StorageError::TooLarge(
                "archive expands to more than 4 GiB".into(),
            ));
        }
        // ---- mimetype + manifest
        if names.contains("mimetype") {
            let m = read_entry(&mut zip, "mimetype", 256)?;
            if m != MIMETYPE.as_bytes() {
                return Err(StorageError::Archive("wrong mimetype".into()));
            }
        }
        let manifest: Manifest = serde_json::from_slice(&read_entry(
            &mut zip,
            "manifest.json",
            limits::MAX_MANIFEST,
        )?)
        .map_err(|e| StorageError::Archive(format!("manifest.json: {e}")))?;
        if manifest.format != FORMAT {
            return Err(StorageError::Archive(format!(
                "unknown format {:?}",
                manifest.format
            )));
        }
        if manifest.format_version > FORMAT_VERSION {
            return Err(StorageError::Archive(format!("archive format version {} is newer than this app supports ({FORMAT_VERSION}); please update Penroam", manifest.format_version)));
        }
        let mut nb = notebook_from_value(manifest.notebook.clone())
            .map_err(|e| StorageError::Archive(format!("notebook: {e}")))?;
        validate_notebook(&nb).map_err(|e| StorageError::Archive(format!("notebook: {}", e.0)))?;
        if !nb.trashed_pages.is_empty() {
            warnings.push("archive listed trashed pages; they were not imported".into());
            nb.trashed_pages.clear();
        }
        nb.deleted_at = None;
        let listed: Vec<Uuid> = manifest.pages.iter().map(|p| p.id).collect();
        if listed != nb.page_order {
            return Err(StorageError::Archive(
                "manifest page list does not match the notebook page order".into(),
            ));
        }
        // ---- stage
        // ids of purged notebooks still in the journal count as taken
        let renamed_copy = self.contains_notebook(nb.id);
        let old_id = nb.id;
        if renamed_copy {
            nb.id = Uuid::new_v4();
            nb.title = format!("{} (imported)", nb.title)
                .chars()
                .take(penroam_core::validate::limits::MAX_TITLE_CHARS)
                .collect();
        }
        let stage = self
            .staging_dir()
            .join(format!("import-{}", Uuid::new_v4()));
        let res = (|| -> Result<()> {
            fs::create_dir_all(stage.join("pages"))?;
            fs::create_dir_all(stage.join("assets"))?;
            // assets
            let mut have_assets: HashMap<String, u64> = HashMap::new();
            for a in &manifest.assets {
                if !is_sha256_hex(&a.sha256) || a.path != format!("assets/{}", a.sha256) {
                    return Err(StorageError::Archive(format!("bad asset entry {}", a.path)));
                }
                let bytes = read_entry(&mut zip, &a.path, limits::MAX_ASSET)?;
                if sha256_hex(&bytes) != a.sha256 {
                    return Err(StorageError::Archive(format!(
                        "asset {} is corrupt (checksum mismatch)",
                        a.sha256
                    )));
                }
                if bytes.len() as u64 != a.byte_size {
                    return Err(StorageError::Archive(format!(
                        "asset {} has the wrong size",
                        a.sha256
                    )));
                }
                crate::fsutil::write_atomic(&stage.join("assets").join(&a.sha256), &bytes)?;
                have_assets.insert(a.sha256.clone(), a.byte_size);
            }
            for a in &nb.assets {
                if !have_assets.contains_key(&a.sha256) {
                    return Err(StorageError::Archive(format!(
                        "notebook lists asset {} which is missing",
                        a.sha256
                    )));
                }
            }
            for h in have_assets.keys() {
                if !nb.assets.iter().any(|a| &a.sha256 == h) {
                    return Err(StorageError::Archive(format!(
                        "asset {h} is not listed by the notebook"
                    )));
                }
            }
            // pages
            for mp in &manifest.pages {
                let ext = if manifest.format_version >= 2 {
                    "prp"
                } else {
                    "json"
                };
                if mp.path != format!("pages/{}.{ext}", mp.id) {
                    return Err(StorageError::Archive(format!("bad page entry {}", mp.path)));
                }
                let bytes = read_entry(&mut zip, &mp.path, limits::MAX_PAGE)?;
                if sha256_hex(&bytes) != mp.sha256 {
                    return Err(StorageError::Archive(format!(
                        "page {} is corrupt (checksum mismatch)",
                        mp.id
                    )));
                }
                let page = crate::library::decode_page_bytes(&bytes)
                    .map_err(|e| StorageError::Archive(format!("page {}: {e}", mp.id)))?;
                validate_page(&page)
                    .map_err(|e| StorageError::Archive(format!("page {}: {}", mp.id, e.0)))?;
                if page.id != mp.id {
                    return Err(StorageError::Archive(format!(
                        "page file {} contains page {}",
                        mp.id, page.id
                    )));
                }
                for a in page.asset_refs() {
                    if !have_assets.contains_key(a) {
                        return Err(StorageError::Archive(format!(
                            "page {} references missing asset {a}",
                            mp.id
                        )));
                    }
                }
                // canonical bytes are written verbatim: lossless
                crate::fsutil::write_atomic(
                    &stage.join("pages").join(format!("{}.{ext}", mp.id)),
                    &bytes,
                )?;
            }
            nb.updated_at = nb.updated_at.max(manifest.exported_at);
            crate::fsutil::write_atomic(
                &stage.join("notebook.json"),
                &serde_json::to_vec_pretty(&nb).map_err(|e| StorageError::Io(e.to_string()))?,
            )?;
            Ok(())
        })();
        if let Err(e) = res {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
        if renamed_copy {
            warnings.push(format!(
                "a notebook with id {old_id} already exists; imported as a copy"
            ));
        }
        let nb = self.insert_imported(nb, &stage).inspect_err(|_| {
            let _ = fs::remove_dir_all(&stage);
        })?;
        Ok(ImportReport {
            notebook: nb,
            renamed_copy,
            warnings,
        })
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 512
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn entry_name_ok(name: &str) -> bool {
    if name == "mimetype" || name == "manifest.json" {
        return true;
    }
    if let Some(rest) = name.strip_prefix("pages/") {
        return rest
            .strip_suffix(".prp")
            .or_else(|| rest.strip_suffix(".json"))
            .is_some_and(|id| Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id));
    }
    if let Some(h) = name.strip_prefix("assets/") {
        return is_sha256_hex(h);
    }
    false
}

/// Reads one entry, enforcing `limit` on the actual decompressed byte count.
fn read_entry<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let f = zip
        .by_name(name)
        .map_err(|_| StorageError::Archive(format!("missing entry {name}")))?;
    if f.size() > limit {
        return Err(StorageError::TooLarge(format!("{name} is too large")));
    }
    let mut buf = Vec::with_capacity(f.size().min(64 << 20) as usize);
    let n = f
        .take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| StorageError::Archive(format!("{name}: {e}")))?;
    if n as u64 > limit {
        return Err(StorageError::TooLarge(format!(
            "{name} expands beyond its limit"
        )));
    }
    Ok(buf)
}
