//! Library search: the background indexing worker and the search commands.
//!
//! The index (`penroam_search::SearchIndex`) is derived data in the app's **cache**
//! directory (`<cache>/search/<library device id>/index.db`): the OS may clear it, it is
//! left out of device backups, and it is never exported or synced. It is rebuilt from the
//! notes whenever it is missing, damaged or from another library.
//!
//! Each vault unlocked here has an index of its own (`…/vaults/<vault id>/index.db`), so a
//! vault's text never sits in the main index and is encrypted with its notebooks when the
//! vault is locked ([`SearchSlot::close_vault`]). Notebooks are routed by the owners file;
//! queries run over every open index. An index that lost notebooks is scrubbed when the
//! worker is idle (`SearchIndex::scrub`), so removed text doesn't linger in its files.
//!
//! # Worker
//!
//! One thread owns the index writer. It learns about changes from the commands
//! ([`SearchSlot::page_changed`], [`SearchSlot::notebook_changed`]) and from sync installs,
//! and at launch it checks every live page against the revision it indexed. For each page it
//! takes the library lock only to copy the page (`Library::read_page_detached`, which leaves
//! the editor's page cache alone) and, for a PDF page whose text is not cached, the PDF bytes.
//! Text extraction and handwriting recognition run without the lock.
//!
//! Scheduling protects writing: edited pages wait until the pen has been idle for
//! [`EDIT_QUIET`], nothing is indexed during that time, the thread runs at background
//! priority (Windows, Linux), and handwriting recognition sees one region at a time with a
//! cancellation check in between. Indexing can be paused, and by default waits while the
//! device runs on battery (where that is detectable: Windows, Linux).
//!
//! Handwriting recognition uses the platform's on-device recognizer where there is one
//! (Windows handwriting recognizers; Apple Vision on macOS and iOS) and the optional Penroam
//! handwriting model, a bundle in `<app data>/models/handwriting/`, where there is none, or as
//! a fallback for regions the platform recognizer fails on (`penroam_search::ChainRecognizer`).
//! Settings → Advanced can pick one of the two ([`RecognizerChoice`]). The model is downloaded
//! ([`crate::models`]) or installed from a file. Without either, typed text, titles and PDF text
//! are still indexed. No search data, query, ink or recognized text leaves the device.

use crate::commands::CmdError;
use crate::AppState;
use penroam_search::index::Recognition;
use penroam_search::{IndexStats, Recognizer, RecognizerInfo, SearchHit, SearchIndex};
use penroam_storage::ErrorCode;
use penroam_sync::vault::NotebookOwners;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

/// Pages edited less than this long ago are not indexed yet (and nothing else is either):
/// recognition must never compete with live writing.
const EDIT_QUIET: Duration = Duration::from_millis(1500);
/// How often power state is re-checked while waiting for power or while idle.
const POWER_POLL: Duration = Duration::from_secs(60);
/// Most results returned by one query.
const MAX_RESULTS: usize = 100;

/// Which index a notebook's rows live in: `None` for the main index, or the vault it belongs
/// to. Each open vault has its own index, so a vault's text can be encrypted with its notebooks
/// when the vault is locked, and never sits in the main index.
type Owner = Option<String>;

/// The index file of `owner`, next to the main one.
fn index_path(main: &Path, owner: &Owner) -> PathBuf {
    match owner {
        None => main.to_path_buf(),
        Some(v) => vault_index_dir(main, v).join("index.db"),
    }
}

fn vault_index_dir(main: &Path, vault: &str) -> PathBuf {
    main.parent()
        .unwrap_or(Path::new("."))
        .join("vaults")
        .join(vault)
}

fn add_stats(a: &mut IndexStats, b: IndexStats) {
    a.pages += b.pages;
    a.incomplete_pages += b.incomplete_pages;
    a.text_entries += b.text_entries;
    a.ink_entries += b.ink_entries;
    a.pdf_entries += b.pdf_entries;
    a.corrections += b.corrections;
}

/// User settings for search, stored in the app config directory (`search.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchSettings {
    /// Keep a search index at all. Off deletes the index file.
    pub enabled: bool,
    /// Recognize handwriting (when a recognizer is available).
    pub handwriting: bool,
    /// Recognizer language (provider-specific id; `None` = the provider's default).
    pub language: Option<String>,
    /// Pause all indexing.
    pub paused: bool,
    /// Wait while running on battery.
    pub pause_on_battery: bool,
    /// Which recognizer to prefer (Settings → Advanced).
    pub recognizer: RecognizerChoice,
}

/// Which handwriting recognizer the user prefers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecognizerChoice {
    /// The platform's recognizer first, with the Penroam model as a fallback.
    #[default]
    Auto,
    /// Only the platform's recognizer.
    Platform,
    /// Only the Penroam model.
    Model,
}

impl RecognizerChoice {
    /// Which of the available recognizers to use, as `(platform, model)`. A choice that cannot
    /// be honoured falls back to what the device has: a preference must never turn handwriting
    /// search off, for example after the model was removed.
    fn resolve(self, has_platform: bool, has_model: bool) -> (bool, bool) {
        (
            has_platform && (self != RecognizerChoice::Model || !has_model),
            has_model && (self != RecognizerChoice::Platform || !has_platform),
        )
    }
}

impl Default for SearchSettings {
    fn default() -> Self {
        SearchSettings {
            enabled: true,
            handwriting: true,
            language: None,
            paused: false,
            pause_on_battery: true,
            recognizer: RecognizerChoice::Auto,
        }
    }
}

