// Ninimma modification: independent project branding and integration.
//! The optional search index: a per-user, encrypted list of files in the
//! folders the user chooses, for searching subfolders and indexed places.
//!
//! [`IndexService`] runs two background threads and talks to the UI only
//! through bounded channels, so nothing here runs on the UI thread:
//! - the worker finds the key, opens the database and scans, at low
//!   priority, pausing on battery, when the system is busy or disk space
//!   is low, while files are being copied, and when the user pauses it;
//! - the searcher sleeps until a query arrives, and always answers the
//!   newest one.
//!
//! When there's nothing to do both threads block; nothing polls.
//!
//! Browsing and the current-folder filter never depend on any of this.

pub mod config;
pub mod db;
pub mod extractor;
pub mod places;
pub mod scanner;
pub mod schema;
pub mod search;
pub mod system;
pub mod types;
pub mod vectors;
pub mod watcher;

use crate::security::key_store::{IndexKeyStore, KeyStoreError};
use crate::security::permissions;
use crate::security::secret::{SecretBytes, KEY_LEN};
use crate::security::vault;
use config::{Exclusions, IndexSettings};
use db::DbError;
use rusqlite::Connection;
use scanner::ScanControl;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use types::{Command, Event, IndexState, IndexStatus, SearchRequest};
use vectors::VectorIndex;

/// The key's name in the credential store and the vault.
pub const DATABASE_ID: &str = "metadata";
/// How often everything is rescanned to catch changes.
const RECONCILE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Freed space is given back to the disk once there's more than this.
const RECLAIM_OVER: u64 = 32 * 1024 * 1024;
/// Pause between batches of 256 entries, so a scan stays in the background.
const PACE: Duration = Duration::from_millis(15);
/// Indexing pauses below this much free disk space.
const MIN_FREE_BYTES: u64 = 1 << 30;
/// File changes are handled this long after they arrive, so a burst of
/// saves is one update.
const SETTLE: Duration = Duration::from_millis(800);
/// Pause between documents read, so reading stays in the background.
const READ_PACE: Duration = Duration::from_millis(10);
/// Documents read per round between checks for changes and commands.
const READ_BATCH: usize = 16;
/// Images and scanned PDFs changed this recently are read with OCR before
/// the rest of a long backlog: a receipt just downloaded is searchable by
/// what's printed on it in minutes, not after every older file is read.
const RECENT_OCR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How often a long pass checks whether recent files are waiting for OCR.
const RECENT_OCR_CHECK: Duration = Duration::from_secs(10);

/// Connects to the credential store; run on the worker thread because it
/// can block (D-Bus, Keychain).
pub type KeyStoreFactory =
    Box<dyn FnOnce() -> Result<Arc<dyn IndexKeyStore>, KeyStoreError> + Send>;

struct Shared {
    dir: PathBuf,
    user_paused: AtomicBool,
    interactive_busy: AtomicBool,
    /// Stop everything (shutdown, delete).
    stop: AtomicBool,
    /// Stop the current scan to apply new settings.
    interrupt: AtomicBool,
    search_generation: AtomicU64,
    /// The newest search waiting to run: a newer one replaces it.
    search_slot: Mutex<Option<SearchRequest>>,
    search_wake: Condvar,
    /// The key, once unlocked, for the searcher's own connection. `None`
    /// tells the searcher to close its connection.
    key: Mutex<Option<Arc<SecretBytes>>>,
    searcher_closed: (Mutex<bool>, Condvar),
    events: SyncSender<Event>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    /// The smart-search model, shared by indexing and searching, loaded
    /// only while in use.
    embedder: Arc<crate::embedding::Embedder>,
    /// The experimental query model, loaded only while in use.
    understander: crate::understand::Understander,
    /// The picture model (what photos show), shared by indexing and
    /// searching, loaded only while in use.
    pictures: crate::pictures::Pictures,
    /// Bumped when semantic.db is deleted, so the searcher reopens it.
    semantic_generation: AtomicU64,
}

impl Shared {
    fn send(&self, event: Event) {
        // Bounded: if the UI isn't reading, status updates are dropped
        // rather than piling up.
        if self.events.try_send(event).is_ok() {
            (self.repaint)();
        }
    }
}

pub struct IndexService {
    shared: Arc<Shared>,
    commands: SyncSender<Command>,
    events: Receiver<Event>,
    next_generation: u64,
}

impl IndexService {
    pub fn start(
        settings: IndexSettings,
        dir: PathBuf,
        key_store: KeyStoreFactory,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (events_tx, events) = mpsc::sync_channel(64);
        let (commands, commands_rx) = mpsc::sync_channel(16);
        let shared = Arc::new(Shared {
            dir,
            user_paused: AtomicBool::new(false),
            interactive_busy: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            interrupt: AtomicBool::new(false),
            search_generation: AtomicU64::new(0),
            search_slot: Mutex::new(None),
            search_wake: Condvar::new(),
            key: Mutex::new(None),
            searcher_closed: (Mutex::new(true), Condvar::new()),
            events: events_tx,
            repaint,
            embedder: Arc::new(embedder()),
            pictures: picture_model(),
            understander: understander(),
            semantic_generation: AtomicU64::new(0),
        });
        let worker_shared = shared.clone();
        let searcher_commands = commands.clone();
        let wake = commands.clone();
        std::thread::Builder::new()
            .name("index-worker".into())
            .spawn(move || Worker::new(worker_shared, settings, commands_rx, wake).run(key_store))
            .expect("spawn index worker");
        let searcher_shared = shared.clone();
        std::thread::Builder::new()
            .name("index-search".into())
            .spawn(move || run_searcher(searcher_shared, searcher_commands))
            .expect("spawn index searcher");
        Self {
            shared,
            commands,
            events,
            next_generation: 0,
        }
    }

    /// Events since the last call; never blocks.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Starts a search, replacing any still running. Returns its generation.
    pub fn search(&mut self, mut request: SearchRequest) -> u64 {
        self.next_generation += 1;
        request.generation = self.next_generation;
        self.shared
            .search_generation
            .store(request.generation, Ordering::SeqCst);
        *self.shared.search_slot.lock().unwrap() = Some(request);
        self.shared.search_wake.notify_all();
        self.next_generation
    }

    /// Abandons the running search (e.g. the search box was cleared).
    pub fn cancel_search(&mut self) {
        self.next_generation += 1;
        self.shared
            .search_generation
            .store(self.next_generation, Ordering::SeqCst);
        *self.shared.search_slot.lock().unwrap() = None;
    }

