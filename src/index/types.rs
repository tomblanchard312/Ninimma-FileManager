// Ninimma modification: independent project branding and integration.
//! Messages between the UI and the index threads, and what they report.

use super::config::IndexSettings;
use crate::file_system::FileItem;
use std::path::PathBuf;
use zeroize::Zeroizing;

/// Where a search looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SearchScope {
    /// The folder on screen only: the quick filter that needs no index.
    #[default]
    ThisFolder,
    /// The folder on screen and everything below it, from the index.
    Subfolders,
    /// Every indexed location.
    Everywhere,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            SearchScope::ThisFolder => "This folder",
            SearchScope::Subfolders => "This folder and subfolders",
            SearchScope::Everywhere => "All indexed locations",
        }
    }

    pub fn uses_index(self) -> bool {
        self != SearchScope::ThisFolder
    }
}

#[derive(Clone, Debug)]
pub struct SearchRequest {
    /// Newer requests replace older ones; results carry this back.
    pub generation: u64,
    pub text: String,
    pub scope: SearchScope,
    /// The folder on screen (for `Subfolders`).
    pub folder: PathBuf,
    pub offset: usize,
    pub limit: usize,
    /// Search document text, folders and places too (Full text mode).
    pub full_text: bool,
    /// Also match by meaning with the local model (Smart mode).
    pub smart: bool,
    /// Let the experimental query model re-rank sentences (opt-in).
    pub understand: bool,
}

/// Why a result matched, shown without revealing any indexed text.
#[derive(Clone, Debug, PartialEq)]
pub enum MatchReason {
    ExactName,
    NameStartsWith,
    NameContains,
    /// Matched filters only (e.g. `ext:pdf` with no words).
    Filters,
    /// Words in the file's text; a short excerpt with «matches» marked.
    Content(String),
    /// Where the photo was taken.
    Place(String),
    /// Words in the folders it's in.
    Folder,
    /// Similar in meaning (smart search), with a score from 0 to 1.
    Meaning(f32),
    /// The picture shows what was searched for (photos and videos).
    Picture,
}

impl MatchReason {
    pub fn describe(&self) -> String {
        match self {
            MatchReason::ExactName => "the name".into(),
            MatchReason::NameStartsWith => "the start of the name".into(),
            MatchReason::NameContains => "part of the name".into(),
            MatchReason::Filters => "the filters".into(),
            MatchReason::Content(snippet) => format!("the text: {snippet}"),
            MatchReason::Place(place) => format!("taken in {place}"),
            MatchReason::Folder => "the folder it's in".into(),
            MatchReason::Meaning(score) => format!("similar meaning ({:.0}% match)", score * 100.0),
            MatchReason::Picture => "what the picture shows".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub item: FileItem,
    pub reason: MatchReason,
}

#[derive(Clone, Debug)]
pub struct SearchResults {
    pub generation: u64,
    pub offset: usize,
    pub hits: Vec<SearchHit>,
    /// More results exist past this page.
    pub more: bool,
    /// Filters that were applied, in words, e.g. "type: image".
    pub filters: Vec<String>,
    /// Re-ranked with the query model's reading (sent after the rules'
    /// results for the same search).
    pub refined: bool,
}

/// Commands to the index service. Passphrases are wiped when dropped.
pub enum Command {
    /// Settings changed (roots, exclusions, limits).
    UpdateSettings(IndexSettings),
    /// Create a passphrase vault and the index (no credential store).
    CreateWithPassphrase(Zeroizing<String>),
    /// Unlock an existing passphrase vault.
    Unlock(Zeroizing<String>),
    /// Rescan every root now.
    Rescan,
    /// Delete and rebuild the metadata index (keeps the key).
    Rebuild,
    /// Stop, delete the index and its key; source files are untouched.
    DeleteIndexAndKey,
    /// Forget entries found missing or unreadable when searching.
    Forget(Vec<PathBuf>),
    /// Files changed on disk (from the watcher; internal).
    Wake,
    /// Delete the smart-search vectors and make them again.
    RebuildSemantic,
    /// Move the key from the passphrase vault into the system keyring
    /// (reached through this factory), so the index unlocks at login.
    UseKeyring(super::KeyStoreFactory),
    Shutdown,
}

/// The index's state, as shown in Settings and the status bar.
#[derive(Clone, Debug, PartialEq)]
pub enum IndexState {
    Starting,
    /// No credential store: the user must choose a passphrase to create the
    /// index.
    NeedsNewPassphrase {
        error: Option<String>,
    },
    /// A passphrase vault exists and must be unlocked this session.
    NeedsPassphrase {
        error: Option<String>,
    },
    /// Scanning; `files` seen so far.
    Indexing {
        files: u64,
    },
    /// Reading document text and photo details; `left` to go.
    Reading {
        left: u64,
    },
    /// Making smart-search vectors; `left` pieces of text to go.
    Embedding {
        left: u64,
    },
    /// Reading text in images and scanned PDFs (OCR); `left` to go.
    ReadingImages {
        left: u64,
    },
    /// Looking at what photos and videos show; `left` to go.
    LookingAtPictures {
        left: u64,
    },
    /// Paused, and why (in words: "on battery", "paused by you").
    Paused(String),
    Ready,
    /// The index can't be used; the message says why and `can_rebuild`
    /// offers Delete and rebuild.
    Failed {
        message: String,
        can_rebuild: bool,
    },
    /// Deleted (index and key); the UI turns indexing off.
    Deleted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexStatus {
    pub state: IndexState,
    pub indexed_files: u64,
    pub database_bytes: u64,
    /// Seconds since the Unix epoch when the last full scan finished.
    pub last_scan: Option<i64>,
    /// "System keyring" or "Passphrase".
    pub key_source: Option<&'static str>,
    /// Roots skipped and why ("network share", "excluded location").
    pub skipped_roots: Vec<(PathBuf, &'static str)>,
    /// Changes are picked up as they happen (false: only by rescans).
    pub live_updates: bool,
    /// Roots whose changes are picked up only by rescans, and why.
    pub unwatched_roots: Vec<(PathBuf, &'static str)>,
    /// Smart search: vectors stored, their database's size, and why it
    /// can't run, if it can't.
    pub vectors: u64,
    pub semantic_bytes: u64,
    pub smart_problem: Option<String>,
    /// Why OCR can't run, if it can't.
    pub ocr_problem: Option<String>,
    /// Photos and videos searchable by what they show, and why that can't
    /// run, if it can't.
    pub pictures: u64,
    pub picture_problem: Option<String>,
    /// How moving the key into the system keyring went.
    pub key_message: Option<String>,
}

impl Default for IndexStatus {
    fn default() -> Self {
        Self {
            state: IndexState::Starting,
            indexed_files: 0,
            database_bytes: 0,
            last_scan: None,
            key_source: None,
            skipped_roots: Vec::new(),
            live_updates: false,
            unwatched_roots: Vec::new(),
            vectors: 0,
            semantic_bytes: 0,
            smart_problem: None,
            ocr_problem: None,
            pictures: 0,
            picture_problem: None,
            key_message: None,
        }
    }
}

/// Messages from the index threads to the UI.
#[derive(Clone, Debug)]
pub enum Event {
    Status(IndexStatus),
    Results(SearchResults),
    /// The query couldn't be understood; the text says why.
    SearchError {
        generation: u64,
        message: String,
    },
}