/// Worker status shown in settings (`search://status`).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchStatus {
    /// Current settings.
    pub settings: SearchSettings,
    /// `off`, `starting`, `indexing`, `idle`, `paused`, `waiting-for-power` or `error`.
    pub state: String,
    /// Pages waiting to be checked or indexed.
    pub queued: usize,
    /// The handwriting recognition in use (platform and/or model), if any.
    pub recognizer: Option<RecognizerInfo>,
    /// The platform's own recognizer, if this device has one.
    pub platform: Option<RecognizerInfo>,
    /// The installed Penroam handwriting model, if any.
    pub model: Option<ModelSummary>,
    /// Why the installed model could not be loaded.
    pub model_error: Option<String>,
    /// The language recognition actually uses.
    pub language: Option<String>,
    /// Index counters.
    pub stats: IndexStats,
    /// Size of the index files in bytes.
    pub index_bytes: u64,
    /// `Some(true)` on battery, `Some(false)` on power, `None` when unknown.
    pub on_battery: Option<bool>,
    /// Last problem (index or recognizer), for the settings screen.
    pub last_error: Option<String>,
}

/// What the settings screen shows about a handwriting model before and after installing it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    /// Model id.
    pub id: String,
    /// Model version.
    pub version: String,
    /// Display name.
    pub name: String,
    /// Licence text of the weights.
    pub license: String,
    /// Languages.
    pub languages: Vec<String>,
    /// Size of the model files in bytes.
    pub bytes: u64,
    /// Training provenance as declared by the bundle.
    pub provenance: serde_json::Value,
}

impl From<&penroam_search::model::Manifest> for ModelSummary {
    fn from(m: &penroam_search::model::Manifest) -> Self {
        ModelSummary {
            id: m.id.clone(),
            version: m.version.clone(),
            name: m.name.clone(),
            license: m.license.clone(),
            languages: m.languages.clone(),
            bytes: m.total_bytes(),
            provenance: m.provenance.clone(),
        }
    }
}

enum Msg {
    Page(Uuid, Uuid),
    InstallModel {
        bytes: Vec<u8>,
        reply: Sender<Result<ModelSummary, String>>,
    },
    /// Installs an archive that was already verified on disk (a download).
    InstallModelFile {
        path: PathBuf,
        reply: Sender<Result<ModelSummary, String>>,
    },
    RemoveModel {
        id: String,
        reply: Sender<Result<(), String>>,
    },
    Notebook(Uuid),
    Settings(SearchSettings),
    Rebuild,
    /// A vault was unlocked here: open its index.
    VaultOpened(String),
    /// A vault is about to be locked: flush and close its index, then answer.
    VaultClosing(String, Sender<()>),
    Correct {
        notebook: Uuid,
        page: Uuid,
        region: String,
        text: Option<String>,
        reply: Sender<Result<(), String>>,
    },
}

struct Shared {
    cancel: AtomicBool,
    status: Mutex<SearchStatus>,
    /// The main index file.
    path: Mutex<Option<PathBuf>>,
    /// The library folder (for the notebook owners file).
    root: Mutex<Option<PathBuf>>,
    /// Vaults unlocked here, each with an index of its own.
    vaults: Mutex<BTreeSet<String>>,
    /// Bumped whenever an index file is replaced or closed, so readers reopen it.
    generation: AtomicU64,
    readers: Mutex<HashMap<Owner, (u64, SearchIndex)>>,
}

/// Handle to the search worker, kept in [`AppState`].
pub struct SearchSlot {
    tx: Mutex<Option<Sender<Msg>>>,
    shared: Arc<Shared>,
}

impl Default for SearchSlot {
    fn default() -> Self {
        SearchSlot {
            tx: Mutex::new(None),
            shared: Arc::new(Shared {
                cancel: AtomicBool::new(false),
                status: Mutex::new(SearchStatus {
                    state: "starting".into(),
                    ..Default::default()
                }),
                path: Mutex::new(None),
                root: Mutex::new(None),
                vaults: Mutex::new(BTreeSet::new()),
                generation: AtomicU64::new(0),
                readers: Mutex::new(HashMap::new()),
            }),
        }
    }
}

impl SearchSlot {
    fn send(&self, m: Msg) {
        if let Some(tx) = self.tx.lock().ok().and_then(|g| g.clone()) {
            let _ = tx.send(m);
        }
    }