    pub fn send(&self, command: Command) {
        if matches!(
            command,
            Command::UpdateSettings(_)
                | Command::Rescan
                | Command::Rebuild
                | Command::DeleteIndexAndKey
        ) {
            self.shared.interrupt.store(true, Ordering::SeqCst);
        }
        if matches!(command, Command::DeleteIndexAndKey | Command::Shutdown) {
            self.shared.stop.store(true, Ordering::SeqCst);
        }
        // The worker reads commands between batches, so this only fills up
        // if the user clicks very fast; then extra clicks are dropped.
        if let Err(TrySendError::Full(c)) = self.commands.try_send(command) {
            drop(c);
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.user_paused.store(paused, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.shared.user_paused.load(Ordering::SeqCst)
    }

    /// Copying/moving files in the UI: indexing waits so they aren't slowed.
    pub fn set_interactive_busy(&self, busy: bool) {
        self.shared.interactive_busy.store(busy, Ordering::Relaxed);
    }
}

impl Drop for IndexService {
    fn drop(&mut self) {
        // The threads finish their current batch and exit on their own; the
        // database is safe however they stop (WAL + transactions).
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.commands.try_send(Command::Shutdown);
        self.shared.search_wake.notify_all();
    }
}

// ----------------------------------------------------------------------
// The worker thread
// ----------------------------------------------------------------------

struct Worker {
    shared: Arc<Shared>,
    settings: IndexSettings,
    commands: Receiver<Command>,
    status: IndexStatus,
    conn: Option<Connection>,
    key: Option<Arc<SecretBytes>>,
    key_store: Option<Arc<dyn IndexKeyStore>>,
    scan_due: bool,
    /// Cached system checks, refreshed every few seconds during a scan.
    checks: Mutex<(Instant, Option<String>)>,
    /// For the file watcher to wake this thread.
    wake: SyncSender<Command>,
    watcher: Option<watcher::IndexWatcher>,
    /// Some roots wait for their first scan before being watched.
    watch_after_scan: bool,
    /// Rescan once watching is set up (see `check_watching`).
    rescan_once_watched: bool,
    changes: Arc<watcher::Changes>,
    geocoder: places::Geocoder,
    /// semantic.db, open while smart search is on.
    vectors: Option<vectors::SqliteVectors>,
    /// Why making vectors failed; not retried until settings change.
    embed_failed: bool,
    /// The OCR model, loaded only during an OCR pass.
    ocr: crate::ocr::Ocr,
    ocr_failed: bool,
    pictures_failed: bool,
}

/// What to do after waiting for the user.
enum Next {
    Continue,
    Exit,
}

impl Worker {
    fn new(
        shared: Arc<Shared>,
        settings: IndexSettings,
        commands: Receiver<Command>,
        wake: SyncSender<Command>,
    ) -> Self {
        Self {
            shared,
            settings: settings.sanitized(),
            commands,
            status: IndexStatus::default(),
            conn: None,
            key: None,
            key_store: None,
            scan_due: true,
            checks: Mutex::new((Instant::now() - Duration::from_secs(60), None)),
            wake,
            watcher: None,
            watch_after_scan: false,
            rescan_once_watched: false,
            changes: Arc::new(watcher::Changes::default()),
            geocoder: places::Geocoder::default(),
            vectors: None,
            embed_failed: false,
            ocr: ocr_model(),
            ocr_failed: false,
            pictures_failed: false,
        }
    }

    fn exclusions(&self) -> Exclusions {
        let home = crate::file_system::home_dir();
        // Tests index folders inside the temp folder, which is normally
        // excluded; the system-folder rules are tested in `config`.
        if cfg!(test) {
            Exclusions::with_system_paths(&self.settings, home.as_deref(), Vec::new())
        } else {
            Exclusions::new(&self.settings, home.as_deref())
        }
    }

    fn stopping(&self) -> bool {
        self.shared.stop.load(Ordering::SeqCst)
    }

    /// Whether document text and photo details are indexed.
    fn reads_contents(&self) -> bool {
        self.settings.mode != config::IndexMode::Basic
    }

    /// Whether images and scanned PDFs should be read with OCR now.
    fn reads_images(&self) -> bool {
        self.settings.ocr && self.reads_contents() && self.ocr.is_available() && !self.ocr_failed
    }

    /// Reads the text in photographed documents and scanned PDFs, one at a
    /// time (~20 s each on two threads). Images that don't look like paper
    /// are skipped without running the model. Returns true if it stopped
    /// early.
    /// The oldest modification time (ns) a file can have to be read first.
    fn recent_ocr_since() -> i64 {
        (db::unix_now() - RECENT_OCR.as_secs() as i64) * 1_000_000_000
    }

    /// Whether recently changed images or scans are waiting for OCR, so a
    /// long pass should make way for them. Checked every `RECENT_OCR_CHECK`.
    fn recent_images_waiting(&self, conn: &Connection, last_check: &mut Instant) -> bool {
        if last_check.elapsed() < RECENT_OCR_CHECK || !self.reads_images() {
            return false;
        }
        *last_check = Instant::now();
        db::ocr_candidate_count(conn, Self::recent_ocr_since()).unwrap_or(0) > 0
    }

    /// With `recent`, only files changed in the last `RECENT_OCR` are read.
    fn read_images(&mut self, recent: bool) -> bool {
        let Some(mut conn) = self.conn.take() else {
            return false;
        };
        let since = if recent {
            Self::recent_ocr_since()
        } else {
            i64::MIN
        };
        let left = db::ocr_candidate_count(&conn, since).unwrap_or(0);
        if left == 0 {
            self.conn = Some(conn);
            return false;
        }
        self.ocr.set_threads(self.settings.worker_threads);
        let work = self.shared.dir.join("ocr-pages");
        let mut stopped = false;
        let mut problem = None;
        {
            let control = Control::new(self, Phase::Images);
            control.send_state(IndexState::ReadingImages { left });
            'outer: loop {
                let batch = db::ocr_candidates(&conn, 4, since).unwrap_or_default();
                if batch.is_empty() {
                    break;
                }
                for file in batch {
                    if !control.keep_going() || !self.changes.is_empty() || self.database_full() {
                        stopped = true;
                        break 'outer;
                    }
                    let is_pdf = file
                        .path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
                    let images = if is_pdf {
                        crate::ocr::render_pdf(&file.path, &work, Duration::from_secs(30))
                            .unwrap_or_default()
                    } else {
                        crate::thumbnail::decode_oriented(&file.path)
                            .filter(crate::ocr::looks_like_a_document)
                            .into_iter()
                            .collect()
                    };
                    if images.is_empty() {
                        // Not a document (or unreadable): never run the model on it.
                        let _ = db::set_ocr_state(&conn, file.id, 2);
                        continue;
                    }
                    let mut text = String::new();
                    for image in &images {
                        match self.ocr.read(image) {
                            Ok(t) => {
                                text.push_str(&t);
                                text.push('\n');
                            }
                            Err(e) => {
                                problem =
                                    Some(format!("Reading text in images is unavailable: {e}."));
                                break 'outer;
                            }
                        }
                    }
                    let text = text.trim();
                    let result = if text.is_empty() {
                        db::set_ocr_state(&conn, file.id, 2)
                    } else {
                        db::store_ocr_text(&mut conn, file.id, text, &extractor::chunks(text))
                    };
                    if result.is_err() {
                        let _ = db::set_ocr_state(&conn, file.id, 3);
                    }
                    control.progress(db::ocr_candidate_count(&conn, since).unwrap_or(0));
                    std::thread::sleep(READ_PACE);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&work);
        self.ocr.unload();
        self.ocr_failed = problem.is_some();
        // Ready only when finished: a pass that stopped early carries on,
        // and so does a backlog left after the recent files (its own pass
        // says when it's done).
        let backlog = recent
            && (db::pending_count(&conn).unwrap_or(0) > 0
                || db::ocr_candidate_count(&conn, i64::MIN).unwrap_or(0) > 0);
        self.status.ocr_problem = problem;
        self.conn = Some(conn);
        if !stopped && !backlog {
            self.set_state(IndexState::Ready);
        }
        stopped
    }

    /// Whether photos and videos should be looked at now.
    fn looks_at_pictures(&self) -> bool {
        self.settings.pictures
            && self.settings.mode == config::IndexMode::Smart
            && self.shared.pictures.is_available()
            && !self.pictures_failed
    }

    /// Gives each photo (and each video, from a few frames) its point for
    /// searching what it shows, a batch at a time, at background pace.
    /// Returns true if it stopped early.
    fn look_at_pictures(&mut self) -> bool {
        let Some(key) = self.key.clone() else {
            return false;
        };
        let videos = crate::thumbnail::can_read_videos();
        if self
            .conn
            .as_ref()
            .and_then(|c| db::picture_candidate_count(c, videos).ok())
            .unwrap_or(0)
            == 0
        {
            return false;
        }
        if self.vectors.is_none() {
            match vectors::SqliteVectors::open(&self.shared.dir, &key, false) {
                Ok(v) => self.vectors = Some(v),
                Err(e) => {
                    self.pictures_failed = true;
                    self.status.picture_problem =
                        Some(format!("The smart-search index can't be opened: {e}."));
                    self.publish();
                    return false;
                }
            }
        }
        let (Some(conn), Some(mut vecs)) = (self.conn.take(), self.vectors.take()) else {
            return false;
        };
        // Points from another model can't be compared: start over.
        let model = crate::models::PICTURE_MODEL.id;
        if vecs.picture_model().ok().flatten().as_deref() != Some(model) {
            let _ = vecs.reset_pictures(model);
            let _ = db::reset_pictures(&conn);
        }
        let pictures = &self.shared.pictures;
        pictures.set_threads(self.settings.worker_threads);
        let work = self.shared.dir.join("video-frames");
        let limit = self.settings.max_semantic_mb as u64 * 1024 * 1024;
        let mut stopped = false;
        let mut problem = None;
        {
            let control = Control::new(self, Phase::Pictures);
            control.send_state(IndexState::LookingAtPictures {
                left: db::picture_candidate_count(&conn, videos).unwrap_or(0),
            });
            'outer: loop {
                let batch = db::picture_candidates(&conn, videos, crate::pictures::BATCH)
                    .unwrap_or_default();
                if batch.is_empty() || vecs.size_on_disk() >= limit {
                    break;
                }
                if !control.keep_going() || !self.changes.is_empty() || self.database_full() {
                    stopped = true;
                    break;
                }
                let mut done = Vec::new();
                let mut unreadable = Vec::new();
                let mut photos = Vec::new();
                let mut photo_ids = Vec::new();
                for file in &batch {
                    let is_video = file.path.extension().is_some_and(|e| {
                        db::VIDEO_EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str())
                    });
                    if is_video {
                        let frames = crate::thumbnail::video_frames(&file.path, &work);
                        match pictures.video(&frames) {
                            Ok(Some(p)) => done.push((file.id, p)),
                            Ok(None) => unreadable.push(file.id),
                            Err(e) => {
                                problem = Some(format!("Searching pictures is unavailable: {e}."));
                                break 'outer;
                            }
                        }
                    } else {
                        match crate::thumbnail::decode_oriented(&file.path) {
                            Some(image) => {
                                photos.push(image);
                                photo_ids.push(file.id);
                            }
                            None => unreadable.push(file.id),
                        }
                    }
                }
                match pictures.pictures(&photos) {
                    Ok(points) => done.extend(photo_ids.into_iter().zip(points)),
                    Err(e) => {
                        problem = Some(format!("Searching pictures is unavailable: {e}."));
                        break;
                    }
                }
                if vecs.insert_pictures(&done).is_err() {
                    problem = Some("The smart-search index couldn't be written.".to_string());
                    break;
                }
                let ids: Vec<i64> = done.iter().map(|(id, _)| *id).collect();
                let _ = db::set_picture_state(&conn, &ids, 1);
                let _ = db::set_picture_state(&conn, &unreadable, 2);
                control.progress(db::picture_candidate_count(&conn, videos).unwrap_or(0));
                std::thread::sleep(READ_PACE);
            }
        }
        let _ = std::fs::remove_dir_all(&work);
        if let Ok(live) = db::picture_file_ids(&conn) {
            let _ = vecs.retain_pictures(&live);
        }
        self.shared.pictures.unload();
        self.pictures_failed = problem.is_some();
        self.status.picture_problem = problem;
        self.status.pictures = vecs.picture_count().unwrap_or(0);
        self.status.semantic_bytes = vecs.size_on_disk();
        self.conn = Some(conn);
        self.vectors = Some(vecs);
        if !stopped {
            self.set_state(IndexState::Ready);
        }
        stopped
    }

    /// Whether smart-search vectors are wanted and can be made.
    fn makes_vectors(&self) -> bool {
        self.settings.mode == config::IndexMode::Smart
            && self.shared.embedder.is_available()
            && !self.embed_failed
    }

    /// Deletes all vectors (smart search turned off, or a rebuild). The
    /// searcher's connection is closed first, then reopened on demand.
    fn drop_vectors(&mut self) {
        self.vectors = None;
        self.shared
            .semantic_generation
            .fetch_add(1, Ordering::SeqCst);
        vectors::SqliteVectors::delete_files(&self.shared.dir);
        if let Some(conn) = &self.conn {
            let _ = db::reset_embedded(conn);
        }
        self.status.vectors = 0;
        self.status.semantic_bytes = 0;
    }

    /// Makes vectors for document chunks that don't have one yet, a few at
    /// a time, at background pace. Returns true if it stopped early.
    fn make_vectors(&mut self) -> bool {
        let Some(key) = self.key.clone() else {
            return false;
        };
        if self
            .conn
            .as_ref()
            .and_then(|c| db::chunks_to_embed_count(c).ok())
            .unwrap_or(0)
            == 0
        {
            return false;
        }
        if self.vectors.is_none() {
            match vectors::SqliteVectors::open(&self.shared.dir, &key, false) {
                Ok(v) => self.vectors = Some(v),
                Err(e) => {
                    self.embed_failed = true;
                    self.status.smart_problem =
                        Some(format!("The smart-search index can't be opened: {e}."));
                    self.publish();
                    return false;
                }
            }
        }
        let embedder = self.shared.embedder.clone();
        embedder.set_threads(self.settings.worker_threads);
        let spec = embedder.spec();
        let (Some(conn), Some(vecs)) = (self.conn.take(), self.vectors.take()) else {
            return false;
        };
        let mut vecs = vecs;
        // Vectors from another model can't be compared: start over.
        if vecs.model().ok().flatten() != Some((spec.id.to_string(), spec.dimensions)) {
            let _ = vecs.reset(spec.id, spec.dimensions);
            let _ = db::reset_embedded(&conn);
        }
        let limit = self.settings.max_semantic_mb as u64 * 1024 * 1024;
        let mut stopped = false;
        let mut problem = None;
        {
            let control = Control::new(self, Phase::Vectors);
            control.send_state(IndexState::Embedding {
                left: db::chunks_to_embed_count(&conn).unwrap_or(0),
            });
            let mut checked = Instant::now();
            loop {
                let batch =
                    db::chunks_to_embed(&conn, crate::embedding::MAX_BATCH).unwrap_or_default();
                if batch.is_empty() {
                    break;
                }
                // The size limit ends the pass (it isn't "stopped early"):
                // otherwise the worker would retry it at once, forever, and
                // never get to OCR.
                if vecs.size_on_disk() >= limit {
                    break;
                }
                if !control.keep_going()
                    || !self.changes.is_empty()
                    || self.recent_images_waiting(&conn, &mut checked)
                {
                    stopped = true;
                    break;
                }
                let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
                match embedder.embed(&texts, crate::embedding::Kind::Document) {
                    Ok(embeddings) => {
                        let items: Vec<(i64, Vec<u8>, Vec<f32>)> = batch
                            .iter()
                            .zip(embeddings)
                            .map(|(c, v)| (c.file_id, c.hash.clone(), v))
                            .collect();
                        if vecs.insert(&items).is_err() {
                            problem =
                                Some("The smart-search index couldn't be written.".to_string());
                            break;
                        }
                        let ids: Vec<i64> = batch.iter().map(|c| c.id).collect();
                        let _ = db::mark_embedded(&conn, &ids);
                    }
                    Err(e) => {
                        problem = Some(format!("Smart search is unavailable: {e}."));
                        break;
                    }
                }
                let left = db::chunks_to_embed_count(&conn).unwrap_or(0);
                if control.last_sent.get().elapsed() > Duration::from_millis(500) {
                    control.send_state(IndexState::Embedding { left });
                }
                std::thread::sleep(READ_PACE);
            }
        }
        if let Ok(live) = db::live_file_ids(&conn) {
            let _ = vecs.retain_files(&live);
        }
        embedder.unload();
        self.embed_failed = problem.is_some();
        self.status.smart_problem = problem;
        if !stopped && vecs.size_on_disk() >= limit {
            self.status.smart_problem = Some(
                "Smart search reached its size limit; newer documents aren't included.".into(),
            );
        }
        self.status.vectors = vecs.count().unwrap_or(0);
        self.status.semantic_bytes = vecs.size_on_disk();
        self.conn = Some(conn);
        self.vectors = Some(vecs);
        if !stopped {
            self.set_state(IndexState::Ready);
        }
        stopped
    }

    /// (Re)starts watching the roots for changes, in the background (see
    /// `watcher`): smallest roots first, within the system's budget. Roots
    /// not scanned yet are watched after their first scan.
    fn start_watching(&mut self) {
        self.watcher = None;
        let roots: Vec<(PathBuf, Option<u64>)> = self
            .settings
            .roots
            .iter()
            .filter(|r| !system::is_network_path(r) || self.settings.network_roots.contains(r))
            .map(|r| {
                let folders = self
                    .conn
                    .as_ref()
                    .and_then(|c| db::folder_count(c, r).ok().flatten());
                (r.clone(), folders)
            })
            .collect();
        let budget = watcher::folder_budget();
        let plan = watcher::plan(&roots, budget);
        self.watch_after_scan = !plan.after_scan.is_empty();
        self.status.live_updates = false;
        self.status.unwatched_roots = plan
            .too_big
            .iter()
            .map(|r| {
                (
                    r.clone(),
                    "too many folders to watch; kept current by rescans",
                )
            })
            .chain(
                plan.after_scan
                    .iter()
                    .map(|r| (r.clone(), "watched after its first scan")),
            )
            .collect();
        if plan.watch.is_empty() {
            return;
        }
        let wake = self.wake.clone();
        self.watcher = Some(watcher::watch(
            &plan.watch,
            budget,
            Arc::new(self.exclusions()),
            self.changes.clone(),
            move || {
                // A full channel already has a wake-up queued.
                let _ = wake.try_send(Command::Wake);
            },
        ));
    }

    /// Takes in the result once watching has been set up.
    fn check_watching(&mut self) {
        let Some(finished) = self.watcher.as_mut().and_then(|w| w.take_finished()) else {
            return;
        };
        self.status.live_updates =
            finished == watcher::Finished::default() && self.status.unwatched_roots.is_empty();
        self.status.unwatched_roots.extend(
            finished
                .too_big
                .into_iter()
                .map(|r| (r, "too many folders to watch; kept current by rescans")),
        );
        self.status.unwatched_roots.extend(
            finished
                .failed
                .into_iter()
                .map(|r| (r, "couldn't be watched (the system's watch limit?)")),
        );
        if std::mem::take(&mut self.rescan_once_watched) {
            // Roots watched only after their first scan: catch what changed
            // during that scan, which no watcher saw.
            self.scan_due = true;
        }
        self.publish();
    }

    fn db_path(&self) -> PathBuf {
        self.shared.dir.join(db::METADATA_DB)
    }

    fn set_state(&mut self, state: IndexState) {
        self.status.state = state;
        self.publish();
    }

    fn publish(&mut self) {
        if let Some(conn) = &self.conn {
            self.status.indexed_files = db::file_count(conn).unwrap_or(self.status.indexed_files);
            self.status.last_scan = db::last_scan(conn).ok().flatten();
        }
        self.status.database_bytes = db::size_on_disk(&self.db_path());
        self.shared.send(Event::Status(self.status.clone()));
    }

    fn fail(&mut self, message: String, can_rebuild: bool) {
        self.set_state(IndexState::Failed {
            message,
            can_rebuild,
        });
    }

    fn run(mut self, key_store: KeyStoreFactory) {
        system::lower_thread_priority();
        if let Err(e) = permissions::prepare_index_dir(&self.shared.dir) {
            self.fail(format!("The index folder isn't safe to use: {e}."), false);
            return self.wait_for_exit();
        }
        match self.unlock(key_store) {
            Some(key) => self.key = Some(Arc::new(key)),
            None => return self.wait_for_exit(),
        }
        if !self.open_database() {
            return self.wait_for_exit();
        }
        self.serve();
    }

    /// Finds or creates the key. `None` means stop (an error was shown, or
    /// the app is closing).
    fn unlock(&mut self, factory: KeyStoreFactory) -> Option<SecretBytes> {
        let dir = self.shared.dir.clone();
        let db_exists = self.db_path().exists();

        if vault::exists(&dir) {
            self.status.key_source = Some("Passphrase");
            self.set_state(IndexState::NeedsPassphrase { error: None });
            return self.wait_for_passphrase(|pass| vault::unlock(&dir, DATABASE_ID, pass), true);
        }

        let keyring_down = "The system keyring isn't available, so the index can't be opened. \
                            Choose Try again once it's running, or delete and rebuild the index.";
        let store = match factory() {
            Ok(store) => store,
            Err(_) if db_exists => {
                self.fail(keyring_down.into(), true);
                return None;
            }
            // No credential store on this system: protect the key with a
            // passphrase instead.
            Err(_) => return self.new_passphrase(),
        };
        self.key_store = Some(store.clone());
        match store.load(DATABASE_ID) {
            Ok(Some(key)) => {
                self.status.key_source = Some("System keyring");
                Some(key)
            }
            Ok(None) if !db_exists => {
                // First run: a new random key, saved, then read back to be
                // sure the store really kept it.
                let Ok(key) = SecretBytes::random(KEY_LEN) else {
                    self.fail("The system's random number generator failed.".into(), false);
                    return None;
                };
                let kept = store
                    .store(DATABASE_ID, &key)
                    .and_then(|()| store.load(DATABASE_ID))
                    .is_ok_and(|k| k.is_some_and(|k| k == key));
                if !kept {
                    let _ = store.delete(DATABASE_ID);
                    self.key_store = None;
                    return self.new_passphrase();
                }
                self.status.key_source = Some("System keyring");
                Some(key)
            }
            Ok(None) => {
                self.fail(
                    "The index's key is missing from the system keyring, so the index can't be opened. \
                     Delete and rebuild it to start again."
                        .into(),
                    true,
                );
                None
            }
            Err(KeyStoreError::Locked) => {
                self.fail(
                    "The system keyring is locked. Unlock it (or log in again), then choose Try again.".into(),
                    false,
                );
                None
            }
            Err(_) if db_exists => {
                self.fail(keyring_down.into(), true);
                None
            }
            Err(_) => {
                self.key_store = None;
                self.new_passphrase()
            }
        }
    }

    /// Moves the key from the passphrase vault into the system keyring, so
    /// the index unlocks at login like other saved passwords. The vault is
    /// deleted only once the keyring has given the key back intact; until
    /// then, and if anything fails, the passphrase stays in use.
    fn use_keyring(&mut self, factory: KeyStoreFactory) {
        let dir = self.shared.dir.clone();
        let Some(key) = self.key.clone().filter(|_| vault::exists(&dir)) else {
            return; // already in the keyring, or not unlocked
        };
        let kept = factory().and_then(|store| {
            store.store(DATABASE_ID, &key)?;
            match store.load(DATABASE_ID)? {
                Some(k) if k == *key => Ok(store),
                _ => {
                    let _ = store.delete(DATABASE_ID);
                    Err(KeyStoreError::Failed)
                }
            }
        });
        self.status.key_message = Some(match kept {
            Ok(store) => {
                if vault::delete(&dir).is_ok() {
                    self.key_store = Some(store);
                    self.status.key_source = Some("System keyring");
                    "The key is now in the system keyring: the index unlocks when you log in."
                        .into()
                } else {
                    // Keep one copy: the passphrase still works.
                    let _ = store.delete(DATABASE_ID);
                    "The passphrase file couldn't be removed, so the passphrase is still used."
                        .into()
                }
            }
            Err(KeyStoreError::Unavailable) => {
                "No system keyring is running (GNOME Keyring or KWallet), so the passphrase is still used."
                    .into()
            }
            Err(KeyStoreError::Locked) => {
                "The system keyring is locked or refused access. Unlock it and try again."
                    .into()
            }
            Err(_) => "The system keyring didn't keep the key, so the passphrase is still used."
                .into(),
        });
        self.publish();
    }

    fn new_passphrase(&mut self) -> Option<SecretBytes> {
        let dir = self.shared.dir.clone();
        self.status.key_source = Some("Passphrase");
        self.set_state(IndexState::NeedsNewPassphrase { error: None });
        self.wait_for_passphrase(|pass| vault::create(&dir, DATABASE_ID, pass), false)
    }

    /// Waits for the user's passphrase and tries it until it works.
    fn wait_for_passphrase(
        &mut self,
        attempt: impl Fn(&str) -> Result<SecretBytes, vault::VaultError>,
        existing: bool,
    ) -> Option<SecretBytes> {
        loop {
            let pass = match self.commands.recv().ok()? {
                Command::Unlock(p) if existing => p,
                Command::CreateWithPassphrase(p) if !existing => p,
                Command::DeleteIndexAndKey => {
                    self.delete_all();
                    return None;
                }
                Command::Shutdown => return None,
                Command::UpdateSettings(s) => {
                    self.settings = s.sanitized();
                    continue;
                }
                _ => continue,
            };
            match attempt(&pass) {
                Ok(key) => return Some(key),
                Err(e) => {
                    let error = Some(format!("{}.", capitalize(&e.to_string())));
                    self.set_state(if existing {
                        IndexState::NeedsPassphrase { error }
                    } else {
                        IndexState::NeedsNewPassphrase { error }
                    });
                }
            }
        }
    }

    fn open_database(&mut self) -> bool {
        let key = self.key.clone().expect("unlocked");
        match db::open(&self.db_path(), &key, true, false) {
            Ok(conn) => {
                self.conn = Some(conn);
                *self.shared.key.lock().unwrap() = Some(key);
                self.shared.search_wake.notify_all();
                true
            }
            Err(e) => {
                let can_rebuild =
                    !matches!(e, DbError::EncryptionUnavailable | DbError::Permissions(_));
                let advice = if can_rebuild {
                    " Delete and rebuild it to start again; your files aren't affected."
                } else {
                    ""
                };
                self.fail(
                    format!("The index can't be opened: {e}.{advice}"),
                    can_rebuild,
                );
                false
            }
        }
    }

    /// After an error: waits until the app closes or the user deletes the
    /// index.
    fn wait_for_exit(mut self) {
        while let Ok(command) = self.commands.recv() {
            match command {
                Command::DeleteIndexAndKey => return self.delete_all(),
                Command::Shutdown => return,
                _ => {}
            }
        }
    }

    fn serve(mut self) {
        let since = db::unix_now() - RECONCILE_EVERY.as_secs() as i64;
        self.scan_due = self
            .conn
            .as_ref()
            .is_none_or(|c| db::needs_scan(c, &self.settings.roots, since).unwrap_or(true));
        if self.settings.mode != config::IndexMode::Smart {
            // Vectors from an earlier Smart session aren't wanted now.
            self.drop_vectors();
        }
        if let Some(conn) = &self.conn {
            if !self.reads_contents() {
                // Text was indexed before but isn't wanted now: drop it.
                let _ = db::clear_contents(conn);
            } else if extractor::pdf_reader_available() {
                let _ = db::retry_needing_tool(conn, extractor::State::NeedsTool as i64);
            }
        }
        self.start_watching();
        self.set_state(IndexState::Ready);
        loop {
            self.check_watching();
            if self.scan_due && !self.stopping() {
                self.scan_due = false;
                self.scan_all();
            }
            if !self.changes.is_empty() && !self.stopping() {
                self.apply_changes();
                continue;
            }
            // Vectors come before OCR, so documents are searchable by
            // meaning without waiting for slow OCR (20-35 s a page); a
            // second vector pass takes in the text OCR read.
            // Recent images and scans come first (a receipt just saved);
            // the long passes below make way when more arrive.
            let stopped_early = (self.reads_images() && !self.stopping() && self.read_images(true))
                || (self.reads_contents() && !self.stopping() && self.read_contents())
                || (self.makes_vectors() && !self.stopping() && self.make_vectors())
                || (self.looks_at_pictures() && !self.stopping() && self.look_at_pictures())
                || (self.reads_images() && !self.stopping() && self.read_images(false))
                || (self.makes_vectors() && !self.stopping() && self.make_vectors());
            if stopped_early {
                // Handle changes or a command, then go on.
                if let Ok(command) = self.commands.try_recv() {
                    if let Next::Exit = self.handle(command) {
                        return;
                    }
                }
                continue;
            }
            // Idle: block until a command or the next reconcile.
            let command = match self.commands.recv_timeout(RECONCILE_EVERY) {
                Ok(c) => c,
                Err(RecvTimeoutError::Timeout) => {
                    self.scan_due = true;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };
            match self.handle(command) {
                Next::Continue => {}
                Next::Exit => return,
            }
        }
    }

    fn handle(&mut self, command: Command) -> Next {
        self.shared.interrupt.store(false, Ordering::SeqCst);
        match command {
            Command::UpdateSettings(s) => {
                let s = s.sanitized();
                let rescan = s.roots != self.settings.roots
                    || s.exclusions != self.settings.exclusions
                    || s.network_roots != self.settings.network_roots
                    || s.index_hidden != self.settings.index_hidden;
                let text_off = s.mode == config::IndexMode::Basic && self.reads_contents();
                let smart_off = s.mode != config::IndexMode::Smart;
                let ocr_off = self.settings.ocr && !s.ocr;
                self.settings = s;
                self.embed_failed = false;
                self.ocr_failed = false;
                self.status.smart_problem = None;
                self.status.ocr_problem = None;
                if ocr_off {
                    if let Some(conn) = &mut self.conn {
                        let _ = db::clear_ocr(conn);
                    }
                }
                if smart_off {
                    self.drop_vectors();
                }
                self.scan_due |= rescan;
                if rescan {
                    self.start_watching();
                }
                if text_off {
                    if let Some(conn) = &self.conn {
                        let _ = db::clear_contents(conn);
                    }
                }
                self.publish();
            }
            Command::Wake => {
                // Let a burst of changes finish arriving.
                std::thread::sleep(SETTLE);
            }
            Command::RebuildSemantic => {
                self.drop_vectors();
                self.embed_failed = false;
                self.publish();
            }
            Command::Rescan => self.scan_due = true,
            Command::Rebuild => {
                self.watcher = None;
                self.close_searcher();
                self.conn = None;
                remove_database_files(&self.db_path());
                self.vectors = None;
                vectors::SqliteVectors::delete_files(&self.shared.dir);
                self.shared
                    .semantic_generation
                    .fetch_add(1, Ordering::SeqCst);
                if !self.open_database() {
                    return Next::Exit;
                }
                self.scan_due = true;
                self.start_watching();
                self.set_state(IndexState::Ready);
            }
            Command::DeleteIndexAndKey => {
                self.delete_all();
                return Next::Exit;
            }
            Command::Forget(paths) => {
                if let Some(conn) = &self.conn {
                    let _ = db::remove_paths(conn, &paths);
                }
            }
            Command::Shutdown => return Next::Exit,
            Command::UseKeyring(factory) => self.use_keyring(factory),
            Command::Unlock(_) | Command::CreateWithPassphrase(_) => {}
        }
        Next::Continue
    }

    fn scan_all(&mut self) {
        let Some(mut conn) = self.conn.take() else {
            return;
        };
        let exclusions = self.exclusions();
        let _ = db::remove_other_roots(&conn, &self.settings.roots);
        // A removed folder can leave most of the file free: give it back,
        // so the size limit (and the disk) see the real size.
        let _ = db::reclaim_space(&conn, RECLAIM_OVER);
        self.status.skipped_roots.clear();
        self.set_state(IndexState::Indexing { files: 0 });
        let mut finished = true;
        for root in self.settings.roots.clone() {
            if exclusions.excludes_root(&root) {
                self.status
                    .skipped_roots
                    .push((root, "inside an excluded location"));
                continue;
            }
            if system::is_network_path(&root) && !self.settings.network_roots.contains(&root) {
                self.status
                    .skipped_roots
                    .push((root, "network location (allow it in settings)"));
                continue;
            }
            let control = Control::new(self, Phase::Scan);
            match scanner::scan_root(&mut conn, &root, &exclusions, &control, PACE) {
                Ok(stats) => finished &= !stats.incomplete,
                Err(_) => finished = false,
            }
            if self.shared.stop.load(Ordering::SeqCst)
                || self.shared.interrupt.load(Ordering::SeqCst)
            {
                finished = false;
                break;
            }
        }
        let _ = db::checkpoint(&conn);
        let _ = conn.execute_batch("PRAGMA optimize");
        // Files gone since the last scan were deleted above.
        let _ = db::reclaim_space(&conn, RECLAIM_OVER);
        self.conn = Some(conn);
        if finished && self.watch_after_scan {
            // Roots scanned for the first time can now be sized and watched;
            // once they are, one more scan catches changes made meanwhile.
            self.start_watching();
            self.rescan_once_watched = true;
        }
        let full = self.database_full();
        self.set_state(if full {
            IndexState::Paused("the index reached its size limit".into())
        } else {
            IndexState::Ready
        });
        if !finished && !full && !self.shared.stop.load(Ordering::SeqCst) {
            // Interrupted to apply new settings: run again with them.
            self.scan_due |= self.shared.interrupt.load(Ordering::SeqCst);
        }
    }

    /// Updates the index for files changed on disk.
    fn apply_changes(&mut self) {
        let (paths, overflowed) = self.changes.take();
        if overflowed {
            // Too many to follow one by one: rescan instead.
            self.scan_due = true;
            return;
        }
        let Some(mut conn) = self.conn.take() else {
            return;
        };
        let exclusions = self.exclusions();
        {
            let control = Control::new(self, Phase::Scan);
            for path in &paths {
                if self.shared.stop.load(Ordering::SeqCst) {
                    break;
                }
                let _ = scanner::update_path(&mut conn, path, &exclusions, &control);
            }
        }
        self.conn = Some(conn);
        self.publish();
    }

    /// Reads the text and photo details of files waiting for it, newest
    /// first. Returns true if it stopped early (changes arrived, a command
    /// came, or the index is full).
    fn read_contents(&mut self) -> bool {
        let Some(mut conn) = self.conn.take() else {
            return false;
        };
        let pending = db::pending_count(&conn).unwrap_or(0);
        if pending == 0 {
            self.conn = Some(conn);
            return false;
        }
        let limits = extractor::Limits::new(self.settings.max_extract_bytes);
        let mut geocoder = std::mem::take(&mut self.geocoder);
        let mut stopped = false;
        {
            let control = Control::new(self, Phase::Read);
            control.send_state(IndexState::Reading { left: pending });
            let mut checked = Instant::now();
            'outer: loop {
                let batch = db::pending_extraction(&conn, READ_BATCH).unwrap_or_default();
                if batch.is_empty() {
                    break;
                }
                if self.recent_images_waiting(&conn, &mut checked) {
                    stopped = true;
                    break;
                }
                control.progress(db::pending_count(&conn).unwrap_or(0));
                for file in batch {
                    if !control.keep_going() || !self.changes.is_empty() || self.database_full() {
                        stopped = true;
                        break 'outer;
                    }
                    let outcome = if file.is_dir {
                        extractor::Outcome::Skipped(extractor::State::Done)
                    } else {
                        extractor::extract(&file.path, file.size, &limits)
                    };
                    // Only files that were actually opened and read are
                    // paced; skipping an unsupported file costs nothing.
                    let did_work = !matches!(outcome, extractor::Outcome::Skipped(_));
                    let (state, error, details) = match outcome {
                        extractor::Outcome::Read(d) => (extractor::State::Done, None, d),
                        extractor::Outcome::Skipped(s) => (s, None, Default::default()),
                        extractor::Outcome::Failed(why) => {
                            (extractor::State::Failed, Some(why), Default::default())
                        }
                    };
                    let place = details.gps.and_then(|(lat, lon)| geocoder.place(lat, lon));
                    let chunks = extractor::chunks(&details.text);
                    let contents = db::Contents {
                        state: state as i64,
                        error,
                        text: &details.text,
                        chunks: &chunks,
                        taken_ns: details.taken_ns,
                        gps: details.gps,
                        place: place
                            .as_ref()
                            .map(|p| (p.display.as_str(), p.searchable.as_str())),
                    };
                    if db::store_contents(&mut conn, file.id, &contents).is_err() {
                        // Never retry the same file forever.
                        let _ = db::mark_failed(&conn, file.id);
                    }
                    if did_work {
                        std::thread::sleep(READ_PACE);
                    }
                }
            }
        }
        geocoder.release();
        self.geocoder = geocoder;
        self.conn = Some(conn);
        let full = self.database_full();
        // Ready only when finished: a pass that stopped early carries on.
        if !stopped || full {
            self.set_state(if full {
                IndexState::Paused("the index reached its size limit".into())
            } else {
                IndexState::Ready
            });
        }
        stopped && !full
    }

    /// Whether the index has reached its size limit: the space it really
    /// uses (not counting pages freed by deletions), or, while a scan holds
    /// the connection, the files' size (made accurate by `reclaim_space`).
    fn database_full(&self) -> bool {
        let used = self
            .conn
            .as_ref()
            .and_then(|c| db::used_bytes(c, &self.db_path()).ok())
            .unwrap_or_else(|| db::size_on_disk(&self.db_path()));
        used >= self.settings.max_database_mb as u64 * 1024 * 1024
    }

    /// Why indexing should wait right now, if it should. The system checks
    /// are cached for a few seconds; they're asked after every batch.
    fn pause_reason(&self) -> Option<String> {
        if self.shared.user_paused.load(Ordering::SeqCst) {
            return Some("paused".into());
        }
        if self.shared.interactive_busy.load(Ordering::Relaxed) {
            return Some("waiting while files are copied or moved".into());
        }
        let mut cache = self.checks.lock().unwrap();
        if cache.0.elapsed() > Duration::from_secs(10) {
            let reason = if self.settings.pause_on_battery && system::on_battery() {
                Some("on battery power".to_string())
            } else if system::under_pressure() {
                Some("the computer is busy".to_string())
            } else if system::free_space(&self.shared.dir).is_some_and(|f| f < MIN_FREE_BYTES) {
                Some("disk space is low".to_string())
            } else {
                None
            };
            *cache = (Instant::now(), reason);
        }
        cache.1.clone()
    }

    fn close_searcher(&self) {
        *self.shared.key.lock().unwrap() = None;
        self.shared.search_generation.fetch_add(1, Ordering::SeqCst);
        self.shared.search_wake.notify_all();
        let (lock, cv) = &self.shared.searcher_closed;
        let closed = lock.lock().unwrap();
        let _ = cv.wait_timeout_while(closed, Duration::from_secs(10), |closed| !*closed);
    }

    /// Deletes the index and its key. The user's own files are never
    /// touched: only files this module created in the index folder.
    fn delete_all(&mut self) {
        self.watcher = None;
        self.close_searcher();
        self.conn = None;
        let dir = self.shared.dir.clone();
        remove_database_files(&self.db_path());
        self.vectors = None;
        vectors::SqliteVectors::delete_files(&dir);
        let _ = vault::delete(&dir);
        let _ = std::fs::remove_file(dir.join(format!("{}.new", vault::FILE_NAME)));
        let store = self.key_store.clone().or_else(|| {
            crate::security::key_store::OsKeyStore::connect()
                .ok()
                .map(|s| Arc::new(s) as Arc<dyn IndexKeyStore>)
        });
        if let Some(store) = store {
            let _ = store.delete(DATABASE_ID);
        }
        self.key = None;
        self.status = IndexStatus {
            state: IndexState::Deleted,
            ..IndexStatus::default()
        };
        self.shared.send(Event::Status(self.status.clone()));
    }
}

fn remove_database_files(db_path: &Path) {
    let _ = std::fs::remove_file(db_path);
    for suffix in db::SIDE_FILES {
        let _ = std::fs::remove_file(db::side_file(db_path, suffix));
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The scanner's view of the worker: when to pause or stop.
/// Which pass is running, for progress and the state after a pause.
#[derive(Clone, Copy)]
enum Phase {
    Scan,
    Read,
    Images,
    Vectors,
    Pictures,
}

struct Control<'a> {
    worker: &'a Worker,
    phase: Phase,
    files: Cell<u64>,
    last_sent: Cell<Instant>,
}

impl<'a> Control<'a> {
    fn new(worker: &'a Worker, phase: Phase) -> Self {
        Self {
            worker,
            phase,
            files: Cell::new(0),
            last_sent: Cell::new(Instant::now() - Duration::from_secs(1)),
        }
    }

    fn busy_state(&self, n: u64) -> IndexState {
        match self.phase {
            Phase::Scan => IndexState::Indexing { files: n },
            Phase::Read => IndexState::Reading { left: n },
            Phase::Images => IndexState::ReadingImages { left: n },
            Phase::Vectors => IndexState::Embedding { left: n },
            Phase::Pictures => IndexState::LookingAtPictures { left: n },
        }
    }

    fn send_state(&self, state: IndexState) {
        let mut status = self.worker.status.clone();
        status.state = state;
        self.worker.shared.send(Event::Status(status));
        self.last_sent.set(Instant::now());
    }
}

impl scanner::ScanControl for Control<'_> {
    fn keep_going(&self) -> bool {
        let shared = &self.worker.shared;
        let mut announced: Option<String> = None;
        loop {
            if shared.stop.load(Ordering::SeqCst) || shared.interrupt.load(Ordering::SeqCst) {
                return false;
            }
            match self.worker.pause_reason() {
                None => {
                    if announced.is_some() {
                        self.send_state(self.busy_state(self.files.get()));
                    }
                    return true;
                }
                Some(reason) => {
                    if announced.as_ref() != Some(&reason) {
                        self.send_state(IndexState::Paused(reason.clone()));
                        announced = Some(reason);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    fn progress(&self, files: u64) {
        self.files.set(files);
        // At most two updates a second, so the UI isn't kept busy.
        if self.last_sent.get().elapsed() > Duration::from_millis(500) {
            self.send_state(self.busy_state(files));
        }
    }

    fn database_full(&self) -> bool {
        self.worker.database_full()
    }
}

// ----------------------------------------------------------------------
// The searcher thread
// ----------------------------------------------------------------------

/// The smart-search model. Tests can point at a model file with
/// NINIMMA_TEST_MODEL (the opt-in model tests do).
fn embedder() -> crate::embedding::Embedder {
    let spec = &crate::models::EMBEDDING_MODEL;
    match std::env::var_os("NINIMMA_TEST_MODEL") {
        Some(path) if cfg!(test) => crate::embedding::Embedder::at(spec, path.into()),
        _ => crate::embedding::Embedder::new(spec),
    }
}

/// The query model. Tests can point at its file with
/// NINIMMA_TEST_QUERY_MODEL.
fn understander() -> crate::understand::Understander {
    match std::env::var_os("NINIMMA_TEST_QUERY_MODEL") {
        Some(path) if cfg!(test) => crate::understand::Understander::at(path.into()),
        _ => crate::understand::Understander::new(),
    }
}

/// The OCR model. Tests can point at a folder holding its files with
/// NINIMMA_TEST_OCR (the opt-in OCR tests do).
/// Tests use the picture model only when NINIMMA_TEST_PICTURES names
/// a folder with its files.
fn picture_model() -> crate::pictures::Pictures {
    match std::env::var_os("NINIMMA_TEST_PICTURES") {
        Some(dir) if cfg!(test) => {
            let dir = PathBuf::from(dir);
            crate::pictures::Pictures::at(
                dir.join(crate::models::PICTURE_MODEL.file),
                dir.join(crate::models::PICTURE_TOKENIZER.file),
            )
        }
        _ => crate::pictures::Pictures::new(),
    }
}

fn ocr_model() -> crate::ocr::Ocr {
    match std::env::var_os("NINIMMA_TEST_OCR") {
        Some(dir) if cfg!(test) => {
            let dir = PathBuf::from(dir);
            crate::ocr::Ocr::at(
                dir.join(crate::models::OCR_MODEL.file),
                dir.join(crate::models::OCR_PROJECTOR.file),
            )
        }
        _ => crate::ocr::Ocr::new(),
    }
}

/// Models are freed after this long without a search that uses them.
const MODEL_IDLE: Duration = Duration::from_secs(60);
/// Files considered from the vectors per search.
const MEANING_CANDIDATES: usize = 200;

fn run_searcher(shared: Arc<Shared>, commands: SyncSender<Command>) {
    let mut conn: Option<Connection> = None;
    let mut semantic: Option<(vectors::SqliteVectors, u64)> = None;
    // The generation being run, for the interrupt check inside SQLite.
    let running = Arc::new(AtomicU64::new(0));
    loop {
        let request = {
            let mut slot = shared.search_slot.lock().unwrap();
            loop {
                if shared.stop.load(Ordering::SeqCst) && slot.is_none() {
                    return;
                }
                let key_gone = shared.key.lock().unwrap().is_none();
                if key_gone && conn.is_some() {
                    break None; // close the connection below
                }
                if let Some(r) = slot.take() {
                    break Some(r);
                }
                if shared.embedder.is_loaded()
                    || shared.understander.is_loaded()
                    || shared.pictures.is_loaded()
                {
                    // One timed wake-up to free the models once idle.
                    let (s, timeout) = shared.search_wake.wait_timeout(slot, MODEL_IDLE).unwrap();
                    slot = s;
                    if timeout.timed_out() {
                        shared.embedder.unload_if_idle(MODEL_IDLE);
                        shared.understander.unload_if_idle(MODEL_IDLE);
                        shared.pictures.unload_if_idle(MODEL_IDLE);
                    }
                } else {
                    slot = shared.search_wake.wait(slot).unwrap();
                }
            }
        };
        if shared.key.lock().unwrap().is_none() {
            conn = None;
            semantic = None;
            let (lock, cv) = &shared.searcher_closed;
            *lock.lock().unwrap() = true;
            cv.notify_all();
            if let Some(r) = request {
                shared.send(Event::SearchError {
                    generation: r.generation,
                    message: "The index isn't ready yet.".into(),
                });
            }
            continue;
        }
        let Some(request) = request else { continue };
        if conn.is_none() {
            let key = shared.key.lock().unwrap().clone();
            conn =
                key.and_then(|k| db::open(&shared.dir.join(db::METADATA_DB), &k, false, true).ok());
            if let Some(c) = &conn {
                let (shared2, running2) = (shared.clone(), running.clone());
                // If this fails, searches still work; they just can't be
                // interrupted part-way.
                let _ = c.progress_handler(
                    5_000,
                    Some(move || {
                        shared2.stop.load(Ordering::Relaxed)
                            || shared2.search_generation.load(Ordering::Relaxed)
                                != running2.load(Ordering::Relaxed)
                    }),
                );
                *shared.searcher_closed.0.lock().unwrap() = false;
            }
        }
        let Some(c) = &conn else {
            shared.send(Event::SearchError {
                generation: request.generation,
                message: "The index couldn't be opened for searching.".into(),
            });
            continue;
        };
        let superseded = || shared.search_generation.load(Ordering::SeqCst) != request.generation;
        if superseded() {
            continue; // already replaced by a newer search
        }
        let mut query = match search::parse(&request.text) {
            Ok(q) => q,
            Err(message) => {
                shared.send(Event::SearchError {
                    generation: request.generation,
                    message: capitalize(&message),
                });
                continue;
            }
        };
        running.store(request.generation, Ordering::SeqCst);
        let meaning = if request.smart && !query.words.is_empty() && shared.embedder.is_available()
        {
            let generation = shared.semantic_generation.load(Ordering::SeqCst);
            if semantic.as_ref().is_some_and(|(_, g)| *g != generation) {
                semantic = None;
            }
            if semantic.is_none() {
                let key = shared.key.lock().unwrap().clone();
                semantic = key
                    .and_then(|k| vectors::SqliteVectors::open(&shared.dir, &k, true).ok())
                    .map(|v| (v, generation));
            }
            match &semantic {
                Some((vecs, _)) => {
                    let text = query.words.join(" ");
                    match shared
                        .embedder
                        .embed(&[text], crate::embedding::Kind::Query)
                    {
                        Ok(q) => vecs
                            .search(&q[0], MEANING_CANDIDATES, &superseded)
                            .unwrap_or_default(),
                        Err(_) => Vec::new(),
                    }
                }
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        // Photos and videos by what they show: the words compared with each
        // picture's point (no model load unless pictures were indexed).
        let pictures = if request.smart && !query.words.is_empty() && shared.pictures.is_available()
        {
            if semantic.is_none() {
                let generation = shared.semantic_generation.load(Ordering::SeqCst);
                let key = shared.key.lock().unwrap().clone();
                semantic = key
                    .and_then(|k| vectors::SqliteVectors::open(&shared.dir, &k, true).ok())
                    .map(|v| (v, generation));
            }
            match &semantic {
                Some((vecs, _)) if vecs.picture_count().unwrap_or(0) > 0 => {
                    match shared.pictures.query(&query.words.join(" ")) {
                        Ok(q) => vecs
                            .search_pictures(&q, crate::pictures::MATCH_FLOOR, &superseded)
                            .unwrap_or_default(),
                        Err(_) => Vec::new(),
                    }
                }
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        // The experimental query model's reading, for sentences. The first
        // page is shown from the rules straight away and replaced when the
        // model answers; later pages use its (cached) reading directly.
        let understand = request.understand
            && request.full_text
            && query.words.len() > 1
            && crate::understand::is_sentence(&request.text)
            && shared.understander.is_available();
        let ask = |query: &search::Query| {
            shared
                .understander
                .understand(&request.text, &superseded)
                .map(|u| crate::understand::merge(query.clone(), &u))
                .filter(|q| !q.emphasis.is_empty())
        };
        let mut refined = false;
        if understand && request.offset > 0 {
            if let Some(q) = ask(&query) {
                query = q;
                refined = true;
            }
        }
        let send = |query: &search::Query, refined: bool| -> bool {
            match search::run(c, &request, query, &meaning, &pictures, &superseded) {
                Ok((mut results, stale)) => {
                    if refined {
                        results.refined = true;
                        results.filters.push("re-ranked by the local model".into());
                    }
                    if !superseded() {
                        shared.send(Event::Results(results));
                    }
                    if !stale.is_empty() {
                        let _ = commands.try_send(Command::Forget(stale));
                    }
                    true
                }
                Err(_) if superseded() => false, // interrupted by a newer search
                Err(_) => {
                    shared.send(Event::SearchError {
                        generation: request.generation,
                        message: "The search failed.".into(),
                    });
                    false
                }
            }
        };
        if !send(&query, refined) || refined || !understand || superseded() {
            continue;
        }
        if let Some(q) = ask(&query) {
            if !superseded() {
                send(&q, true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod bench;
    // Needs the models (llama.cpp), so only with smart search.
    #[cfg(feature = "smart-search")]
    mod eval;
    mod fixtures;
    use crate::security::key_store::MemoryKeyStore;
    use chrono::TimeZone;
    use types::{MatchReason, SearchScope};
    use zeroize::Zeroizing;

    struct Running {
        service: IndexService,
        store: Arc<MemoryKeyStore>,
        dir: tempfile::TempDir,
        root: tempfile::TempDir,
        last: Option<IndexStatus>,
    }

    fn start_with(
        store: Arc<MemoryKeyStore>,
        dir: tempfile::TempDir,
        root: tempfile::TempDir,
    ) -> Running {
        let settings = IndexSettings {
            enabled: true,
            roots: vec![root.path().to_path_buf()],
            pause_on_battery: false,
            ..Default::default()
        };
        let factory_store = store.clone();
        let service = IndexService::start(
            settings,
            dir.path().join("ninimma/index"),
            Box::new(move || Ok(factory_store as Arc<dyn IndexKeyStore>)),
            Arc::new(|| {}),
        );
        Running {
            service,
            store,
            dir,
            root,
            last: None,
        }
    }

    fn start(files: &[&str]) -> Running {
        let root = tempfile::tempdir().unwrap();
        for f in files {
            std::fs::write(root.path().join(f), "").unwrap();
        }
        start_with(
            Arc::new(MemoryKeyStore::default()),
            tempfile::tempdir().unwrap(),
            root,
        )
    }

    impl Running {
        fn index_dir(&self) -> PathBuf {
            self.dir.path().join("ninimma/index")
        }

        /// Waits (up to 20 s) for an event matching `want`.
        fn wait_for(&mut self, what: &str, want: impl FnMut(&Event) -> bool) -> Event {
            self.wait_longer(what, Duration::from_secs(20), want)
        }

        fn wait_longer(
            &mut self,
            what: &str,
            limit: Duration,
            mut want: impl FnMut(&Event) -> bool,
        ) -> Event {
            let deadline = Instant::now() + limit;
            while Instant::now() < deadline {
                for event in self.service.poll() {
                    if let Event::Status(s) = &event {
                        self.last = Some(s.clone());
                    }
                    if want(&event) {
                        return event;
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("timed out waiting for {what}; last status {:?}", self.last);
        }

        fn wait_until_indexed(&mut self, files: u64) {
            self.wait_for("the scan", |e| {
                matches!(e, Event::Status(s) if s.state == IndexState::Ready && s.indexed_files == files)
            });
        }

        fn search(&mut self, text: &str) -> Vec<String> {
            let generation = self.service.search(SearchRequest {
                generation: 0,
                text: text.into(),
                scope: SearchScope::Everywhere,
                folder: self.root.path().into(),
                offset: 0,
                limit: 50,
                full_text: true,
                smart: false,
                understand: false,
            });
            match self.wait_for("results", |e| match e {
                Event::Results(r) => r.generation == generation,
                Event::SearchError { generation: g, .. } => *g == generation,
                _ => false,
            }) {
                Event::Results(r) => r.hits.into_iter().map(|h| h.item.name).collect(),
                Event::SearchError { message, .. } => panic!("search failed: {message}"),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn first_run_creates_a_key_indexes_and_searches() {
        let mut r = start(&["holiday.jpg", "tax-2025.pdf", "notes.txt"]);
        r.wait_until_indexed(3);
        assert!(
            r.store.load(DATABASE_ID).unwrap().is_some(),
            "key saved in the store"
        );
        assert_eq!(r.last.as_ref().unwrap().key_source, Some("System keyring"));
        assert_eq!(r.search("tax"), ["tax-2025.pdf"]);
        assert_eq!(r.search("type:image"), ["holiday.jpg"]);
        // The database is encrypted and private.
        let db_file = r.index_dir().join(db::METADATA_DB);
        assert!(!std::fs::read(&db_file)
            .unwrap()
            .starts_with(b"SQLite format 3"));
    }

    #[test]
    fn the_next_launch_reuses_the_key_without_asking() {
        let mut r = start(&["a.txt"]);
        r.wait_until_indexed(1);
        let Running {
            service,
            store,
            dir,
            root,
            ..
        } = r;
        drop(service);
        std::thread::sleep(Duration::from_millis(200));
        let mut again = start_with(store, dir, root);
        again.wait_for(
            "ready",
            |e| matches!(e, Event::Status(s) if s.state == IndexState::Ready),
        );
        assert_eq!(again.search("a"), ["a.txt"]);
    }

    #[test]
    fn a_missing_key_offers_delete_and_rebuild_and_never_overwrites() {
        let mut r = start(&["a.txt"]);
        r.wait_until_indexed(1);
        let Running {
            service,
            store,
            dir,
            root,
            ..
        } = r;
        drop(service);
        std::thread::sleep(Duration::from_millis(200));
        store.delete(DATABASE_ID).unwrap();
        let db_file = dir.path().join("ninimma/index").join(db::METADATA_DB);
        let before = std::fs::read(&db_file).unwrap();
        let mut again = start_with(store, dir, root);
        again.wait_for("failure", |e| {
            matches!(e, Event::Status(s) if matches!(s.state, IndexState::Failed { can_rebuild: true, .. }))
        });
        assert_eq!(
            std::fs::read(&db_file).unwrap(),
            before,
            "the old index is left alone"
        );
    }

    #[test]
    fn deleting_removes_the_index_and_key_but_no_user_files() {
        let mut r = start(&["keep-me.txt"]);
        r.wait_until_indexed(1);
        r.service.send(Command::DeleteIndexAndKey);
        r.wait_for(
            "deletion",
            |e| matches!(e, Event::Status(s) if s.state == IndexState::Deleted),
        );
        assert!(r.store.load(DATABASE_ID).unwrap().is_none(), "key deleted");
        let left: Vec<_> = std::fs::read_dir(r.index_dir()).unwrap().collect();
        assert!(left.is_empty(), "index files left: {left:?}");
        assert!(
            r.root.path().join("keep-me.txt").exists(),
            "user files untouched"
        );
    }

    #[test]
    fn without_a_key_store_a_passphrase_protects_the_key() {
        let store = Arc::new(MemoryKeyStore::default());
        *store.fail_with.lock().unwrap() = Some(KeyStoreError::Unavailable);
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("doc.txt"), "").unwrap();
        let mut r = start_with(store.clone(), tempfile::tempdir().unwrap(), root);
        r.wait_for("passphrase request", |e| {
            matches!(e, Event::Status(s) if matches!(s.state, IndexState::NeedsNewPassphrase { .. }))
        });
        r.service.send(Command::CreateWithPassphrase(Zeroizing::new(
            "correct horse battery".into(),
        )));
        r.wait_until_indexed(1);
        assert!(vault::exists(&r.index_dir()));

        // Next launch: the passphrase is asked for, and a wrong one refused.
        let Running {
            service,
            store,
            dir,
            root,
            ..
        } = r;
        drop(service);
        std::thread::sleep(Duration::from_millis(200));
        let mut again = start_with(store, dir, root);
        again.wait_for("unlock request", |e| {
            matches!(e, Event::Status(s) if matches!(s.state, IndexState::NeedsPassphrase { error: None }))
        });
        again
            .service
            .send(Command::Unlock(Zeroizing::new("wrong passphrase".into())));
        again.wait_for("rejection", |e| {
            matches!(e, Event::Status(s) if matches!(s.state, IndexState::NeedsPassphrase { error: Some(_) }))
        });
        again.service.send(Command::Unlock(Zeroizing::new(
            "correct horse battery".into(),
        )));
        again.wait_for(
            "ready",
            |e| matches!(e, Event::Status(s) if s.state == IndexState::Ready),
        );
        assert_eq!(again.search("doc"), ["doc.txt"]);
    }

    #[test]
    fn a_passphrase_key_moves_into_the_keyring_and_then_unlocks_by_itself() {
        // Created with a passphrase while no keyring was available.
        let store = Arc::new(MemoryKeyStore::default());
        *store.fail_with.lock().unwrap() = Some(KeyStoreError::Unavailable);
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("doc.txt"), "").unwrap();
        let mut r = start_with(store.clone(), tempfile::tempdir().unwrap(), root);
        r.wait_for("passphrase request", |e| {
            matches!(e, Event::Status(s) if matches!(s.state, IndexState::NeedsNewPassphrase { .. }))
        });
        r.service.send(Command::CreateWithPassphrase(Zeroizing::new(
            "correct horse battery".into(),
        )));
        r.wait_until_indexed(1);
        let use_keyring = |store: &Arc<MemoryKeyStore>| {
            let store = store.clone();
            Command::UseKeyring(Box::new(move || Ok(store as Arc<dyn IndexKeyStore>)))
        };
        let message = |r: &mut Running, what: &str| -> IndexStatus {
            let Event::Status(s) = r.wait_for(
                what,
                |e| matches!(e, Event::Status(s) if s.key_message.is_some()),
            ) else {
                unreachable!()
            };
            s
        };

        // Still no keyring, then a locked one: the passphrase stays.
        r.service.send(use_keyring(&store));
        let s = message(&mut r, "no keyring");
        assert!(s.key_message.unwrap().contains("No system keyring"));
        assert_eq!(s.key_source, Some("Passphrase"));
        *store.fail_with.lock().unwrap() = Some(KeyStoreError::Locked);
        r.service.send(use_keyring(&store));
        let s = message(&mut r, "locked keyring");
        assert!(s.key_message.unwrap().contains("locked"));
        assert!(vault::exists(&r.index_dir()), "the passphrase still works");

        // A working keyring: the key moves, and the vault goes.
        *store.fail_with.lock().unwrap() = None;
        r.service.send(use_keyring(&store));
        let s = loop {
            let s = message(&mut r, "moved");
            if s.key_source == Some("System keyring") {
                break s;
            }
        };
        assert!(s.key_message.unwrap().contains("unlocks when you log in"));
        assert!(!vault::exists(&r.index_dir()));
        assert!(store.load(DATABASE_ID).unwrap().is_some());

        // Next launch: no passphrase asked for.
        let Running {
            service,
            store,
            dir,
            root,
            ..
        } = r;
        drop(service);
        std::thread::sleep(Duration::from_millis(200));
        let mut again = start_with(store, dir, root);
        let mut asked = false;
        again.wait_for("ready without a passphrase", |e| {
            if let Event::Status(s) = e {
                asked |= matches!(s.state, IndexState::NeedsPassphrase { .. });
                return s.state == IndexState::Ready;
            }
            false
        });
        assert!(!asked, "the keyring unlocked it");
        assert_eq!(
            again.last.as_ref().unwrap().key_source,
            Some("System keyring")
        );
        assert_eq!(again.search("doc"), ["doc.txt"]);
    }

    #[test]
    fn a_locked_key_store_gives_a_clear_bounded_state() {
        let store = Arc::new(MemoryKeyStore::default());
        *store.fail_with.lock().unwrap() = Some(KeyStoreError::Locked);
        let mut r = start_with(
            store,
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let Event::Status(s) = r.wait_for(
            "failure",
            |e| matches!(e, Event::Status(s) if matches!(s.state, IndexState::Failed { .. })),
        ) else {
            unreachable!()
        };
        let IndexState::Failed { message, .. } = s.state else {
            unreachable!()
        };
        assert!(message.contains("locked"));
    }

    #[test]
    fn a_newer_search_replaces_an_older_one() {
        let mut r = start(&["alpha.txt", "beta.txt"]);
        r.wait_until_indexed(2);
        let first = r.service.search(SearchRequest {
            generation: 0,
            text: "alpha".into(),
            scope: SearchScope::Everywhere,
            folder: r.root.path().into(),
            offset: 0,
            limit: 50,
            full_text: true,
            smart: false,
            understand: false,
        });
        assert_eq!(r.search("beta"), ["beta.txt"]);
        // Nothing for the replaced search arrives afterwards.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!r
            .service
            .poll()
            .iter()
            .any(|e| matches!(e, Event::Results(res) if res.generation == first)));
    }

    #[test]
    fn an_idle_index_never_wakes_the_ui() {
        let repaints = Arc::new(AtomicU64::new(0));
        let counter = repaints.clone();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryKeyStore::default());
        let settings = IndexSettings {
            enabled: true,
            roots: vec![root.path().to_path_buf()],
            pause_on_battery: false,
            ..Default::default()
        };
        let service = IndexService::start(
            settings,
            dir.path().join("ninimma/index"),
            Box::new(move || Ok(store as Arc<dyn IndexKeyStore>)),
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        // Done means scanned, then the file's text read.
        let deadline = Instant::now() + Duration::from_secs(20);
        let (mut read, mut ready) = (false, false);
        while !(read && ready) {
            assert!(Instant::now() < deadline, "index never became ready");
            for e in service.poll() {
                if let Event::Status(s) = e {
                    read |= matches!(s.state, IndexState::Reading { .. });
                    ready = read && s.state == IndexState::Ready;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let before = repaints.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            repaints.load(Ordering::SeqCst),
            before,
            "no repaints while idle"
        );
        assert!(service.poll().is_empty(), "no events while idle");
    }

    /// Sets a file's modified time to noon on a day of 2025 (local time).
    fn set_date(path: &Path, month: u32, day: u32) {
        let t = chrono::Local
            .with_ymd_and_hms(2025, month, day, 12, 0, 0)
            .unwrap();
        let when = std::time::UNIX_EPOCH + Duration::from_secs(t.timestamp() as u64);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    impl Running {
        fn ask(&mut self, text: &str) -> Vec<(String, MatchReason)> {
            let generation = self.service.search(SearchRequest {
                generation: 0,
                text: text.into(),
                scope: SearchScope::Everywhere,
                folder: self.root.path().into(),
                offset: 0,
                limit: 50,
                full_text: true,
                smart: false,
                understand: false,
            });
            match self.wait_for("results", |e| match e {
                Event::Results(r) => r.generation == generation,
                Event::SearchError { generation: g, .. } => *g == generation,
                _ => false,
            }) {
                Event::Results(r) => r
                    .hits
                    .into_iter()
                    .map(|h| (h.item.name, h.reason))
                    .collect(),
                Event::SearchError { message, .. } => panic!("search failed: {message}"),
                _ => unreachable!(),
            }
        }

        /// Waits until scanning and reading are both done.
        fn wait_until_read(&mut self) {
            let mut read = false;
            self.wait_for("reading", |e| {
                if let Event::Status(s) = e {
                    read |= matches!(s.state, IndexState::Reading { .. });
                    return read && s.state == IndexState::Ready;
                }
                false
            });
        }
    }

    #[test]
    fn receipts_from_the_arizona_trip_in_december() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        std::fs::create_dir_all(r.join("Trips/Southwest")).unwrap();
        std::fs::create_dir_all(r.join("Notes")).unwrap();
        if !extractor::pdf_reader_available() {
            eprintln!("skipped: needs Poppler's pdftotext to read the PDF receipts");
            return;
        }
        // Receipts are PDFs or pictures ("receipts" narrows to those).
        let files = [
            // A receipt from the trip, saved in December.
            (
                "Trips/Southwest/hotel.pdf",
                "Red Rock Lodge, Sedona AZ. RECEIPT. Total $412.50",
                12,
            ),
            // A receipt from another time of year.
            (
                "Notes/march-receipt.pdf",
                "Receipt: Phoenix AZ parking, total $14",
                3,
            ),
            // Something else from December.
            ("Notes/groceries.txt", "milk, eggs, coffee", 12),
        ];
        for (name, text, month) in files {
            if name.ends_with(".pdf") {
                std::fs::write(r.join(name), fixtures::text_pdf(text, true)).unwrap();
            } else {
                std::fs::write(r.join(name), text).unwrap();
            }
            set_date(&r.join(name), month, 20);
        }
        // A photo taken in Sedona in December 2025 (EXIF date and GPS).
        let photo = r.join("Trips/Southwest/IMG_0001.jpg");
        std::fs::write(
            &photo,
            crate::index::extractor::tests::test_jpeg_with_exif(),
        )
        .unwrap();

        let mut running = start_with(
            Arc::new(MemoryKeyStore::default()),
            tempfile::tempdir().unwrap(),
            root,
        );
        running.wait_until_read();
        let found = running.ask("Show all receipts from Arizona trip on December");
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names.first(), Some(&"hotel.pdf"), "{found:?}");
        assert!(
            names.contains(&"IMG_0001.jpg"),
            "the photo taken in Arizona in December: {found:?}"
        );
        assert!(
            !names.contains(&"march-receipt.pdf"),
            "wrong month: {found:?}"
        );
        assert!(
            !names.contains(&"groceries.txt"),
            "not a receipt, not Arizona: {found:?}"
        );
        assert!(
            matches!(&found[0].1, MatchReason::Content(s) if s.contains('«')),
            "{:?}",
            found[0].1
        );
        let photo_reason = &found.iter().find(|(n, _)| n == "IMG_0001.jpg").unwrap().1;
        assert!(
            matches!(photo_reason, MatchReason::Place(p) if p.contains("Arizona")),
            "{photo_reason:?}"
        );

        // Photos by place and date, without words for their names.
        let photos = running.ask("photos from Arizona in December 2025");
        assert_eq!(
            photos.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["IMG_0001.jpg"]
        );
    }

    #[test]
    fn a_type_word_alone_lists_that_type_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        std::fs::write(
            r.join("IMG_0001.jpg"),
            crate::index::extractor::tests::test_jpeg_with_exif(),
        )
        .unwrap();
        // Code about images: "photo" must not find these.
        std::fs::write(r.join("image_ops.rs"), "// rotate a photo, flip a picture").unwrap();
        std::fs::write(r.join("photo-notes.txt"), "photo shoot ideas").unwrap();
        let mut running = start_with(
            Arc::new(MemoryKeyStore::default()),
            tempfile::tempdir().unwrap(),
            root,
        );
        running.wait_until_read();
        for query in ["photo", "photos", "pictures"] {
            let found = running.ask(query);
            let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, ["IMG_0001.jpg"], "{query}");
        }
        // Quoted, it's a name again.
        let found = running.ask("\"photo\"");
        assert!(
            found.iter().any(|(n, _)| n == "photo-notes.txt"),
            "{found:?}"
        );
    }

    /// The picture model end to end: indexing photos and a video, then
    /// searching them by what they show. Needs the model and three photos,
    /// so it's opt-in: `NINIMMA_TEST_PICTURES=<dir with both model
    /// files> NINIMMA_TEST_PHOTOS=<dir with mountain-landscape.jpg,
    /// tabby-cat-face.jpg and pizza.jpg> cargo test --release -- --ignored
    /// photos_and_videos`
    #[test]
    #[ignore]
    fn photos_and_videos_are_found_by_what_they_show() {
        let photos = PathBuf::from(
            std::env::var_os("NINIMMA_TEST_PHOTOS").expect("set NINIMMA_TEST_PHOTOS"),
        );
        assert!(
            std::env::var_os("NINIMMA_TEST_PICTURES").is_some(),
            "set NINIMMA_TEST_PICTURES"
        );
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        // Camera names: nothing in them says what they show.
        for (from, to) in [
            ("mountain-landscape.jpg", "IMG_0001.jpg"),
            ("tabby-cat-face.jpg", "IMG_0002.jpg"),
            ("pizza.jpg", "IMG_0003.jpg"),
        ] {
            std::fs::copy(photos.join(from), r.join(to)).unwrap();
        }
        // A short video of the cat, if ffmpeg can make one.
        let video = crate::thumbnail::can_read_videos()
            && std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-loop", "1", "-i"])
                .arg(photos.join("tabby-cat-face.jpg"))
                .args(["-t", "5", "-vf", "scale=320:-2", "-pix_fmt", "yuv420p"])
                .arg(r.join("MOV_0001.mp4"))
                .status()
                .is_ok_and(|s| s.success());
        let settings = IndexSettings {
            enabled: true,
            mode: config::IndexMode::Smart,
            roots: vec![r.to_path_buf()],
            pause_on_battery: false,
            ..Default::default()
        };
        let store = Arc::new(MemoryKeyStore::default());
        let factory_store = store.clone();
        let dir = tempfile::tempdir().unwrap();
        let mut running = Running {
            service: IndexService::start(
                settings,
                dir.path().join("ninimma/index"),
                Box::new(move || Ok(factory_store as Arc<dyn IndexKeyStore>)),
                Arc::new(|| {}),
            ),
            store,
            dir,
            root,
            last: None,
        };
        let want = if video { 4 } else { 3 };
        running.wait_longer(
            "pictures to be looked at",
            Duration::from_secs(120),
            |e| matches!(e, Event::Status(s) if s.state == IndexState::Ready && s.pictures >= want),
        );
        assert_eq!(running.last.as_ref().unwrap().picture_problem, None);
        let mut ask = |text: &str| -> Vec<String> {
            let generation = running.service.search(SearchRequest {
                generation: 0,
                text: text.into(),
                scope: SearchScope::Everywhere,
                folder: running.root.path().into(),
                offset: 0,
                limit: 50,
                full_text: true,
                smart: true,
                understand: false,
            });
            match running.wait_for(
                "results",
                |e| matches!(e, Event::Results(r) if r.generation == generation),
            ) {
                Event::Results(r) => r.hits.into_iter().map(|h| h.item.name).collect(),
                _ => unreachable!(),
            }
        };
        assert_eq!(ask("photo of mountains"), ["IMG_0001.jpg"]);
        assert_eq!(ask("pizza"), ["IMG_0003.jpg"]);
        assert!(ask("a horse").is_empty(), "nothing shows a horse");
        if video {
            assert_eq!(ask("video of a cat"), ["MOV_0001.mp4"]);
            let cats = ask("cat");
            assert!(
                cats.contains(&"IMG_0002.jpg".to_string())
                    && cats.contains(&"MOV_0001.mp4".to_string()),
                "{cats:?}"
            );
        } else {
            assert_eq!(ask("photo of a cat"), ["IMG_0002.jpg"]);
        }
    }

    #[test]
    fn a_new_root_is_rescanned_once_it_is_watched() {
        // The first scan runs before the root is watched (its size isn't
        // known until then), so changes made during it would be seen by
        // neither: a scan must follow once watching is active.
        let mut r = start(&["a.txt"]);
        let (mut scans, mut live, mut scans_after_live) = (0, false, 0);
        let mut was_scanning = false;
        r.wait_for("a scan after watching started", |e| {
            if let Event::Status(s) = e {
                let scanning = matches!(s.state, IndexState::Indexing { .. });
                if scanning && !was_scanning {
                    scans += 1;
                    if live {
                        scans_after_live += 1;
                    }
                }
                was_scanning = scanning;
                live |= s.live_updates;
            }
            scans_after_live > 0
        });
        assert!(scans >= 2, "the first scan, then the one after watching");
    }

    #[test]
    fn file_changes_are_picked_up_without_a_rescan() {
        let mut r = start(&["first.txt"]);
        r.wait_until_read();
        // Watching is set up in the background once the root is scanned.
        r.wait_for(
            "live updates",
            |e| matches!(e, Event::Status(s) if s.live_updates),
        );
        let new = r.root.path().join("brand-new-notes.txt");
        std::fs::write(&new, "quarterly forecast for the zebra project").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !r
            .ask("zebra")
            .iter()
            .any(|(n, _)| n == "brand-new-notes.txt")
        {
            assert!(Instant::now() < deadline, "the new file was never indexed");
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(r.last.as_ref().unwrap().live_updates);
        std::fs::remove_file(&new).unwrap();
        while !r.ask("zebra").is_empty() {
            assert!(
                Instant::now() < deadline,
                "the deleted file was never forgotten"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    #[test]
    fn turning_full_text_off_forgets_document_text() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("secret-plan.txt"),
            "the walrus migrates at dawn",
        )
        .unwrap();
        let mut r = start_with(
            Arc::new(MemoryKeyStore::default()),
            tempfile::tempdir().unwrap(),
            root,
        );
        r.wait_until_read();
        assert_eq!(r.ask("walrus").len(), 1);
        r.service.send(Command::UpdateSettings(IndexSettings {
            enabled: true,
            mode: config::IndexMode::Basic,
            roots: vec![r.root.path().to_path_buf()],
            pause_on_battery: false,
            ..Default::default()
        }));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !r.ask("walrus").is_empty() {
            assert!(
                Instant::now() < deadline,
                "text still searchable after turning it off"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        // Names are still found.
        assert_eq!(r.ask("secret").len(), 1);
    }

    /// Smart search through the real service, with the real model:
    /// `NINIMMA_TEST_MODEL=<bge .gguf> cargo test -- --ignored smart`.
    #[test]
    #[ignore]
    fn smart_search_finds_by_meaning() {
        assert!(
            std::env::var_os("NINIMMA_TEST_MODEL").is_some(),
            "set NINIMMA_TEST_MODEL"
        );
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        // PDFs: "receipts" narrows to PDFs and pictures.
        let files = [
            (
                "hotel.pdf",
                "Red Rock Lodge, Sedona AZ. RECEIPT. Total $412.50",
            ),
            // Neither "receipt" nor "Arizona": only meaning connects it.
            (
                "tours-2231.pdf",
                "Invoice 2231. Grand Canyon helicopter tours. Amount paid: $89.00. Thank you!",
            ),
            (
                "zebra.pdf",
                "Quarterly forecast for the zebra project: headcount and milestones",
            ),
        ];
        for (name, text) in files {
            std::fs::write(r.join(name), fixtures::text_pdf(text, false)).unwrap();
        }
        let settings = IndexSettings {
            enabled: true,
            mode: config::IndexMode::Smart,
            roots: vec![r.to_path_buf()],
            pause_on_battery: false,
            ..Default::default()
        };
        let store = Arc::new(MemoryKeyStore::default());
        let factory_store = store.clone();
        let dir = tempfile::tempdir().unwrap();
        let mut running = Running {
            service: IndexService::start(
                settings,
                dir.path().join("ninimma/index"),
                Box::new(move || Ok(factory_store as Arc<dyn IndexKeyStore>)),
                Arc::new(|| {}),
            ),
            store,
            dir,
            root,
            last: None,
        };
        let mut embedded = false;
        running.wait_for("smart search to be ready", |e| {
            if let Event::Status(s) = e {
                embedded |= matches!(s.state, IndexState::Embedding { .. });
                return embedded && s.state == IndexState::Ready && s.vectors >= 3;
            }
            false
        });
        assert_eq!(running.last.as_ref().unwrap().smart_problem, None);
        let generation = running.service.search(SearchRequest {
            generation: 0,
            text: "receipts from Arizona".into(),
            scope: SearchScope::Everywhere,
            folder: running.root.path().into(),
            offset: 0,
            limit: 50,
            full_text: true,
            smart: true,
            understand: false,
        });
        let Event::Results(results) = running.wait_for(
            "results",
            |e| matches!(e, Event::Results(r) if r.generation == generation),
        ) else {
            unreachable!()
        };
        let found: Vec<(String, MatchReason)> = results
            .hits
            .into_iter()
            .map(|h| (h.item.name, h.reason))
            .collect();
        assert_eq!(found[0].0, "hotel.pdf", "{found:?}");
        let tours = found.iter().find(|(n, _)| n == "tours-2231.pdf");
        assert!(
            matches!(tours, Some((_, MatchReason::Meaning(_)))),
            "the invoice is found by meaning: {found:?}"
        );
        assert!(
            !found.iter().any(|(n, _)| n == "zebra.pdf"),
            "unrelated text isn't: {found:?}"
        );
    }

    /// The query model through the real service:
    /// `NINIMMA_TEST_QUERY_MODEL=<qwen .gguf> cargo test --release
    /// -- --ignored sentences_are_re_ranked`. The rules' results come first,
    /// then the same files, re-ranked.
    #[test]
    #[ignore]
    fn sentences_are_re_ranked_after_the_rules_results() {
        assert!(
            std::env::var_os("NINIMMA_TEST_QUERY_MODEL").is_some(),
            "set NINIMMA_TEST_QUERY_MODEL"
        );
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("notes.txt"),
            "call with acme about pricing",
        )
        .unwrap();
        std::fs::write(root.path().join("draft.txt"), "contract terms for review").unwrap();
        let mut r = start_with(
            Arc::new(MemoryKeyStore::default()),
            tempfile::tempdir().unwrap(),
            root,
        );
        r.wait_until_read();
        let search = |r: &mut Running, offset: usize| {
            r.service.search(SearchRequest {
                generation: 0,
                text: "the contract I signed with Acme".into(),
                scope: SearchScope::Everywhere,
                folder: r.root.path().into(),
                offset,
                limit: 50,
                full_text: true,
                smart: false,
                understand: true,
            })
        };
        let names = |e: Event| match e {
            Event::Results(res) => {
                let mut n: Vec<String> = res.hits.into_iter().map(|h| h.item.name).collect();
                (res.refined, {
                    n.sort();
                    n
                })
            }
            _ => unreachable!(),
        };
        let started = Instant::now();
        let generation = search(&mut r, 0);
        let first = names(r.wait_for(
            "rules' results",
            |e| matches!(e, Event::Results(x) if x.generation == generation),
        ));
        let rules_took = started.elapsed();
        let second = names(r.wait_for(
            "refined results",
            |e| matches!(e, Event::Results(x) if x.generation == generation),
        ));
        eprintln!("rules: {rules_took:?}, refined: {:?}", started.elapsed());
        assert_eq!(
            first,
            (
                false,
                vec!["draft.txt".to_string(), "notes.txt".to_string()]
            )
        );
        assert_eq!(
            second,
            (true, first.1.clone()),
            "the same files, only re-ranked"
        );
        // A later page uses the cached reading: one answer, already refined.
        let generation = search(&mut r, 1);
        let page = names(r.wait_for(
            "page 2",
            |e| matches!(e, Event::Results(x) if x.generation == generation),
        ));
        assert!(page.0);
    }

    /// OCR through the real service, with the real model:
    /// `NINIMMA_TEST_OCR=<dir with the granite .gguf files> cargo test
    /// --release -- --ignored photographed`.
    #[test]
    #[ignore]
    fn a_photographed_receipt_is_found_by_its_printed_text() {
        assert!(
            std::env::var_os("NINIMMA_TEST_OCR").is_some(),
            "set NINIMMA_TEST_OCR"
        );
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        crate::ocr::tests_support::receipt_image()
            .save(r.join("IMG_2041.png"))
            .unwrap();
        // A landscape: no paper, so the model never runs on it.
        let mut scene = image::RgbImage::from_pixel(400, 300, image::Rgb([190, 80, 40]));
        for y in 0..120 {
            for x in 0..400 {
                scene.put_pixel(x, y, image::Rgb([90, 150, 230]));
            }
        }
        scene.save(r.join("IMG_2042.png")).unwrap();
        let settings = IndexSettings {
            enabled: true,
            mode: config::IndexMode::FullText,
            ocr: true,
            worker_threads: 2,
            roots: vec![r.to_path_buf()],
            pause_on_battery: false,
            ..Default::default()
        };
        let store = Arc::new(MemoryKeyStore::default());
        let factory_store = store.clone();
        let dir = tempfile::tempdir().unwrap();
        let mut running = Running {
            service: IndexService::start(
                settings,
                dir.path().join("ninimma/index"),
                Box::new(move || Ok(factory_store as Arc<dyn IndexKeyStore>)),
                Arc::new(|| {}),
            ),
            store,
            dir,
            root,
            last: None,
        };
        let mut read_images = false;
        running.wait_longer("OCR to finish", Duration::from_secs(300), |e| {
            if let Event::Status(s) = e {
                read_images |= matches!(s.state, IndexState::ReadingImages { .. });
                return read_images && s.state == IndexState::Ready;
            }
            false
        });
        assert_eq!(running.last.as_ref().unwrap().ocr_problem, None);
        let found = running.ask("Red Rock Lodge receipt");
        assert_eq!(
            found.first().map(|(n, _)| n.as_str()),
            Some("IMG_2041.png"),
            "{found:?}"
        );
        assert!(
            matches!(&found[0].1, MatchReason::Content(s) if s.contains('«')),
            "{:?}",
            found[0].1
        );
        assert!(!found.iter().any(|(n, _)| n == "IMG_2042.png"));
        // Its total is searchable too.
        assert!(running
            .ask("412.50")
            .iter()
            .any(|(n, _)| n == "IMG_2041.png"));
    }

    #[test]
    fn ocr_without_its_model_does_nothing_and_breaks_nothing() {
        let root = tempfile::tempdir().unwrap();
        crate::ocr::tests_support::receipt_pdf();
        std::fs::write(root.path().join("notes.txt"), "plain notes about walruses").unwrap();
        image::RgbImage::from_pixel(50, 80, image::Rgb([250, 250, 250]))
            .save(root.path().join("scan.png"))
            .unwrap();
        let mut r = start_with(
            Arc::new(MemoryKeyStore::default()),
            tempfile::tempdir().unwrap(),
            root,
        );
        r.service.send(Command::UpdateSettings(IndexSettings {
            enabled: true,
            ocr: true,
            roots: vec![r.root.path().to_path_buf()],
            pause_on_battery: false,
            ..Default::default()
        }));
        r.wait_for(
            "the index to open",
            |e| matches!(e, Event::Status(s) if s.state == IndexState::Ready),
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while r.ask("walruses").is_empty() {
            assert!(Instant::now() < deadline, "text never indexed");
            std::thread::sleep(Duration::from_millis(100));
        }
        // No model: OCR never starts, and nothing reports a problem.
        assert!(!matches!(
            r.last.as_ref().unwrap().state,
            IndexState::ReadingImages { .. }
        ));
        assert_eq!(r.last.as_ref().unwrap().ocr_problem, None);
    }

    #[test]
    fn pausing_holds_the_scan_until_resumed() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..600 {
            std::fs::write(root.path().join(format!("f{i}.txt")), "").unwrap();
        }
        let store = Arc::new(MemoryKeyStore::default());
        let mut r = start_with(store, tempfile::tempdir().unwrap(), root);
        r.service.set_paused(true);
        r.wait_for("pause", |e| matches!(e, Event::Status(s) if matches!(&s.state, IndexState::Paused(why) if why == "paused")));
        r.service.set_paused(false);
        r.wait_until_indexed(600);
    }

    /// Measures a full index with text, paced as in normal use:
    /// `NINIMMA_BENCH_ROOT=/usr/share/doc cargo test --release
    /// measure_full_text -- --ignored --nocapture`. Read-only on the root.
    #[test]
    #[ignore]
    fn measure_full_text() {
        let Some(root) = std::env::var_os("NINIMMA_BENCH_ROOT") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let smart = std::env::var("NINIMMA_BENCH_MODE").as_deref() == Ok("smart");
        let settings = IndexSettings {
            enabled: true,
            roots: vec![PathBuf::from(&root)],
            pause_on_battery: false,
            mode: if smart {
                config::IndexMode::Smart
            } else {
                config::IndexMode::FullText
            },
            ..Default::default()
        };
        let store = Arc::new(MemoryKeyStore::default());
        let start = Instant::now();
        let service = IndexService::start(
            settings,
            dir.path().join("ninimma/index"),
            Box::new(move || Ok(store as Arc<dyn IndexKeyStore>)),
            Arc::new(|| {}),
        );
        // Reading is paced for the background, so a big tree takes a while:
        // stop after a budget (seconds) and report the rate.
        let budget = Duration::from_secs(
            std::env::var("NINIMMA_BENCH_SECONDS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(120),
        );
        let (mut scanned_at, mut last) = (None, IndexStatus::default());
        let (mut read, mut first_left, mut last_left) = (false, None, 0u64);
        let mut embed_started: Option<(Duration, u64)> = None;
        let mut embed_last: Option<(Duration, u64)> = None;
        loop {
            for e in service.poll() {
                if let Event::Status(s) = e {
                    if let IndexState::Embedding { left } = s.state {
                        embed_started.get_or_insert((start.elapsed(), left));
                        embed_last = Some((start.elapsed(), left));
                    }
                    if let IndexState::Reading { left } = s.state {
                        if scanned_at.is_none() {
                            scanned_at = Some(start.elapsed());
                        }
                        first_left.get_or_insert(left);
                        last_left = left;
                        read = true;
                    }
                    last = s;
                }
            }
            let done =
                read && last.state == IndexState::Ready && (!smart || embed_started.is_some());
            if done || start.elapsed() > budget {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let total = start.elapsed();
        if let (Some((t0, left0)), Some((t1, left1))) = (embed_started, embed_last) {
            let secs = (t1 - t0).as_secs_f64().max(0.001);
            println!(
                "smart search: {} of {left0} pieces embedded in {secs:.0}s: {:.1}/s",
                left0 - left1,
                (left0 - left1) as f64 / secs
            );
        }
        let reading_time = total
            .saturating_sub(scanned_at.unwrap_or_default())
            .as_secs_f64()
            .max(0.001);
        let done = if last.state == IndexState::Ready {
            first_left.unwrap_or(0)
        } else {
            first_left.unwrap_or(0) - last_left
        };
        println!(
            "read {done} of {} files waiting in {reading_time:.0}s: {:.0} files/s",
            first_left.unwrap_or(0),
            done as f64 / reading_time
        );
        let rss = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmHWM"))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        println!(
            "{} items: names in {:.1}s, stopped at {:.1}s; index {:.1} MB; peak {rss}",
            last.indexed_files,
            scanned_at.unwrap_or_default().as_secs_f64(),
            total.as_secs_f64(),
            last.database_bytes as f64 / 1e6,
        );
        let mut service = service;
        for text in [
            "copyright notices from 2019",
            "license gpl",
            "changelog for python",
        ] {
            let started = Instant::now();
            let generation = service.search(SearchRequest {
                generation: 0,
                text: text.into(),
                scope: SearchScope::Everywhere,
                folder: PathBuf::from(&root),
                offset: 0,
                limit: 200,
                full_text: true,
                smart,
                understand: false,
            });
            let hits = loop {
                if let Some(n) = service.poll().into_iter().find_map(|e| match e {
                    Event::Results(r) if r.generation == generation => Some(r.hits.len()),
                    _ => None,
                }) {
                    break n;
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            println!(
                "search {text:?}: {hits} shown in {:.0} ms",
                started.elapsed().as_secs_f64() * 1e3
            );
        }
    }
}
