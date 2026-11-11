//! The derived search index: one SQLite database with an FTS5 full-text table.
//!
//! # What it holds
//!
//! One row per searchable piece of a page (`entries`): typed text blocks, recognized
//! handwriting regions, the selectable text of a PDF page background, and notebook titles.
//! Each row keeps where it came from (notebook, page, element ids or region key, page
//! revision, bounds, the recognizer id/version/language and its confidence) so a hit can open
//! the page and highlight the source without the index ever being a copy of the note.
//! Per page, `pages` records the revision that was indexed and whether every region was
//! recognized. `pdf_text` caches the text of each PDF asset. `corrections` holds the user's
//! corrected readings of handwriting regions, keyed by region identity.
//!
//! # Invariants
//!
//! * The index is **derived and disposable**. Deleting the file loses nothing but
//!   corrections; [`SearchIndex::open`] recreates it when it is unreadable, from another
//!   schema version, or from another library, and the app re-indexes from the notes.
//! * It never writes to notes. Source ink stays vectors; recognized text lives only here.
//! * A page's rows are replaced in one transaction, so a crash leaves either the old or the
//!   new rows, and the page's recorded revision always matches its rows.
//! * Recognition never happens inside a write transaction: [`SearchIndex::index_page`] reads
//!   the cache, recognizes, then writes.
//!
//! Queries are parsed by [`crate::query`]; ranking is described at [`SearchIndex::search`].
//! Design notes: `docs/architecture/overview.md` (search) and `docs/security/search-index.md`.

use crate::query::{self, ParsedQuery};
use crate::recognize::{RecognitionCandidate, Recognizer, RecognizerInfo};
use crate::segment::{self, Rect};
use penroam_core::{Element, Notebook, Page, PdfBackgroundRef};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Index schema version (SQLite `user_version`). A file with another version is recreated.
pub const SCHEMA_VERSION: i64 = 1;

/// Index failures. None of them affect notes; the worst case is re-indexing.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// SQLite reported an error (disk full, I/O, a damaged file).
    #[error("search index error: {0}")]
    Db(#[from] rusqlite::Error),
    /// Creating or removing the index file failed.
    #[error("search index file error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, SearchError>;

/// What kind of source an entry (and so a hit) comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    /// A notebook title.
    Title,
    /// A typed text block.
    Text,
    /// Recognized handwriting.
    Ink,
    /// Selectable text of a PDF page background.
    Pdf,
}

impl EntryKind {
    fn as_str(self) -> &'static str {
        match self {
            EntryKind::Title => "title",
            EntryKind::Text => "text",
            EntryKind::Ink => "ink",
            EntryKind::Pdf => "pdf",
        }
    }

    fn parse(s: &str) -> EntryKind {
        match s {
            "title" => EntryKind::Title,
            "text" => EntryKind::Text,
            "pdf" => EntryKind::Pdf,
            _ => EntryKind::Ink,
        }
    }

    /// Ranking weight: titles first, then typed text, then handwriting, then PDF text.
    fn weight(self) -> f64 {
        match self {
            EntryKind::Title => 4.0,
            EntryKind::Text => 2.0,
            EntryKind::Ink => 1.5,
            EntryKind::Pdf => 1.0,
        }
    }
}

/// One recognized word of a handwriting entry (stored as JSON).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// Best reading.
    pub t: String,
    /// Alternate readings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alts: Vec<String>,
    /// Bounds in page points.
    pub b: Rect,
    /// Source strokes.
    pub ids: Vec<Uuid>,
    /// Confidence, when the recognizer reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c: Option<f32>,
}

/// One piece of a highlighted snippet.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetPart {
    /// The text.
    pub text: String,
    /// Whether it matched the query.
    pub hit: bool,
}

/// A search result: where the match is and how to show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// Notebook id.
    pub notebook_id: Uuid,
    /// Notebook title (as indexed).
    pub notebook_title: String,
    /// Page id (`None` for a title hit).
    pub page_id: Option<Uuid>,
    /// Zero-based position of the page in the notebook.
    pub page_index: Option<u32>,
    /// Source kind.
    pub kind: EntryKind,
    /// The matched text with the matching words marked.
    pub snippet: Vec<SnippetPart>,
    /// Bounds of the whole source (text block, handwriting line) in page points.
    pub bounds: Option<Rect>,
    /// Bounds of just the matching handwritten words, when they can be told apart.
    pub match_bounds: Vec<Rect>,
    /// Region key of a handwriting hit (for corrections).
    pub region_key: Option<String>,
    /// Recognizer confidence of a handwriting hit.
    pub confidence: Option<f32>,
    /// The match needed fuzzy expansion (likely a recognition error or a typo).
    pub fuzzy: bool,
    /// Ranking score (higher is better; only meaningful within one result list).
    pub score: f64,
}