    /// Runs `f` on a read-only view of `owner`'s index, or returns `None` if there isn't one.
    fn read<T>(&self, owner: &Owner, f: impl FnOnce(&SearchIndex) -> Option<T>) -> Option<T> {
        let main = self.shared.path.lock().ok()?.clone()?;
        if let Some(v) = owner {
            if !self.shared.vaults.lock().ok()?.contains(v) {
                return None;
            }
        }
        let path = index_path(&main, owner);
        if !path.exists() {
            return None;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        let mut g = self.shared.readers.lock().ok()?;
        if g.get(owner).is_none_or(|(gen, _)| *gen != generation) {
            g.insert(
                owner.clone(),
                (generation, SearchIndex::open_reader(&path).ok()?),
            );
        }
        f(&g.get(owner)?.1)
    }

    /// The vault a notebook belongs to, by the owners file.
    fn owner_of(&self, notebook: Uuid) -> Owner {
        let root = self.shared.root.lock().ok()?.clone()?;
        NotebookOwners::load(&root)
            .ok()?
            .owner(&notebook)
            .map(String::from)
    }

    /// The text read from a page's handwriting, with corrections applied. `None` when
    /// nothing has been read, for instance while indexing hasn't reached it.
    pub fn page_text(&self, notebook: Uuid, page: Uuid) -> Option<String> {
        self.read(&self.owner_of(notebook), |r| {
            r.page_text(notebook, page).ok()
        })
        .filter(|t| !t.trim().is_empty())
    }

    /// Every search correction of the main index as (notebook, page, region, text), for
    /// backups. Vault notebooks aren't in backups, so neither are their corrections.
    pub fn all_corrections(&self) -> Vec<(String, String, String, String)> {
        self.read(&None, |r| r.all_corrections().ok())
            .unwrap_or_default()
    }

    /// Where `vault`'s index lives (to lock it with the vault), once search has started.
    pub fn vault_dir(&self, vault: &str) -> Option<PathBuf> {
        let main = self.shared.path.lock().ok()?.clone()?;
        Some(vault_index_dir(&main, vault))
    }

    /// A vault was unlocked: its notebooks are indexed into its own index.
    pub fn vault_opened(&self, vault: &str) {
        if let Ok(mut v) = self.shared.vaults.lock() {
            v.insert(vault.to_string());
        }
        self.send(Msg::VaultOpened(vault.to_string()));
    }

    /// A vault is being locked: waits until its index is flushed and closed, so its files can
    /// be encrypted. Returns false when the worker didn't answer in time.
    pub fn close_vault(&self, vault: &str) -> bool {
        if let Ok(mut v) = self.shared.vaults.lock() {
            v.remove(vault);
        }
        if let Ok(mut r) = self.shared.readers.lock() {
            r.remove(&Some(vault.to_string()));
        }
        self.shared.cancel.store(true, Ordering::Relaxed);
        let (reply, rx) = mpsc::channel();
        if self.tx.lock().ok().and_then(|g| g.clone()).is_none() {
            return true; // no worker: nothing holds the index open
        }
        self.send(Msg::VaultClosing(vault.to_string(), reply));
        rx.recv_timeout(Duration::from_secs(60)).is_ok()
    }

    /// Stores a correction brought back from a backup. It is applied in order with
    /// indexing, and attaches to its region when the page is read again.
    pub fn restore_correction(&self, notebook: Uuid, page: Uuid, region: String, text: String) {
        let (reply, _rx) = mpsc::channel();
        self.send(Msg::Correct {
            notebook,
            page,
            region,
            text: Some(text),
            reply,
        });
    }

    /// A page's content changed (edit, import, sync install).
    pub fn page_changed(&self, notebook: Uuid, page: Uuid) {
        self.send(Msg::Page(notebook, page));
    }

    /// A notebook changed (title, page list, trash, purge, import).
    pub fn notebook_changed(&self, notebook: Uuid) {
        self.send(Msg::Notebook(notebook));
    }
}

fn cmd_err(m: impl Into<String>) -> CmdError {
    CmdError {
        code: ErrorCode::Io,
        message: m.into(),
    }
}

// ------------------------------------------------------------------ platform --

/// Whether the device runs on battery (`None` when that cannot be told).
fn on_battery() -> Option<bool> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut s = SYSTEM_POWER_STATUS::default();
        // SAFETY: plain out-parameter call
        if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
            return None;
        }
        if s.BatteryFlag == 128 {
            return Some(false); // no system battery
        }
        match s.ACLineStatus {
            0 => Some(true),
            1 => Some(false),
            _ => None,
        }
    }
    #[cfg(target_os = "linux")]
    {
        let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
        let (mut battery, mut mains_online) = (false, false);
        for e in dir.flatten() {
            let p = e.path();
            let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
            match kind.trim() {
                "Battery" => battery = true,
                "Mains" | "USB"
                    if std::fs::read_to_string(p.join("online")).is_ok_and(|s| s.trim() == "1") =>
                {
                    mains_online = true;
                }
                _ => {}
            }
        }
        if mains_online {
            Some(false)
        } else if battery {
            Some(true)
        } else {
            None
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        crate::power::on_battery()
    }
}

/// Lowers the current thread's CPU (and on Windows I/O) priority.
fn background_priority() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
        };
        // SAFETY: pseudo-handle of the calling thread
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // per-thread nice value: Linux applies setpriority(PRIO_PROCESS, tid) to the thread
        // SAFETY: plain syscalls on the calling thread
        unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::setpriority(libc::PRIO_PROCESS, tid, 10);
        }
    }
}

/// The on-device handwriting recognizer, if this platform has one.
fn platform_recognizer() -> Option<Box<dyn Recognizer>> {
    #[cfg(windows)]
    {
        penroam_search::windows_ink::WindowsInkRecognizer::new()
            .map(|r| Box::new(r) as Box<dyn Recognizer>)
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        penroam_search::vision::VisionRecognizer::new().map(|r| Box::new(r) as Box<dyn Recognizer>)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "ios")))]
    {
        None
    }
}

/// Where handwriting model bundles are installed.
pub(crate) fn models_dir(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("models").join("handwriting"))
}

/// The recognition stack: platform recognizer first, installed model as fallback.
struct Stack {
    recognizer: Option<Box<dyn Recognizer>>,
    platform: Option<RecognizerInfo>,
    model: Option<ModelSummary>,
    model_error: Option<String>,
}

fn build_stack(app: &AppHandle, choice: RecognizerChoice) -> Stack {
    let platform = platform_recognizer();
    let platform_info = platform.as_ref().map(|p| p.info().clone());
    let (mut model, mut model_summary, mut model_error) = (None, None, None);
    if let Some((dir, manifest)) = models_dir(app)
        .map(|d| penroam_search::model::installed(&d))
        .and_then(|v| v.into_iter().next())
    {
        match penroam_search::model::OnnxRecognizer::load(&dir) {
            Ok(m) => {
                model_summary = Some(ModelSummary::from(&manifest));
                model = Some(Box::new(m) as Box<dyn Recognizer>);
            }
            Err(e) => model_error = Some(e.to_string()),
        }
    }
    let (use_platform, use_model) = choice.resolve(platform.is_some(), model.is_some());
    Stack {
        recognizer: penroam_search::ChainRecognizer::new(
            platform.filter(|_| use_platform),
            model.filter(|_| use_model),
        )
        .map(|c| Box::new(c) as Box<dyn Recognizer>),
        platform: platform_info,
        model: model_summary,
        model_error,
    }
}