/// Index counters for the settings screen.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStats {
    /// Pages with rows.
    pub pages: u64,
    /// Pages whose handwriting is not fully recognized yet.
    pub incomplete_pages: u64,
    /// Entries by kind.
    pub text_entries: u64,
    /// Handwriting entries.
    pub ink_entries: u64,
    /// PDF page entries.
    pub pdf_entries: u64,
    /// User corrections.
    pub corrections: u64,
}

/// What the index knows about one page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageState {
    /// Page revision the rows were built from.
    pub revision: u64,
    /// `recognizer id/model version/language` used, or empty when handwriting was not
    /// recognized.
    pub recognizer: String,
    /// Every region was recognized (false after a cancelled or failed pass).
    pub complete: bool,
}

/// Recognition settings for [`SearchIndex::index_page`].
pub struct Recognition<'a> {
    /// The provider.
    pub recognizer: &'a mut dyn Recognizer,
    /// BCP 47 language tag.
    pub language: &'a str,
}

/// What [`SearchIndex::index_page`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageOutcome {
    /// Handwriting regions on the page.
    pub regions: usize,
    /// Regions sent to the recognizer.
    pub recognized: usize,
    /// Regions whose earlier result was reused (unchanged ink).
    pub reused: usize,
    /// Regions left for a later pass (cancelled or recognizer error).
    pub pending: usize,
    /// The last recognizer error, if any (the regions are retried later).
    pub error: Option<String>,
}

fn recognizer_key(info: &RecognizerInfo, language: &str) -> String {
    format!("{}/{}/{}", info.id, info.model_version, language)
}

fn rect_json(r: &Rect) -> String {
    serde_json::to_string(r).expect("rect serializes")
}

/// The open index. One writer (the indexing worker) and any number of readers
/// ([`SearchIndex::open_reader`]) may use the same file concurrently (WAL mode).
pub struct SearchIndex {
    conn: Connection,
    path: Option<PathBuf>,
}

const SCHEMA: &str = r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE notebooks (
  notebook TEXT PRIMARY KEY,
  title TEXT NOT NULL
);
CREATE TABLE pages (
  notebook TEXT NOT NULL,
  page TEXT NOT NULL,
  page_index INTEGER NOT NULL,
  revision INTEGER NOT NULL,
  recognizer TEXT NOT NULL,
  complete INTEGER NOT NULL,
  PRIMARY KEY (notebook, page)
);
CREATE TABLE entries (
  id INTEGER PRIMARY KEY,
  notebook TEXT NOT NULL,
  page TEXT,
  kind TEXT NOT NULL,
  source_key TEXT NOT NULL,
  fingerprint TEXT,
  element_ids TEXT NOT NULL,
  text TEXT NOT NULL,
  alternates TEXT NOT NULL,
  correction TEXT NOT NULL DEFAULT '',
  confidence REAL,
  recognizer TEXT,
  source_revision INTEGER,
  bounds TEXT,
  words TEXT
);
CREATE INDEX entries_page ON entries (notebook, page);
CREATE VIRTUAL TABLE entries_fts USING fts5 (
  text, alternates, correction,
  content = 'entries', content_rowid = 'id',
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3'
);
CREATE VIRTUAL TABLE entries_vocab USING fts5vocab (entries_fts, 'row');
CREATE TRIGGER entries_ai AFTER INSERT ON entries BEGIN
  INSERT INTO entries_fts (rowid, text, alternates, correction)
  VALUES (new.id, new.text, new.alternates, new.correction);
END;
CREATE TRIGGER entries_ad AFTER DELETE ON entries BEGIN
  INSERT INTO entries_fts (entries_fts, rowid, text, alternates, correction)
  VALUES ('delete', old.id, old.text, old.alternates, old.correction);
END;
CREATE TRIGGER entries_au AFTER UPDATE ON entries BEGIN
  INSERT INTO entries_fts (entries_fts, rowid, text, alternates, correction)
  VALUES ('delete', old.id, old.text, old.alternates, old.correction);
  INSERT INTO entries_fts (rowid, text, alternates, correction)
  VALUES (new.id, new.text, new.alternates, new.correction);
END;
CREATE TABLE pdf_assets (asset TEXT PRIMARY KEY, pages INTEGER NOT NULL);
CREATE TABLE pdf_text (
  asset TEXT NOT NULL, page_index INTEGER NOT NULL, text TEXT NOT NULL,
  PRIMARY KEY (asset, page_index)
);
CREATE TABLE corrections (
  notebook TEXT NOT NULL, page TEXT NOT NULL, region TEXT NOT NULL, text TEXT NOT NULL,
  PRIMARY KEY (notebook, page, region)
);
"#;