fn pick_language(info: &RecognizerInfo, wanted: Option<&str>) -> Option<String> {
    if let Some(w) = wanted {
        if info.languages.iter().any(|l| l == w) {
            return Some(w.to_string());
        }
    }
    info.languages
        .iter()
        .find(|l| l.contains("English (US)") || l.as_str() == "en-US" || l.as_str() == "en")
        .or_else(|| info.languages.first())
        .cloned()
}

// -------------------------------------------------------------------- worker --

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("search.json"))
}

fn load_settings(app: &AppHandle) -> SearchSettings {
    settings_path(app)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_settings(app: &AppHandle, s: &SearchSettings) {
    if let Some(p) = settings_path(app) {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, serde_json::to_vec_pretty(s).expect("settings serialize"));
    }
}

fn index_bytes(path: &std::path::Path) -> u64 {
    ["", "-wal"]
        .iter()
        .filter_map(|s| {
            let mut p = path.as_os_str().to_owned();
            p.push(s);
            std::fs::metadata(PathBuf::from(p)).ok()
        })
        .map(|m| m.len())
        .sum()
}

struct Worker {
    app: AppHandle,
    shared: Arc<Shared>,
    settings: SearchSettings,
    /// The main index file; vault indexes sit next to it ([`index_path`]).
    path: PathBuf,
    library_id: Uuid,
    /// Open indexes: the main one and one per unlocked vault.
    indexes: BTreeMap<Owner, SearchIndex>,
    /// Vaults unlocked here.
    vaults: BTreeSet<String>,
    /// Which notebooks belong to vaults, reread before routing.
    owners: NotebookOwners,
    root: PathBuf,
    /// Indexes that lost notebooks and must be scrubbed when idle.
    needs_scrub: BTreeSet<Owner>,
    recognizer: Option<Box<dyn Recognizer>>,
    platform: Option<RecognizerInfo>,
    model: Option<ModelSummary>,
    model_error: Option<String>,
    language: Option<String>,
    hot: VecDeque<(Uuid, Uuid)>,
    background: VecDeque<(Uuid, Uuid)>,
    queued: HashSet<(Uuid, Uuid)>,
    last_edit: Option<Instant>,
    last_error: Option<String>,
    last_emit: Option<Instant>,
}

impl Worker {
    fn state(&self) -> AppHandle {
        self.app.clone()
    }

    /// Runs `f` with the library locked (`None` when the lock is poisoned).
    fn with_lib<T>(&self, f: impl FnOnce(&mut penroam_storage::Library) -> T) -> Option<T> {
        let app = self.state();
        let st = app.state::<AppState>();
        let mut g = st.lib.lock().ok()?;
        Some(f(&mut g))
    }

    fn recognizer_key(&self) -> String {
        match (&self.recognizer, &self.language) {
            (Some(r), Some(l)) if self.settings.handwriting => {
                let i = r.info();
                format!("{}/{}/{}", i.id, i.model_version, l)
            }
            _ => String::new(),
        }
    }

    /// Opens the main index and every unlocked vault's.
    fn open_index(&mut self) {
        let owners: Vec<Owner> = std::iter::once(None)
            .chain(self.vaults.iter().cloned().map(Some))
            .collect();
        for o in owners {
            self.open_one(o);
        }
    }

    fn open_one(&mut self, owner: Owner) {
        match SearchIndex::open(&index_path(&self.path, &owner), self.library_id) {
            Ok((ix, _fresh)) => {
                self.indexes.insert(owner, ix);
                self.shared.generation.fetch_add(1, Ordering::SeqCst);
            }
            Err(e) => {
                self.indexes.remove(&owner);
                self.last_error = Some(e.to_string());
            }
        }
    }

    fn reload_owners(&mut self) {
        if let Ok(o) = NotebookOwners::load(&self.root) {
            self.owners = o;
        }
    }

    fn owner_of(&self, nb: &Uuid) -> Owner {
        self.owners.owner(nb).map(String::from)
    }

    fn enqueue(&mut self, key: (Uuid, Uuid), hot: bool) {
        if hot {
            self.background.retain(|k| *k != key);
            if !self.hot.contains(&key) {
                self.hot.push_back(key);
            }
            self.queued.insert(key);
        } else if self.queued.insert(key) {
            self.background.push_back(key);
        }
    }

    fn rescan(&mut self) {
        let Some(nbs) = self.with_lib(|lib| {
            lib.notebook_ids()
                .into_iter()
                .filter_map(|id| lib.notebook(id).ok().cloned())
                .collect::<Vec<_>>()
        }) else {
            return;
        };
        self.reload_owners();
        let mut pages = Vec::new();
        let owned: Vec<(Owner, &penroam_core::Notebook)> =
            nbs.iter().map(|n| (self.owner_of(&n.id), n)).collect();
        let mut errors = Vec::new();
        for (owner, ix) in self.indexes.iter_mut() {
            let live: HashSet<Uuid> = owned
                .iter()
                .filter(|(o, _)| o == owner)
                .map(|(_, n)| n.id)
                .collect();
            for id in ix.notebook_ids().unwrap_or_default() {
                if !live.contains(&id) {
                    let _ = ix.remove_notebook(id);
                    self.needs_scrub.insert(owner.clone());
                }
            }
            for (_, nb) in owned.iter().filter(|(o, _)| o == owner) {
                if let Err(e) = ix.sync_notebook(nb) {
                    errors.push(e.to_string());
                }
                if nb.deleted_at.is_none() {
                    pages.extend(nb.page_order.iter().map(|p| (nb.id, *p)));
                }
            }
        }
        if let Some(e) = errors.pop() {
            self.last_error = Some(e);
        }
        for k in pages {
            self.enqueue(k, false);
        }
    }