/// The PDF pages a page shows, with an entry key for each and the area it
/// covers: its background (the whole page), then the PDF page elements of an
/// infinite canvas. Keys start `pdf:<asset>:<page index>`, which scrubbing
/// relies on; an element's id follows, since one PDF page can be placed twice.
fn pdf_sources(page: &Page) -> Vec<(String, &PdfBackgroundRef, Rect)> {
    let mut out = Vec::new();
    if let Some(bg) = &page.background {
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: page.size.width,
            h: page.size.height,
        };
        out.push((
            format!("pdf:{}:{}", bg.asset_hash, bg.page_index),
            bg,
            bounds,
        ));
    }
    for e in &page.elements {
        if let Element::Image(i) = e {
            if let Some(bg) = &i.pdf_page {
                let bounds = Rect {
                    x: i.x,
                    y: i.y,
                    w: i.width,
                    h: i.height,
                };
                let key = format!("pdf:{}:{}:{}", bg.asset_hash, bg.page_index, i.id);
                out.push((key, bg, bounds));
            }
        }
    }
    out
}

impl SearchIndex {
    /// Opens the index at `path` for writing, creating it if needed. `library_id` names the
    /// library the index belongs to: an index built for another library (or with another
    /// schema version, or unreadable) is deleted and recreated, so indexes never mix.
    /// Returns the index and whether it was (re)created empty.
    pub fn open(path: &Path, library_id: Uuid) -> Result<(SearchIndex, bool)> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        match Self::try_open(path, library_id) {
            Ok(Some(ix)) => Ok((ix, false)),
            Ok(None) | Err(_) => {
                Self::delete_files(path)?;
                let conn = Connection::open(path)?;
                let ix = Self::init(conn, Some(path.to_path_buf()), library_id)?;
                Ok((ix, true))
            }
        }
    }

    fn try_open(path: &Path, library_id: Uuid) -> Result<Option<SearchIndex>> {
        if !path.exists() {
            return Ok(None);
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if v != SCHEMA_VERSION {
            return Ok(None);
        }
        let lib: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'library'", [], |r| {
                r.get(0)
            })
            .optional()?;
        if lib.as_deref() != Some(&library_id.to_string()) {
            return Ok(None);
        }
        let ok: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if ok != "ok" {
            return Ok(None);
        }
        Self::tune(&conn)?;
        Ok(Some(SearchIndex {
            conn,
            path: Some(path.to_path_buf()),
        }))
    }

    /// Opens an existing index read-only, for queries alongside the writer.
    pub fn open_reader(path: &Path) -> Result<SearchIndex> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        Ok(SearchIndex {
            conn,
            path: Some(path.to_path_buf()),
        })
    }

    /// A private in-memory index (tests, evaluation).
    pub fn open_in_memory() -> Result<SearchIndex> {
        Self::init(Connection::open_in_memory()?, None, Uuid::nil())
    }

    fn tune(conn: &Connection) -> Result<()> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // derived data: losing the last transactions in a power cut only means re-indexing
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // deleted rows are overwritten, so text removed from this index (a notebook moved into a
        // vault, which has its own index) doesn't linger in free pages; see `scrub`
        conn.pragma_update(None, "secure_delete", "ON")?;
        Ok(())
    }

    fn init(conn: Connection, path: Option<PathBuf>, library_id: Uuid) -> Result<SearchIndex> {
        if path.is_some() {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        Self::tune(&conn)?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('library', ?1)",
            [library_id.to_string()],
        )?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(SearchIndex { conn, path })
    }

    /// Removes an index file and its WAL side files. Missing files are not an error.
    pub fn delete_files(path: &Path) -> Result<()> {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut p = path.as_os_str().to_owned();
            p.push(suffix);
            match std::fs::remove_file(PathBuf::from(p)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// The file backing this index (`None` in memory).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Drops every derived row so the library is indexed again from scratch. Corrections and
    /// cached PDF text are kept (they do not depend on the recognizer).
    pub fn clear(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch("DELETE FROM entries; DELETE FROM pages; DELETE FROM notebooks;")?;
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------ notebooks --

    /// Brings one notebook's title and page positions up to date and removes rows of pages
    /// that are no longer live (trashed, purged or moved out). A trashed notebook is removed
    /// entirely. Returns the live pages the index has no rows for yet.
    ///
    /// Cheap (no page content is read), so call it after every notebook change, including
    /// a page reorder, which does not change any page revision.
    pub fn sync_notebook(&mut self, nb: &Notebook) -> Result<Vec<Uuid>> {
        if nb.deleted_at.is_some() {
            self.remove_notebook(nb.id)?;
            return Ok(vec![]);
        }
        let nb_s = nb.id.to_string();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO notebooks (notebook, title) VALUES (?1, ?2)
             ON CONFLICT (notebook) DO UPDATE SET title = excluded.title",
            params![nb_s, nb.title],
        )?;
        let first = nb.page_order.first().map(|p| p.to_string());
        let title_changed: bool = tx
            .query_row(
                "SELECT text, page FROM entries WHERE notebook = ?1 AND kind = 'title'",
                [&nb_s],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?
            .is_none_or(|(t, p)| t != nb.title || p != first);
        if title_changed {
            tx.execute(
                "DELETE FROM entries WHERE notebook = ?1 AND kind = 'title'",
                [&nb_s],
            )?;
            tx.execute(
                "INSERT INTO entries (notebook, page, kind, source_key, element_ids, text, alternates)
                 VALUES (?1, ?2, 'title', 'title', '[]', ?3, '')",
                params![nb_s, first, nb.title],
            )?;
        }
        let live: HashMap<String, usize> = nb
            .page_order
            .iter()
            .enumerate()
            .map(|(i, p)| (p.to_string(), i))
            .collect();
        let known: Vec<String> = {
            let mut st = tx.prepare("SELECT page FROM pages WHERE notebook = ?1")?;
            let rows = st.query_map([&nb_s], |r| r.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for p in &known {
            match live.get(p) {
                Some(i) => {
                    tx.execute(
                        "UPDATE pages SET page_index = ?3 WHERE notebook = ?1 AND page = ?2",
                        params![nb_s, p, *i as i64],
                    )?;
                }
                None => {
                    tx.execute(
                        "DELETE FROM entries WHERE notebook = ?1 AND page = ?2 AND kind != 'title'",
                        params![nb_s, p],
                    )?;
                    tx.execute(
                        "DELETE FROM pages WHERE notebook = ?1 AND page = ?2",
                        params![nb_s, p],
                    )?;
                    tx.execute(
                        "DELETE FROM corrections WHERE notebook = ?1 AND page = ?2",
                        params![nb_s, p],
                    )?;
                }
            }
        }
        tx.commit()?;
        let known: HashSet<String> = known.into_iter().collect();
        Ok(nb
            .page_order
            .iter()
            .filter(|p| !known.contains(&p.to_string()))
            .copied()
            .collect())
    }

    /// Removes every row of a notebook (trashed or purged).
    pub fn remove_notebook(&mut self, nb: Uuid) -> Result<()> {
        let s = nb.to_string();
        let tx = self.conn.transaction()?;
        for t in ["entries", "pages", "notebooks", "corrections"] {
            tx.execute(&format!("DELETE FROM {t} WHERE notebook = ?1"), [&s])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Makes text that was removed from the index unrecoverable from its files: drops cached
    /// PDF text that no remaining entry uses, merges the full-text index so deleted terms leave
    /// its segments, and truncates the write-ahead log. With `secure_delete` on, freed pages are
    /// overwritten. Run it after removing notebooks whose text must not stay (one moved into a
    /// vault, which has its own index); it is slow on a large index, so batch removals first.
    /// PDF text that is dropped but still needed is extracted again when its page is indexed.
    pub fn scrub(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "DELETE FROM pdf_text WHERE NOT EXISTS (
               SELECT 1 FROM entries e WHERE e.kind = 'pdf'
                 AND e.source_key LIKE 'pdf:' || pdf_text.asset || ':%');
             DELETE FROM pdf_assets WHERE NOT EXISTS (
               SELECT 1 FROM pdf_text t WHERE t.asset = pdf_assets.asset);
             INSERT INTO entries_fts (entries_fts) VALUES ('optimize');",
        )?;
        tx.commit()?;
        self.checkpoint()
    }

    /// Flushes the write-ahead log into the main file and empties it, so the index is one
    /// self-contained file (before it is copied or encrypted).
    pub fn checkpoint(&self) -> Result<()> {
        if self.path.is_some() {
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        }
        Ok(())
    }

    /// Notebooks that have rows.
    pub fn notebook_ids(&self) -> Result<Vec<Uuid>> {
        let mut st = self.conn.prepare("SELECT notebook FROM notebooks")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(id) = Uuid::parse_str(&r?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// What the index holds for a page, if anything.
    pub fn page_state(&self, nb: Uuid, page: Uuid) -> Result<Option<PageState>> {
        Ok(self
            .conn
            .query_row(
                "SELECT revision, recognizer, complete FROM pages WHERE notebook = ?1 AND page = ?2",
                params![nb.to_string(), page.to_string()],
                |r| {
                    Ok(PageState {
                        revision: r.get::<_, i64>(0)? as u64,
                        recognizer: r.get(1)?,
                        complete: r.get::<_, i64>(2)? != 0,
                    })
                },
            )
            .optional()?)
    }

    /// Whether a page needs indexing: unknown, older revision, incomplete, or built with
    /// another recognizer setting (`recognizer` as in [`PageState::recognizer`]).
    pub fn page_is_stale(
        &self,
        nb: Uuid,
        page: Uuid,
        revision: u64,
        recognizer: &str,
    ) -> Result<bool> {
        Ok(match self.page_state(nb, page)? {
            None => true,
            Some(s) => s.revision != revision || !s.complete || s.recognizer != recognizer,
        })
    }

    // ----------------------------------------------------------------- PDF --

    /// Whether the text of a PDF asset is cached.
    pub fn has_pdf(&self, asset: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM pdf_assets WHERE asset = ?1", [asset], |_| {
                Ok(())
            })
            .optional()?
            .is_some())
    }

    /// Caches the per-page text of a PDF asset (from `penroam_pdf::page_texts`; pass an
    /// empty list for an unreadable PDF so it is not parsed again).
    pub fn store_pdf(&mut self, asset: &str, texts: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM pdf_text WHERE asset = ?1", [asset])?;
        for (i, t) in texts.iter().enumerate() {
            if !t.trim().is_empty() {
                tx.execute(
                    "INSERT INTO pdf_text (asset, page_index, text) VALUES (?1, ?2, ?3)",
                    params![asset, i as i64, t],
                )?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO pdf_assets (asset, pages) VALUES (?1, ?2)",
            params![asset, texts.len() as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn pdf_page_text(&self, asset: &str, page_index: u32) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT text FROM pdf_text WHERE asset = ?1 AND page_index = ?2",
                params![asset, page_index as i64],
                |r| r.get(0),
            )
            .optional()?)
    }

    // --------------------------------------------------------------- pages --

    fn cached_regions(
        &self,
        nb: &str,
        page: &str,
        rkey: &str,
    ) -> Result<HashMap<String, (Vec<Word>, Rect)>> {
        let mut st = self.conn.prepare(
            "SELECT fingerprint, words, bounds FROM entries
             WHERE notebook = ?1 AND page = ?2 AND kind = 'ink' AND recognizer = ?3",
        )?;
        let rows = st.query_map(params![nb, page, rkey], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (fp, words, bounds) = row?;
            if let (Ok(w), Ok(b)) = (serde_json::from_str(&words), serde_json::from_str(&bounds)) {
                out.insert(fp, (w, b));
            }
        }
        Ok(out)
    }

    /// Indexes one page: typed text, PDF background text (from the cache filled by
    /// [`SearchIndex::store_pdf`]) and, with `recognition`, handwriting. Handwriting regions
    /// whose ink is unchanged since the last pass reuse the stored result, so an edit only
    /// re-recognizes the lines it touched. `cancel` is checked before each recognition; a
    /// cancelled or failed pass stores what it has and marks the page incomplete, and the
    /// next pass continues from there.
    ///
    /// `nb` must already be known through [`SearchIndex::sync_notebook`] (for the page
    /// position); `page_index` is the page's position in the notebook.
    pub fn index_page(
        &mut self,
        nb: Uuid,
        page_index: u32,
        page: &Page,
        mut recognition: Option<Recognition<'_>>,
        cancel: &dyn Fn() -> bool,
    ) -> Result<PageOutcome> {
        let nb_s = nb.to_string();
        let page_s = page.id.to_string();
        let rkey = recognition
            .as_ref()
            .map(|r| recognizer_key(r.recognizer.info(), r.language))
            .unwrap_or_default();
        let regions = if recognition.is_some() {
            segment::regions(page)
        } else {
            vec![]
        };
        let cache = if recognition.is_some() {
            self.cached_regions(&nb_s, &page_s, &rkey)?
        } else {
            HashMap::new()
        };
        let mut out = PageOutcome {
            regions: regions.len(),
            ..Default::default()
        };
        // (region, words) for every region with a result
        let mut ink: Vec<(&segment::Region<'_>, Vec<Word>)> = Vec::new();
        for region in &regions {
            if let Some((words, _)) = cache.get(&region.fingerprint) {
                out.reused += 1;
                ink.push((region, words.clone()));
                continue;
            }
            let Some(rec) = recognition.as_mut() else {
                break;
            };
            if cancel() || out.error.is_some() {
                out.pending += 1;
                continue;
            }
            match rec.recognizer.recognize(region, rec.language) {
                Ok(cands) => {
                    out.recognized += 1;
                    ink.push((region, cands.into_iter().map(word_of).collect()));
                }
                Err(e) => {
                    out.pending += 1;
                    out.error = Some(e.to_string());
                }
            }
        }
        // the page's PDF background, and PDF pages placed on it (infinite canvases)
        let mut pdf = Vec::new();
        for (key, bg, bounds) in pdf_sources(page) {
            if let Some(text) = self.pdf_page_text(&bg.asset_hash, bg.page_index)? {
                pdf.push((key, text, bounds));
            }
        }
        let corrections = self.corrections_for(&nb_s, &page_s)?;

        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM entries WHERE notebook = ?1 AND page = ?2 AND kind != 'title'",
            params![nb_s, page_s],
        )?;
        let rev = page.revision as i64;
        let mut ins = tx.prepare(
            "INSERT INTO entries (notebook, page, kind, source_key, fingerprint, element_ids, text,
               alternates, correction, confidence, recognizer, source_revision, bounds, words)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        for e in &page.elements {
            if let Element::Text(t) = e {
                if t.text.trim().is_empty() {
                    continue;
                }
                let b = Rect {
                    x: t.x,
                    y: t.y,
                    w: t.width,
                    h: t.height,
                };
                ins.execute(params![
                    nb_s,
                    page_s,
                    EntryKind::Text.as_str(),
                    t.id.to_string(),
                    Option::<String>::None,
                    serde_json::to_string(&[t.id]).expect("ids serialize"),
                    t.text,
                    "",
                    "",
                    Option::<f64>::None,
                    Option::<String>::None,
                    rev,
                    rect_json(&b),
                    Option::<String>::None,
                ])?;
            }
        }
        for (region, words) in &ink {
            let text = words
                .iter()
                .map(|w| w.t.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let alts = words
                .iter()
                .flat_map(|w| w.alts.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            let conf = {
                let cs: Vec<f32> = words.iter().filter_map(|w| w.c).collect();
                (!cs.is_empty()).then(|| cs.iter().sum::<f32>() as f64 / cs.len() as f64)
            };
            let correction = corrections.get(&region.key).cloned().unwrap_or_default();
            ins.execute(params![
                nb_s,
                page_s,
                EntryKind::Ink.as_str(),
                region.key,
                region.fingerprint,
                serde_json::to_string(&region.stroke_ids()).expect("ids serialize"),
                text,
                alts,
                correction,
                conf,
                rkey,
                rev,
                rect_json(&region.bounds),
                serde_json::to_string(words).expect("words serialize"),
            ])?;
        }
        for (key, text, bounds) in pdf {
            ins.execute(params![
                nb_s,
                page_s,
                EntryKind::Pdf.as_str(),
                key,
                Option::<String>::None,
                "[]",
                text,
                "",
                "",
                Option::<f64>::None,
                Option::<String>::None,
                rev,
                rect_json(&bounds),
                Option::<String>::None,
            ])?;
        }
        drop(ins);
        tx.execute(
            "INSERT OR REPLACE INTO pages (notebook, page, page_index, revision, recognizer, complete)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![nb_s, page_s, page_index as i64, rev, rkey, (out.pending == 0) as i64],
        )?;
        tx.commit()?;
        Ok(out)
    }

    // --------------------------------------------------------- corrections --

    fn corrections_for(&self, nb: &str, page: &str) -> Result<HashMap<String, String>> {
        let mut st = self
            .conn
            .prepare("SELECT region, text FROM corrections WHERE notebook = ?1 AND page = ?2")?;
        let rows = st.query_map(params![nb, page], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Stores (or with `None` / empty text, removes) the user's reading of a handwriting
    /// region. It is searched with the highest weight, never written into the note, and is
    /// kept while the region's strokes exist (moving them keeps it; erasing them drops it
    /// on the next pass over the page).
    pub fn set_correction(
        &mut self,
        nb: Uuid,
        page: Uuid,
        region_key: &str,
        text: Option<&str>,
    ) -> Result<()> {
        let (nb_s, page_s) = (nb.to_string(), page.to_string());
        let text = text.map(str::trim).filter(|t| !t.is_empty());
        let tx = self.conn.transaction()?;
        match text {
            Some(t) => tx.execute(
                "INSERT OR REPLACE INTO corrections (notebook, page, region, text) VALUES (?1, ?2, ?3, ?4)",
                params![nb_s, page_s, region_key, t],
            )?,
            None => tx.execute(
                "DELETE FROM corrections WHERE notebook = ?1 AND page = ?2 AND region = ?3",
                params![nb_s, page_s, region_key],
            )?,
        };
        tx.execute(
            "UPDATE entries SET correction = ?4
             WHERE notebook = ?1 AND page = ?2 AND kind = 'ink' AND source_key = ?3",
            params![nb_s, page_s, region_key, text.unwrap_or("")],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The text read from a page's handwriting, with the user's corrections applied,
    /// in the order the regions were indexed. Empty when nothing has been read yet.
    pub fn page_text(&self, nb: Uuid, page: Uuid) -> Result<String> {
        let mut st = self.conn.prepare(
            "SELECT CASE WHEN correction != '' THEN correction ELSE text END FROM entries
             WHERE notebook = ?1 AND page = ?2 AND kind = 'ink' ORDER BY id",
        )?;
        let rows = st.query_map(params![nb.to_string(), page.to_string()], |r| {
            r.get::<_, String>(0)
        })?;
        let lines: Vec<String> = rows.collect::<std::result::Result<_, _>>()?;
        Ok(lines.join("\n"))
    }

    /// Every correction in the index as (notebook, page, region, text), for backups.
    pub fn all_corrections(&self) -> Result<Vec<(String, String, String, String)>> {
        let mut st = self
            .conn
            .prepare("SELECT notebook, page, region, text FROM corrections ORDER BY 1, 2, 3")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Drops corrections whose region no longer exists on an indexed, complete page.
    pub fn prune_corrections(&mut self) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM corrections WHERE EXISTS (
               SELECT 1 FROM pages p WHERE p.notebook = corrections.notebook
                 AND p.page = corrections.page AND p.complete = 1 AND p.recognizer != '')
             AND NOT EXISTS (
               SELECT 1 FROM entries e WHERE e.notebook = corrections.notebook
                 AND e.page = corrections.page AND e.kind = 'ink' AND e.source_key = corrections.region)",
            [],
        )?)
    }

    // --------------------------------------------------------------- query --

    /// Counters for the settings screen.
    pub fn stats(&self) -> Result<IndexStats> {
        let count = |sql: &str| -> Result<u64> {
            Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as u64)
        };
        Ok(IndexStats {
            pages: count("SELECT COUNT(*) FROM pages")?,
            incomplete_pages: count("SELECT COUNT(*) FROM pages WHERE complete = 0")?,
            text_entries: count("SELECT COUNT(*) FROM entries WHERE kind = 'text'")?,
            ink_entries: count("SELECT COUNT(*) FROM entries WHERE kind = 'ink'")?,
            pdf_entries: count("SELECT COUNT(*) FROM entries WHERE kind = 'pdf'")?,
            corrections: count("SELECT COUNT(*) FROM corrections")?,
        })
    }

    fn vocabulary_near(&self, term: &str, max_distance: usize) -> Result<Vec<String>> {
        // the vocabulary is small (distinct words in the library); a linear scan with an early
        // length filter is fast enough and needs no extra index
        let first: String = term.chars().take(1).collect();
        let mut st = self.conn.prepare_cached(
            "SELECT term FROM entries_vocab WHERE length(term) BETWEEN ?1 AND ?2",
        )?;
        let n = term.chars().count();
        let rows = st.query_map(
            params![
                n.saturating_sub(max_distance) as i64,
                (n + max_distance) as i64
            ],
            |r| r.get::<_, String>(0),
        )?;
        let mut out = Vec::new();
        for t in rows {
            let t = t?;
            // OCR mistakes rarely hit the first letter alone; requiring it keeps noise down
            if t != term && t.starts_with(&first) && query::edit_distance(&t, term) <= max_distance
            {
                out.push(t);
            }
        }
        // recognizers sometimes run two words together ("indexphoto"); the prefix query
        // already finds the first one, this finds the second
        let mut st = self.conn.prepare_cached(
            "SELECT term FROM entries_vocab WHERE instr(term, ?1) > 1 AND length(term) <= ?2",
        )?;
        let rows = st.query_map(params![term, (n * 4) as i64], |r| r.get::<_, String>(0))?;
        for t in rows {
            out.push(t?);
        }
        out.truncate(16);
        Ok(out)
    }

    /// Searches the index. Every query word must match (as a word prefix); `"quoted words"`
    /// match as a phrase. When the exact query finds fewer than `limit` hits, words of four
    /// or more letters are also matched against similar indexed words (edit distance 1, or 2
    /// for words of seven or more letters) and against indexed words that contain them
    /// (words a recognizer ran together), which catches recognition errors; those hits are
    /// marked `fuzzy` and ranked lower.
    ///
    /// Ranking: SQLite's BM25 over the recognized text (weight 1), alternate readings (0.3)
    /// and user corrections (2), multiplied by the source kind (title 4, typed text 2,
    /// handwriting 1.5, PDF text 1) and, for handwriting, by `0.5 + confidence / 2`.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let parsed = query::parse(query);
        if parsed.is_empty() {
            return Ok(vec![]);
        }
        let exact = parsed.to_fts(&HashMap::new());
        let mut hits = self.run(&exact, &parsed, &HashMap::new(), false, limit)?;
        if hits.len() < limit {
            let mut expansions: HashMap<String, Vec<String>> = HashMap::new();
            for t in parsed.fuzzy_candidates() {
                let d = if t.chars().count() >= 7 { 2 } else { 1 };
                let near = self.vocabulary_near(&t, d)?;
                if !near.is_empty() {
                    expansions.insert(t, near);
                }
            }
            if !expansions.is_empty() {
                let fuzzy = parsed.to_fts(&expansions);
                let seen: HashSet<(Uuid, Option<Uuid>, Option<String>, EntryKind)> = hits
                    .iter()
                    .map(|h| (h.notebook_id, h.page_id, h.region_key.clone(), h.kind))
                    .collect();
                for h in self.run(&fuzzy, &parsed, &expansions, true, limit)? {
                    if !seen.contains(&(h.notebook_id, h.page_id, h.region_key.clone(), h.kind)) {
                        hits.push(h);
                    }
                }
            }
        }
        hits.truncate(limit);
        Ok(hits)
    }

    fn run(
        &self,
        fts: &str,
        parsed: &ParsedQuery,
        expansions: &HashMap<String, Vec<String>>,
        fuzzy: bool,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let mut st = self.conn.prepare_cached(
            "SELECT e.notebook, e.page, e.kind, e.source_key, e.text, e.correction, e.confidence,
                    e.bounds, e.words, n.title, p.page_index,
                    bm25(entries_fts, 1.0, 0.3, 2.0),
                    snippet(entries_fts, 0, char(2), char(3), '…', 12),
                    snippet(entries_fts, 2, char(2), char(3), '…', 12),
                    snippet(entries_fts, 1, char(2), char(3), '…', 12)
             FROM entries_fts
             JOIN entries e ON e.id = entries_fts.rowid
             LEFT JOIN notebooks n ON n.notebook = e.notebook
             LEFT JOIN pages p ON p.notebook = e.notebook AND p.page = e.page
             WHERE entries_fts MATCH ?1
             ORDER BY bm25(entries_fts, 1.0, 0.3, 2.0)
             LIMIT ?2",
        )?;
        // over-fetch: the final order also depends on kind and confidence
        let rows = st.query_map(params![fts, (limit * 4).max(50) as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<f64>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, f64>(11)?,
                r.get::<_, String>(12)?,
                r.get::<_, String>(13)?,
                r.get::<_, String>(14)?,
            ))
        });
        let rows = match rows {
            Ok(r) => r,
            // a query FTS5 cannot parse finds nothing rather than failing the search
            Err(rusqlite::Error::SqliteFailure(_, Some(m))) if m.contains("fts5") => {
                return Ok(vec![])
            }
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for row in rows {
            let (
                nb,
                page,
                kind,
                key,
                text,
                correction,
                conf,
                bounds,
                words,
                title,
                idx,
                bm25,
                s0,
                s2,
                s1,
            ) = match row {
                Ok(r) => r,
                Err(rusqlite::Error::SqliteFailure(_, Some(m))) if m.contains("fts5") => {
                    return Ok(vec![])
                }
                Err(e) => return Err(e.into()),
            };
            let Ok(notebook_id) = Uuid::parse_str(&nb) else {
                continue;
            };
            let kind = EntryKind::parse(&kind);
            let conf = conf.map(|c| c as f32);
            let conf_factor = match (kind, conf) {
                (EntryKind::Ink, Some(c)) => 0.5 + c.clamp(0.0, 1.0) as f64 / 2.0,
                _ => 1.0,
            };
            let mut score = -bm25 * kind.weight() * conf_factor;
            if fuzzy {
                score *= 0.5;
            }
            // show the column that matched: correction, then text, then alternates
            let snippet_src = if !correction.is_empty() && s2.contains('\u{2}') {
                s2
            } else if s0.contains('\u{2}') || s1.is_empty() {
                s0
            } else {
                s1
            };
            let snippet = split_snippet(if snippet_src.is_empty() {
                &text
            } else {
                &snippet_src
            });
            let words: Vec<Word> = words
                .as_deref()
                .and_then(|w| serde_json::from_str(w).ok())
                .unwrap_or_default();
            let match_bounds = words
                .iter()
                .filter(|w| {
                    std::iter::once(&w.t)
                        .chain(w.alts.iter())
                        .any(|t| parsed.matches_word(t, expansions))
                })
                .map(|w| w.b)
                .collect();
            out.push(SearchHit {
                notebook_id,
                notebook_title: title.unwrap_or_default(),
                page_id: page.and_then(|p| Uuid::parse_str(&p).ok()),
                page_index: idx.map(|i| i as u32),
                kind,
                snippet,
                bounds: bounds.and_then(|b| serde_json::from_str(&b).ok()),
                match_bounds,
                region_key: (kind == EntryKind::Ink).then_some(key),
                confidence: conf,
                fuzzy,
                score,
            });
        }
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out.truncate(limit);
        Ok(out)
    }
}

fn word_of(c: RecognitionCandidate) -> Word {
    Word {
        t: c.text,
        alts: c.alternates,
        b: c.bounds,
        ids: c.stroke_ids,
        c: c.confidence,
    }
}

fn split_snippet(s: &str) -> Vec<SnippetPart> {
    let mut out = Vec::new();
    let mut hit = false;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '\u{2}' | '\u{3}' => {
                if !cur.is_empty() {
                    out.push(SnippetPart {
                        text: std::mem::take(&mut cur),
                        hit,
                    });
                }
                hit = ch == '\u{2}';
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(SnippetPart { text: cur, hit });
    }
    out
}