    fn notebook(&mut self, id: Uuid) {
        let nb = self
            .with_lib(|lib| lib.notebook(id).ok().cloned())
            .flatten();
        self.reload_owners();
        let owner = self.owner_of(&id);
        let mut new_pages: Vec<(Uuid, Uuid)> = Vec::new();
        let mut error = None;
        for (o, ix) in self.indexes.iter_mut() {
            match &nb {
                Some(nb) if *o == owner => match ix.sync_notebook(nb) {
                    Ok(v) => new_pages.extend(v.into_iter().map(|p| (id, p))),
                    Err(e) => error = Some(e.to_string()),
                },
                // gone, or in another index now (moved into a vault)
                _ => {
                    if ix.notebook_ids().unwrap_or_default().contains(&id) {
                        if let Err(e) = ix.remove_notebook(id) {
                            error = Some(e.to_string());
                        }
                        self.needs_scrub.insert(o.clone());
                    }
                }
            }
        }
        if let Some(e) = error {
            self.last_error = Some(e);
        }
        for k in new_pages {
            self.enqueue(k, false);
        }
    }

    fn index_one(&mut self, (nb_id, page_id): (Uuid, Uuid)) {
        let has_pdf = |ix: &SearchIndex, h: &str| ix.has_pdf(h).unwrap_or(true);
        let owner = self.owner_of(&nb_id);
        let Some(ix) = self.indexes.get(&owner) else {
            return;
        };
        // copy what is needed under the lock, then let go of it
        let got = self.with_lib(|lib| {
            let nb = lib.notebook(nb_id).ok()?.clone();
            let idx = nb.page_order.iter().position(|p| *p == page_id)?;
            let page = lib.read_page_detached(nb_id, page_id).ok()?;
            let pdf = page
                .background
                .as_ref()
                .filter(|bg| !has_pdf(ix, &bg.asset_hash))
                .and_then(|bg| {
                    lib.read_asset(nb_id, &bg.asset_hash)
                        .ok()
                        .map(|b| (bg.asset_hash.clone(), b))
                });
            Some((nb, idx as u32, page, pdf))
        });
        let Some(Some((nb, idx, page, pdf))) = got else {
            // gone (trashed, purged) or unreadable: bring the notebook's rows up to date
            self.notebook(nb_id);
            return;
        };
        let rkey = self.recognizer_key();
        let Some(ix) = self.indexes.get_mut(&owner) else {
            return;
        };
        if let Some((hash, bytes)) = pdf {
            let texts = penroam_pdf::page_texts(&bytes).unwrap_or_default();
            drop(bytes);
            if let Err(e) = ix.store_pdf(&hash, &texts) {
                self.last_error = Some(e.to_string());
                return;
            }
        }
        if let Err(e) = ix.sync_notebook(&nb) {
            self.last_error = Some(e.to_string());
            return;
        }
        match ix.page_is_stale(nb_id, page_id, page.revision, &rkey) {
            Ok(false) => return,
            Ok(true) => {}
            Err(e) => {
                self.last_error = Some(e.to_string());
                return;
            }
        }
        let shared = self.shared.clone();
        let cancel = move || shared.cancel.load(Ordering::Relaxed);
        let recognition = match (&mut self.recognizer, &self.language) {
            (Some(r), Some(l)) if self.settings.handwriting => Some(Recognition {
                recognizer: r.as_mut(),
                language: l.as_str(),
            }),
            _ => None,
        };
        match ix.index_page(nb_id, idx, &page, recognition, &cancel) {
            Ok(out) => {
                if let Some(e) = out.error {
                    self.last_error = Some(e);
                }
            }
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    fn emit(&mut self, state: &str, force: bool) {
        if !force
            && self
                .last_emit
                .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        self.last_emit = Some(Instant::now());
        let status = SearchStatus {
            settings: self.settings.clone(),
            state: state.into(),
            queued: self.queued.len(),
            recognizer: self.recognizer.as_ref().map(|r| r.info().clone()),
            platform: self.platform.clone(),
            model: self.model.clone(),
            model_error: self.model_error.clone(),
            language: self.language.clone(),
            stats: {
                let mut total = IndexStats::default();
                for ix in self.indexes.values() {
                    if let Ok(st) = ix.stats() {
                        add_stats(&mut total, st);
                    }
                }
                total
            },
            index_bytes: self
                .indexes
                .keys()
                .map(|o| index_bytes(&index_path(&self.path, o)))
                .sum(),
            on_battery: on_battery(),
            last_error: self.last_error.clone(),
        };
        if let Ok(mut g) = self.shared.status.lock() {
            *g = status.clone();
        }
        let _ = self.app.emit("search://status", status);
    }

    fn apply_settings(&mut self, s: SearchSettings) {
        let was = self.settings.clone();
        self.settings = s;
        save_settings(&self.app, &self.settings);
        if let Some(r) = &self.recognizer {
            self.language = pick_language(r.info(), self.settings.language.as_deref());
        }
        if !self.settings.enabled {
            let owners: Vec<Owner> = std::iter::once(None)
                .chain(self.vaults.iter().cloned().map(Some))
                .collect();
            self.indexes.clear();
            self.hot.clear();
            self.background.clear();
            self.queued.clear();
            if let Ok(mut g) = self.shared.readers.lock() {
                g.clear();
            }
            self.shared.generation.fetch_add(1, Ordering::SeqCst);
            for o in owners {
                if let Err(e) = SearchIndex::delete_files(&index_path(&self.path, &o)) {
                    self.last_error = Some(e.to_string());
                }
            }
            return;
        }
        if was.recognizer != self.settings.recognizer {
            // reload_stack re-checks every page, so nothing else below has to
            self.reload_stack();
        }
        let rkey_changed =
            was.handwriting != self.settings.handwriting || was.language != self.settings.language;
        if self.indexes.is_empty() {
            self.open_index();
            self.rescan();
        } else if rkey_changed || (was.paused && !self.settings.paused) {
            self.rescan();
        }
    }

    /// Rebuilds the recognition stack (after installing or removing a model) and re-checks
    /// every page; pages whose recognizer changed are recognized again in the background.
    fn reload_stack(&mut self) {
        self.recognizer = None;
        let st = build_stack(&self.app, self.settings.recognizer);
        self.recognizer = st.recognizer;
        self.platform = st.platform;
        self.model = st.model;
        self.model_error = st.model_error;
        self.language = self
            .recognizer
            .as_ref()
            .and_then(|r| pick_language(r.info(), self.settings.language.as_deref()));
        if !self.indexes.is_empty() {
            self.rescan();
        }
    }

    fn finish_install(
        &mut self,
        result: Result<penroam_search::model::ModelBundle, penroam_search::model::ModelError>,
        dir: &std::path::Path,
        reply: Sender<Result<ModelSummary, String>>,
    ) {
        let r = result
            .map(|b| ModelSummary::from(&b.manifest))
            .map_err(|e| e.to_string());
        // one model at a time: remove any other installed id
        if let Ok(sum) = &r {
            for (path, m) in penroam_search::model::installed(dir) {
                if m.id != sum.id {
                    let _ = std::fs::remove_dir_all(path);
                }
            }
        }
        self.reload_stack();
        let _ = reply.send(r);
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::InstallModel { bytes, reply } => {
                let Some(dir) = models_dir(&self.app) else {
                    let _ = reply.send(Err("no app data directory".into()));
                    return;
                };
                // release the loaded model first: its files are about to be replaced
                self.recognizer = None;
                let r = penroam_search::model::install_archive(&bytes, &dir);
                self.finish_install(r, &dir, reply);
            }
            Msg::InstallModelFile { path, reply } => {
                let Some(dir) = models_dir(&self.app) else {
                    let _ = reply.send(Err("no app data directory".into()));
                    return;
                };
                self.recognizer = None;
                let r = std::fs::File::open(&path)
                    .map_err(|e| penroam_search::model::ModelError::Io(e.to_string()))
                    .and_then(|f| penroam_search::model::install_archive_from(f, &dir));
                self.finish_install(r, &dir, reply);
            }
            Msg::RemoveModel { id, reply } => {
                self.recognizer = None;
                let r = match models_dir(&self.app) {
                    Some(dir) => penroam_search::model::installed(&dir)
                        .into_iter()
                        .filter(|(_, m)| m.id == id)
                        .try_for_each(|(p, _)| std::fs::remove_dir_all(p))
                        .map_err(|e| e.to_string()),
                    None => Ok(()),
                };
                self.reload_stack();
                let _ = reply.send(r);
            }
            Msg::Page(nb, p) => {
                if !self.indexes.is_empty() {
                    self.last_edit = Some(Instant::now());
                    self.enqueue((nb, p), true);
                }
            }
            Msg::Notebook(nb) => self.notebook(nb),
            Msg::Settings(s) => self.apply_settings(s),
            Msg::Rebuild => {
                let mut error = None;
                for ix in self.indexes.values_mut() {
                    if let Err(e) = ix.clear() {
                        error = Some(e.to_string());
                    }
                }
                self.last_error = error;
                self.rescan();
            }
            Msg::VaultOpened(v) => {
                if self.vaults.insert(v.clone()) && self.settings.enabled {
                    self.open_one(Some(v));
                    self.rescan();
                }
            }
            Msg::VaultClosing(v, reply) => {
                let owner = Some(v.clone());
                if let Some(ix) = self.indexes.remove(&owner) {
                    if let Err(e) = ix.checkpoint() {
                        self.last_error = Some(e.to_string());
                    }
                }
                self.vaults.remove(&v);
                self.needs_scrub.remove(&owner);
                if let Ok(mut g) = self.shared.readers.lock() {
                    g.remove(&owner);
                }
                self.shared.generation.fetch_add(1, Ordering::SeqCst);
                // its notebooks are leaving the library: drop their queued pages
                self.reload_owners();
                let owners = &self.owners;
                let theirs = |k: &(Uuid, Uuid)| owners.owner(&k.0) == Some(v.as_str());
                self.hot.retain(|k| !theirs(k));
                self.background.retain(|k| !theirs(k));
                self.queued.retain(|k| !theirs(k));
                let _ = reply.send(());
            }
            Msg::Correct {
                notebook,
                page,
                region,
                text,
                reply,
            } => {
                self.reload_owners();
                let owner = self.owner_of(&notebook);
                let r = match self.indexes.get_mut(&owner) {
                    Some(ix) => ix
                        .set_correction(notebook, page, &region, text.as_deref())
                        .map_err(|e| e.to_string()),
                    None => Err("search is turned off".into()),
                };
                let _ = reply.send(r);
            }
        }
    }

    fn next_job(&mut self) -> Option<(Uuid, Uuid)> {
        let k = self
            .hot
            .pop_front()
            .or_else(|| self.background.pop_front())?;
        self.queued.remove(&k);
        Some(k)
    }

    fn run(mut self, rx: Receiver<Msg>) {
        background_priority();
        // vaults unlocked before this thread got going
        if let Ok(v) = self.shared.vaults.lock() {
            self.vaults.extend(v.iter().cloned());
        }
        if self.settings.enabled {
            self.open_index();
            self.rescan();
        }
        let mut pruned = false;
        loop {
            let has_work = !self.queued.is_empty();
            let quiet_left = self
                .last_edit
                .map(|t| EDIT_QUIET.saturating_sub(t.elapsed()))
                .unwrap_or_default();
            let blocked = !self.settings.enabled
                || self.settings.paused
                || (self.settings.pause_on_battery && on_battery() == Some(true));
            let wait = if !has_work || blocked {
                POWER_POLL
            } else {
                quiet_left
            };
            let state = if !self.settings.enabled {
                "off"
            } else if !self.indexes.contains_key(&None) {
                "error"
            } else if self.settings.paused {
                "paused"
            } else if blocked && has_work {
                "waiting-for-power"
            } else if has_work {
                "indexing"
            } else {
                "idle"
            };
            self.emit(state, !has_work || blocked);
            let first = if wait.is_zero() {
                rx.try_recv().map_err(|e| match e {
                    mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                    mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                })
            } else {
                rx.recv_timeout(wait)
            };
            match first {
                Ok(m) => {
                    self.handle(m);
                    while let Ok(m) = rx.try_recv() {
                        self.handle(m);
                    }
                    self.shared.cancel.store(false, Ordering::Relaxed);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if blocked || self.last_edit.is_some_and(|t| t.elapsed() < EDIT_QUIET) {
                continue;
            }
            match self.next_job() {
                Some(k) => {
                    self.index_one(k);
                    pruned = false;
                }
                None if !pruned => {
                    for ix in self.indexes.values_mut() {
                        let _ = ix.prune_corrections();
                    }
                    for o in std::mem::take(&mut self.needs_scrub) {
                        if let Some(ix) = self.indexes.get_mut(&o) {
                            if let Err(e) = ix.scrub() {
                                self.last_error = Some(e.to_string());
                            }
                        }
                    }
                    pruned = true;
                }
                None => {}
            }
        }
    }
}

/// Starts the worker (once, at launch). The index lives in the cache directory, keyed by the
/// library's device id.
pub fn start(app: &AppHandle) {
    let st = app.state::<AppState>();
    let library_id = match st.lib.lock() {
        Ok(l) => l.device_id(),
        Err(_) => return,
    };
    let Ok(cache) = app.path().app_cache_dir() else {
        return;
    };
    let path = cache
        .join("search")
        .join(library_id.to_string())
        .join("index.db");
    let settings = load_settings(app);
    let stack = build_stack(app, settings.recognizer);
    let recognizer = stack.recognizer;
    let language = recognizer
        .as_ref()
        .and_then(|r| pick_language(r.info(), settings.language.as_deref()));
    let root = match st.lib.lock() {
        Ok(l) => l.root().to_path_buf(),
        Err(_) => return,
    };
    let shared = st.search.shared.clone();
    if let Ok(mut p) = shared.path.lock() {
        *p = Some(path.clone());
    }
    if let Ok(mut r) = shared.root.lock() {
        *r = Some(root.clone());
    }
    let (tx, rx) = mpsc::channel();
    if let Ok(mut g) = st.search.tx.lock() {
        *g = Some(tx);
    }
    let w = Worker {
        app: app.clone(),
        shared,
        settings,
        path,
        library_id,
        indexes: BTreeMap::new(),
        vaults: BTreeSet::new(),
        owners: NotebookOwners::load(&root).unwrap_or_default(),
        root,
        needs_scrub: BTreeSet::new(),
        recognizer,
        platform: stack.platform,
        model: stack.model,
        model_error: stack.model_error,
        language,
        hot: VecDeque::new(),
        background: VecDeque::new(),
        queued: HashSet::new(),
        last_edit: None,
        last_error: None,
        last_emit: None,
    };
    std::thread::Builder::new()
        .name("penroam-search".into())
        .spawn(move || w.run(rx))
        .expect("spawn search worker");
}

// ------------------------------------------------------------------ commands --

/// Searches every live notebook. Results come from the local index only.
#[tauri::command(async)]
pub fn search_query(state: State<'_, AppState>, query: String) -> Result<Vec<SearchHit>, CmdError> {
    let shared = &state.search.shared;
    let main = shared
        .path
        .lock()
        .ok()
        .and_then(|p| p.clone())
        .ok_or_else(|| cmd_err("search is not available"))?;
    if !shared.status.lock().is_ok_and(|s| s.settings.enabled) {
        return Err(cmd_err("search is turned off"));
    }
    let owners: Vec<Owner> = std::iter::once(None)
        .chain(
            shared
                .vaults
                .lock()
                .map(|v| v.iter().cloned().map(Some).collect::<Vec<_>>())
                .unwrap_or_default(),
        )
        .collect();
    let generation = shared.generation.load(Ordering::SeqCst);
    let mut g = shared
        .readers
        .lock()
        .map_err(|_| cmd_err("search lock poisoned"))?;
    let mut hits = Vec::new();
    for owner in owners {
        let path = index_path(&main, &owner);
        if g.get(&owner).is_none_or(|(gen, _)| *gen != generation) {
            if !path.exists() {
                continue; // not created yet
            }
            match SearchIndex::open_reader(&path) {
                Ok(r) => {
                    g.insert(owner.clone(), (generation, r));
                }
                // a vault's index can fail on its own; the rest still answers
                Err(e) if owner.is_some() => {
                    eprintln!("penroam: a vault's search index could not be read: {e}");
                    continue;
                }
                Err(e) => return Err(cmd_err(e.to_string())),
            }
        }
        let (_, reader) = g.get(&owner).expect("just opened");
        hits.extend(
            reader
                .search(&query, MAX_RESULTS)
                .map_err(|e| cmd_err(e.to_string()))?,
        );
    }
    // scores come from separate indexes; close enough to interleave by
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(MAX_RESULTS);
    Ok(hits)
}

/// Current worker status.
#[tauri::command(async)]
pub fn search_status(state: State<'_, AppState>) -> Result<SearchStatus, CmdError> {
    state
        .search
        .shared
        .status
        .lock()
        .map(|s| s.clone())
        .map_err(|_| cmd_err("search lock poisoned"))
}

/// Changes search settings (persisted). Turning search off deletes the index; pausing stops
/// recognition at the next region.
#[tauri::command(async)]
pub fn search_set_settings(
    state: State<'_, AppState>,
    settings: SearchSettings,
) -> Result<(), CmdError> {
    let cur = state
        .search
        .shared
        .status
        .lock()
        .map(|s| s.settings.clone());
    if let Ok(cur) = cur {
        if (cur.enabled && !settings.enabled)
            || (!cur.paused && settings.paused)
            || (cur.handwriting && !settings.handwriting)
        {
            state.search.shared.cancel.store(true, Ordering::Relaxed);
        }
    }
    state.search.send(Msg::Settings(settings));
    Ok(())
}

/// Drops every derived entry and indexes the library again (corrections are kept).
#[tauri::command(async)]
pub fn search_rebuild(state: State<'_, AppState>) -> Result<(), CmdError> {
    state.search.shared.cancel.store(true, Ordering::Relaxed);
    state.search.send(Msg::Rebuild);
    Ok(())
}

/// Stores the user's reading of a handwriting region (`None` removes it). The note is not
/// changed.
#[tauri::command(async)]
pub fn search_correct(
    state: State<'_, AppState>,
    notebook_id: Uuid,
    page_id: Uuid,
    region_key: String,
    text: Option<String>,
) -> Result<(), CmdError> {
    let (reply, rx) = mpsc::channel();
    state.search.send(Msg::Correct {
        notebook: notebook_id,
        page: page_id,
        region: region_key,
        text,
        reply,
    });
    rx.recv_timeout(Duration::from_secs(30))
        .map_err(|_| cmd_err("search is busy; try again"))?
        .map_err(cmd_err)
}

fn ask_worker<T>(
    state: &State<'_, AppState>,
    make: impl FnOnce(Sender<Result<T, String>>) -> Msg,
) -> Result<T, CmdError> {
    let (reply, rx) = mpsc::channel();
    state.search.send(make(reply));
    rx.recv_timeout(Duration::from_secs(120))
        .map_err(|_| cmd_err("search is busy; try again"))?
        .map_err(cmd_err)
}

/// Reads a handwriting model file (`.penroam-model`) the user picked and returns what it is
/// (name, version, size, licence) without installing it.
#[tauri::command(async)]
pub fn search_inspect_model(app: AppHandle, path: String) -> Result<ModelSummary, CmdError> {
    let bytes =
        crate::commands::read_user_file(&app, &path, penroam_search::model::MAX_ARCHIVE_BYTES)?;
    penroam_search::model::inspect_archive(&bytes)
        .map(|m| ModelSummary::from(&m))
        .map_err(|e| cmd_err(e.to_string()))
}

/// Verifies and installs a handwriting model file, replacing any installed model. The library
/// is then re-checked and handwriting re-recognized in the background.
#[tauri::command(async)]
pub fn search_install_model(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<ModelSummary, CmdError> {
    let bytes =
        crate::commands::read_user_file(&app, &path, penroam_search::model::MAX_ARCHIVE_BYTES)?;
    state.search.shared.cancel.store(true, Ordering::Relaxed);
    ask_worker(&state, |reply| Msg::InstallModel { bytes, reply })
}

/// Removes the installed handwriting model. It stays removed: the first-run download does not
/// bring it back.
#[tauri::command(async)]
pub fn search_remove_model(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<(), CmdError> {
    state.search.shared.cancel.store(true, Ordering::Relaxed);
    ask_worker(&state, |reply| Msg::RemoveModel { id, reply })?;
    crate::models::mark_first_run_handled(&app);
    Ok(())
}

/// Installs a verified, downloaded model archive through the worker. Blocks until it is done.
pub(crate) fn install_downloaded(app: &AppHandle, path: PathBuf) -> Result<ModelSummary, String> {
    let state = app.state::<AppState>();
    state.search.shared.cancel.store(true, Ordering::Relaxed);
    let (reply, rx) = mpsc::channel();
    state.search.send(Msg::InstallModelFile { path, reply });
    rx.recv_timeout(Duration::from_secs(600))
        .map_err(|_| "search is busy; try again".to_string())?
}

/// Whether search would use a downloaded model right now: handwriting search is on, the device
/// has no recognizer of its own, and no model is installed. Devices with a platform recognizer
/// only get the model if the user asks for it.
pub(crate) fn needs_a_model(app: &AppHandle) -> bool {
    let s = load_settings(app);
    let installed = models_dir(app)
        .map(|d| !penroam_search::model::installed(&d).is_empty())
        .unwrap_or(false);
    s.enabled && s.handwriting && !installed && platform_recognizer().is_none()
}

#[cfg(test)]
mod recognizer_choice_tests {
    use super::RecognizerChoice::{Auto, Model, Platform};

    #[test]
    fn automatic_uses_everything_available() {
        assert_eq!(Auto.resolve(true, true), (true, true));
        assert_eq!(Auto.resolve(true, false), (true, false));
        assert_eq!(Auto.resolve(false, true), (false, true));
        assert_eq!(Auto.resolve(false, false), (false, false));
    }

    #[test]
    fn a_choice_excludes_the_other_recognizer() {
        assert_eq!(Platform.resolve(true, true), (true, false));
        assert_eq!(Model.resolve(true, true), (false, true));
    }

    #[test]
    fn an_impossible_choice_falls_back_instead_of_turning_search_off() {
        assert_eq!(Model.resolve(true, false), (true, false));
        assert_eq!(Platform.resolve(false, true), (false, true));
    }
}
