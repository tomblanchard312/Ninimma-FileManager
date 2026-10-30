// Ninimma modification: independent project branding and integration.
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Align2, Button, Color32, CornerRadius, Event, FontId, Galley, Id, Key, Layout, Margin,
    Modifiers, PointerButton, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind,
    TextFormat, Ui, Vec2,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::archive;
use crate::bookmarks::{self, Bookmark};
use crate::disk_images::{self, Operation as DiskOperation};
use crate::eject;
use crate::file_ops::{self, display_name, Conflict, FileOperations, PasteMode, Template};
use crate::file_system::{format_bytes, DriveInfo, DriveType, FileItem, FileSystem};
use crate::folder_tree;
use crate::fonts::FallbackFonts;
use crate::image_ops::{self, Rotation};
use crate::index::config::IndexSettings;
use crate::index::types::SearchScope;
use crate::index_ui::IndexUi;
use crate::network::{self, ConnectError, Credentials, Place, PlaceKind};
#[cfg(all(unix, not(target_os = "macos")))]
use crate::open_with;
use crate::preview;
use crate::properties;
use crate::recent;
use crate::selection::Selection;
use crate::system_clipboard;
use crate::theme::pal;
use crate::thumbnail::{ThumbSize, ThumbnailCache};
use crate::trash_bin;
use crate::undo::{self, History, Step};
use crate::wallpaper;

// High contrast dark theme colors for better accessibility

// Accessibility settings
pub(crate) const FONT_SIZE_SMALL: f32 = 12.0;
const FONT_SIZE_NORMAL: f32 = 14.0;
const FONT_SIZE_LARGE: f32 = 16.0;
const BUTTON_MIN_SIZE: f32 = 36.0;

// Icon view sizing, in logical pixels. Ctrl+scroll moves continuously
// between these.
const MIN_ICON_SIZE: f32 = 32.0;
const MAX_ICON_SIZE: f32 = 256.0;
const DETAILS_ROW_HEIGHT: f32 = 28.0;
const TILE_SIZE: Vec2 = Vec2::new(290.0, 72.0);
const GRID_SPACING: f32 = 6.0;

/// Named view presets (Ctrl+Shift+1..4).
const ICON_PRESETS: [(&str, f32); 4] = [
    ("Extra large icons", 256.0),
    ("Large icons", 128.0),
    ("Medium icons", 64.0),
    ("Small icons", 32.0),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ViewMode {
    Details,
    Tiles,
    Icons,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SortBy {
    Name,
    Size,
    Modified,
    Type,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct ViewSettings {
    pub mode: ViewMode,
    pub icon_size: f32,
    pub sort_by: SortBy,
    pub sort_ascending: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            mode: ViewMode::Details,
            icon_size: 96.0,
            sort_by: SortBy::Name,
            sort_ascending: true,
        }
    }
}

/// Everything that is remembered between runs.
/// A search to run again: its words, where it looks and from which folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedSearch {
    pub text: String,
    pub scope: SearchScope,
    pub folder: PathBuf,
}

impl SavedSearch {
    /// "invoices in Documents", "receipts (everywhere)".
    fn label(&self) -> String {
        match self.scope {
            SearchScope::Everywhere => format!("{} (everywhere)", self.text),
            _ => format!("{} in {}", self.text, display_name(&self.folder)),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub default_view: ViewSettings,
    /// The view for picture folders, once chosen with "Apply to all
    /// folders" in one (otherwise large icons).
    pub picture_view: Option<ViewSettings>,
    /// Remember the view per folder.
    pub folder_views: HashMap<PathBuf, (ViewSettings, u64)>,
    pub show_hidden: bool,
    pub show_nav_pane: bool,
    pub show_preview_pane: bool,
    pub last_path: Option<PathBuf>,
    /// Folders open in tabs at exit, and which one was active.
    pub open_tabs: Vec<PathBuf>,
    pub active_tab: usize,
    /// Network addresses connected to before (never with passwords).
    pub recent_servers: Vec<String>,
    /// Show thumbnails in streaming cloud folders (downloads each file).
    pub cloud_thumbnails: bool,
    /// Bookmarks on platforms without GTK's shared bookmarks file.
    pub bookmarks: Vec<(String, PathBuf)>,
    /// The optional search index (off unless turned on).
    pub index: IndexSettings,
    /// Where the search box looks.
    pub search_scope: SearchScope,
    /// Files opened and folders visited here, newest first, with when
    /// (seconds since the epoch). Kept only on this computer.
    pub recent: Vec<(PathBuf, i64)>,
    /// Keep and show recent files and folders.
    pub show_recent: bool,
    /// Folders pinned to Quick Access, in order.
    pub pinned: Vec<PathBuf>,
    /// Check boxes on items, to select several without the keyboard.
    pub item_checkboxes: bool,
    /// View → Theme: light, dark, high contrast, or as the system is set.
    pub appearance: crate::theme::Appearance,
    /// View → Interface size (1.0 = 100%).
    pub ui_scale: f32,
    /// Searches kept in the navigation pane, to run again with a click.
    pub saved_searches: Vec<SavedSearch>,
    /// Bulk rename presets, by name.
    pub rename_presets: Vec<crate::bulk_rename::RenamePreset>,
    stamp: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_view: ViewSettings::default(),
            picture_view: None,
            folder_views: HashMap::new(),
            show_hidden: false,
            show_nav_pane: true,
            show_preview_pane: true,
            last_path: None,
            open_tabs: Vec::new(),
            active_tab: 0,
            recent_servers: Vec::new(),
            cloud_thumbnails: false,
            bookmarks: Vec::new(),
            index: IndexSettings::default(),
            search_scope: SearchScope::ThisFolder,
            recent: Vec::new(),
            show_recent: true,
            pinned: Vec::new(),
            item_checkboxes: false,
            saved_searches: Vec::new(),
            rename_presets: Vec::new(),
            appearance: crate::theme::Appearance::default(),
            ui_scale: 1.0,
            stamp: 0,
        }
    }
}

const MAX_REMEMBERED_FOLDERS: usize = 2000;

impl Settings {
    /// Picture folders open as large icons unless told otherwise.
    fn picture_view(&self) -> ViewSettings {
        self.picture_view.unwrap_or(ViewSettings {
            mode: ViewMode::Icons,
            icon_size: 160.0,
            ..self.default_view
        })
    }

    fn remember_view(&mut self, folder: &Path, view: ViewSettings) {
        self.stamp += 1;
        self.folder_views
            .insert(folder.to_path_buf(), (view, self.stamp));
        if self.folder_views.len() > MAX_REMEMBERED_FOLDERS {
            if let Some(oldest) = self
                .folder_views
                .iter()
                .min_by_key(|(_, (_, s))| *s)
                .map(|(k, _)| k.clone())
            {
                self.folder_views.remove(&oldest);
            }
        }
    }
}

/// Things the user asked for this frame; applied after drawing so the
/// item list isn't mutated while it's being iterated.
/// A place that isn't a folder, shown in place of the folder's contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Special {
    /// Drives and network places shown in This PC.
    ThisPc,
    /// Files opened and folders visited recently.
    Recent,
    /// The Recycle Bin / Trash.
    Trash,
}

impl Special {
    fn label(self) -> &'static str {
        match self {
            Special::ThisPc => "💻 This PC",
            Special::Recent => "🕘 Recent",
            Special::Trash => TRASH_LABEL,
        }
    }
}

/// The platform's name for it.
const TRASH_LABEL: &str = if cfg!(windows) {
    "🗑 Recycle Bin"
} else {
    "🗑 Trash"
};

/// What the Recycle Bin page shows.
#[derive(Default)]
struct TrashPage {
    items: Vec<trash_bin::Item>,
    selected: Vec<std::ffi::OsString>,
    error: Option<String>,
}

enum Action {
    Navigate(PathBuf),
    /// View → Theme.
    SetAppearance(crate::theme::Appearance),
    /// View → Interface size.
    SetScale(f32),
    ToggleItemCheckboxes,
    /// Make the current view the one every folder of this kind opens with.
    ApplyViewToAll,
    /// Make this picture the desktop background.
    SetWallpaper(PathBuf),
    /// Run a saved search again, or forget it.
    RunSavedSearch(SavedSearch),
    ForgetSavedSearch(SavedSearch),
    /// Move a Quick Access pin up (-1) or down (+1).
    MovePin(PathBuf, isize),
    /// Safely remove the removable drive mounted here.
    Eject(PathBuf),
    /// Put the selection into a new zip file next to it.
    Compress,
    /// Unpack a zip into a new folder next to it.
    Extract(PathBuf),
    ExtractApproved(PathBuf, u64),
    /// Put items from the Recycle Bin back where they were.
    RestoreFromTrash(Vec<std::ffi::OsString>),
    /// Ask before deleting items in the Recycle Bin for good.
    ConfirmPurge(Vec<std::ffi::OsString>),
    /// Show This PC or Recent.
    ShowSpecial(Special),
    /// Expand or collapse a folder in the navigation pane's tree.
    ToggleTree(PathBuf),
    /// Pin a folder to Quick Access, or unpin it.
    Pin(PathBuf),
    Unpin(PathBuf),
    /// Properties of the selection, or of the folder shown.
    Properties,
    /// Open files with this app; with a type, also make it the default.
    #[cfg(all(unix, not(target_os = "macos")))]
    OpenWithApp(open_with::App, Vec<PathBuf>, Option<String>),
    /// Choose another app for the selection.
    ChooseApp,
    /// Open the folder holding this item, with the item selected; with
    /// `true`, in a new tab.
    OpenLocation(PathBuf, bool),
    /// Forget the recent files and folders.
    ClearRecent,
    /// Turn keeping and showing recent files on or off.
    ToggleRecent,
    OpenItem(PathBuf),
    OpenSelected,
    Back,
    Forward,
    Up,
    Refresh,
    Rotate(Rotation),
    Trash,
    ConfirmDeletePermanently,
    Copy,
    Cut,
    Paste(Option<String>),
    CopyPath,
    Rename,
    NewFolder,
    SelectAll,
    Undo,
    Redo,
    /// Open a tab on a folder (current folder if None); bool = switch to it.
    NewTab(Option<PathBuf>, bool),
    CloseTab(usize),
    SwitchTab(usize),
    CycleTab(isize),
    ShowConnect,
    ShowDisks(Option<PathBuf>, DiskOperation),
    NewFile(NewKind),
    OpenTerminal(PathBuf),
    AddBookmark(PathBuf),
    RemoveBookmark(PathBuf),
    Home,
    InvertSelection,
    Zoom(f32),
    FocusAddress,
    FocusSearch,
    TogglePane(Pane),
    ShowShortcuts,
    ShowAbout,
    Quit,
    /// Connect to a network address, with credentials if the user gave some.
    Connect(String, Option<Credentials>),
    Disconnect(Place),
    ToggleCloudThumbnails,
    SetView(ViewMode, Option<f32>),
    Sort(SortBy),
    ToggleHidden,
    ShowIndexSettings,
    MoreResults,
}

#[derive(Clone, Debug, PartialEq)]
enum NewKind {
    Empty,
    TextDocument,
    Template(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pane {
    Navigation,
    Preview,
}

/// Shown in Help > Keyboard Shortcuts (F1).
const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl + mouse wheel, Ctrl +/-", "Resize icons"),
    (
        "Ctrl+Shift+1 … 4",
        "Extra large / Large / Medium / Small icons",
    ),
    ("Ctrl+Shift+5 / 6", "Tiles / Details"),
    ("Alt+← / Alt+→, Backspace", "Back / Forward"),
    ("Alt+↑", "Up one folder"),
    ("Alt+Home", "Home folder"),
    ("Enter, double-click", "Open"),
    ("Middle-click a folder", "Open in a new tab"),
    ("Ctrl+T / Ctrl+W", "New tab / close tab"),
    ("Ctrl+Tab / Ctrl+Shift+Tab", "Next / previous tab"),
    ("Ctrl+A / Ctrl+Shift+I / Esc", "Select all / invert / clear"),
    ("Ctrl+X / Ctrl+C / Ctrl+V", "Cut / copy / paste"),
    ("Ctrl+Shift+C", "Copy as path"),
    ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
    ("F2", "Rename (several items: numbered)"),
    ("Alt+Enter", "Properties"),
    (
        "Delete / Shift+Delete",
        "Move to trash / delete permanently",
    ),
    ("Ctrl+Shift+N", "New folder"),
    ("Ctrl+D", "Bookmark this folder"),
    ("Ctrl+L, Alt+D, F4", "Type a location"),
    ("Ctrl+F, F3", "Search"),
    ("F5, Ctrl+R", "Refresh"),
    ("Ctrl+H", "Show hidden files"),
    ("F1", "This list"),
    ("Ctrl+Q", "Quit"),
];

enum Dialog {
    /// Choosing an app to open files with (Linux, FreeBSD).
    #[cfg(all(unix, not(target_os = "macos")))]
    OpenWith {
        paths: Vec<PathBuf>,
        mime: Option<String>,
        filter: String,
        always: bool,
    },
    /// Properties of the selection (or the folder shown).
    Properties {
        paths: Vec<PathBuf>,
        /// Details, for a single item.
        details: Option<properties::Details>,
        /// Sizes of folders and selections, counted in the background.
        count: properties::SizeCount,
    },
    Disks {
        operation: DiskOperation,
        target: String,
        read_only: bool,
        recovery_password: bool,
        error: Option<String>,
    },
    Shortcuts,
    About,
    IndexSettings,
    Rename {
        targets: Vec<PathBuf>,
        text: String,
        error: Option<String>,
        first_frame: bool,
    },
    /// Renaming several items with a live preview.
    BulkRename(Box<BulkRenameDialog>),
    ConfirmLargeArchive {
        archive: PathBuf,
        bytes: u64,
    },
    ConfirmDelete {
        targets: Vec<PathBuf>,
    },
    /// Deleting items in the Recycle Bin for good.
    ConfirmPurge {
        ids: Vec<std::ffi::OsString>,
    },
    Errors {
        title: String,
        messages: Vec<String>,
    },
    /// Connect to a server; asks for a user name and password only when
    /// the server wants them.
    Connect {
        address: String,
        user: String,
        domain: String,
        password: String,
        needs_credentials: bool,
        asks_domain: bool,
        error: Option<String>,
        busy: bool,
        first_frame: bool,
    },
    /// Pasting/dropping onto items with the same names.
    Conflict {
        paths: Vec<PathBuf>,
        dest: PathBuf,
        mode: PasteMode,
        clashes: Vec<PathBuf>,
    },
}

struct BulkRenameDialog {
    items: Vec<crate::bulk_rename::Item>,
    rules: crate::bulk_rename::RenameRules,
    /// The preview for `rules`, recomputed only when they change.
    preview: Option<(
        crate::bulk_rename::RenameRules,
        Result<Vec<crate::bulk_rename::Row>, String>,
    )>,
    preset_name: String,
    error: Option<String>,
}

#[derive(Default)]
struct JobProgress {
    /// Bytes copied, the total, and Pause/Cancel.
    meter: Arc<file_ops::Meter>,
    done: AtomicUsize,
    finished: AtomicBool,
    errors: Mutex<Vec<String>>,
    /// Files that changed and need fresh thumbnails.
    touched: Mutex<Vec<PathBuf>>,
    /// Undo steps recorded by the operation.
    steps: Mutex<Vec<Step>>,
    /// For undo/redo jobs: the entry reversing what succeeded, and the
    /// steps that failed (kept for a retry).
    reversed: Mutex<Option<(undo::Entry, undo::Entry)>>,
}

/// What a finished job means for the undo history.
enum JobKind {
    /// A new user action, recorded under this label ("Move 3 items").
    Record(String),
    /// Not undoable (permanent delete).
    NoHistory,
    Undo,
    Redo,
}

/// Sets `finished` when dropped, including during a panic unwind.
struct FinishGuard(Arc<JobProgress>, egui::Context);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Relaxed);
        self.1.request_repaint();
    }
}

struct Job {
    verb: String,
    total: usize,
    progress: Arc<JobProgress>,
    kind: JobKind,
    /// History position reserved when the job started.
    order: u64,
    started: Instant,
}

/// What the cached list was built from: folder generation, sort, search
/// text and scope, index results, This PC/Recent, Recent's contents.
type ListKey = (
    u64,
    SortBy,
    bool,
    String,
    SearchScope,
    u64,
    Option<Special>,
    u64,
);

struct ListCache {
    key: Option<ListKey>,
    folder: PathBuf,
    items: Vec<FileItem>,
    paths: Vec<PathBuf>,
}

struct Marquee {
    start: Pos2, // in content coordinates
    additive: bool,
    base: Vec<PathBuf>,
}

/// Geometry of the current item layout in scroll-content coordinates.
#[derive(Clone, Copy)]
struct Grid {
    cols: usize,
    cell: Vec2,
    spacing: f32,
}

impl Grid {
    fn rect_of(&self, index: usize) -> Rect {
        let (row, col) = (index / self.cols, index % self.cols);
        Rect::from_min_size(
            Pos2::new(
                col as f32 * (self.cell.x + self.spacing),
                row as f32 * (self.cell.y + self.spacing),
            ),
            self.cell,
        )
    }
    fn row_stride(&self) -> f32 {
        self.cell.y + self.spacing
    }
}

impl ListCache {
    fn new() -> Self {
        Self {
            key: None,
            folder: PathBuf::new(),
            items: Vec::new(),
            paths: Vec::new(),
        }
    }
}

/// Everything that belongs to one tab. The active tab's state lives directly
/// in `NinimmaUI` (and its `FileSystem` in the app); inactive tabs are
/// parked here and swapped in on switch.
struct TabState {
    fs: FileSystem,
    view: ViewSettings,
    search_text: String,
    selection: Selection,
    list: ListCache,
    address_text: String,
    editing_address: bool,
    shown_path: PathBuf,
    scroll_offset: f32,
    viewport_height: f32,
    scroll_to_focus: bool,
    reload_requested_at: Option<Instant>,
}

impl TabState {
    fn new(fs: FileSystem, view: ViewSettings) -> Self {
        Self {
            fs,
            view,
            search_text: String::new(),
            selection: Selection::default(),
            list: ListCache::new(),
            address_text: String::new(),
            editing_address: false,
            // Differs from the folder, so the tab initializes on first show.
            shown_path: PathBuf::new(),
            scroll_offset: 0.0,
            viewport_height: 0.0,
            scroll_to_focus: false,
            reload_requested_at: None,
        }
    }
}

pub struct NinimmaUI {
    speech: enki_speech_ui::SpeechTools,
    /// One slot per tab, in order; the active tab's slot is `None`.
    tabs: Vec<Option<TabState>>,
    active_tab: usize,
    settings: Settings,
    view: ViewSettings,
    search_text: String,
    selection: Selection,
    thumbnails: ThumbnailCache,
    fonts: FallbackFonts,
    list: ListCache,
    index: IndexUi,
    /// This PC or Recent, when shown instead of the folder.
    special: Option<Special>,
    /// The navigation pane's folder tree.
    tree: folder_tree::FolderTree,
    /// The shown folder's generation, to refresh its tree entry on change.
    tree_generation: u64,
    /// An item to select once its folder has loaded (Open file location).
    reveal: Option<PathBuf>,
    /// Search words to put back once a saved search's folder has opened.
    pending_search: Option<String>,
    /// A folder the tree should open out to and show (the one just opened).
    tree_follow: Option<PathBuf>,
    /// Scroll the navigation pane to the open folder's row once drawn.
    tree_scroll: bool,
    /// "Show in folder" requests from other apps (D-Bus FileManager1).
    #[cfg(all(unix, not(target_os = "macos")))]
    file_manager1: Option<std::sync::mpsc::Receiver<crate::file_manager1::Request>>,
    /// What the Recent view lists, loaded when it's opened.
    recent_items: Vec<FileItem>,
    /// Bumped when `recent_items` changes, so the list is rebuilt.
    recent_version: u64,
    address_text: String,
    editing_address: bool,
    focus_address: bool,
    focus_search: bool,
    preview_text: Arc<Mutex<Option<(PathBuf, String)>>>,
    preview_requested: Option<PathBuf>,
    /// Shared with preview workers so stale results are dropped.
    preview_wanted: Arc<Mutex<Option<PathBuf>>>,
    drives: Arc<Mutex<Vec<DriveInfo>>>,
    drives_thread_started: bool,
    quick_access: Vec<(String, PathBuf)>,
    quick_access_refreshed: Option<Instant>,
    clipboard: Option<(Vec<PathBuf>, PasteMode)>,
    /// Whether another app's files are on the system clipboard, and when
    /// that was last checked (it's a round trip to the clipboard owner).
    system_files: std::cell::Cell<Option<(Instant, bool)>>,
    trash: TrashPage,
    jobs: Vec<Job>,
    history: History,
    dialog: Option<Dialog>,
    status_message: Option<(String, Instant)>,
    type_ahead: (String, Instant),
    zoom_accumulator: f32,
    scroll_to_focus: bool,
    grid: Grid,
    scroll_offset: f32,
    viewport_height: f32,
    marquee: Option<Marquee>,
    shown_path: PathBuf,
    reload_requested_at: Option<Instant>,
    /// Drag-and-drop: a drag the app should start (paths, move?), whether one
    /// is in progress, and the folder currently under the pointer.
    drag_out_request: Option<(Vec<PathBuf>, bool)>,
    drag_out_active: Arc<AtomicBool>,
    drop_folder: Option<PathBuf>,
    /// Network shares, cloud storage and devices; refreshed in the background.
    places: Arc<Mutex<Vec<Place>>>,
    /// Ask the background thread to refresh drives and places now.
    places_stale: Arc<AtomicBool>,
    /// The current folder streams from the cloud (thumbnails would download).
    folder_is_cloud: bool,
    /// A connection attempt in progress: (address, result).
    connecting: Option<std::sync::mpsc::Receiver<(String, Result<PathBuf, ConnectError>)>>,
    disk_launch: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    /// No saved view for this folder yet: pick one once its items are known.
    detect_view: bool,
    templates: Vec<Template>,
    bookmarks: Vec<Bookmark>,
    /// Places from Dolphin's sidebar on KDE (shown, but managed in Dolphin).
    dolphin_places: Vec<Bookmark>,
    dolphin_places_file: Option<PathBuf>,
    /// GTK's bookmarks file (shared with Nemo/Nautilus), where that applies.
    bookmarks_file: Option<PathBuf>,
    /// When templates and bookmarks were last read from disk.
    menus_refreshed: Option<Instant>,
    /// Last item clicked and when, for double-click detection.
    last_click: Option<(PathBuf, f64)>,
    /// Files "opened" during tests, which must never launch real apps.
    #[cfg(test)]
    opened: Vec<PathBuf>,
    /// Apps "launched" with Open with during tests: (app id, files).
    #[cfg(test)]
    launched: Vec<(String, Vec<PathBuf>)>,
    /// Defaults "set" during tests: (app id, MIME type).
    #[cfg(test)]
    defaults_set: Vec<(String, String)>,
    /// Installed apps and file associations, read the first time Open
    /// with is shown.
    #[cfg(all(unix, not(target_os = "macos")))]
    apps: std::cell::OnceCell<open_with::Catalog>,
}

impl NinimmaUI {
    pub fn new(settings: Settings, fs: &FileSystem) -> Self {
        Self::with_thumbnails(settings, fs, ThumbnailCache::new())
    }

    fn with_thumbnails(settings: Settings, fs: &FileSystem, thumbnails: ThumbnailCache) -> Self {
        let mut ui = Self {
            speech: enki_speech_ui::SpeechTools::new("ninimma"),
            tabs: vec![None],
            active_tab: 0,
            view: settings.default_view,
            settings,
            search_text: String::new(),
            selection: Selection::default(),
            thumbnails,
            fonts: FallbackFonts::default(),
            list: ListCache {
                key: None,
                folder: PathBuf::new(),
                items: Vec::new(),
                paths: Vec::new(),
            },
            index: IndexUi::default(),
            special: None,
            tree: folder_tree::FolderTree::default(),
            tree_generation: 0,
            reveal: None,
            pending_search: None,
            tree_follow: None,
            tree_scroll: false,
            #[cfg(all(unix, not(target_os = "macos")))]
            file_manager1: None,
            recent_items: Vec::new(),
            recent_version: 0,
            address_text: String::new(),
            editing_address: false,
            focus_address: false,
            focus_search: false,
            preview_text: Arc::new(Mutex::new(None)),
            preview_requested: None,
            preview_wanted: Arc::new(Mutex::new(None)),
            drives: Arc::new(Mutex::new(Vec::new())),
            drives_thread_started: false,
            quick_access: Vec::new(),
            quick_access_refreshed: None,
            clipboard: None,
            system_files: std::cell::Cell::new(None),
            trash: TrashPage::default(),
            jobs: Vec::new(),
            history: History::default(),
            dialog: None,
            status_message: None,
            type_ahead: (String::new(), Instant::now()),
            zoom_accumulator: 0.0,
            scroll_to_focus: false,
            grid: Grid {
                cols: 1,
                cell: Vec2::splat(1.0),
                spacing: 0.0,
            },
            scroll_offset: 0.0,
            viewport_height: 0.0,
            marquee: None,
            shown_path: PathBuf::new(),
            reload_requested_at: None,
            drag_out_request: None,
            drag_out_active: Arc::new(AtomicBool::new(false)),
            drop_folder: None,
            places: Arc::new(Mutex::new(Vec::new())),
            places_stale: Arc::new(AtomicBool::new(false)),
            folder_is_cloud: false,
            connecting: None,
            disk_launch: None,
            detect_view: false,
            templates: Vec::new(),
            bookmarks: Vec::new(),
            dolphin_places: Vec::new(),
            dolphin_places_file: bookmarks::dolphin_places_file(),
            bookmarks_file: bookmarks::gtk_bookmarks_file(),
            menus_refreshed: None,
            last_click: None,
            #[cfg(test)]
            opened: Vec::new(),
            #[cfg(test)]
            launched: Vec::new(),
            #[cfg(test)]
            defaults_set: Vec::new(),
            #[cfg(all(unix, not(target_os = "macos")))]
            apps: std::cell::OnceCell::new(),
        };
        ui.on_folder_changed(fs, None);
        ui.restore_tabs(fs);
        ui
    }

    /// A drag the user started this frame, for the app to hand to the OS.
    pub fn take_drag_out(&mut self) -> Option<(Vec<PathBuf>, bool)> {
        self.drag_out_request.take()
    }

    pub fn drag_out_flag(&self) -> Arc<AtomicBool> {
        self.drag_out_active.clone()
    }

    pub fn report(&mut self, msg: impl Into<String>) {
        self.status(msg);
    }

    pub fn settings(&self, fs: &FileSystem) -> Settings {
        let mut s = self.settings.clone();
        s.last_path = Some(fs.get_current_path().to_path_buf());
        s.open_tabs = (0..self.tabs.len()).map(|i| self.tab_path(i, fs)).collect();
        s.active_tab = self.active_tab;
        s.show_hidden = fs.is_showing_hidden();
        s
    }

    pub fn show(&mut self, ui: &mut Ui, fs: &mut FileSystem) {
        let ctx = ui.ctx().clone();
        // Before anything is drawn: a new theme or size shows at once.
        crate::theme::apply(&ctx, self.settings.appearance, self.settings.ui_scale);
        fs.set_repaint_context(&ctx);
        self.thumbnails.begin_frame(&ctx);
        self.start_drive_refresh(&ctx);
        fs.poll_load();
        for tab in self.tabs.iter_mut().flatten() {
            tab.fs.poll_load();
        }
        self.poll_connect(fs);
        self.poll_disk_launch(&ctx);
        self.refresh_menus();
        self.folder_is_cloud =
            network::is_streaming_cloud(fs.get_current_path(), &self.places.lock().unwrap());
        self.poll_jobs(&ctx, fs);
        self.index
            .poll(&ctx, &mut self.settings.index, !self.jobs.is_empty());
        self.handle_drops(&ctx, fs);
        self.auto_refresh(&ctx, fs);
        let dragging = self.drag_out_active.load(Ordering::Relaxed);
        if dragging {
            // The window keeps the pointer grab during the drag, so this cursor
            // shows even over other apps.
            ctx.set_cursor_icon(if ctx.input(|i| i.modifiers.shift) {
                egui::CursorIcon::Grabbing
            } else {
                egui::CursorIcon::Copy
            });
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.drop_folder = None;
        self.refresh_list(fs);

        self.follow_tree(ui.ctx(), fs);
        let mut actions = Vec::new();
        #[cfg(all(unix, not(target_os = "macos")))]
        self.answer_file_manager1(ui.ctx(), &mut actions);
        let typing_at_start = ctx.egui_wants_keyboard_input() || self.dialog.is_some();

        egui::Panel::top("menu_bar")
            .frame(
                egui::Frame::NONE
                    .fill(pal().sidebar)
                    .inner_margin(Margin::symmetric(6, 2)),
            )
            .show(ui, |ui| self.show_menu_bar(ui, fs, &mut actions));

        egui::Panel::top("tab_bar")
            .frame(egui::Frame::NONE.fill(pal().sidebar).inner_margin(Margin {
                left: 8,
                right: 8,
                top: 6,
                bottom: 0,
            }))
            .show(ui, |ui| self.show_tab_bar(ui, fs, &mut actions));

        egui::Panel::top("toolbar_panel")
            .frame(egui::Frame::NONE.fill(pal().header))
            .show(ui, |ui| {
                self.show_toolbar(ui, fs, &mut actions);
            });

        egui::Panel::bottom("status_panel")
            .exact_size(30.0)
            .frame(egui::Frame::NONE.fill(pal().header))
            .show(ui, |ui| self.show_status_bar(ui, fs, &mut actions));

        if self.settings.show_nav_pane {
            egui::Panel::left("nav_panel")
                .resizable(true)
                .default_size(230.0)
                .size_range(150.0..=420.0)
                .frame(egui::Frame::NONE.fill(pal().sidebar))
                .show(ui, |ui| self.show_navigation_pane(ui, fs, &mut actions));
        }

        if self.settings.show_preview_pane {
            egui::Panel::right("preview_panel")
                .resizable(true)
                .default_size(300.0)
                .size_range(220.0..=600.0)
                .frame(egui::Frame::NONE.fill(pal().sidebar))
                .show(ui, |ui| self.show_preview_pane(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(pal().bg).inner_margin(Margin {
                left: 8,
                right: 0,
                top: 4,
                bottom: 0,
            }))
            .show(ui, |ui| self.show_file_area(ui, fs, &mut actions));

        self.show_dialog(&ctx, fs);
        let typing = typing_at_start || self.dialog.is_some();
        self.handle_keyboard(&ctx, &mut actions, typing);
        self.handle_mouse_buttons(&ctx, &mut actions);

        for action in actions {
            self.apply(action, &ctx, fs);
        }
        if fs.get_current_path() != self.shown_path {
            let previous = std::mem::take(&mut self.shown_path);
            self.on_folder_changed(fs, Some(&previous));
        }
        self.refresh_list(fs);
        if fs.generation() != self.tree_generation {
            // The shown folder changed on disk: its tree entry too.
            self.tree_generation = fs.generation();
            let repaint = ctx.clone();
            self.tree
                .refresh(fs.get_current_path(), fs.is_showing_hidden(), move || {
                    repaint.request_repaint()
                });
        }
        if let Some(target) = self.reveal.clone() {
            if self.list.paths.contains(&target) {
                self.selection.select_only(&target);
                self.scroll_to_focus = true;
                self.reveal = None;
            } else if !fs.is_loading() || target.parent() != Some(fs.get_current_path()) {
                // Loaded without it (gone), or we went elsewhere.
                self.reveal = None;
            }
        }
        if self.detect_view && !fs.is_loading() {
            // A slow folder finished loading after we arrived: choose its view now.
            self.detect_view = false;
            if looks_like_picture_folder(fs.get_items()) {
                self.view = self.settings.picture_view();
            }
        }
        self.request_preview(&ctx);
        let names_key = self.list.key.as_ref().map(|k| k.0).unwrap_or(0);
        self.fonts.ensure_coverage(
            &ctx,
            names_key,
            self.list.items.iter().map(|i| i.name.as_str()),
        );
        let scope = format!("{}:{}", self.active_tab, fs.get_current_path().display());
        if let Some(action) = self.speech.show(&ctx, &scope, "Use as search text") {
            use enki_speech_ui::{Action as SpeechAction, VoiceCommand};
            match action {
                SpeechAction::Insert(text) => self.search_text = text,
                SpeechAction::Command(command) => match command {
                    VoiceCommand::Previous => self.apply(Action::Back, &ctx, fs),
                    VoiceCommand::Next => self.apply(Action::Forward, &ctx, fs),
                    VoiceCommand::OpenAssistant => self.speech.open = true,
                    VoiceCommand::Stop => self.speech.open = false,
                    VoiceCommand::ZoomIn => {
                        self.settings.ui_scale = (self.settings.ui_scale + 0.1).min(2.0)
                    }
                    VoiceCommand::ZoomOut => {
                        self.settings.ui_scale = (self.settings.ui_scale - 0.1).max(0.75)
                    }
                    VoiceCommand::Undo | VoiceCommand::Redo => self
                        .speech
                        .notice("Use the file action controls to review changes."),
                },
            }
        }
    }

    // ------------------------------------------------------------------
    // State upkeep
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // Tabs
    // ------------------------------------------------------------------

    fn restore_tabs(&mut self, fs: &FileSystem) {
        let saved = std::mem::take(&mut self.settings.open_tabs);
        let active = self.settings.active_tab;
        // Only if the app opened on the saved active tab's folder (not a
        // folder given on the command line).
        if saved.len() < 2 || saved.get(active).map(PathBuf::as_path) != Some(fs.get_current_path())
        {
            return;
        }
        self.tabs.clear();
        for (i, path) in saved.into_iter().enumerate() {
            if i == active {
                self.active_tab = self.tabs.len();
                self.tabs.push(None);
            } else if path.is_dir() {
                let tab_fs = FileSystem::new(path, fs.is_showing_hidden());
                self.tabs
                    .push(Some(TabState::new(tab_fs, self.settings.default_view)));
            }
        }
    }

    fn tab_path(&self, i: usize, active_fs: &FileSystem) -> PathBuf {
        match &self.tabs[i] {
            Some(t) => t.fs.get_current_path().to_path_buf(),
            None => active_fs.get_current_path().to_path_buf(),
        }
    }

    /// Exchange the active tab's state with a parked tab's.
    fn swap_tab_state(&mut self, t: &mut TabState) {
        std::mem::swap(&mut self.view, &mut t.view);
        std::mem::swap(&mut self.search_text, &mut t.search_text);
        std::mem::swap(&mut self.selection, &mut t.selection);
        std::mem::swap(&mut self.list, &mut t.list);
        std::mem::swap(&mut self.address_text, &mut t.address_text);
        std::mem::swap(&mut self.editing_address, &mut t.editing_address);
        std::mem::swap(&mut self.shown_path, &mut t.shown_path);
        std::mem::swap(&mut self.scroll_offset, &mut t.scroll_offset);
        std::mem::swap(&mut self.viewport_height, &mut t.viewport_height);
        std::mem::swap(&mut self.scroll_to_focus, &mut t.scroll_to_focus);
        std::mem::swap(&mut self.reload_requested_at, &mut t.reload_requested_at);
    }

    fn switch_tab(&mut self, fs: &mut FileSystem, index: usize) {
        if index == self.active_tab || index >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs[index].take().expect("inactive tabs are parked");
        std::mem::swap(fs, &mut tab.fs);
        self.swap_tab_state(&mut tab);
        self.tabs[self.active_tab] = Some(tab);
        self.active_tab = index;
        self.thumbnails.cancel_pending();
        self.marquee = None;
        self.preview_requested = None;
        // Files may have changed while the tab was in the background.
        if fs.take_changed_on_disk() {
            fs.load_directory();
        }
    }

    fn open_tab(&mut self, fs: &mut FileSystem, path: PathBuf, switch: bool) {
        if !path.is_dir() {
            return;
        }
        let tab_fs = FileSystem::new(path.clone(), fs.is_showing_hidden());
        let index = self.active_tab + 1;
        self.tabs.insert(
            index,
            Some(TabState::new(tab_fs, self.settings.default_view)),
        );
        if switch {
            self.switch_tab(fs, index);
        } else {
            self.status(format!("Opened {} in a new tab", display_name(&path)));
        }
    }

    fn close_tab(&mut self, fs: &mut FileSystem, index: usize) {
        if self.tabs.len() < 2 || index >= self.tabs.len() {
            return;
        }
        if index == self.active_tab {
            let next = if index + 1 < self.tabs.len() {
                index + 1
            } else {
                index - 1
            };
            self.switch_tab(fs, next);
        }
        self.tabs.remove(index);
        if index < self.active_tab {
            self.active_tab -= 1;
        }
    }

    // ------------------------------------------------------------------
    // Network connections
    // ------------------------------------------------------------------

    fn start_connect(&mut self, ctx: &egui::Context, uri: String, creds: Option<Credentials>) {
        if self.connecting.is_some() {
            return;
        }
        match &mut self.dialog {
            Some(Dialog::Connect {
                busy,
                error,
                address,
                ..
            }) => {
                *busy = true;
                *error = None;
                *address = uri.clone();
            }
            _ => {
                self.dialog = Some(Dialog::Connect {
                    address: uri.clone(),
                    user: default_user(),
                    domain: String::new(),
                    password: String::new(),
                    needs_credentials: false,
                    asks_domain: false,
                    error: None,
                    busy: true,
                    first_frame: false,
                })
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.connecting = Some(rx);
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = network::connect(&uri, creds.as_ref());
            drop(creds); // don't keep the password around longer than needed
            let _ = tx.send((uri, result));
            ctx.request_repaint();
        });
    }

    fn poll_connect(&mut self, fs: &mut FileSystem) {
        let Some(rx) = &self.connecting else { return };
        let Ok((uri, result)) = rx.try_recv() else {
            return;
        };
        self.connecting = None;
        match result {
            Ok(path) => {
                self.dialog = None;
                let remembered = network::redact_password(&uri);
                self.settings.recent_servers.retain(|s| *s != remembered);
                self.settings.recent_servers.insert(0, remembered);
                self.settings.recent_servers.truncate(8);
                self.places_stale.store(true, Ordering::Relaxed);
                if !fs.set_path(path.clone()) {
                    self.status(format!("Connected, but couldn't open {}", path.display()));
                }
            }
            Err(err) => {
                // The dialog may have been cancelled meanwhile; then just report.
                let Some(Dialog::Connect {
                    needs_credentials,
                    asks_domain,
                    error,
                    busy,
                    password,
                    ..
                }) = &mut self.dialog
                else {
                    if let ConnectError::Failed(msg) = err {
                        self.status(msg);
                    }
                    return;
                };
                *busy = false;
                password.clear();
                match err {
                    ConnectError::NeedsCredentials { asks_domain: d } => {
                        *needs_credentials = true;
                        *asks_domain = d;
                        *error = None;
                    }
                    ConnectError::Failed(msg) => *error = Some(msg),
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Menu bar
    // ------------------------------------------------------------------

    fn show_menu_bar(&mut self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| self.file_menu(ui, fs, actions));
            ui.menu_button("Edit", |ui| self.edit_menu(ui, actions));
            ui.menu_button("View", |ui| self.view_main_menu(ui, fs, actions));
            ui.menu_button("Go", |ui| self.go_menu(ui, fs, actions));
            ui.menu_button("Bookmarks", |ui| self.bookmarks_menu(ui, fs, actions));
            ui.menu_button("Help", |ui| {
                ui.set_min_width(200.0);
                menu_item(
                    ui,
                    "Keyboard Shortcuts",
                    "F1",
                    true,
                    Action::ShowShortcuts,
                    actions,
                );
                menu_item(ui, "About Ninimma", "", true, Action::ShowAbout, actions);
            });
        });
    }

    fn file_menu(&self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        ui.set_min_width(240.0);
        menu_item(
            ui,
            "New Tab",
            "Ctrl+T",
            true,
            Action::NewTab(None, true),
            actions,
        );
        menu_item(
            ui,
            "New Folder",
            "Ctrl+Shift+N",
            true,
            Action::NewFolder,
            actions,
        );
        ui.menu_button("New Document", |ui| self.new_document_menu(ui, actions));
        if disk_images::SUPPORTED {
            menu_item(
                ui,
                "Disk images & BitLocker…",
                "",
                self.disk_launch.is_none(),
                Action::ShowDisks(None, DiskOperation::Mount),
                actions,
            );
        }
        ui.separator();
        let here = fs.get_current_path().to_path_buf();
        menu_item(
            ui,
            "Open in Terminal",
            "",
            true,
            Action::OpenTerminal(here),
            actions,
        );
        menu_item(
            ui,
            "Connect to Server…",
            "",
            true,
            Action::ShowConnect,
            actions,
        );
        ui.separator();
        menu_item(
            ui,
            "Close Tab",
            "Ctrl+W",
            self.tabs.len() > 1,
            Action::CloseTab(self.active_tab),
            actions,
        );
        menu_item(ui, "Quit", "Ctrl+Q", true, Action::Quit, actions);
    }

    /// "Empty File", "Text Document" and everything in the Templates folder
    /// (subfolders become submenus), as in Nemo and Nautilus.
    fn new_document_menu(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.set_min_width(200.0);
        menu_item(
            ui,
            "📄 Empty File",
            "",
            true,
            Action::NewFile(NewKind::Empty),
            actions,
        );
        menu_item(
            ui,
            "📃 Text Document",
            "",
            true,
            Action::NewFile(NewKind::TextDocument),
            actions,
        );
        ui.separator();
        fn templates(ui: &mut Ui, list: &[Template], actions: &mut Vec<Action>) {
            for t in list {
                if t.children.is_empty() {
                    menu_item(
                        ui,
                        &t.label,
                        "",
                        true,
                        Action::NewFile(NewKind::Template(t.path.clone())),
                        actions,
                    );
                } else {
                    ui.menu_button(format!("📁 {}", t.label), |ui| {
                        templates(ui, &t.children, actions)
                    });
                }
            }
        }
        if self.templates.is_empty() {
            ui.label(RichText::new("No templates").color(pal().text_secondary)).on_hover_text(
                "Files you put in your Templates folder appear here, ready to create new documents from.",
            );
        } else {
            templates(ui, &self.templates, actions);
        }
        if let Some(dir) = crate::file_system::templates_dir() {
            ui.separator();
            menu_item(
                ui,
                "Open Templates Folder",
                "",
                true,
                Action::Navigate(dir),
                actions,
            );
        }
    }

    /// Paste is possible with our own copy or files copied in another app.
    fn can_paste(&self) -> bool {
        if self.clipboard.is_some() {
            return true;
        }
        match self.system_files.get() {
            Some((at, has)) if at.elapsed() < Duration::from_millis(500) => has,
            _ => {
                let has = system_clipboard::files().is_some();
                self.system_files.set(Some((Instant::now(), has)));
                has
            }
        }
    }

    fn edit_menu(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.set_min_width(240.0);
        let idle = self.jobs.is_empty();
        let undo = self
            .history
            .undo_label()
            .map_or("Undo".to_string(), |l| format!("Undo {}", l.to_lowercase()));
        menu_item(
            ui,
            &undo,
            "Ctrl+Z",
            idle && self.history.undo_label().is_some() && self.history.undo_blocked().is_none(),
            Action::Undo,
            actions,
        );
        let redo = self
            .history
            .redo_label()
            .map_or("Redo".to_string(), |l| format!("Redo {}", l.to_lowercase()));
        menu_item(
            ui,
            &redo,
            "Ctrl+Y",
            idle && self.history.redo_label().is_some(),
            Action::Redo,
            actions,
        );
        ui.separator();
        let has_sel = !self.selection.is_empty();
        menu_item(ui, "Cut", "Ctrl+X", has_sel, Action::Cut, actions);
        menu_item(ui, "Copy", "Ctrl+C", has_sel, Action::Copy, actions);
        menu_item(
            ui,
            "Paste",
            "Ctrl+V",
            self.can_paste(),
            Action::Paste(None),
            actions,
        );
        menu_item(
            ui,
            "Copy as Path",
            "Ctrl+Shift+C",
            true,
            Action::CopyPath,
            actions,
        );
        ui.separator();
        menu_item(ui, "Select All", "Ctrl+A", true, Action::SelectAll, actions);
        menu_item(
            ui,
            "Invert Selection",
            "Ctrl+Shift+I",
            true,
            Action::InvertSelection,
            actions,
        );
        ui.separator();
        menu_item(
            ui,
            "Search Indexing…",
            "",
            true,
            Action::ShowIndexSettings,
            actions,
        );
        ui.separator();
        menu_item(ui, "Rename…", "F2", has_sel, Action::Rename, actions);
        menu_item(ui, "Move to Trash", "Del", has_sel, Action::Trash, actions);
        menu_item(
            ui,
            "Delete Permanently…",
            "Shift+Del",
            has_sel,
            Action::ConfirmDeletePermanently,
            actions,
        );
        ui.separator();
        let can_rotate = self.rotatable_selection().is_some();
        menu_item(
            ui,
            "Rotate Left",
            "",
            can_rotate,
            Action::Rotate(Rotation::CounterClockwise),
            actions,
        );
        menu_item(
            ui,
            "Rotate Right",
            "",
            can_rotate,
            Action::Rotate(Rotation::Clockwise),
            actions,
        );
        menu_item(
            ui,
            "Flip Horizontally",
            "",
            can_rotate,
            Action::Rotate(Rotation::FlipHorizontal),
            actions,
        );
        menu_item(
            ui,
            "Flip Vertically",
            "",
            can_rotate,
            Action::Rotate(Rotation::FlipVertical),
            actions,
        );
    }

    fn view_main_menu(&self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        ui.set_min_width(240.0);
        self.view_menu(ui, actions);
        ui.separator();
        ui.menu_button("Sort By", |ui| self.sort_menu(ui, actions));
        ui.separator();
        ui.menu_button("Theme", |ui| {
            for appearance in crate::theme::Appearance::ALL {
                if ui
                    .radio(self.settings.appearance == appearance, appearance.label())
                    .clicked()
                {
                    actions.push(Action::SetAppearance(appearance));
                    ui.close();
                }
            }
        });
        ui.menu_button("Interface Size", |ui| {
            for scale in crate::theme::SCALES {
                let label = format!("{:.0}%", scale * 100.0);
                if ui
                    .radio((self.settings.ui_scale - scale).abs() < 0.01, label)
                    .clicked()
                {
                    actions.push(Action::SetScale(scale));
                    ui.close();
                }
            }
        });
        ui.separator();
        let mut show_recent = self.settings.show_recent;
        if ui
            .checkbox(&mut show_recent, "Show Recent Files and Folders")
            .on_hover_text("Kept only on this computer")
            .clicked()
        {
            actions.push(Action::ToggleRecent);
            ui.close();
        }
        if ui.button("🧹 Clear Recent").clicked() {
            actions.push(Action::ClearRecent);
            ui.close();
        }
        let mut hidden = fs.is_showing_hidden();
        if ui
            .checkbox(&mut hidden, "Show Hidden Files  (Ctrl+H)")
            .clicked()
        {
            actions.push(Action::ToggleHidden);
            ui.close();
        }
        let mut nav = self.settings.show_nav_pane;
        if ui.checkbox(&mut nav, "Navigation Pane").clicked() {
            actions.push(Action::TogglePane(Pane::Navigation));
            ui.close();
        }
        let mut boxes = self.settings.item_checkboxes;
        if ui
            .checkbox(&mut boxes, "Item Check Boxes")
            .on_hover_text("Tick items to select several, without Ctrl")
            .clicked()
        {
            actions.push(Action::ToggleItemCheckboxes);
            ui.close();
        }
        let mut preview = self.settings.show_preview_pane;
        if ui.checkbox(&mut preview, "Preview Pane").clicked() {
            actions.push(Action::TogglePane(Pane::Preview));
            ui.close();
        }
        if ui
            .button("Apply to All Folders")
            .on_hover_text("Open every folder of this kind (pictures, or everything else) with this view; views set for single folders are forgotten")
            .clicked()
        {
            actions.push(Action::ApplyViewToAll);
            ui.close();
        }
        ui.separator();
        menu_item(ui, "Zoom In", "Ctrl++", true, Action::Zoom(0.25), actions);
        menu_item(ui, "Zoom Out", "Ctrl+-", true, Action::Zoom(-0.25), actions);
        menu_item(ui, "Refresh", "F5", true, Action::Refresh, actions);
    }

    fn go_menu(&self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        ui.set_min_width(220.0);
        menu_item(
            ui,
            "Back",
            "Alt+←",
            self.special.is_some() || fs.can_go_back(),
            Action::Back,
            actions,
        );
        menu_item(
            ui,
            "Forward",
            "Alt+→",
            fs.can_go_forward(),
            Action::Forward,
            actions,
        );
        menu_item(
            ui,
            "Up",
            "Alt+↑",
            self.special.is_none(),
            Action::Up,
            actions,
        );
        menu_item(ui, "Home", "Alt+Home", true, Action::Home, actions);
        menu_item(
            ui,
            "This PC",
            "",
            true,
            Action::ShowSpecial(Special::ThisPc),
            actions,
        );
        if self.settings.show_recent {
            menu_item(
                ui,
                "Recent",
                "",
                true,
                Action::ShowSpecial(Special::Recent),
                actions,
            );
        }
        ui.separator();
        for (name, path) in &self.quick_access {
            if name != "Home" {
                menu_item(ui, name, "", true, Action::Navigate(path.clone()), actions);
            }
        }
        ui.separator();
        menu_item(
            ui,
            "Location…",
            "Ctrl+L",
            true,
            Action::FocusAddress,
            actions,
        );
        menu_item(ui, "Search", "Ctrl+F", true, Action::FocusSearch, actions);
        menu_item(
            ui,
            "Connect to Server…",
            "",
            true,
            Action::ShowConnect,
            actions,
        );
    }

    fn bookmarks_menu(&self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        ui.set_min_width(220.0);
        let here = fs.get_current_path().to_path_buf();
        if self.is_bookmarked(&here) {
            menu_item(
                ui,
                "Remove Bookmark",
                "Ctrl+D",
                true,
                Action::RemoveBookmark(here),
                actions,
            );
        } else {
            menu_item(
                ui,
                "Add Bookmark",
                "Ctrl+D",
                true,
                Action::AddBookmark(here),
                actions,
            );
        }
        if !self.bookmarks.is_empty() {
            ui.separator();
        }
        for b in &self.bookmarks {
            menu_item(
                ui,
                &format!("📁 {}", b.label),
                "",
                true,
                Action::Navigate(b.path.clone()),
                actions,
            );
        }
    }

    fn show_tab_bar(&mut self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let closable = self.tabs.len() > 1;
            let width = ((ui.available_width() - 40.0) / self.tabs.len() as f32).clamp(90.0, 220.0);
            for i in 0..self.tabs.len() {
                let path = self.tab_path(i, fs);
                let title = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let active = i == self.active_tab;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::click());
                let drop_target = !active && self.drag_hover(ui, rect);
                if drop_target {
                    self.drop_folder = Some(path.clone());
                }
                let bg = if active {
                    pal().header
                } else if resp.hovered() || drop_target {
                    pal().hover
                } else {
                    pal().sidebar
                };
                let painter = ui.painter();
                painter.rect_filled(
                    rect,
                    CornerRadius {
                        nw: 8,
                        ne: 8,
                        sw: 0,
                        se: 0,
                    },
                    bg,
                );
                if active {
                    let accent = Rect::from_min_size(
                        rect.left_top() + Vec2::new(8.0, 0.0),
                        Vec2::new(rect.width() - 16.0, 2.0),
                    );
                    painter.rect_filled(accent, CornerRadius::same(1), pal().accent);
                }
                if drop_target {
                    painter.rect_stroke(
                        rect,
                        CornerRadius::same(6),
                        Stroke::new(2.0, pal().accent),
                        StrokeKind::Inside,
                    );
                }
                painter.text(
                    rect.left_center() + Vec2::new(10.0, 0.0),
                    Align2::LEFT_CENTER,
                    "📁",
                    FontId::proportional(FONT_SIZE_SMALL),
                    pal().text_secondary,
                );
                let text_color = if active {
                    pal().text
                } else {
                    pal().text_secondary
                };
                let galley = elided(
                    painter,
                    &title,
                    FONT_SIZE_SMALL,
                    text_color,
                    rect.width() - (if closable { 58.0 } else { 36.0 }),
                    1,
                    Align::Min,
                );
                painter.galley(
                    Pos2::new(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0),
                    galley,
                    text_color,
                );
                resp.widget_info(|| {
                    egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, active, &title)
                });

                // Close button, drawn on top of the tab.
                let mut closed = false;
                if closable {
                    let close_rect = Rect::from_center_size(
                        rect.right_center() - Vec2::new(16.0, 0.0),
                        Vec2::splat(20.0),
                    );
                    let close = ui.interact(
                        close_rect,
                        ui.scope_id().with(("close_tab", i)),
                        Sense::click(),
                    );
                    if close.hovered() {
                        ui.painter()
                            .rect_filled(close_rect, CornerRadius::same(4), pal().hover);
                    }
                    if close.hovered() || resp.hovered() || active {
                        ui.painter().text(
                            close_rect.center(),
                            Align2::CENTER_CENTER,
                            "×",
                            FontId::proportional(FONT_SIZE_NORMAL),
                            if close.hovered() {
                                pal().text
                            } else {
                                pal().text_secondary
                            },
                        );
                    }
                    close.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::Button,
                            true,
                            format!("Close {title}"),
                        )
                    });
                    if close.on_hover_text("Close tab (Ctrl+W)").clicked() {
                        actions.push(Action::CloseTab(i));
                        closed = true;
                    }
                }
                let resp = resp.on_hover_text(path.to_string_lossy());
                if !closed {
                    if resp.middle_clicked() {
                        actions.push(Action::CloseTab(i));
                    } else if resp.clicked() {
                        actions.push(Action::SwitchTab(i));
                    }
                }
            }
            if ui
                .add(
                    Button::new(RichText::new("+").size(FONT_SIZE_LARGE))
                        .frame(false)
                        .min_size(Vec2::new(30.0, 30.0)),
                )
                .on_hover_text("New tab (Ctrl+T)")
                .clicked()
            {
                actions.push(Action::NewTab(None, true));
            }
        });
    }

    /// Opens the tree out to the folder just opened, from the closest
    /// place it's under (Quick Access, a pin or a drive).
    fn follow_tree(&mut self, ctx: &egui::Context, fs: &FileSystem) {
        let Some(target) = self.tree_follow.take() else {
            return;
        };
        let drives: Vec<PathBuf> = self
            .drives
            .lock()
            .unwrap()
            .iter()
            .map(|d| d.path.clone())
            .collect();
        let root = self
            .quick_access
            .iter()
            .map(|(_, p)| p.clone())
            .chain(self.settings.pinned.iter().cloned())
            .chain(drives)
            .filter(|r| target.starts_with(r))
            .max_by_key(|r| r.components().count());
        let Some(root) = root else { return };
        let ancestors: Vec<PathBuf> = target
            .ancestors()
            .skip(1)
            .take_while(|a| a.starts_with(&root))
            .map(Path::to_path_buf)
            .collect();
        for folder in ancestors.iter().rev() {
            let repaint = ctx.clone();
            self.tree.expand(folder, fs.is_showing_hidden(), move || {
                repaint.request_repaint()
            });
        }
        self.tree_scroll = true;
    }

    fn on_folder_changed(&mut self, fs: &FileSystem, previous: Option<&Path>) {
        let path = fs.get_current_path().to_path_buf();
        // Arriving in a folder, by any route, leaves This PC or Recent.
        self.special = None;
        if previous.is_some_and(|p| !p.as_os_str().is_empty()) {
            self.remember_recent(&path);
        }
        self.shown_path = path.clone();
        self.tree_follow = Some(path.clone());
        self.address_text = path.to_string_lossy().to_string();
        self.editing_address = false;
        self.search_text.clear();
        if let Some(text) = self.pending_search.take() {
            self.search_text = text;
        }
        self.selection.clear();
        self.thumbnails.cancel_pending();
        self.marquee = None;
        self.list.key = None;
        self.detect_view = false;
        self.view = match self.settings.folder_views.get(&path) {
            Some((v, _)) => *v,
            None if fs.is_loading() => {
                self.detect_view = true;
                self.settings.default_view
            }
            None if looks_like_picture_folder(fs.get_items()) => self.settings.picture_view(),
            None => self.settings.default_view,
        };
        self.refresh_list(fs);
        // Going up selects the folder we came from.
        if let Some(prev) = previous {
            if prev.parent() == Some(path.as_path()) && self.list.paths.iter().any(|p| p == prev) {
                self.selection.select_only(prev);
                self.scroll_to_focus = true;
            }
        }
    }

    fn set_view(&mut self, view: ViewSettings) {
        if view != self.view {
            self.view = view;
            self.settings.remember_view(&self.shown_path, view);
            self.scroll_to_focus = true;
        }
    }

    /// Whether the search box is searching the index rather than
    /// filtering the folder on screen.
    fn searching_index(&self) -> bool {
        self.settings.search_scope.uses_index() && !self.search_text.trim().is_empty()
    }

    fn refresh_list(&mut self, fs: &FileSystem) {
        let disk_changed = self
            .list
            .key
            .as_ref()
            .is_some_and(|k| k.0 != fs.generation());
        if self.searching_index() {
            let (text, scope) = (
                self.search_text.trim().to_string(),
                self.settings.search_scope,
            );
            self.index.search(&text, scope, fs.get_current_path());
            if disk_changed {
                // Files changed (e.g. renamed in the results): ask again.
                self.index.refresh();
            }
        } else if self.index.results.query.is_some() {
            self.index.clear_results();
        }
        let key = (
            fs.generation(),
            self.view.sort_by,
            self.view.sort_ascending,
            self.search_text.clone(),
            self.settings.search_scope,
            self.index.results.version,
            self.special,
            self.recent_version,
        );
        if self.list.key.as_ref() == Some(&key) {
            return;
        }
        let same_folder = self.list.folder == fs.get_current_path();
        if same_folder && self.list.key.as_ref().map(|k| k.0) != Some(key.0) {
            // Reloaded after a change on disk: refresh thumbnails of edited files.
            let old: HashMap<&Path, _> = self
                .list
                .items
                .iter()
                .map(|i| (i.path.as_path(), (i.modified, i.size)))
                .collect();
            for item in fs.get_items() {
                if old
                    .get(item.path.as_path())
                    .is_some_and(|o| *o != (item.modified, item.size))
                {
                    self.thumbnails.invalidate(&item.path);
                }
            }
        }

        let search = self.search_text.to_lowercase();
        let recent = self.special == Some(Special::Recent);
        let from_index = self.searching_index() && self.special.is_none();
        let mut items: Vec<FileItem> = if self.special == Some(Special::ThisPc) {
            Vec::new() // drawn as drive tiles instead
        } else if recent {
            // Newest first, filtered by the search box like a folder.
            self.recent_items
                .iter()
                .filter(|i| search.is_empty() || i.name.to_lowercase().contains(&search))
                .cloned()
                .collect()
        } else if from_index {
            // Index results keep their ranking (best matches first).
            self.index.results.items.clone()
        } else {
            fs.get_items()
                .iter()
                .filter(|i| search.is_empty() || i.name.to_lowercase().contains(&search))
                .cloned()
                .collect()
        };
        if !(from_index || recent) {
            sort_items(&mut items, self.view.sort_by, self.view.sort_ascending);
        }
        self.list.paths = items.iter().map(|i| i.path.clone()).collect();
        self.list.items = items;
        self.list.folder = fs.get_current_path().to_path_buf();
        self.list.key = Some(key);
        self.selection.retain_existing(&self.list.paths);
    }

    fn auto_refresh(&mut self, ctx: &egui::Context, fs: &mut FileSystem) {
        if fs.take_changed_on_disk() {
            self.reload_requested_at.get_or_insert_with(Instant::now);
        }
        if let Some(t) = self.reload_requested_at {
            // Debounce so a burst of changes causes one reload.
            if t.elapsed() >= Duration::from_millis(250) {
                self.reload_requested_at = None;
                fs.load_directory();
            } else {
                ctx.request_repaint_after(Duration::from_millis(260) - t.elapsed());
            }
        }
    }

    fn refresh_menus(&mut self) {
        if self
            .menus_refreshed
            .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        self.menus_refreshed = Some(Instant::now());
        self.templates = crate::file_system::templates_dir()
            .map(|d| file_ops::list_templates(&d))
            .unwrap_or_default();
        self.bookmarks = match &self.bookmarks_file {
            Some(file) => bookmarks::load(file),
            None => self
                .settings
                .bookmarks
                .iter()
                .map(|(label, path)| Bookmark {
                    label: label.clone(),
                    path: path.clone(),
                })
                .collect(),
        };
        self.dolphin_places = self
            .dolphin_places_file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|xml| bookmarks::parse_dolphin_places(&xml))
            .unwrap_or_default();
        // Show each folder once, even if both GTK apps and Dolphin bookmark it.
        for place in self.dolphin_places.clone() {
            if !self.bookmarks.iter().any(|b| b.path == place.path) {
                self.bookmarks.push(place);
            }
        }
    }

    fn is_bookmarked(&self, path: &Path) -> bool {
        self.bookmarks.iter().any(|b| b.path == path)
    }

    fn poll_disk_launch(&mut self, ctx: &egui::Context) {
        let Some(receiver) = &self.disk_launch else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                self.disk_launch = None;
                match result {
                    Ok(()) => {
                        self.status("Disk operation terminal opened. Check its result there.")
                    }
                    Err(message) => {
                        self.dialog = Some(Dialog::Errors {
                            title: "Could not start disk operation".into(),
                            messages: vec![message],
                        })
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(100))
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.disk_launch = None;
                self.status("Disk operation launcher stopped unexpectedly.");
            }
        }
    }

    fn start_drive_refresh(&mut self, ctx: &egui::Context) {
        if self
            .quick_access_refreshed
            .is_none_or(|t| t.elapsed() > Duration::from_secs(5))
        {
            self.quick_access = FileSystem::get_quick_access();
            self.quick_access_refreshed = Some(Instant::now());
        }
        if self.drives_thread_started {
            return;
        }
        self.drives_thread_started = true;
        let drives = self.drives.clone();
        let places = self.places.clone();
        let stale = self.places_stale.clone();
        let ctx = ctx.clone();
        thread::spawn(move || loop {
            let fresh_places = network::discover();
            {
                let mut p = places.lock().unwrap();
                if *p != fresh_places {
                    *p = fresh_places;
                    ctx.request_repaint();
                }
            }
            let fresh = FileSystem::get_drives();
            let changed = {
                let mut d = drives.lock().unwrap();
                let changed = d.len() != fresh.len()
                    || d.iter()
                        .zip(&fresh)
                        .any(|(a, b)| a.path != b.path || a.free_space != b.free_space);
                *d = fresh;
                changed
            };
            if changed {
                ctx.request_repaint();
            }
            // Every 5 s, or sooner after a connect/disconnect.
            for _ in 0..20 {
                if stale.swap(false, Ordering::Relaxed) {
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
        });
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.selection.ordered(&self.list.paths)
    }

    fn rotatable_selection(&self) -> Option<Vec<PathBuf>> {
        let sel = self.selected_paths();
        (!sel.is_empty() && sel.iter().all(|p| image_ops::can_rotate(p))).then_some(sel)
    }

    fn status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    // ------------------------------------------------------------------
    // Background jobs (rotate, paste, delete)
    // ------------------------------------------------------------------

    fn spawn_job<F>(
        &mut self,
        ctx: &egui::Context,
        verb: &str,
        kind: JobKind,
        items: Vec<PathBuf>,
        op: F,
    ) where
        F: Fn(&Path, &mut Vec<Step>) -> Result<(), String> + Send + 'static,
    {
        if items.is_empty() {
            return;
        }
        let progress = Arc::new(JobProgress::default());
        let order = self.history.reserve();
        self.jobs.push(Job {
            verb: verb.to_string(),
            total: items.len(),
            progress: progress.clone(),
            kind,
            order,
            started: Instant::now(),
        });
        // Copies and moves count their bytes, for a total and time left.
        if matches!(verb, "Copied" | "Moved") {
            let meter = progress.meter.clone();
            let items = items.clone();
            thread::spawn(move || {
                let total = items.iter().map(|p| file_ops::size_of(p)).sum();
                meter.total.store(total, Ordering::Relaxed);
                meter.total_known.store(true, Ordering::Relaxed);
            });
        }
        let ctx = ctx.clone();
        thread::spawn(move || {
            // Mark the job finished even if this thread dies unexpectedly.
            let _guard = FinishGuard(progress.clone(), ctx.clone());
            let meter = progress.meter.clone();
            file_ops::metered(meter.clone(), || {
                for item in items {
                    if meter.cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut steps = Vec::new();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        op(&item, &mut steps)
                    }))
                    .unwrap_or_else(|_| Err("unexpected internal error".to_string()));
                    {
                        let mut t = progress.touched.lock().unwrap();
                        t.push(item.clone());
                        t.extend(steps.iter().flat_map(|s| s.paths()).map(Path::to_path_buf));
                    }
                    progress.steps.lock().unwrap().extend(steps);
                    if let Err(e) = result {
                        progress
                            .errors
                            .lock()
                            .unwrap()
                            .push(format!("{}: {e}", display_name(&item)));
                    }
                    progress.done.fetch_add(1, Ordering::Relaxed);
                    ctx.request_repaint();
                }
            });
        });
    }

    /// Undo (or redo) a history entry in the background.
    fn spawn_history(&mut self, ctx: &egui::Context, entry: undo::Entry, was_undo: bool) {
        let progress = Arc::new(JobProgress::default());
        let verb = format!(
            "{} {}",
            if was_undo { "Undid" } else { "Redid" },
            entry.label.to_lowercase()
        );
        self.jobs.push(Job {
            verb,
            total: entry.steps.len(),
            progress: progress.clone(),
            kind: if was_undo {
                JobKind::Undo
            } else {
                JobKind::Redo
            },
            order: entry.order,
            started: Instant::now(),
        });
        let ctx = ctx.clone();
        thread::spawn(move || {
            // Always finish, so a failure can never block undo/redo for good.
            let _guard = FinishGuard(progress.clone(), ctx.clone());
            let (reversed, failed, errors) = undo::reverse(&entry, |step| {
                progress
                    .touched
                    .lock()
                    .unwrap()
                    .extend(step.paths().into_iter().map(Path::to_path_buf));
                progress.done.fetch_add(1, Ordering::Relaxed);
                ctx.request_repaint();
            });
            *progress.errors.lock().unwrap() = errors;
            *progress.reversed.lock().unwrap() = Some((reversed, failed));
        });
    }

    fn poll_jobs(&mut self, ctx: &egui::Context, fs: &mut FileSystem) {
        let mut finished = Vec::new();
        for (i, job) in self.jobs.iter().enumerate() {
            for path in job.progress.touched.lock().unwrap().drain(..) {
                self.thumbnails.invalidate(&path);
            }
            if job.progress.finished.load(Ordering::Relaxed) {
                finished.push(i);
            }
        }
        if !self.jobs.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        for i in finished.into_iter().rev() {
            let job = self.jobs.remove(i);
            let mut errors = std::mem::take(&mut *job.progress.errors.lock().unwrap());
            if job.progress.meter.cancelled.load(Ordering::Relaxed) {
                let before = errors.len();
                errors.retain(|e| !e.ends_with(file_ops::CANCELLED));
                // The item that was stopped part-way isn't done.
                let done = job
                    .progress
                    .done
                    .load(Ordering::Relaxed)
                    .saturating_sub(before - errors.len());
                self.status(format!("Cancelled: {done} of {} done", job.total));
                if errors.is_empty() {
                    if let JobKind::Record(label) = job.kind {
                        let steps = std::mem::take(&mut *job.progress.steps.lock().unwrap());
                        self.history.record_at(label, steps, job.order);
                    }
                    fs.load_directory();
                    continue;
                }
            }
            let is_history = matches!(job.kind, JobKind::Undo | JobKind::Redo);
            match job.kind {
                JobKind::Record(label) => {
                    let steps = std::mem::take(&mut *job.progress.steps.lock().unwrap());
                    self.history.record_at(label, steps, job.order);
                }
                kind @ (JobKind::Undo | JobKind::Redo) => {
                    if let Some((reversed, failed)) = job.progress.reversed.lock().unwrap().take() {
                        self.history
                            .finished(reversed, failed, matches!(kind, JobKind::Undo));
                    }
                }
                JobKind::NoHistory => {}
            }
            let ok = job.total.saturating_sub(errors.len());
            if errors.is_empty() {
                if is_history {
                    self.status(job.verb.clone());
                } else {
                    self.status(format!("{} {} item{}", job.verb, ok, plural(ok)));
                }
            } else {
                self.dialog = Some(Dialog::Errors {
                    title: format!(
                        "{}: {} of {} done. Some items couldn't be processed:",
                        job.verb, ok, job.total
                    ),
                    messages: errors,
                });
            }
            fs.load_directory();
        }
    }

    /// Paste or drop `paths` into `dest`, asking first if names clash.
    fn start_paste(
        &mut self,
        ctx: &egui::Context,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        mode: PasteMode,
    ) {
        let clashes = file_ops::conflicts(&paths, &dest);
        if clashes.is_empty() {
            self.run_paste(ctx, paths, dest, mode, Conflict::KeepBoth);
        } else {
            self.dialog = Some(Dialog::Conflict {
                paths,
                dest,
                mode,
                clashes,
            });
        }
    }

    fn run_paste(
        &mut self,
        ctx: &egui::Context,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        mode: PasteMode,
        conflict: Conflict,
    ) {
        let (verb, label) = match mode {
            PasteMode::Copy => ("Copied", count_label("Copy", paths.len())),
            PasteMode::Move => ("Moved", count_label("Move", paths.len())),
        };
        // A cut can only be pasted once: forget it now that the move is
        // really happening (not when a conflict prompt was cancelled). Clear
        // the system clipboard too, or the next Ctrl+V would paste a copy.
        if mode == PasteMode::Move
            && matches!(&self.clipboard, Some((cut, PasteMode::Move)) if *cut == paths)
        {
            self.clipboard = None;
            system_clipboard::clear();
        }
        self.spawn_job(ctx, verb, JobKind::Record(label), paths, move |p, steps| {
            file_ops::paste_into(p, &dest, mode, conflict, steps)
        });
    }

    // ------------------------------------------------------------------
    // Actions
    // ------------------------------------------------------------------

    fn apply(&mut self, action: Action, ctx: &egui::Context, fs: &mut FileSystem) {
        match action {
            Action::ShowDisks(path, operation) => {
                self.dialog = Some(Dialog::Disks {
                    operation,
                    target: path
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    read_only: true,
                    recovery_password: false,
                    error: None,
                });
            }
            Action::ToggleItemCheckboxes => {
                self.settings.item_checkboxes = !self.settings.item_checkboxes;
            }
            Action::ApplyViewToAll => {
                // Folder templates: pictures, and the rest.
                let pictures = looks_like_picture_folder(fs.get_items());
                if pictures {
                    self.settings.picture_view = Some(self.view);
                } else {
                    self.settings.default_view = self.view;
                }
                // Folders' own views give way (their kind can't be told
                // without listing each, which could hang on a share).
                self.settings.folder_views.clear();
                self.status(if pictures {
                    "Picture folders now open with this view"
                } else {
                    "Folders now open with this view"
                });
            }
            Action::SetWallpaper(path) => match wallpaper::set(&path) {
                Ok(()) => self.status(format!(
                    "\"{}\" is the desktop background",
                    display_name(&path)
                )),
                Err(e) => self.status(e),
            },
            Action::RunSavedSearch(saved) => {
                self.special = None;
                self.settings.search_scope = saved.scope;
                if saved.scope == SearchScope::Everywhere || fs.get_current_path() == saved.folder {
                    self.search_text = saved.text;
                } else if fs.set_path(saved.folder.clone()) {
                    self.pending_search = Some(saved.text);
                } else {
                    self.status(format!("Can't open {}", saved.folder.display()));
                }
            }
            Action::ForgetSavedSearch(saved) => {
                self.settings.saved_searches.retain(|s| *s != saved);
            }
            Action::Eject(drive) => {
                // Step off it first: an open folder keeps it busy.
                if fs.get_current_path().starts_with(&drive) {
                    self.special = Some(Special::ThisPc);
                    if let Some(parent) = drive.parent() {
                        fs.set_path(parent.to_path_buf());
                    }
                }
                self.spawn_job(ctx, "Ejected", JobKind::NoHistory, vec![drive], |d, _| {
                    eject::eject(d)
                });
            }
            Action::Compress => {
                let paths = self.selected_paths();
                if let Some(dest) = archive::zip_name_for(&paths) {
                    self.reveal = Some(dest.clone());
                    self.spawn_job(
                        ctx,
                        "Compressed",
                        JobKind::NoHistory,
                        vec![dest],
                        move |d, _| archive::compress(&paths, d),
                    );
                }
            }
            Action::Extract(zip) => match archive::extracted_size(&zip) {
                Ok(bytes) if bytes > archive::DEFAULT_EXTRACT_LIMIT => {
                    self.dialog = Some(Dialog::ConfirmLargeArchive {
                        archive: zip,
                        bytes,
                    });
                }
                Ok(_) => self.apply(
                    Action::ExtractApproved(zip, archive::DEFAULT_EXTRACT_LIMIT),
                    ctx,
                    fs,
                ),
                Err(e) => self.status(e),
            },
            Action::ExtractApproved(zip, limit) => {
                if let Some(dest) = archive::folder_for(&zip) {
                    self.reveal = Some(dest.clone());
                    self.spawn_job(
                        ctx,
                        "Extracted",
                        JobKind::NoHistory,
                        vec![dest],
                        move |d, _| {
                            if limit == archive::DEFAULT_EXTRACT_LIMIT {
                                archive::extract(&zip, d)
                            } else {
                                archive::extract_with_limit(&zip, d, limit)
                            }
                        },
                    );
                }
            }
            Action::SetAppearance(appearance) => self.settings.appearance = appearance,
            Action::SetScale(scale) => self.settings.ui_scale = scale,
            Action::OpenLocation(path, true) => {
                if let Some(parent) = path.parent().filter(|p| p.is_dir()) {
                    self.open_tab(fs, parent.to_path_buf(), true);
                    // Selected once the new tab's folder has loaded.
                    self.reveal = Some(path);
                }
            }
            Action::OpenLocation(path, false) => {
                if let Some(parent) = path.parent() {
                    self.special = None;
                    self.search_text.clear();
                    if fs.set_path(parent.to_path_buf()) || fs.get_current_path() == parent {
                        // Selected once the folder has loaded.
                        self.reveal = Some(path);
                    }
                }
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            Action::OpenWithApp(app, paths, make_default) => {
                for p in &paths {
                    self.remember_recent(p);
                }
                if let Some(mime) = make_default {
                    // Tests record it: they must never change the desktop's
                    // real associations.
                    #[cfg(test)]
                    self.defaults_set.push((app.id.clone(), mime));
                    #[cfg(not(test))]
                    if let Err(e) = open_with::set_default(&app, &mime) {
                        self.status(e);
                    }
                }
                #[cfg(test)]
                self.launched.push((app.id.clone(), paths));
                #[cfg(not(test))]
                if let Err(e) = open_with::launch(&app, &paths) {
                    self.status(e);
                }
            }
            Action::ChooseApp => {
                let paths: Vec<PathBuf> = self
                    .selected_paths()
                    .into_iter()
                    .filter(|p| !p.is_dir())
                    .collect();
                if let Some(first) = paths.first().cloned() {
                    #[cfg(windows)]
                    {
                        // The system Open With dialog.
                        let _ = std::process::Command::new("rundll32.exe")
                            .arg("shell32.dll,OpenAs_RunDLL")
                            .arg(&first)
                            .spawn();
                    }
                    #[cfg(all(unix, not(target_os = "macos")))]
                    {
                        let mime = self.app_catalog().mime_of(&first);
                        self.dialog = Some(Dialog::OpenWith {
                            paths,
                            mime,
                            filter: String::new(),
                            always: false,
                        });
                    }
                    #[cfg(target_os = "macos")]
                    let _ = first;
                }
            }
            Action::Properties => {
                let mut paths = self.selected_paths();
                if paths.is_empty() && self.special.is_none() {
                    paths.push(fs.get_current_path().to_path_buf());
                }
                if !paths.is_empty() {
                    let details = (paths.len() == 1).then(|| properties::details(&paths[0]));
                    let repaint = ctx.clone();
                    let count = properties::SizeCount::start(paths.clone(), move || {
                        repaint.request_repaint()
                    });
                    self.dialog = Some(Dialog::Properties {
                        paths,
                        details,
                        count,
                    });
                }
            }
            Action::ToggleTree(path) => {
                let repaint = ctx.clone();
                self.tree.toggle(&path, fs.is_showing_hidden(), move || {
                    repaint.request_repaint()
                });
            }
            Action::Pin(path) => {
                if !self.settings.pinned.contains(&path) {
                    self.settings.pinned.push(path);
                }
            }
            Action::Unpin(path) => self.settings.pinned.retain(|p| *p != path),
            Action::MovePin(path, by) => {
                let pins = &mut self.settings.pinned;
                if let Some(i) = pins.iter().position(|p| *p == path) {
                    let j = i.saturating_add_signed(by).min(pins.len() - 1);
                    let pin = pins.remove(i);
                    pins.insert(j, pin);
                }
            }
            Action::ShowSpecial(special) => {
                self.special = Some(special);
                self.selection.clear();
                self.editing_address = false;
                if special == Special::Recent {
                    self.load_recent();
                }
                if special == Special::Trash {
                    self.load_trash();
                }
            }
            Action::RestoreFromTrash(ids) => {
                let errors = trash_bin::restore(&ids);
                let restored = ids.len().saturating_sub(errors.len());
                if restored > 0 {
                    self.status(format!("Restored {restored} item{}", plural(restored)));
                }
                if !errors.is_empty() {
                    self.dialog = Some(Dialog::Errors {
                        title: "Some items weren't restored".into(),
                        messages: errors,
                    });
                }
                self.load_trash();
            }
            Action::ConfirmPurge(ids) => {
                if !ids.is_empty() {
                    self.dialog = Some(Dialog::ConfirmPurge { ids });
                }
            }
            Action::ToggleRecent => {
                self.settings.show_recent = !self.settings.show_recent;
                if !self.settings.show_recent {
                    // Off means not kept either.
                    self.settings.recent.clear();
                    if self.special == Some(Special::Recent) {
                        self.special = None;
                    }
                }
            }
            Action::ClearRecent => {
                self.settings.recent.clear();
                if self.special == Some(Special::Recent) {
                    self.load_recent();
                }
                self.status("Recent files and folders cleared");
            }
            Action::Navigate(path) => {
                self.special = None;
                if !fs.set_path(path.clone()) {
                    self.status(format!("Can't open {}", path.display()));
                }
            }
            Action::OpenItem(path) => {
                if path.is_dir() {
                    self.special = None;
                    fs.set_path(path);
                } else {
                    self.remember_recent(&path);
                    self.open_external(&path);
                }
            }
            Action::OpenSelected => {
                let sel = self.selected_paths();
                if let [only] = sel.as_slice() {
                    self.apply(Action::OpenItem(only.clone()), ctx, fs);
                } else {
                    for p in sel.into_iter().filter(|p| !p.is_dir()) {
                        self.remember_recent(&p);
                        self.open_external(&p);
                    }
                }
            }
            Action::Back => {
                // Leaving This PC or Recent goes back to the folder shown before.
                if self.special.take().is_none() {
                    fs.go_back();
                }
            }
            Action::Forward => {
                self.special = None;
                fs.go_forward();
            }
            Action::Up => {
                if self.special.is_some() {
                    // Already above every folder.
                } else if fs.get_parent_path().is_none() {
                    // Up from the top of a drive goes to This PC.
                    self.special = Some(Special::ThisPc);
                    self.selection.clear();
                } else {
                    fs.navigate_up();
                }
            }
            Action::Refresh => {
                if self.special == Some(Special::Recent) {
                    self.load_recent();
                } else {
                    fs.load_directory();
                }
            }
            Action::Rotate(rotation) => {
                if let Some(paths) = self.rotatable_selection() {
                    let label = count_label("Rotate", paths.len());
                    self.spawn_job(
                        ctx,
                        "Rotated",
                        JobKind::Record(label),
                        paths,
                        move |p, steps| {
                            image_ops::rotate_file(p, rotation)?;
                            steps.push(Step::Rotated {
                                path: p.to_path_buf(),
                                rotation,
                            });
                            Ok(())
                        },
                    );
                }
            }
            Action::Trash => {
                let paths = self.selected_paths();
                let label = count_label("Delete", paths.len());
                self.spawn_job(
                    ctx,
                    "Moved to trash",
                    JobKind::Record(label),
                    paths,
                    |p, steps| {
                        steps.push(undo::trash(p)?);
                        Ok(())
                    },
                );
            }
            Action::ConfirmDeletePermanently => {
                let targets = self.selected_paths();
                if !targets.is_empty() {
                    self.dialog = Some(Dialog::ConfirmDelete { targets });
                }
            }
            Action::Copy | Action::Cut => {
                let paths = self.selected_paths();
                if paths.is_empty() {
                    return;
                }
                let mode = if matches!(action, Action::Copy) {
                    PasteMode::Copy
                } else {
                    PasteMode::Move
                };
                // Also put them on the system clipboard as files, so other
                // apps (file managers, mail, chat) can paste them. Without
                // one, paths as text still reach a terminal.
                if !system_clipboard::set_files(&paths) {
                    ctx.copy_text(clipboard_text(&paths));
                }
                self.status(format!(
                    "{} {} item{}",
                    if mode == PasteMode::Copy {
                        "Copied"
                    } else {
                        "Cut"
                    },
                    paths.len(),
                    plural(paths.len())
                ));
                self.clipboard = Some((paths, mode));
            }
            Action::Paste(_) if self.special.is_some() => {
                self.status("Open a folder to paste into it");
            }
            Action::Paste(system_text) => {
                let dest = fs.get_current_path().to_path_buf();
                let (paths, mode) = match (&self.clipboard, system_text) {
                    // Our own copy, unless something else has been copied since.
                    (Some((paths, mode)), Some(text)) if text == clipboard_text(paths) => {
                        (paths.clone(), *mode)
                    }
                    (_, Some(text)) => match parse_clipboard_paths(&text) {
                        Some(parsed) => parsed,
                        None => return,
                    },
                    (ours, None) => match (ours, system_clipboard::files()) {
                        // Our own copy (a cut stays a cut).
                        (Some((paths, mode)), Some(files)) if *paths == files => {
                            (paths.clone(), *mode)
                        }
                        // Files copied in another app since.
                        (_, Some(files)) => (files, PasteMode::Copy),
                        (Some((paths, mode)), None) => (paths.clone(), *mode),
                        (None, None) => return,
                    },
                };
                self.start_paste(ctx, paths, dest, mode);
            }
            Action::CopyPath => {
                let paths = self.selected_paths();
                let paths = if paths.is_empty() {
                    vec![fs.get_current_path().to_path_buf()]
                } else {
                    paths
                };
                ctx.copy_text(clipboard_text(&paths));
                self.status("Copied path to clipboard");
            }
            Action::Rename => {
                let targets = self.selected_paths();
                if let [only] = targets.as_slice() {
                    self.dialog = Some(Dialog::Rename {
                        text: display_name(only),
                        targets,
                        error: None,
                        first_frame: true,
                    });
                } else if let Some(first) = targets.first() {
                    // Starts as "Holiday (1).jpg", "Holiday (2).jpg"... from the first name.
                    let stem = if first.is_dir() {
                        display_name(first)
                    } else {
                        first
                            .file_stem()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default()
                    };
                    self.dialog = Some(Dialog::BulkRename(Box::new(BulkRenameDialog {
                        items: targets
                            .iter()
                            .map(|p| crate::bulk_rename::Item::read(p))
                            .collect(),
                        rules: crate::bulk_rename::RenameRules::numbered(&stem),
                        preview: None,
                        preset_name: String::new(),
                        error: None,
                    })));
                }
            }
            Action::NewFolder if self.special.is_some() => {
                self.status("Open a folder to make a new folder in it");
            }
            Action::NewFolder => match FileOperations::create_new_folder(fs.get_current_path()) {
                Ok(path) => {
                    self.history
                        .record("New folder", vec![Step::Created { path: path.clone() }]);
                    fs.load_directory();
                    self.refresh_list(fs);
                    self.selection.select_only(&path);
                    self.scroll_to_focus = true;
                    self.dialog = Some(Dialog::Rename {
                        targets: vec![path.clone()],
                        text: display_name(&path),
                        error: None,
                        first_frame: true,
                    });
                }
                Err(e) => self.status(e),
            },
            Action::SelectAll => self.selection.select_all(&self.list.paths),
            Action::NewTab(path, switch) => {
                let path = path.unwrap_or_else(|| fs.get_current_path().to_path_buf());
                self.open_tab(fs, path, switch);
            }
            Action::CloseTab(i) => self.close_tab(fs, i),
            Action::ShowConnect => {
                self.dialog = Some(Dialog::Connect {
                    address: String::new(),
                    user: default_user(),
                    domain: String::new(),
                    password: String::new(),
                    needs_credentials: false,
                    asks_domain: false,
                    error: None,
                    busy: false,
                    first_frame: true,
                });
            }
            Action::Connect(uri, creds) => self.start_connect(ctx, uri, creds),
            Action::Disconnect(place) => {
                // Step out of it first, or the open folder keeps it busy.
                if fs.get_current_path().starts_with(&place.path) {
                    if let Some(home) = crate::file_system::home_dir() {
                        fs.set_path(home);
                    }
                }
                let stale = self.places_stale.clone();
                let path = place.path.clone();
                self.spawn_job(
                    ctx,
                    "Disconnected",
                    JobKind::NoHistory,
                    vec![path],
                    move |_, _| {
                        let result = network::disconnect(&place);
                        stale.store(true, Ordering::Relaxed);
                        result
                    },
                );
            }
            Action::NewFile(kind) => {
                let dir = fs.get_current_path().to_path_buf();
                let result = match &kind {
                    NewKind::Empty => FileOperations::create_empty_file(&dir, "New File", "")
                        .map(|p| (p.clone(), Step::CreatedFile { path: p })),
                    NewKind::TextDocument => {
                        FileOperations::create_empty_file(&dir, "New Text Document", ".txt")
                            .map(|p| (p.clone(), Step::CreatedFile { path: p }))
                    }
                    NewKind::Template(t) => {
                        FileOperations::create_from_template(t, &dir).map(|p| {
                            (
                                p.clone(),
                                Step::Copied {
                                    src: t.clone(),
                                    dest: p,
                                },
                            )
                        })
                    }
                };
                match result {
                    Ok((path, step)) => {
                        self.history.record("New document", vec![step]);
                        fs.load_directory();
                        self.refresh_list(fs);
                        self.selection.select_only(&path);
                        self.scroll_to_focus = true;
                        // Straight into renaming, with the name (not extension) selected.
                        self.dialog = Some(Dialog::Rename {
                            targets: vec![path.clone()],
                            text: display_name(&path),
                            error: None,
                            first_frame: true,
                        });
                    }
                    Err(e) => self.status(e),
                }
            }
            Action::OpenTerminal(dir) => {
                if let Err(e) = open_terminal(&dir) {
                    self.status(e);
                }
            }
            Action::RemoveBookmark(path)
                if self.dolphin_places.iter().any(|b| b.path == path)
                    && !self
                        .bookmarks_file
                        .as_ref()
                        .is_some_and(|f| bookmarks::load(f).iter().any(|b| b.path == path)) =>
            {
                self.status("This place comes from Dolphin's sidebar; remove it there");
            }
            Action::AddBookmark(path) | Action::RemoveBookmark(path) => {
                let adding = !self.is_bookmarked(&path);
                let label = display_name(&path);
                let result = match &self.bookmarks_file {
                    Some(file) => {
                        let bookmark = Bookmark {
                            label: label.clone(),
                            path: path.clone(),
                        };
                        bookmarks::update(
                            file,
                            adding.then_some(&bookmark),
                            (!adding).then_some(path.as_path()),
                        )
                    }
                    None => {
                        self.settings.bookmarks.retain(|(_, p)| *p != path);
                        if adding {
                            self.settings.bookmarks.push((label.clone(), path.clone()));
                        }
                        Ok(())
                    }
                };
                match result {
                    Ok(()) => self.status(if adding {
                        format!("Bookmarked {label}")
                    } else {
                        format!("Removed bookmark {label}")
                    }),
                    Err(e) => self.status(e),
                }
                self.menus_refreshed = None;
                self.refresh_menus();
            }
            Action::Home => {
                if let Some(home) = crate::file_system::home_dir() {
                    fs.set_path(home);
                }
            }
            Action::InvertSelection => self.selection.invert(&self.list.paths),
            Action::Zoom(step) => {
                self.zoom_accumulator += step;
                ctx.request_repaint();
            }
            Action::FocusAddress => {
                self.editing_address = true;
                self.focus_address = true;
            }
            Action::FocusSearch => self.focus_search = true,
            Action::TogglePane(Pane::Navigation) => {
                self.settings.show_nav_pane = !self.settings.show_nav_pane
            }
            Action::TogglePane(Pane::Preview) => {
                self.settings.show_preview_pane = !self.settings.show_preview_pane
            }
            Action::ShowShortcuts => self.dialog = Some(Dialog::Shortcuts),
            Action::ShowAbout => self.dialog = Some(Dialog::About),
            Action::ShowIndexSettings => self.dialog = Some(Dialog::IndexSettings),
            Action::MoreResults => self.index.load_more(),
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::ToggleCloudThumbnails => {
                self.settings.cloud_thumbnails = !self.settings.cloud_thumbnails;
            }
            Action::SwitchTab(i) => self.switch_tab(fs, i),
            Action::CycleTab(d) => {
                let n = self.tabs.len() as isize;
                let next = (self.active_tab as isize + d).rem_euclid(n) as usize;
                self.switch_tab(fs, next);
            }
            Action::Undo | Action::Redo if !self.jobs.is_empty() => {
                self.status("Wait for the current operation to finish");
            }
            Action::Undo if self.history.undo_blocked().is_some() => {
                let why = self.history.undo_blocked().unwrap_or_default();
                self.status(format!("Can't undo: {why}"));
            }
            Action::Undo => match self.history.take_undo() {
                Some(entry) => self.spawn_history(ctx, entry, true),
                None => self.status("Nothing to undo"),
            },
            Action::Redo => match self.history.take_redo() {
                Some(entry) => self.spawn_history(ctx, entry, false),
                None => self.status("Nothing to redo"),
            },
            Action::SetView(mode, size) => {
                let icon_size = size.unwrap_or(self.view.icon_size);
                self.set_view(ViewSettings {
                    mode,
                    icon_size,
                    ..self.view
                });
            }
            Action::Sort(by) => {
                let ascending = if self.view.sort_by == by {
                    !self.view.sort_ascending
                } else {
                    // Sort dates and sizes newest/largest first by default.
                    !matches!(by, SortBy::Modified | SortBy::Size)
                };
                self.set_view(ViewSettings {
                    sort_by: by,
                    sort_ascending: ascending,
                    ..self.view
                });
            }
            Action::ToggleHidden => {
                let show = !fs.is_showing_hidden();
                let repaint = ctx.clone();
                self.tree
                    .reload_all(show, move || repaint.request_repaint());
                fs.set_show_hidden(show);
                for tab in self.tabs.iter_mut().flatten() {
                    tab.fs.set_show_hidden(show);
                }
                self.settings.show_hidden = show;
            }
        }
    }

    /// Files dropped on the window: from our own drag (onto a folder, the
    /// sidebar or the background) or from another app.
    fn handle_drops(&mut self, ctx: &egui::Context, fs: &FileSystem) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let dest = self
            .drop_folder
            .take()
            .unwrap_or_else(|| fs.get_current_path().to_path_buf());
        // Dropping items back where they already are does nothing.
        let paths: Vec<PathBuf> = dropped
            .into_iter()
            .filter(|p| p.parent() != Some(dest.as_path()) && *p != dest)
            .collect();
        let Some(first) = paths.first() else { return };
        let mode = drop_mode(first, &dest, ctx.input(|i| i.modifiers));
        self.start_paste(ctx, paths, dest, mode);
    }

    /// Open a file in its default application.
    /// Installed apps and associations, read once.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn app_catalog(&self) -> &open_with::Catalog {
        self.apps.get_or_init(|| {
            if cfg!(test) {
                // Tests never read the real desktop (they set their own).
                open_with::Catalog::default()
            } else {
                open_with::Catalog::load(&open_with::Dirs::from_env())
            }
        })
    }

    /// Adds a file opened or folder visited to Recent (unless that's off).
    fn remember_recent(&mut self, path: &Path) {
        if self.settings.show_recent {
            recent::record(&mut self.settings.recent, path, recent::now());
        }
    }

    /// Loads what the Recent view lists: this app's history and, on Linux
    /// and FreeBSD, the desktop's shared recent files.
    fn load_recent(&mut self) {
        let desktop = if self.settings.show_recent {
            recent::desktop_recent()
        } else {
            Vec::new()
        };
        let own: &[(PathBuf, i64)] = if self.settings.show_recent {
            &self.settings.recent
        } else {
            &[]
        };
        self.recent_items = recent::merged(own, &desktop)
            .into_iter()
            .filter_map(FileItem::new)
            .collect();
        self.recent_version += 1;
    }

    fn open_external(&mut self, path: &Path) {
        #[cfg(test)]
        {
            self.opened.push(path.to_path_buf());
        }
        #[cfg(not(test))]
        if let Err(e) = open::that_detached(path) {
            self.status(format!("Couldn't open {}: {e}", display_name(path)));
        }
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    fn handle_mouse_buttons(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // Mouse side buttons, as in every Windows app.
        ctx.input(|i| {
            if i.pointer.button_pressed(PointerButton::Extra1) {
                actions.push(Action::Back);
            }
            if i.pointer.button_pressed(PointerButton::Extra2) {
                actions.push(Action::Forward);
            }
        });
    }

    /// `typing` is true if a text field or dialog had keyboard focus at any
    /// point this frame. Checking only the current state isn't enough: the
    /// Enter that confirms a rename also defocuses the text box, and must not
    /// then open the selected file as well.
    /// Opens what other apps asked to show ("Show in folder"), and comes
    /// to the front. Tests never claim the real session bus name.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn answer_file_manager1(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use crate::file_manager1::Request;
        #[cfg(not(test))]
        if self.file_manager1.is_none() {
            self.file_manager1 = Some(crate::file_manager1::start(ctx));
        }
        let Some(rx) = &self.file_manager1 else {
            return;
        };
        for request in rx.try_iter() {
            match request {
                Request::Folders(folders) => {
                    actions.extend(folders.into_iter().take(1).map(Action::Navigate));
                }
                Request::Items(items) => {
                    actions.extend(
                        items
                            .into_iter()
                            .take(1)
                            .map(|item| Action::OpenLocation(item, false)),
                    );
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn handle_keyboard(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>, typing: bool) {
        // These work even while typing in the address/search box.
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::ALT, Key::ArrowLeft) {
                actions.push(Action::Back);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowRight) {
                actions.push(Action::Forward);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowUp) {
                actions.push(Action::Up);
            }
            if i.consume_key(Modifiers::NONE, Key::F5) || i.consume_key(Modifiers::COMMAND, Key::R)
            {
                actions.push(Action::Refresh);
            }
            if i.consume_key(Modifiers::COMMAND, Key::L)
                || i.consume_key(Modifiers::ALT, Key::D)
                || i.consume_key(Modifiers::NONE, Key::F4)
            {
                self.editing_address = true;
                self.focus_address = true;
            }
            if i.consume_key(Modifiers::COMMAND, Key::F) || i.consume_key(Modifiers::NONE, Key::F3)
            {
                self.focus_search = true;
            }
            if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::N) {
                actions.push(Action::NewFolder);
            }
            if i.consume_key(Modifiers::COMMAND, Key::H) {
                actions.push(Action::ToggleHidden);
            }
            if i.consume_key(Modifiers::COMMAND, Key::Q) {
                actions.push(Action::Quit);
            }
            if i.consume_key(Modifiers::COMMAND, Key::D) {
                let here = self.shown_path.clone();
                actions.push(if self.bookmarks.iter().any(|b| b.path == here) {
                    Action::RemoveBookmark(here)
                } else {
                    Action::AddBookmark(here)
                });
            }
            if i.consume_key(Modifiers::NONE, Key::F1) {
                actions.push(Action::ShowShortcuts);
            }
            if i.consume_key(Modifiers::ALT, Key::Home) {
                actions.push(Action::Home);
            }
            if i.consume_key(Modifiers::COMMAND, Key::T) {
                actions.push(Action::NewTab(None, true));
            }
            if i.consume_key(Modifiers::COMMAND, Key::W) {
                actions.push(Action::CloseTab(self.active_tab));
            }
            if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab)
                || i.consume_key(Modifiers::COMMAND, Key::PageUp)
            {
                actions.push(Action::CycleTab(-1));
            }
            if i.consume_key(Modifiers::COMMAND, Key::Tab)
                || i.consume_key(Modifiers::COMMAND, Key::PageDown)
            {
                actions.push(Action::CycleTab(1));
            }
        });

        // Everything else only applies when no text field has focus, so that
        // Delete/Backspace/Enter while typing never touch files.
        if typing || ctx.egui_wants_keyboard_input() {
            return;
        }

        let cols = if self.view.mode == ViewMode::Details {
            1
        } else {
            self.grid.cols.max(1)
        } as isize;
        let page = ((self.viewport_height / self.grid.row_stride().max(1.0)).floor() as isize)
            .max(1)
            * cols;
        let mut moves: Vec<(isize, bool)> = Vec::new();
        let mut typed = String::new();
        let mut zoom = 0.0;
        let mut clear = false;

        ctx.input_mut(|i| {
            for event in &i.events {
                match event {
                    Event::Copy => actions.push(Action::Copy),
                    Event::Cut => actions.push(Action::Cut),
                    Event::Paste(text) => actions.push(Action::Paste(Some(text.clone()))),
                    Event::Text(t) if !i.modifiers.command && !i.modifiers.alt => typed.push_str(t),
                    _ => {}
                }
            }
            // Ctrl+V with an empty system clipboard produces no Paste event;
            // only then use the key itself, so one shortcut never pastes twice.
            let pasted = actions.iter().any(|a| matches!(a, Action::Paste(_)));
            if i.consume_key(Modifiers::COMMAND, Key::V) && !pasted {
                actions.push(Action::Paste(None));
            }

            for (mods, key, action) in [
                (
                    Modifiers::SHIFT,
                    Key::Delete,
                    Action::ConfirmDeletePermanently,
                ),
                (Modifiers::NONE, Key::Delete, Action::Trash),
                (Modifiers::NONE, Key::Backspace, Action::Back),
                (Modifiers::NONE, Key::F2, Action::Rename),
                (Modifiers::ALT, Key::Enter, Action::Properties),
                (Modifiers::NONE, Key::Enter, Action::OpenSelected),
                (
                    Modifiers::COMMAND | Modifiers::SHIFT,
                    Key::I,
                    Action::InvertSelection,
                ),
                (Modifiers::COMMAND, Key::A, Action::SelectAll),
                // Ctrl+Shift+Z before Ctrl+Z, so the shifted one isn't taken as undo.
                (Modifiers::COMMAND | Modifiers::SHIFT, Key::Z, Action::Redo),
                (Modifiers::COMMAND, Key::Z, Action::Undo),
                (Modifiers::COMMAND, Key::Y, Action::Redo),
                (
                    Modifiers::COMMAND | Modifiers::SHIFT,
                    Key::C,
                    Action::CopyPath,
                ),
                (
                    Modifiers::COMMAND | Modifiers::SHIFT,
                    Key::Num5,
                    Action::SetView(ViewMode::Tiles, None),
                ),
                (
                    Modifiers::COMMAND | Modifiers::SHIFT,
                    Key::Num6,
                    Action::SetView(ViewMode::Details, None),
                ),
            ] {
                if i.consume_key(mods, key) {
                    actions.push(action);
                }
            }
            clear = i.consume_key(Modifiers::NONE, Key::Escape);

            // View sizes: Ctrl+Shift+1..4, Ctrl +/- to step.
            let presets = [Key::Num1, Key::Num2, Key::Num3, Key::Num4];
            for (key, (_, size)) in presets.into_iter().zip(ICON_PRESETS) {
                if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, key) {
                    actions.push(Action::SetView(ViewMode::Icons, Some(size)));
                }
            }
            if i.consume_key(Modifiers::COMMAND, Key::Plus)
                || i.consume_key(Modifiers::COMMAND, Key::Equals)
            {
                zoom += 0.25;
            }
            if i.consume_key(Modifiers::COMMAND, Key::Minus) {
                zoom -= 0.25;
            }

            // Arrow-key navigation through the list/grid.
            for (key, delta) in [
                (Key::ArrowDown, cols),
                (Key::ArrowUp, -cols),
                (Key::ArrowRight, if cols > 1 { 1 } else { 0 }),
                (Key::ArrowLeft, if cols > 1 { -1 } else { 0 }),
                (Key::PageDown, page),
                (Key::PageUp, -page),
                (Key::Home, isize::MIN / 2),
                (Key::End, isize::MAX / 2),
            ] {
                // Shift first: consume_key matches logically, so a NONE pattern
                // would also swallow Shift+key and collapse the range.
                for (mods, extend) in [(Modifiers::SHIFT, true), (Modifiers::NONE, false)] {
                    if delta != 0 && i.consume_key(mods, key) {
                        moves.push((delta, extend));
                    }
                }
            }
        });

        if clear {
            self.selection.clear();
        }
        if zoom != 0.0 {
            self.zoom_accumulator += zoom;
            ctx.request_repaint(); // applied by the file area next frame
        }
        for (delta, extend) in moves {
            self.selection.move_focus(&self.list.paths, delta, extend);
            self.scroll_to_focus = true;
        }
        if !typed.is_empty() {
            self.type_ahead_search(&typed);
        }
    }

    /// Typing a name jumps to the first matching item.
    fn type_ahead_search(&mut self, typed: &str) {
        let (buffer, last) = &mut self.type_ahead;
        if last.elapsed() > Duration::from_millis(900) {
            buffer.clear();
        }
        *last = Instant::now();
        buffer.push_str(&typed.to_lowercase());
        let buffer = buffer.clone();
        let items = &self.list.items;
        let current = self
            .selection
            .focus()
            .and_then(|f| items.iter().position(|i| i.path == f));
        // Pressing the same letter repeatedly cycles through items starting with it.
        let first = buffer.chars().next().map(String::from).unwrap_or_default();
        let repeated = buffer.chars().count() > 1 && buffer.chars().all(|c| buffer.starts_with(c));
        let needle = if repeated { first } else { buffer.clone() };
        let start = match (repeated || buffer.chars().count() == 1, current) {
            (true, Some(c)) => c + 1,
            _ => 0,
        };
        let found = (0..items.len())
            .map(|k| (start + k) % items.len())
            .find(|&k| items[k].name.to_lowercase().starts_with(&needle));
        if let Some(index) = found {
            self.selection.move_focus_to(&self.list.paths, index, false);
            self.scroll_to_focus = true;
        }
    }

    /// Ctrl+mouse wheel (or trackpad pinch) resizes icons and walks through
    /// Details → Tiles → Icons.
    fn apply_zoom(&mut self, zoom_delta: f32) {
        if zoom_delta != 1.0 {
            self.zoom_accumulator += zoom_delta.ln();
        }
        if self.zoom_accumulator == 0.0 {
            return;
        }
        let step = self.zoom_accumulator;
        let mut view = self.view;
        match view.mode {
            ViewMode::Icons => {
                self.zoom_accumulator = 0.0;
                let size = view.icon_size * step.exp();
                if size < MIN_ICON_SIZE - 0.5 && step < 0.0 {
                    view.mode = ViewMode::Tiles;
                } else {
                    view.icon_size = size.clamp(MIN_ICON_SIZE, MAX_ICON_SIZE);
                }
            }
            // Discrete modes need a full "notch" before switching.
            _ if step.abs() < 0.2 => return,
            ViewMode::Tiles => {
                self.zoom_accumulator = 0.0;
                if step > 0.0 {
                    view.mode = ViewMode::Icons;
                    view.icon_size = MIN_ICON_SIZE;
                } else {
                    view.mode = ViewMode::Details;
                }
            }
            ViewMode::Details => {
                self.zoom_accumulator = 0.0;
                if step > 0.0 {
                    view.mode = ViewMode::Tiles;
                }
            }
        }
        self.set_view(view);
    }

    // ------------------------------------------------------------------
    // Toolbar
    // ------------------------------------------------------------------

    fn show_toolbar(&mut self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        egui::Frame::NONE
            .fill(pal().header)
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    self.speech.button(ui, "Voice");
                    ui.set_height(BUTTON_MIN_SIZE);
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let nav_button = |ui: &mut Ui, label: &str, enabled: bool, hint: &str| {
                        let btn = Button::new(RichText::new(label).size(FONT_SIZE_NORMAL))
                            .min_size(Vec2::new(BUTTON_MIN_SIZE, BUTTON_MIN_SIZE))
                            .corner_radius(CornerRadius::same(6));
                        ui.add_enabled(enabled, btn).on_hover_text(hint).clicked()
                    };
                    if nav_button(
                        ui,
                        "⬅",
                        self.special.is_some() || fs.can_go_back(),
                        "Back (Alt+Left, mouse back button)",
                    ) {
                        actions.push(Action::Back);
                    }
                    if nav_button(
                        ui,
                        "➡",
                        fs.can_go_forward(),
                        "Forward (Alt+Right, mouse forward button)",
                    ) {
                        actions.push(Action::Forward);
                    }
                    if nav_button(
                        ui,
                        "⬆",
                        self.special.is_none(),
                        "Up to parent folder (Alt+Up)",
                    ) {
                        actions.push(Action::Up);
                    }
                    if nav_button(ui, "⟳", true, "Refresh (F5)") {
                        actions.push(Action::Refresh);
                    }
                    ui.add_space(6.0);

                    let search_width = 220.0;
                    let address_width = (ui.available_width() - search_width - 16.0).max(200.0);
                    self.show_address_bar(ui, fs, address_width, actions);
                    ui.add_space(8.0);
                    self.show_search_box(ui, search_width);
                });
            });

        // Command bar: the essential file actions.
        egui::Frame::NONE
            .fill(pal().bg)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let has_sel = !self.selection.is_empty();
                    let cmd = |ui: &mut Ui, label: &str, enabled: bool, hint: &str| {
                        ui.add_enabled(
                            enabled,
                            Button::new(RichText::new(label).size(FONT_SIZE_SMALL))
                                .min_size(Vec2::new(0.0, 28.0)),
                        )
                        .on_hover_text(hint)
                        .on_disabled_hover_text(hint)
                        .clicked()
                    };

                    if cmd(ui, "📁 New folder", true, "New folder (Ctrl+Shift+N)") {
                        actions.push(Action::NewFolder);
                    }
                    ui.separator();
                    let busy = !self.jobs.is_empty();
                    let blocked = self.history.undo_blocked();
                    let undo_hint = match (self.history.undo_label(), &blocked) {
                        (Some(l), Some(why)) => format!("Can't undo {}: {why}", l.to_lowercase()),
                        (Some(l), None) => format!("Undo {} (Ctrl+Z)", l.to_lowercase()),
                        (None, _) => "Nothing to undo".to_string(),
                    };
                    if cmd(
                        ui,
                        "Undo",
                        self.history.undo_label().is_some() && blocked.is_none() && !busy,
                        &undo_hint,
                    ) {
                        actions.push(Action::Undo);
                    }
                    let redo_hint = match self.history.redo_label() {
                        Some(l) => format!("Redo {} (Ctrl+Y)", l.to_lowercase()),
                        None => "Nothing to redo".to_string(),
                    };
                    if cmd(
                        ui,
                        "Redo",
                        self.history.redo_label().is_some() && !busy,
                        &redo_hint,
                    ) {
                        actions.push(Action::Redo);
                    }
                    ui.separator();
                    if cmd(ui, "✂ Cut", has_sel, "Cut (Ctrl+X)") {
                        actions.push(Action::Cut);
                    }
                    if cmd(ui, "🗐 Copy", has_sel, "Copy (Ctrl+C)") {
                        actions.push(Action::Copy);
                    }
                    if cmd(ui, "📋 Paste", self.can_paste(), "Paste (Ctrl+V)") {
                        actions.push(Action::Paste(None));
                    }
                    if cmd(
                        ui,
                        "✏ Rename",
                        has_sel,
                        "Rename (F2). With several items selected, numbers them.",
                    ) {
                        actions.push(Action::Rename);
                    }
                    if cmd(
                        ui,
                        "🗑 Delete",
                        has_sel,
                        "Move to trash (Delete). Shift+Delete deletes permanently.",
                    ) {
                        actions.push(Action::Trash);
                    }
                    ui.separator();

                    let can_rotate = self.rotatable_selection().is_some();
                    let rotate_hint = if can_rotate {
                        ""
                    } else {
                        " — select one or more JPEG, PNG, BMP, TIFF or WebP images"
                    };
                    if cmd(
                        ui,
                        "⟲ Rotate left",
                        can_rotate,
                        &format!("Rotate the selected pictures 90° counter-clockwise{rotate_hint}"),
                    ) {
                        actions.push(Action::Rotate(Rotation::CounterClockwise));
                    }
                    if cmd(
                        ui,
                        "⟳ Rotate right",
                        can_rotate,
                        &format!("Rotate the selected pictures 90° clockwise{rotate_hint}"),
                    ) {
                        actions.push(Action::Rotate(Rotation::Clockwise));
                    }
                    ui.separator();

                    let view_label = match self.view.mode {
                        ViewMode::Details => "☰ Details".to_string(),
                        ViewMode::Tiles => "▦ Tiles".to_string(),
                        ViewMode::Icons => format!("⊞ {}", icon_preset_name(self.view.icon_size)),
                    };
                    ui.menu_button(
                        RichText::new(format!("{view_label} ⏷")).size(FONT_SIZE_SMALL),
                        |ui| {
                            self.view_menu(ui, actions);
                        },
                    )
                    .response
                    .on_hover_text(
                        "Change the view. Tip: hold Ctrl and use the mouse wheel to resize icons.",
                    );

                    let sort_label = format!(
                        "Sort: {} {}",
                        sort_name(self.view.sort_by),
                        if self.view.sort_ascending {
                            "⏶"
                        } else {
                            "⏷"
                        }
                    );
                    ui.menu_button(RichText::new(sort_label).size(FONT_SIZE_SMALL), |ui| {
                        self.sort_menu(ui, actions)
                    });

                    // Room left on this line of the (wrapping) bar.
                    let width = |ui: &Ui, texts: &[&str]| -> f32 {
                        let padding =
                            ui.spacing().button_padding.x * 2.0 + ui.spacing().item_spacing.x;
                        texts
                            .iter()
                            .map(|t| {
                                let font = FontId::proportional(FONT_SIZE_SMALL);
                                ui.ctx().fonts_mut(|f| {
                                    f.layout_no_wrap(t.to_string(), font, pal().text).size().x
                                }) + padding
                            })
                            .sum::<f32>()
                            + 8.0
                    };
                    let left = |ui: &Ui| ui.max_rect().right() - ui.cursor().left();
                    if left(ui) < width(ui, &["👁", "📂", "👻"]) {
                        ui.end_row();
                    }
                    // Too little room for the toggles' names: just their
                    // icons, named on hover.
                    let compact = left(ui)
                        < width(
                            ui,
                            &["👁 Preview pane", "📂 Navigation pane", "👻 Hidden items"],
                        );
                    let toggle = |icon: &str, name: &str| {
                        RichText::new(if compact {
                            icon.to_string()
                        } else {
                            format!("{icon} {name}")
                        })
                        .size(FONT_SIZE_SMALL)
                    };
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .selectable_label(
                                self.settings.show_preview_pane,
                                toggle("👁", "Preview pane"),
                            )
                            .on_hover_text("Preview pane")
                            .clicked()
                        {
                            self.settings.show_preview_pane = !self.settings.show_preview_pane;
                        }
                        if ui
                            .selectable_label(
                                self.settings.show_nav_pane,
                                toggle("📂", "Navigation pane"),
                            )
                            .on_hover_text("Navigation pane")
                            .clicked()
                        {
                            self.settings.show_nav_pane = !self.settings.show_nav_pane;
                        }
                        if ui
                            .selectable_label(fs.is_showing_hidden(), toggle("👻", "Hidden items"))
                            .on_hover_text("Show hidden files (Ctrl+H)")
                            .clicked()
                        {
                            actions.push(Action::ToggleHidden);
                        }
                    });
                });
            });
    }

    fn view_menu(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        for (name, size) in ICON_PRESETS {
            let selected =
                self.view.mode == ViewMode::Icons && (self.view.icon_size - size).abs() < 1.0;
            if ui.radio(selected, name).clicked() {
                actions.push(Action::SetView(ViewMode::Icons, Some(size)));
                ui.close();
            }
        }
        if ui
            .radio(self.view.mode == ViewMode::Tiles, "Tiles")
            .clicked()
        {
            actions.push(Action::SetView(ViewMode::Tiles, None));
            ui.close();
        }
        if ui
            .radio(self.view.mode == ViewMode::Details, "Details")
            .clicked()
        {
            actions.push(Action::SetView(ViewMode::Details, None));
            ui.close();
        }
    }

    fn sort_menu(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        for by in [SortBy::Name, SortBy::Modified, SortBy::Type, SortBy::Size] {
            if ui.radio(self.view.sort_by == by, sort_name(by)).clicked() {
                actions.push(Action::Sort(by));
                ui.close();
            }
        }
    }

    fn show_address_bar(
        &mut self,
        ui: &mut Ui,
        fs: &FileSystem,
        width: f32,
        actions: &mut Vec<Action>,
    ) {
        // Clicking the empty part of the bar switches to typing. Register it before
        // the folder names (where the bar was last frame) so they sit on top of it: egui gives a click to the
        // widget added last, and added after, it took every click on a
        // folder name.
        let bar_id = Id::unique("address_bar_bg");
        let last_rect = ui.ctx().data(|d| d.get_temp::<Rect>(bar_id));
        if let Some(rect) = last_rect {
            let bar = ui.interact(rect, bar_id, Sense::click());
            if bar.clicked() && !self.editing_address {
                self.editing_address = true;
                self.focus_address = true;
            }
            bar.on_hover_text("Click to type a path (Ctrl+L)");
        }
        let frame = egui::Frame::NONE
            .fill(pal().sidebar)
            .stroke(Stroke::new(
                1.5,
                if self.editing_address {
                    pal().accent
                } else {
                    pal().border
                },
            ))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(8, 6));
        let response = frame.show(ui, |ui| {
            ui.set_width(width - 16.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if self.editing_address {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.address_text)
                            .id(Id::unique("address_bar"))
                            .desired_width(f32::INFINITY)
                            .frame(egui::Frame::NONE)
                            .font(FontId::proportional(FONT_SIZE_NORMAL))
                            .text_color(pal().text),
                    );
                    if self.focus_address {
                        self.focus_address = false;
                        edit.request_focus();
                        select_all_text(ui.ctx(), edit.id, &self.address_text);
                    }
                    if edit.lost_focus() {
                        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                            let typed = expand_home(self.address_text.trim());
                            if let Some(uri) = network::as_network_uri(self.address_text.trim()) {
                                actions.push(Action::Connect(uri, None));
                            } else if typed.is_file() {
                                if let Some(parent) = typed.parent() {
                                    actions.push(Action::Navigate(parent.to_path_buf()));
                                }
                                actions.push(Action::OpenItem(typed));
                            } else {
                                actions.push(Action::Navigate(typed));
                            }
                        }
                        self.editing_address = false;
                        self.address_text = fs.get_current_path().to_string_lossy().to_string();
                    }
                } else if let Some(special) = self.special {
                    ui.label(
                        RichText::new(special.label())
                            .size(FONT_SIZE_NORMAL)
                            .color(pal().text),
                    );
                } else {
                    self.show_breadcrumbs(ui, fs.get_current_path(), actions);
                }
            });
        });
        let rect = response.response.rect;
        ui.ctx().data_mut(|d| d.insert_temp(bar_id, rect));
        if last_rect.is_none() {
            ui.ctx().request_repaint();
        }
    }

    fn show_breadcrumbs(&self, ui: &mut Ui, path: &Path, actions: &mut Vec<Action>) {
        let mut crumbs: Vec<(String, PathBuf)> = path
            .ancestors()
            .map(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.to_string_lossy().to_string());
                (name, p.to_path_buf())
            })
            .collect();
        crumbs.reverse();
        // Long paths: keep the root and the deepest folders.
        const MAX: usize = 6;
        let skip = crumbs.len().saturating_sub(MAX);
        for (i, (name, target)) in crumbs.iter().enumerate() {
            if i > 0 && i <= skip {
                if i == 1 {
                    ui.label(RichText::new("… ›").color(pal().text_secondary));
                }
                continue;
            }
            let is_last = i + 1 == crumbs.len();
            let text = RichText::new(name)
                .size(FONT_SIZE_NORMAL)
                .color(if is_last {
                    pal().text
                } else {
                    pal().text_secondary
                });
            if ui
                .add(Button::new(text).frame(false))
                .on_hover_text(target.to_string_lossy())
                .clicked()
                && !is_last
            {
                actions.push(Action::Navigate(target.clone()));
            }
            if !is_last {
                ui.label(RichText::new("›").color(pal().text_secondary));
            }
        }
    }

    fn show_search_box(&mut self, ui: &mut Ui, width: f32) {
        egui::Frame::NONE
            .fill(pal().sidebar)
            .stroke(Stroke::new(1.5, pal().border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_width(width - 16.0);
                ui.horizontal(|ui| {
                    // Where to search: this folder (no index needed), its
                    // subfolders, or everything indexed.
                    let scope = &mut self.settings.search_scope;
                    let picker = ui
                        .menu_button(
                            RichText::new("🔍⏷")
                                .size(FONT_SIZE_SMALL)
                                .color(pal().text_secondary),
                            |ui| {
                                for choice in [
                                    SearchScope::ThisFolder,
                                    SearchScope::Subfolders,
                                    SearchScope::Everywhere,
                                ] {
                                    if ui.radio_value(scope, choice, choice.label()).clicked() {
                                        self.focus_search = true;
                                        ui.close();
                                    }
                                }
                            },
                        )
                        .response;
                    picker.on_hover_text(format!("Searching: {}", scope.label().to_lowercase()));
                    let hint = match *scope {
                        SearchScope::ThisFolder => "Search this folder",
                        SearchScope::Subfolders => "Search subfolders",
                        SearchScope::Everywhere => "Search indexed locations",
                    };
                    // Keep this search in the navigation pane.
                    let text = self.search_text.trim().to_string();
                    if !text.is_empty() {
                        let saved = SavedSearch {
                            text,
                            scope: self.settings.search_scope,
                            folder: self.shown_path.clone(),
                        };
                        let kept = self.settings.saved_searches.contains(&saved);
                        let star = Button::new(
                            RichText::new(if kept { "★" } else { "☆" }).color(pal().accent),
                        )
                        .frame(false);
                        let resp = ui.add(star).on_hover_text(if kept {
                            "Saved (click to forget)"
                        } else {
                            "Save this search"
                        });
                        if resp.clicked() {
                            if kept {
                                self.settings.saved_searches.retain(|s| *s != saved);
                            } else {
                                self.settings.saved_searches.push(saved);
                            }
                        }
                    }
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.search_text)
                            // A fixed id: the ☆ appearing before it mustn't
                            // take the focus away mid-word.
                            .id(egui::Id::unique("search_box"))
                            .hint_text(RichText::new(hint).color(pal().text_secondary))
                            .desired_width(f32::INFINITY)
                            .frame(egui::Frame::NONE)
                            .font(FontId::proportional(FONT_SIZE_NORMAL))
                            .text_color(pal().text),
                    );
                    if self.focus_search {
                        self.focus_search = false;
                        edit.request_focus();
                    }
                    if edit.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                        self.search_text.clear();
                        edit.surrender_focus();
                    }
                });
            });
    }

    // ------------------------------------------------------------------
    // Navigation pane
    // ------------------------------------------------------------------

    /// This PC: every drive as a tile with its free space, then network
    /// and cloud places. Clicking one opens it.
    fn load_trash(&mut self) {
        match trash_bin::list() {
            Ok(items) => {
                self.trash
                    .selected
                    .retain(|id| items.iter().any(|i| i.id == *id));
                self.trash.items = items;
                self.trash.error = None;
            }
            Err(e) => {
                self.trash.items.clear();
                self.trash.error = Some(e);
            }
        }
    }

    fn show_trash(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.add_space(6.0);
        let page = &mut self.trash;
        let all: Vec<_> = page.items.iter().map(|i| i.id.clone()).collect();
        ui.horizontal_wrapped(|ui| {
            let some = !page.selected.is_empty();
            if ui
                .add_enabled(some, Button::new("↩ Restore"))
                .on_hover_text("Put the selected items back where they were")
                .clicked()
            {
                actions.push(Action::RestoreFromTrash(page.selected.clone()));
            }
            if ui
                .add_enabled(!all.is_empty(), Button::new("↩ Restore all"))
                .clicked()
            {
                actions.push(Action::RestoreFromTrash(all.clone()));
            }
            if ui
                .add_enabled(some, Button::new("❌ Delete"))
                .on_hover_text("Delete the selected items for good")
                .clicked()
            {
                actions.push(Action::ConfirmPurge(page.selected.clone()));
            }
            if ui
                .add_enabled(
                    !all.is_empty(),
                    Button::new(format!("🧹 Empty {}", TRASH_LABEL.trim_start_matches("🗑 "))),
                )
                .clicked()
            {
                actions.push(Action::ConfirmPurge(all.clone()));
            }
        });
        ui.add_space(6.0);
        if let Some(err) = &page.error {
            ui.label(RichText::new(err).color(pal().error));
            return;
        }
        if page.items.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(format!(
                        "The {} is empty.",
                        TRASH_LABEL.trim_start_matches("🗑 ")
                    ))
                    .color(pal().text_secondary),
                )
            });
            return;
        }
        let multi = ui.input(|i| i.modifiers.command || i.modifiers.shift);
        ScrollArea::both()
            .id_salt("trash_scroll")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                egui::Grid::new("trash_items")
                    .num_columns(3)
                    .striped(true)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        for h in ["Name", "Original location", "Date deleted"] {
                            ui.label(RichText::new(h).color(pal().text_secondary).strong());
                        }
                        ui.end_row();
                        for item in &page.items {
                            let selected = page.selected.contains(&item.id);
                            let resp = ui.selectable_label(selected, &item.name);
                            if resp.clicked() {
                                if !multi {
                                    page.selected.clear();
                                }
                                if selected && multi {
                                    page.selected.retain(|id| *id != item.id);
                                } else if !page.selected.contains(&item.id) {
                                    page.selected.push(item.id.clone());
                                }
                            }
                            if resp.double_clicked() {
                                actions.push(Action::RestoreFromTrash(vec![item.id.clone()]));
                            }
                            resp.context_menu(|ui| {
                                if ui.button("↩ Restore").clicked() {
                                    actions.push(Action::RestoreFromTrash(vec![item.id.clone()]));
                                    ui.close();
                                }
                                if ui.button("❌ Delete permanently").clicked() {
                                    actions.push(Action::ConfirmPurge(vec![item.id.clone()]));
                                    ui.close();
                                }
                            });
                            ui.label(item.from.to_string_lossy());
                            ui.label(
                                chrono::DateTime::from_timestamp(item.deleted, 0)
                                    .map(|t| {
                                        t.with_timezone(&chrono::Local)
                                            .format("%Y-%m-%d %H:%M")
                                            .to_string()
                                    })
                                    .unwrap_or_default(),
                            );
                            ui.end_row();
                        }
                    });
            });
    }

    fn show_this_pc(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let drives = self.drives.lock().unwrap().clone();
        let places = self.places.lock().unwrap().clone();
        ScrollArea::vertical()
            .id_salt("this_pc_scroll")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                ui.add_space(8.0);
                // Responsive: tiles fill the width, as many per row as fit.
                let size = place_tile_size(ui.available_width() - 8.0);
                section_heading(ui, "Devices and drives");
                if drives.is_empty() {
                    ui.label(RichText::new("Looking for drives…").color(pal().text_secondary));
                }
                ui.with_layout(
                    Layout::left_to_right(Align::Min).with_main_wrap(true),
                    |ui| {
                        ui.spacing_mut().item_spacing = Vec2::splat(PLACE_TILE_GAP);
                        for drive in &drives {
                            let icon = match drive.drive_type {
                                DriveType::Removable => "💾",
                                DriveType::Network => "🌐",
                                DriveType::CDRom => "💿",
                                _ => "🖴",
                            };
                            let usage = match (drive.total_space, drive.free_space) {
                                (Some(total), Some(free)) if total > 0 => Some((
                                    (total - free.min(total)) as f32 / total as f32,
                                    format!(
                                        "{} free of {}",
                                        format_bytes(free),
                                        format_bytes(total)
                                    ),
                                )),
                                _ => None,
                            };
                            let resp = place_tile(ui, size, icon, &drive.label, &drive.path, usage);
                            resp.context_menu(|ui| {
                                if ui.button("🗂 Open in new tab").clicked() {
                                    actions.push(Action::NewTab(Some(drive.path.clone()), true));
                                    ui.close();
                                }
                                if ui.button("🔗 Copy path").clicked() {
                                    ui.ctx().copy_text(drive.path.to_string_lossy().to_string());
                                    ui.close();
                                }
                                if eject::ejectable(drive) && ui.button("⏏ Eject").clicked() {
                                    actions.push(Action::Eject(drive.path.clone()));
                                    ui.close();
                                }
                                if cfg!(windows) {
                                    ui.separator();
                                    for operation in [DiskOperation::Unlock, DiskOperation::Lock] {
                                        if ui.button(operation.label()).clicked() {
                                            actions.push(Action::ShowDisks(
                                                Some(drive.path.clone()),
                                                operation,
                                            ));
                                            ui.close();
                                        }
                                    }
                                }
                            });
                            if resp.clicked() {
                                actions.push(Action::Navigate(drive.path.clone()));
                            }
                        }
                    },
                );
                if !places.is_empty() {
                    ui.add_space(16.0);
                    section_heading(ui, "Network & Cloud");
                    ui.with_layout(
                        Layout::left_to_right(Align::Min).with_main_wrap(true),
                        |ui| {
                            ui.spacing_mut().item_spacing = Vec2::splat(PLACE_TILE_GAP);
                            for place in &places {
                                let icon = match place.kind {
                                    PlaceKind::Network => "🖧",
                                    PlaceKind::CloudStream | PlaceKind::CloudSync => "☁",
                                    PlaceKind::Device => "📱",
                                };
                                let resp =
                                    place_tile(ui, size, icon, &place.label, &place.path, None);
                                resp.context_menu(|ui| {
                                    if ui.button("🗂 Open in new tab").clicked() {
                                        actions
                                            .push(Action::NewTab(Some(place.path.clone()), true));
                                        ui.close();
                                    }
                                });
                                if resp.clicked() {
                                    actions.push(Action::Navigate(place.path.clone()));
                                }
                            }
                        },
                    );
                }
            });
    }

    /// A folder in the navigation pane's tree: its row (click opens it,
    /// the arrow expands it), its menu, and its subfolders when expanded.
    #[allow(clippy::too_many_arguments)]
    fn tree_folder(
        &mut self,
        ui: &mut Ui,
        fs: &FileSystem,
        depth: usize,
        icon: &str,
        name: &str,
        path: &Path,
        actions: &mut Vec<Action>,
        drop_target: &mut Option<(Rect, PathBuf)>,
    ) {
        let current = self.special.is_none() && fs.get_current_path() == path;
        let resp = tree_row(ui, depth, icon, name, current);
        if current && self.tree_scroll {
            resp.scroll_to_me(None);
            self.tree_scroll = false;
        }
        // An empty folder, once listed, has nothing to expand.
        let empty = matches!(self.tree.children(path), Some(folder_tree::Children::Loaded(v)) if v.is_empty());
        let expanded = self.tree.is_expanded(path);
        if !empty
            && tree_arrow(
                ui,
                resp.rect,
                resp.rect.left() + TREE_INDENT * depth as f32,
                expanded,
                name,
            )
            .clicked()
        {
            actions.push(Action::ToggleTree(path.to_path_buf()));
        }
        if self.drag_hover(ui, resp.rect) {
            *drop_target = Some((resp.rect, path.to_path_buf()));
        }
        let resp = resp.on_hover_text(path.to_string_lossy());
        if resp.clicked() {
            actions.push(Action::Navigate(path.to_path_buf()));
        } else if resp.middle_clicked() {
            actions.push(Action::NewTab(Some(path.to_path_buf()), false));
        }
        let pinned = self.settings.pinned.iter().any(|p| p == path);
        resp.context_menu(|ui| {
            if ui.button("🗂 Open in new tab").clicked() {
                actions.push(Action::NewTab(Some(path.to_path_buf()), true));
                ui.close();
            }
            if ui.button("🔗 Copy path").clicked() {
                ui.ctx().copy_text(path.to_string_lossy().to_string());
                ui.close();
            }
            ui.separator();
            let (label, action) = if pinned {
                (
                    "📌 Unpin from Quick Access",
                    Action::Unpin(path.to_path_buf()),
                )
            } else {
                ("📌 Pin to Quick Access", Action::Pin(path.to_path_buf()))
            };
            if ui.button(label).clicked() {
                actions.push(action);
                ui.close();
            }
            if pinned && depth == 0 {
                let pins = &self.settings.pinned;
                let at = pins.iter().position(|p| p == path).unwrap_or(0);
                if at > 0 && ui.button("⬆ Move up").clicked() {
                    actions.push(Action::MovePin(path.to_path_buf(), -1));
                    ui.close();
                }
                if at + 1 < pins.len() && ui.button("⬇ Move down").clicked() {
                    actions.push(Action::MovePin(path.to_path_buf(), 1));
                    ui.close();
                }
            }
        });
        if expanded && !empty {
            self.tree_children(ui, fs, depth + 1, path, actions, drop_target);
        }
    }

    /// The subfolders of an expanded folder (or that they're loading).
    fn tree_children(
        &mut self,
        ui: &mut Ui,
        fs: &FileSystem,
        depth: usize,
        path: &Path,
        actions: &mut Vec<Action>,
        drop_target: &mut Option<(Rect, PathBuf)>,
    ) {
        let note = |ui: &mut Ui, text: &str| {
            ui.horizontal(|ui| {
                ui.add_space(TREE_INDENT * (depth as f32 + 1.0) + 26.0);
                ui.label(
                    RichText::new(text)
                        .color(pal().text_secondary)
                        .size(FONT_SIZE_SMALL),
                );
            });
        };
        match self.tree.children(path) {
            None | Some(folder_tree::Children::Loading) => note(ui, "…"),
            Some(folder_tree::Children::Failed) => note(ui, "Can't be read"),
            Some(folder_tree::Children::Loaded(children)) => {
                // Deep trees stop expanding rather than run off the pane.
                if depth > 24 {
                    return;
                }
                for (name, child) in children {
                    self.tree_folder(ui, fs, depth, "📁", &name, &child, actions, drop_target);
                }
            }
        }
    }

    fn show_navigation_pane(&mut self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        ui.add_space(10.0);
        ScrollArea::vertical()
            .id_salt("nav_pane_scroll")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let mut drop_target: Option<(Rect, PathBuf)> = None;
                ui.label(
                    RichText::new("⭐ Quick Access")
                        .color(pal().text)
                        .size(FONT_SIZE_NORMAL)
                        .strong(),
                );
                ui.add_space(6.0);
                if self.settings.show_recent {
                    let resp = nav_item(ui, "🕘", "Recent", self.special == Some(Special::Recent))
                        .on_hover_text("Files you opened and folders you visited recently");
                    if resp.clicked() {
                        actions.push(Action::ShowSpecial(Special::Recent));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("🧹 Clear recent").clicked() {
                            actions.push(Action::ClearRecent);
                            ui.close();
                        }
                    });
                }
                if trash_bin::SUPPORTED {
                    let label = TRASH_LABEL.trim_start_matches("🗑 ");
                    let resp = nav_item(ui, "🗑", label, self.special == Some(Special::Trash))
                        .on_hover_text("Deleted files, to restore or delete for good");
                    if resp.clicked() {
                        actions.push(Action::ShowSpecial(Special::Trash));
                    }
                }
                for (name, path) in self.quick_access.clone() {
                    let icon = match name.as_str() {
                        "Desktop" => "🖥",
                        "Downloads" => "⬇",
                        "Documents" => "📄",
                        "Pictures" => "🖼",
                        "Music" => "🎵",
                        "Videos" => "🎬",
                        "Home" => "🏠",
                        _ => "📁",
                    };
                    let (path, name) = (path.clone(), name.clone());
                    self.tree_folder(ui, fs, 0, icon, &name, &path, actions, &mut drop_target);
                }
                for path in self.settings.pinned.clone() {
                    self.tree_folder(
                        ui,
                        fs,
                        0,
                        "📌",
                        &display_name(&path),
                        &path,
                        actions,
                        &mut drop_target,
                    );
                }

                if !self.settings.saved_searches.is_empty() {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("🔎 Saved searches")
                            .color(pal().text)
                            .size(FONT_SIZE_NORMAL)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    for saved in self.settings.saved_searches.clone() {
                        let current = self.search_text.trim() == saved.text
                            && self.settings.search_scope == saved.scope
                            && (saved.scope == SearchScope::Everywhere
                                || self.shown_path == saved.folder);
                        let resp = nav_item(ui, "🔎", &saved.label(), current);
                        if resp.clicked() {
                            actions.push(Action::RunSavedSearch(saved.clone()));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("✖ Remove saved search").clicked() {
                                actions.push(Action::ForgetSavedSearch(saved.clone()));
                                ui.close();
                            }
                        });
                    }
                    ui.add_space(8.0);
                }
                if !self.bookmarks.is_empty() {
                    ui.add_space(20.0);
                    ui.label(
                        RichText::new("🔖 Bookmarks")
                            .color(pal().text)
                            .size(FONT_SIZE_NORMAL)
                            .strong(),
                    );
                    ui.add_space(6.0);
                    for b in self.bookmarks.clone() {
                        let resp = nav_item(ui, "📁", &b.label, fs.get_current_path() == b.path);
                        if self.drag_hover(ui, resp.rect) {
                            drop_target = Some((resp.rect, b.path.clone()));
                        }
                        let resp = resp.on_hover_text(b.path.to_string_lossy());
                        if resp.clicked() {
                            actions.push(Action::Navigate(b.path.clone()));
                        } else if resp.middle_clicked() {
                            actions.push(Action::NewTab(Some(b.path.clone()), false));
                        }
                        resp.context_menu(|ui| {
                            if ui.button("🗂 Open in new tab").clicked() {
                                actions.push(Action::NewTab(Some(b.path.clone()), true));
                                ui.close();
                            }
                            if ui.button("☆ Remove Bookmark").clicked() {
                                actions.push(Action::RemoveBookmark(b.path.clone()));
                                ui.close();
                            }
                        });
                    }
                }

                ui.add_space(20.0);
                // A collapsible heading that opens all drives at once.
                let this_pc = self.special == Some(Special::ThisPc);
                let heading = ui
                    .add(
                        egui::Label::new(
                            RichText::new(Special::ThisPc.label())
                                .color(if this_pc { pal().accent } else { pal().text })
                                .size(FONT_SIZE_NORMAL)
                                .strong(),
                        )
                        .sense(Sense::click()),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("Show all drives");
                if heading.clicked() {
                    actions.push(Action::ShowSpecial(Special::ThisPc));
                }
                ui.add_space(6.0);
                let drives = self.drives.lock().unwrap().clone();
                for drive in &drives {
                    let resp = drive_item(
                        ui,
                        drive,
                        self.special.is_none() && fs.get_current_path() == drive.path,
                        TREE_INDENT,
                    );
                    let expanded = self.tree.is_expanded(&drive.path);
                    if tree_arrow(
                        ui,
                        resp.rect,
                        resp.rect.left() + 2.0,
                        expanded,
                        &drive.label,
                    )
                    .clicked()
                    {
                        actions.push(Action::ToggleTree(drive.path.clone()));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("🗂 Open in new tab").clicked() {
                            actions.push(Action::NewTab(Some(drive.path.clone()), true));
                            ui.close();
                        }
                        if ui.button("🔗 Copy path").clicked() {
                            ui.ctx().copy_text(drive.path.to_string_lossy().to_string());
                            ui.close();
                        }
                        if eject::ejectable(drive) && ui.button("⏏ Eject").clicked() {
                            actions.push(Action::Eject(drive.path.clone()));
                            ui.close();
                        }
                        if cfg!(windows) {
                            ui.separator();
                            for operation in [DiskOperation::Unlock, DiskOperation::Lock] {
                                if ui.button(operation.label()).clicked() {
                                    actions.push(Action::ShowDisks(
                                        Some(drive.path.clone()),
                                        operation,
                                    ));
                                    ui.close();
                                }
                            }
                        }
                    });
                    if resp.middle_clicked() {
                        actions.push(Action::NewTab(Some(drive.path.clone()), false));
                    }
                    if self.drag_hover(ui, resp.rect) {
                        drop_target = Some((resp.rect, drive.path.clone()));
                    }
                    if resp.clicked() {
                        actions.push(Action::Navigate(drive.path.clone()));
                    }
                    if expanded {
                        self.tree_children(ui, fs, 1, &drive.path, actions, &mut drop_target);
                    }
                }
                ui.add_space(20.0);
                ui.label(
                    RichText::new("🖧 Network & Cloud")
                        .color(pal().text)
                        .size(FONT_SIZE_NORMAL)
                        .strong(),
                );
                ui.add_space(6.0);
                let places = self.places.lock().unwrap().clone();
                for place in &places {
                    let icon = match place.kind {
                        PlaceKind::Network => "🖧",
                        PlaceKind::CloudStream | PlaceKind::CloudSync => "☁",
                        PlaceKind::Device => "📱",
                    };
                    let resp =
                        nav_item(ui, icon, &place.label, fs.get_current_path() == place.path);
                    if self.drag_hover(ui, resp.rect) {
                        drop_target = Some((resp.rect, place.path.clone()));
                    }
                    let mut tip = place.path.to_string_lossy().to_string();
                    if let Some(note) = &place.note {
                        tip = format!("{tip}\n\n{note}");
                    }
                    let resp = resp.on_hover_text(tip);
                    if resp.clicked() {
                        actions.push(Action::Navigate(place.path.clone()));
                    } else if resp.middle_clicked() {
                        actions.push(Action::NewTab(Some(place.path.clone()), false));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("🗂 Open in new tab").clicked() {
                            actions.push(Action::NewTab(Some(place.path.clone()), true));
                            ui.close();
                        }
                        if ui.button("🔗 Copy path").clicked() {
                            ui.ctx().copy_text(place.path.to_string_lossy().to_string());
                            ui.close();
                        }
                        if place.unmount.is_some() {
                            ui.separator();
                            if ui.button("⏏ Disconnect").clicked() {
                                actions.push(Action::Disconnect(place.clone()));
                                ui.close();
                            }
                        }
                    });
                }
                if nav_item(ui, "+", "Connect to server…", false)
                    .on_hover_text(
                        "Connect to a network share: smb://, sftp://, \\\\server\\share …",
                    )
                    .clicked()
                {
                    actions.push(Action::ShowConnect);
                }
                if let Some((rect, path)) = drop_target {
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(6),
                        Stroke::new(2.0, pal().accent),
                        StrokeKind::Inside,
                    );
                    self.drop_folder = Some(path);
                }
            });
    }

    // ------------------------------------------------------------------
    // File area
    // ------------------------------------------------------------------

    fn show_file_area(&mut self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        match self.special {
            Some(Special::ThisPc) => return self.show_this_pc(ui, actions),
            Some(Special::Trash) => return self.show_trash(ui, actions),
            Some(Special::Recent) if self.list.items.is_empty() => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(if self.settings.show_recent {
                            "Files you open and folders you visit will show here."
                        } else {
                            "Recent files are turned off (View → Show Recent Files and Folders)."
                        })
                        .color(pal().text_secondary),
                    )
                });
                return;
            }
            _ => {}
        }
        // Ctrl+wheel / pinch over the file list resizes icons.
        if ui.rect_contains_pointer(ui.max_rect()) && self.dialog.is_none() {
            let zoom = ui.input(|i| i.zoom_delta());
            self.apply_zoom(zoom);
        } else {
            self.apply_zoom(1.0);
        }

        if let Some(err) = fs.load_error() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(err).color(pal().error).size(FONT_SIZE_NORMAL))
            });
            return;
        }
        if fs.is_loading() && self.list.items.is_empty() {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading…").color(pal().text_secondary));
            });
            return;
        }

        if self.view.mode == ViewMode::Details {
            self.show_column_headers(ui, actions);
        }

        let n = self.list.items.len();
        let avail = ui.available_width();
        self.grid = match self.view.mode {
            ViewMode::Details => Grid {
                cols: 1,
                cell: Vec2::new(avail - 12.0, DETAILS_ROW_HEIGHT),
                spacing: 0.0,
            },
            ViewMode::Tiles => {
                let cols = (((avail - 12.0 + GRID_SPACING) / (TILE_SIZE.x + GRID_SPACING)).floor()
                    as usize)
                    .max(1);
                Grid {
                    cols,
                    cell: TILE_SIZE,
                    spacing: GRID_SPACING,
                }
            }
            ViewMode::Icons => {
                let s = self.view.icon_size;
                let cell = Vec2::new(
                    (s + 24.0).max(96.0),
                    s + 16.0 + 2.0 * FONT_SIZE_SMALL * 1.35,
                );
                let cols = (((avail - 12.0 + GRID_SPACING) / (cell.x + GRID_SPACING)).floor()
                    as usize)
                    .max(1);
                Grid {
                    cols,
                    cell,
                    spacing: GRID_SPACING,
                }
            }
        };
        let grid = self.grid;
        let rows = n.div_ceil(grid.cols);

        // Background: click empty space to deselect, drag to rubber-band select,
        // right-click for the folder menu. Items drawn later sit on top of it.
        let area = ui.available_rect_before_wrap();
        let bg = ui.interact(
            area,
            ui.scope_id().with("file_area_bg"),
            Sense::click_and_drag(),
        );

        let mut scroll = ScrollArea::vertical()
            .id_salt(("files", self.shown_path.clone()))
            .auto_shrink([false; 2])
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: egui::scroll_area::DragScroll::Never,
                ..egui::scroll_area::ScrollSource::ALL
            });
        if self.scroll_to_focus {
            self.scroll_to_focus = false;
            if let Some(index) = self
                .selection
                .focus()
                .and_then(|f| self.list.paths.iter().position(|p| p == f))
            {
                let r = grid.rect_of(index);
                if r.top() < self.scroll_offset {
                    scroll = scroll.vertical_scroll_offset(r.top());
                } else if r.bottom() > self.scroll_offset + self.viewport_height {
                    scroll = scroll.vertical_scroll_offset(r.bottom() - self.viewport_height + 4.0);
                }
            }
        }

        let mut origin = Pos2::ZERO;
        let out = scroll.show_rows(ui, grid.cell.y, rows, |ui, range| {
            ui.spacing_mut().item_spacing = Vec2::splat(grid.spacing);
            // Top-left of the whole (virtual) content in screen space.
            origin = ui.min_rect().min - Vec2::new(0.0, range.start as f32 * grid.row_stride());
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = grid.spacing;
                    for col in 0..grid.cols {
                        let index = row * grid.cols + col;
                        if index >= n {
                            break;
                        }
                        self.show_item(ui, index, grid, actions);
                    }
                });
            }
        });
        self.scroll_offset = out.state.clamped_offset().y;
        self.viewport_height = out.inner_rect.height();

        if n == 0 {
            let msg = if self.search_text.is_empty() {
                "This folder is empty."
            } else if !self.searching_index() {
                "No items match your search."
            } else if let Some(error) = &self.index.results.error {
                error.as_str()
            } else if self.index.results.searching {
                "Searching…"
            } else {
                "No indexed items match your search."
            };
            ui.put(
                area,
                egui::Label::new(RichText::new(msg).color(pal().text_secondary)),
            );
        }

        self.handle_background(ui, &bg, origin, out.inner_rect, grid, actions);
    }

    fn handle_background(
        &mut self,
        ui: &Ui,
        bg: &egui::Response,
        origin: Pos2,
        viewport: Rect,
        grid: Grid,
        actions: &mut Vec<Action>,
    ) {
        let mods = ui.input(|i| i.modifiers);
        if bg.clicked() && !mods.command && !mods.shift {
            self.selection.clear();
        }
        if bg.secondary_clicked() {
            self.selection.clear();
        }
        bg.context_menu(|ui| self.background_menu(ui, actions));

        let hit_item = |pos: Pos2| -> bool {
            let local = pos - origin;
            let col = (local.x / (grid.cell.x + grid.spacing)).floor();
            let row = (local.y / grid.row_stride()).floor();
            if col < 0.0 || row < 0.0 || col as usize >= grid.cols {
                return false;
            }
            let index = row as usize * grid.cols + col as usize;
            index < self.list.items.len() && grid.rect_of(index).contains(Pos2::ZERO + local)
        };

        if bg.drag_started_by(PointerButton::Primary) {
            if let Some(pos) = bg.interact_pointer_pos() {
                if !hit_item(pos) {
                    let base = if mods.command {
                        self.selection.paths().cloned().collect()
                    } else {
                        Vec::new()
                    };
                    self.marquee = Some(Marquee {
                        start: Pos2::ZERO + (pos - origin),
                        additive: mods.command,
                        base,
                    });
                }
            }
        }
        if let (Some(m), Some(pos)) = (
            &self.marquee,
            bg.interact_pointer_pos().or(ui.ctx().pointer_latest_pos()),
        ) {
            let current = Pos2::ZERO + (pos - origin);
            let band = Rect::from_two_pos(m.start, current);
            let mut chosen: Vec<PathBuf> = (0..self.list.items.len())
                .filter(|&i| grid.rect_of(i).intersects(band))
                .map(|i| self.list.paths[i].clone())
                .collect();
            if m.additive {
                chosen.extend(m.base.iter().cloned());
            }
            self.selection.set(chosen);

            let screen = band.translate(origin.to_vec2());
            let painter = ui.painter().with_clip_rect(viewport);
            painter.rect_filled(
                screen,
                CornerRadius::ZERO,
                pal().selected.gamma_multiply(0.25),
            );
            painter.rect_stroke(
                screen,
                CornerRadius::ZERO,
                Stroke::new(1.0, pal().accent),
                StrokeKind::Inside,
            );

            // Auto-scroll when dragging past the top or bottom edge.
            let edge = 30.0;
            if pos.y > viewport.bottom() - edge || pos.y < viewport.top() + edge {
                let dir = if pos.y > viewport.bottom() - edge {
                    1.0
                } else {
                    -1.0
                };
                ui.scroll_with_delta(Vec2::new(0.0, -dir * 12.0));
                ui.ctx().request_repaint();
            }
        }
        if bg.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
            self.marquee = None;
        }
    }

    fn show_column_headers(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (name_w, date_w, type_w, size_w) = details_columns(ui.available_width() - 12.0);
        egui::Frame::NONE.fill(pal().header).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_height(30.0);
                ui.spacing_mut().item_spacing.x = 0.0;
                for (label, by, width, align) in [
                    ("Name", SortBy::Name, name_w, Align::Min),
                    ("Date modified", SortBy::Modified, date_w, Align::Min),
                    ("Type", SortBy::Type, type_w, Align::Min),
                    ("Size", SortBy::Size, size_w, Align::Max),
                ] {
                    let arrow = if self.view.sort_by == by {
                        if self.view.sort_ascending {
                            " ⏶"
                        } else {
                            " ⏷"
                        }
                    } else {
                        ""
                    };
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(width, 30.0), Sense::click());
                    if resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::ZERO, pal().hover);
                    }
                    let (anchor, pos) = match align {
                        Align::Max => (
                            Align2::RIGHT_CENTER,
                            rect.right_center() - Vec2::new(12.0, 0.0),
                        ),
                        _ => (
                            Align2::LEFT_CENTER,
                            rect.left_center() + Vec2::new(10.0, 0.0),
                        ),
                    };
                    ui.painter().text(
                        pos,
                        anchor,
                        format!("{label}{arrow}"),
                        FontId::proportional(FONT_SIZE_SMALL),
                        pal().text,
                    );
                    if resp
                        .on_hover_text(format!("Sort by {}", label.to_lowercase()))
                        .clicked()
                    {
                        actions.push(Action::Sort(by));
                    }
                }
            });
        });
        ui.add_space(2.0);
    }

    fn show_item(&mut self, ui: &mut Ui, index: usize, grid: Grid, actions: &mut Vec<Action>) {
        let item = &self.list.items[index];
        let path = item.path.clone();
        let (rect, response) = ui.allocate_exact_size(grid.cell, Sense::click_and_drag());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let selected = self.selection.contains(&path);
        let is_drop_target = item.is_dir && !selected && self.drag_hover(ui, rect);
        if is_drop_target {
            self.drop_folder = Some(path.clone());
        }
        let focused = self.selection.focus() == Some(path.as_path());
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::ListBoxOption,
                true,
                selected,
                &self.list.items[index].name,
            )
        });

        let bg = if selected {
            if response.hovered() {
                pal().selected_hover
            } else {
                pal().selected
            }
        } else if response.hovered() {
            pal().hover
        } else if self.view.mode == ViewMode::Tiles {
            pal().sidebar
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(4), bg);
        if focused && self.selection.len() > 1 {
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(1.0, pal().text_secondary),
                StrokeKind::Inside,
            );
        }
        if self.cut_pending(&path) {
            // Dim items that were cut.
            painter.rect_filled(rect, CornerRadius::same(4), pal().bg.gamma_multiply(0.5));
        }
        if is_drop_target {
            painter.rect_filled(
                rect,
                CornerRadius::same(4),
                pal().selected.gamma_multiply(0.35),
            );
            painter.rect_stroke(
                rect,
                CornerRadius::same(4),
                Stroke::new(2.0, pal().accent),
                StrokeKind::Inside,
            );
        }

        let item = self.list.items[index].clone();
        match self.view.mode {
            ViewMode::Details => self.paint_details_row(ui, rect, &item, selected),
            ViewMode::Tiles => self.paint_tile(ui, rect, &item, selected),
            ViewMode::Icons => self.paint_icon_cell(ui, rect, &item, selected),
        }

        // Check box: on hover, or ticked while selected.
        let checkbox = self.settings.item_checkboxes.then(|| {
            let size = 18.0;
            let at = match self.view.mode {
                ViewMode::Details => Pos2::new(rect.left() + 2.0, rect.center().y - size / 2.0),
                _ => rect.left_top() + Vec2::splat(4.0),
            };
            Rect::from_min_size(at, Vec2::splat(size))
        });
        if let Some(b) = checkbox.filter(|_| selected || response.hovered()) {
            let painter = ui.painter();
            painter.rect_filled(
                b,
                CornerRadius::same(3),
                if selected { pal().accent } else { pal().bg },
            );
            painter.rect_stroke(
                b,
                CornerRadius::same(3),
                Stroke::new(
                    1.5,
                    if selected {
                        pal().accent
                    } else {
                        pal().text_secondary
                    },
                ),
                StrokeKind::Inside,
            );
            if selected {
                // A tick drawn as lines (no font has to have it).
                let at = |x: f32, y: f32| b.min + Vec2::new(b.width() * x, b.height() * y);
                painter.add(egui::Shape::line(
                    vec![at(0.24, 0.52), at(0.42, 0.72), at(0.78, 0.3)],
                    Stroke::new(2.0, pal().on_selected),
                ));
            }
        }
        let on_checkbox = checkbox.is_some_and(|b| {
            response
                .interact_pointer_pos()
                .is_some_and(|p| b.expand(2.0).contains(p))
        });

        let mods = ui.input(|i| i.modifiers);
        if response.clicked() && on_checkbox {
            // Ticks add to (or take from) the selection, like Ctrl+click.
            self.last_click = None;
            self.selection.click(&self.list.paths, index, true, false);
        } else if response.clicked() {
            // Track double-clicks per item ourselves: egui counts clicks
            // anywhere within 600 ms as a triple-click, so clicking one file
            // and quickly double-clicking a folder wouldn't open it.
            let now = ui.input(|i| i.time);
            let delay = ui.ctx().options(|o| o.input_options.max_double_click_delay);
            let double = matches!(&self.last_click, Some((p, t)) if *p == path && now - t <= delay);
            if double {
                self.last_click = None;
                actions.push(Action::OpenItem(path.clone()));
            } else {
                self.last_click = Some((path.clone(), now));
                self.selection
                    .click(&self.list.paths, index, mods.command, mods.shift);
            }
        } else if response.secondary_clicked() {
            self.selection.right_click(&path);
        } else if response.middle_clicked() && item.is_dir {
            actions.push(Action::NewTab(Some(path.clone()), false));
        }
        if response.drag_started_by(PointerButton::Primary)
            && !self.drag_out_active.load(Ordering::Relaxed)
        {
            // Dragging an unselected item drags just that item.
            if !selected {
                self.selection.select_only(&path);
            }
            self.drag_out_request = Some((self.selected_paths(), mods.shift));
        }
        let mut hover = if item.is_symlink {
            format!("{}\n{} (link)", item.name, item.get_type_name())
        } else {
            item.name.clone()
        };
        if self.searching_index() {
            if let Some(folder) = item.path.parent() {
                hover.push_str(&format!("\nIn {}", folder.display()));
            }
            if let Some(reason) = self.index.reason(&item.path) {
                hover.push_str(&format!("\nMatched: {}", reason.describe()));
            }
        }
        let response = response.on_hover_text_at_pointer(hover);
        response.context_menu(|ui| self.item_menu(ui, &item, actions));
    }

    /// True while one of our drags is over `rect`.
    fn drag_hover(&self, ui: &Ui, rect: Rect) -> bool {
        self.drag_out_active.load(Ordering::Relaxed)
            && ui
                .ctx()
                .pointer_latest_pos()
                .is_some_and(|p| rect.contains(p))
    }

    fn cut_pending(&self, path: &Path) -> bool {
        matches!(&self.clipboard, Some((paths, PasteMode::Move)) if paths.iter().any(|p| p == path))
    }

    /// Draw a thumbnail (or the file-type icon while it loads) fitted into `rect`.
    /// `fg` is the text colour of what's behind it (selection or not).
    fn paint_icon(&mut self, ui: &Ui, rect: Rect, item: &FileItem, fg: Color32) {
        // Don't download cloud files just to draw thumbnails.
        let allowed =
            !item.online_only && (!self.folder_is_cloud || self.settings.cloud_thumbnails);
        let thumb = if allowed && ThumbnailCache::can_generate_thumbnail(&item.path) {
            self.thumbnails.get(&item.path, ThumbSize::Icon)
        } else {
            None
        };
        let painter = ui.painter();
        if let Some(tex) = thumb {
            let size = tex.size_vec2();
            let scale = (rect.width() / size.x).min(rect.height() / size.y);
            let fitted = Rect::from_center_size(rect.center(), size * scale);
            painter.image(
                tex.id(),
                fitted,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            if ThumbnailCache::is_video_file(&item.path) && rect.height() >= 48.0 {
                // Film-strip hint so videos are distinguishable from photos.
                painter.text(
                    fitted.left_bottom() + Vec2::new(4.0, -4.0),
                    Align2::LEFT_BOTTOM,
                    "▶",
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
            }
        } else {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                get_file_icon(item),
                FontId::proportional((rect.height() * 0.5).clamp(14.0, 96.0)),
                fg,
            );
        }
        if item.is_symlink && rect.height() >= 24.0 {
            painter.text(
                rect.left_bottom(),
                Align2::LEFT_BOTTOM,
                "↪",
                FontId::proportional(12.0),
                // On the selection, its foreground (the accent can be the
                // selection colour itself, as in high contrast dark).
                if fg == pal().text { pal().accent } else { fg },
            );
        }
    }

    fn paint_details_row(&mut self, ui: &Ui, rect: Rect, item: &FileItem, selected: bool) {
        let (fg, fg2) = crate::theme::text_on(selected);
        let (name_w, date_w, type_w, _) = details_columns(rect.width());
        let icon_rect =
            Rect::from_center_size(rect.left_center() + Vec2::new(18.0, 0.0), Vec2::splat(22.0));
        self.paint_icon(ui, icon_rect, item, fg);

        let painter = ui.painter();
        let name = elided(
            painter,
            &item.name,
            FONT_SIZE_NORMAL,
            fg,
            name_w - 44.0,
            1,
            Align::Min,
        );
        painter.galley(
            Pos2::new(rect.left() + 36.0, rect.center().y - name.size().y / 2.0),
            name,
            fg,
        );

        let mut x = rect.left() + name_w + 10.0;
        if let Some(modified) = item.modified {
            painter.text(
                Pos2::new(x, rect.center().y),
                Align2::LEFT_CENTER,
                modified.format("%Y-%m-%d %H:%M").to_string(),
                FontId::proportional(FONT_SIZE_SMALL),
                fg2,
            );
        }
        x += date_w;
        let type_galley = elided(
            painter,
            &item.get_type_name(),
            FONT_SIZE_SMALL,
            fg2,
            type_w - 10.0,
            1,
            Align::Min,
        );
        painter.galley(
            Pos2::new(x, rect.center().y - type_galley.size().y / 2.0),
            type_galley,
            fg2,
        );
        if !item.is_dir {
            painter.text(
                rect.right_center() - Vec2::new(12.0, 0.0),
                Align2::RIGHT_CENTER,
                item.format_size(),
                FontId::proportional(FONT_SIZE_SMALL),
                fg2,
            );
        }
    }

    fn paint_tile(&mut self, ui: &Ui, rect: Rect, item: &FileItem, selected: bool) {
        let (fg, fg2) = crate::theme::text_on(selected);
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            Stroke::new(1.0, pal().border),
            StrokeKind::Inside,
        );
        let icon_rect = Rect::from_min_size(
            rect.min + Vec2::new(8.0, 8.0),
            Vec2::splat(rect.height() - 16.0),
        );
        self.paint_icon(ui, icon_rect, item, fg);
        let painter = ui.painter();
        let text_x = icon_rect.right() + 10.0;
        let width = rect.right() - text_x - 8.0;
        let name = elided(
            painter,
            &item.name,
            FONT_SIZE_NORMAL,
            fg,
            width,
            1,
            Align::Min,
        );
        painter.galley(
            Pos2::new(text_x, rect.center().y - name.size().y - 1.0),
            name,
            fg,
        );
        let info = if item.is_dir {
            item.get_type_name()
        } else {
            format!("{} · {}", item.get_type_name(), item.format_size())
        };
        let info = elided(painter, &info, FONT_SIZE_SMALL, fg2, width, 1, Align::Min);
        painter.galley(Pos2::new(text_x, rect.center().y + 3.0), info, fg2);
    }

    fn paint_icon_cell(&mut self, ui: &Ui, rect: Rect, item: &FileItem, selected: bool) {
        let (fg, _) = crate::theme::text_on(selected);
        let s = self.view.icon_size;
        let icon_rect = Rect::from_min_size(
            Pos2::new(rect.center().x - s / 2.0, rect.top() + 6.0),
            Vec2::splat(s),
        );
        self.paint_icon(ui, icon_rect, item, fg);
        let painter = ui.painter();
        let name = elided(
            painter,
            &item.name,
            FONT_SIZE_SMALL,
            fg,
            rect.width() - 8.0,
            2,
            Align::Center,
        );
        painter.galley(
            Pos2::new(rect.center().x, icon_rect.bottom() + 5.0),
            name,
            fg,
        );
    }

    /// "Open with": the default app, the others for the file's type, and
    /// a way to choose any app.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn open_with_menu(&self, ui: &mut Ui, item: &FileItem, actions: &mut Vec<Action>) {
        let paths: Vec<PathBuf> = {
            let sel = self.selected_paths();
            if sel.contains(&item.path) {
                sel.into_iter().filter(|p| !p.is_dir()).collect()
            } else {
                vec![item.path.clone()]
            }
        };
        ui.menu_button("🧰 Open with", |ui| {
            let catalog = self.app_catalog();
            let mime = catalog.mime_of(&item.path);
            let (default, others) = mime
                .as_deref()
                .map(|m| catalog.apps_for(m))
                .unwrap_or_default();
            if let Some(app) = default {
                if ui
                    .button(RichText::new(format!("{} (default)", app.name)).strong())
                    .clicked()
                {
                    actions.push(Action::OpenWithApp(app.clone(), paths.clone(), None));
                    ui.close();
                }
            }
            for app in others.iter().take(12) {
                if ui.button(&app.name).clicked() {
                    actions.push(Action::OpenWithApp((*app).clone(), paths.clone(), None));
                    ui.close();
                }
            }
            if default.is_some() || !others.is_empty() {
                ui.separator();
            }
            if ui.button("Choose another app…").clicked() {
                actions.push(Action::ChooseApp);
                ui.close();
            }
        });
    }

    #[cfg(not(all(unix, not(target_os = "macos"))))]
    fn open_with_menu(&self, ui: &mut Ui, _item: &FileItem, actions: &mut Vec<Action>) {
        if cfg!(windows) && ui.button("🧰 Open with…").clicked() {
            actions.push(Action::ChooseApp);
            ui.close();
        }
    }

    fn item_menu(&self, ui: &mut Ui, item: &FileItem, actions: &mut Vec<Action>) {
        ui.set_min_width(220.0);
        if disk_images::SUPPORTED && !item.is_dir && disk_images::is_image(&item.path) {
            for operation in [DiskOperation::Mount, DiskOperation::Unmount] {
                if ui.button(operation.label()).clicked() {
                    actions.push(Action::ShowDisks(Some(item.path.clone()), operation));
                    ui.close();
                }
            }
            ui.separator();
        }
        let count = self.selection.len().max(1);
        let open_label = if item.is_dir { "📂 Open" } else { "↗ Open" };
        if ui
            .add(Button::new(RichText::new(open_label).strong()).shortcut_text("Enter"))
            .clicked()
        {
            actions.push(Action::OpenSelected);
            ui.close();
        }
        if !item.is_dir {
            self.open_with_menu(ui, item, actions);
        }
        // Recent and search results list items from other folders.
        if (item.path.parent() != Some(self.shown_path.as_path()) || self.special.is_some())
            && ui.button("📂 Open file location").clicked()
        {
            actions.push(Action::OpenLocation(item.path.clone(), false));
            ui.close();
        }
        // Every search result, wherever it was found.
        if !self.search_text.is_empty() && ui.button("🗂 Open file location in new tab").clicked()
        {
            actions.push(Action::OpenLocation(item.path.clone(), true));
            ui.close();
        }
        if item.is_dir {
            let pinned = self.settings.pinned.contains(&item.path);
            let (label, action) = if pinned {
                (
                    "📌 Unpin from Quick Access",
                    Action::Unpin(item.path.clone()),
                )
            } else {
                ("📌 Pin to Quick Access", Action::Pin(item.path.clone()))
            };
            if ui.button(label).clicked() {
                actions.push(action);
                ui.close();
            }
        }
        if item.is_dir
            && ui
                .add(Button::new("🗂 Open in new tab").shortcut_text("Middle-click"))
                .clicked()
        {
            actions.push(Action::NewTab(Some(item.path.clone()), false));
            ui.close();
        }
        if item.is_dir {
            if ui.add(Button::new("⌨ Open in Terminal")).clicked() {
                actions.push(Action::OpenTerminal(item.path.clone()));
                ui.close();
            }
            let (label, action) = if self.is_bookmarked(&item.path) {
                (
                    "☆ Remove Bookmark",
                    Action::RemoveBookmark(item.path.clone()),
                )
            } else {
                (
                    "⭐ Add to Bookmarks",
                    Action::AddBookmark(item.path.clone()),
                )
            };
            if ui.add(Button::new(label)).clicked() {
                actions.push(action);
                ui.close();
            }
        }
        ui.separator();
        if ui.button("🗜 Compress to ZIP file").clicked() {
            actions.push(Action::Compress);
            ui.close();
        }
        if !item.is_dir && archive::is_zip(&item.path) && ui.button("📦 Extract all").clicked() {
            actions.push(Action::Extract(item.path.clone()));
            ui.close();
        }
        if self.rotatable_selection().is_some() {
            ui.separator();
            if ui.add(Button::new("⟳ Rotate right")).clicked() {
                actions.push(Action::Rotate(Rotation::Clockwise));
                ui.close();
            }
            if ui.add(Button::new("⟲ Rotate left")).clicked() {
                actions.push(Action::Rotate(Rotation::CounterClockwise));
                ui.close();
            }
            if ui.add(Button::new("⇆ Flip horizontally")).clicked() {
                actions.push(Action::Rotate(Rotation::FlipHorizontal));
                ui.close();
            }
            if ui.add(Button::new("⇅ Flip vertically")).clicked() {
                actions.push(Action::Rotate(Rotation::FlipVertical));
                ui.close();
            }
        }
        if !item.is_dir && ThumbnailCache::is_image_file(&item.path) {
            if self.rotatable_selection().is_none() {
                ui.separator();
            }
            if ui.add(Button::new("🖼 Set as desktop background")).clicked() {
                actions.push(Action::SetWallpaper(item.path.clone()));
                ui.close();
            }
        }
        ui.separator();
        for (label, shortcut, action) in [
            ("✂ Cut", "Ctrl+X", Action::Cut),
            ("🗐 Copy", "Ctrl+C", Action::Copy),
            ("🔗 Copy as path", "Ctrl+Shift+C", Action::CopyPath),
        ] {
            if ui.add(Button::new(label).shortcut_text(shortcut)).clicked() {
                actions.push(action);
                ui.close();
            }
        }
        ui.separator();
        let rename = if count > 1 {
            format!("✏ Rename {count} items…")
        } else {
            "✏ Rename…".to_string()
        };
        if ui.add(Button::new(rename).shortcut_text("F2")).clicked() {
            actions.push(Action::Rename);
            ui.close();
        }
        if ui
            .add(Button::new("🗑 Move to trash").shortcut_text("Del"))
            .clicked()
        {
            actions.push(Action::Trash);
            ui.close();
        }
        if ui
            .add(
                Button::new(RichText::new("⚠ Delete permanently…").color(pal().error))
                    .shortcut_text("Shift+Del"),
            )
            .clicked()
        {
            actions.push(Action::ConfirmDeletePermanently);
            ui.close();
        }
        ui.separator();
        if ui
            .add(Button::new("ℹ Properties").shortcut_text("Alt+Enter"))
            .clicked()
        {
            actions.push(Action::Properties);
            ui.close();
        }
    }

    fn background_menu(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.set_min_width(220.0);
        if let Some(label) = self.history.undo_label() {
            let blocked = self.history.undo_blocked();
            let button = ui.add_enabled(
                blocked.is_none(),
                Button::new(format!("Undo {}", label.to_lowercase())).shortcut_text("Ctrl+Z"),
            );
            let button = match &blocked {
                Some(why) => button.on_disabled_hover_text(format!("Can't undo: {why}")),
                None => button,
            };
            if button.clicked() {
                actions.push(Action::Undo);
                ui.close();
            }
        }
        if let Some(label) = self.history.redo_label() {
            if ui
                .add(Button::new(format!("Redo {}", label.to_lowercase())).shortcut_text("Ctrl+Y"))
                .clicked()
            {
                actions.push(Action::Redo);
                ui.close();
            }
        }
        if self.history.undo_label().is_some() || self.history.redo_label().is_some() {
            ui.separator();
        }
        ui.menu_button("⊞ View", |ui| self.view_menu(ui, actions));
        ui.menu_button("Sort by", |ui| self.sort_menu(ui, actions));
        if ui
            .add(Button::new("⟳ Refresh").shortcut_text("F5"))
            .clicked()
        {
            actions.push(Action::Refresh);
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(
                self.can_paste(),
                Button::new("📋 Paste").shortcut_text("Ctrl+V"),
            )
            .clicked()
        {
            actions.push(Action::Paste(None));
            ui.close();
        }
        if ui
            .add(Button::new("📁 New folder").shortcut_text("Ctrl+Shift+N"))
            .clicked()
        {
            actions.push(Action::NewFolder);
            ui.close();
        }
        ui.menu_button("📄 New Document", |ui| {
            self.new_document_menu(ui, actions)
        });
        ui.separator();
        if let Some(here) = self.list.folder.to_str().map(PathBuf::from) {
            if ui.add(Button::new("⌨ Open in Terminal")).clicked() {
                actions.push(Action::OpenTerminal(here.clone()));
                ui.close();
            }
            let (label, action) = if self.is_bookmarked(&here) {
                ("☆ Remove Bookmark", Action::RemoveBookmark(here))
            } else {
                ("⭐ Add to Bookmarks", Action::AddBookmark(here))
            };
            if ui.add(Button::new(label).shortcut_text("Ctrl+D")).clicked() {
                actions.push(action);
                ui.close();
            }
        }
        if ui.add(Button::new("🔗 Copy folder path")).clicked() {
            actions.push(Action::CopyPath);
            ui.close();
        }
        ui.separator();
        if ui
            .add(Button::new("👻 Show hidden items").shortcut_text("Ctrl+H"))
            .clicked()
        {
            actions.push(Action::ToggleHidden);
            ui.close();
        }
        ui.separator();
        if ui
            .add(Button::new("ℹ Properties").shortcut_text("Alt+Enter"))
            .clicked()
        {
            // Nothing selected here: the folder itself.
            actions.push(Action::Properties);
            ui.close();
        }
    }

    // ------------------------------------------------------------------
    // Preview pane
    // ------------------------------------------------------------------

    fn request_preview(&mut self, ctx: &egui::Context) {
        let target = if self.selection.len() == 1 {
            self.selection.focus().map(Path::to_path_buf)
        } else {
            None
        };
        let wants_text = target.as_ref().is_some_and(|p| {
            p.is_file() && (ThumbnailCache::is_text_file(p) || preview::is_office_file(p))
        });
        let target = target.filter(|_| wants_text && self.settings.show_preview_pane);
        if target == self.preview_requested {
            return;
        }
        self.preview_requested = target.clone();
        *self.preview_wanted.lock().unwrap() = target.clone();
        let Some(path) = target else { return };
        let slot = self.preview_text.clone();
        let wanted = self.preview_wanted.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let text = preview::load_preview_text(&path);
            // A slower, older request must not overwrite the current one.
            if wanted.lock().unwrap().as_ref() == Some(&path) {
                *slot.lock().unwrap() = Some((path, text));
                ctx.request_repaint();
            }
        });
    }

    fn show_preview_pane(&mut self, ui: &mut Ui) {
        ScrollArea::vertical()
            .id_salt("preview_pane_scroll")
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                egui::Frame::NONE
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        let selected = self.selected_paths();
                        match selected.as_slice() {
                            [] => {
                                ui.add_space(80.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        RichText::new("Select a file to preview")
                                            .color(pal().text_secondary),
                                    )
                                });
                            }
                            [path] => self.preview_single(ui, path),
                            many => {
                                let total: u64 = self
                                    .list
                                    .items
                                    .iter()
                                    .filter(|i| self.selection.contains(&i.path))
                                    .map(|i| i.size)
                                    .sum();
                                ui.add_space(20.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(RichText::new("🗐").size(56.0));
                                    ui.label(
                                        RichText::new(format!("{} items selected", many.len()))
                                            .size(FONT_SIZE_LARGE)
                                            .color(pal().text),
                                    );
                                    ui.label(
                                        RichText::new(format_bytes(total))
                                            .color(pal().text_secondary),
                                    );
                                });
                            }
                        }
                    });
            });
    }

    fn preview_single(&mut self, ui: &mut Ui, path: &Path) {
        let Some(item) = self.list.items.iter().find(|i| i.path == path).cloned() else {
            return;
        };
        ui.add(
            egui::Label::new(
                RichText::new(&item.name)
                    .size(FONT_SIZE_LARGE)
                    .color(pal().text)
                    .strong(),
            )
            .wrap(),
        );
        ui.add_space(12.0);

        if ThumbnailCache::can_generate_thumbnail(path) {
            let width = ui.available_width();
            match self.thumbnails.get(path, ThumbSize::Preview) {
                Some(tex) => {
                    let size = tex.size_vec2();
                    let scale = (width / size.x).min(1.0).min(480.0 / size.y);
                    ui.add(
                        egui::Image::from_texture((tex.id(), size * scale))
                            .corner_radius(CornerRadius::same(4)),
                    );
                }
                None if self.thumbnails.is_loading(path, ThumbSize::Preview) => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(30.0);
                        ui.spinner();
                        ui.label(RichText::new("Generating preview…").color(pal().text_secondary));
                    });
                }
                None => {
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(get_file_icon(&item)).size(56.0));
                        let hint = if ThumbnailCache::is_video_file(path) {
                            "No preview (video thumbnails need ffmpeg)"
                        } else if path
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
                        {
                            "No preview (PDF thumbnails need pdftoppm or mutool)"
                        } else if path.extension().is_some_and(|e| {
                            e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif")
                        }) {
                            "No preview (HEIC needs ImageMagick with libheif)"
                        } else {
                            "No preview available"
                        };
                        ui.label(
                            RichText::new(hint)
                                .size(FONT_SIZE_SMALL)
                                .color(pal().text_secondary),
                        );
                    });
                }
            }
        } else if let Some(msg) = ThumbnailCache::get_unsupported_message(path) {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(get_file_icon(&item)).size(56.0));
                ui.label(
                    RichText::new(msg)
                        .size(FONT_SIZE_SMALL)
                        .color(pal().text_secondary),
                );
            });
        } else if !item.is_dir
            && (ThumbnailCache::is_text_file(path) || preview::is_office_file(path))
        {
            let content = self.preview_text.lock().unwrap().clone();
            match content {
                Some((p, text)) if p == path => {
                    egui::Frame::NONE
                        .fill(pal().bg)
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(10))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(text)
                                        .size(FONT_SIZE_SMALL)
                                        .color(pal().text)
                                        .monospace(),
                                )
                                .wrap(),
                            );
                        });
                }
                _ => {
                    ui.spinner();
                }
            }
        } else {
            ui.vertical_centered(|ui| ui.label(RichText::new(get_file_icon(&item)).size(56.0)));
        }

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(
            RichText::new("Details")
                .size(FONT_SIZE_NORMAL)
                .color(pal().text)
                .strong(),
        );
        ui.add_space(6.0);
        info_row(ui, "Type", &item.get_type_name());
        if !item.is_dir {
            info_row(
                ui,
                "Size",
                &format!("{} ({} bytes)", format_bytes(item.size), item.size),
            );
        }
        if let Some(m) = item.modified {
            info_row(ui, "Modified", &m.format("%Y-%m-%d %H:%M").to_string());
        }
        if let Some(created) = std::fs::metadata(path).ok().and_then(|m| m.created().ok()) {
            info_row(
                ui,
                "Created",
                &chrono::DateTime::<chrono::Local>::from(created)
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
            );
        }
        info_row(
            ui,
            "Location",
            &path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
    }

    // ------------------------------------------------------------------
    // Status bar
    // ------------------------------------------------------------------

    fn show_status_bar(&mut self, ui: &mut Ui, fs: &FileSystem, actions: &mut Vec<Action>) {
        ui.horizontal_centered(|ui| {
            ui.add_space(12.0);
            let count = self.list.items.len();
            ui.label(
                RichText::new(format!("{count} item{}", plural(count)))
                    .color(pal().text)
                    .size(FONT_SIZE_SMALL),
            );
            if !self.selection.is_empty() {
                let total: u64 = self
                    .list
                    .items
                    .iter()
                    .filter(|i| self.selection.contains(&i.path))
                    .map(|i| i.size)
                    .sum();
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "{} selected  {}",
                        self.selection.len(),
                        format_bytes(total)
                    ))
                    .color(pal().text)
                    .size(FONT_SIZE_SMALL),
                );
            }
            if fs.is_loading() {
                ui.separator();
                ui.spinner();
                ui.label(RichText::new("Loading…").color(pal().accent).size(FONT_SIZE_SMALL));
            }
            if self.folder_is_cloud {
                ui.separator();
                let label = if self.settings.cloud_thumbnails { "☁ Cloud thumbnails: on" } else { "☁ Cloud thumbnails: off" };
                if ui
                    .add(Button::new(RichText::new(label).size(FONT_SIZE_SMALL).color(pal().text_secondary)).frame(false))
                    .on_hover_text(
                        "This folder streams from the cloud. Showing thumbnails downloads every picture \
                         and video in it, so they're off unless you turn them on. Click to toggle.",
                    )
                    .clicked()
                {
                    actions.push(Action::ToggleCloudThumbnails);
                }
            }
            for job in &self.jobs {
                ui.separator();
                let meter = &job.progress.meter;
                let paused = meter.paused.load(Ordering::Relaxed);
                if !paused {
                    ui.spinner();
                }
                let done = job.progress.done.load(Ordering::Relaxed);
                let verb = job.verb.to_lowercase();
                let mut text = format!("{verb}: {done} of {}", job.total);
                let bytes = meter.bytes.load(Ordering::Relaxed);
                if bytes > 0 {
                    text += &job_bytes_text(
                        bytes,
                        meter.total_known.load(Ordering::Relaxed).then(|| meter.total.load(Ordering::Relaxed)),
                        job.started.elapsed(),
                        paused,
                    );
                }
                ui.label(RichText::new(text).color(pal().accent).size(FONT_SIZE_SMALL));
                if bytes > 0 || matches!(job.verb.as_str(), "Copied" | "Moved") {
                    let small = |t: &str| Button::new(RichText::new(t).size(FONT_SIZE_SMALL)).frame(false);
                    if ui.add(small(if paused { "▶" } else { "⏸" })).on_hover_text(if paused { "Resume" } else { "Pause" }).clicked() {
                        meter.paused.store(!paused, Ordering::Relaxed);
                    }
                    if ui.add(small("✖")).on_hover_text("Cancel").clicked() {
                        meter.cancelled.store(true, Ordering::Relaxed);
                    }
                }
            }
            if self.searching_index() && !self.index.results.searching {
                let results = &self.index.results;
                if !results.filters.is_empty() {
                    ui.separator();
                    ui.label(
                        RichText::new(results.filters.join(" · "))
                            .color(pal().text_secondary)
                            .size(FONT_SIZE_SMALL),
                    );
                }
                if results.more {
                    ui.separator();
                    if ui
                        .add(Button::new(RichText::new("Show more results").size(FONT_SIZE_SMALL).color(pal().accent)).frame(false))
                        .clicked()
                    {
                        actions.push(Action::MoreResults);
                    }
                }
            }
            if let Some(text) = self.index.status_text() {
                ui.separator();
                if ui
                    .add(Button::new(RichText::new(text).size(FONT_SIZE_SMALL).color(pal().text_secondary)).frame(false))
                    .on_hover_text("Search indexing settings")
                    .clicked()
                {
                    actions.push(Action::ShowIndexSettings);
                }
            }
            if let Some((msg, at)) = &self.status_message {
                if at.elapsed() < Duration::from_secs(6) {
                    ui.separator();
                    ui.label(
                        RichText::new(msg)
                            .color(pal().text_secondary)
                            .size(FONT_SIZE_SMALL),
                    );
                    ui.ctx()
                        .request_repaint_after(Duration::from_secs(6).saturating_sub(at.elapsed()));
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(10.0);
                // Bottom-right view switches, plus a size slider.
                if ui
                    .selectable_label(
                        self.view.mode == ViewMode::Icons,
                        RichText::new("⊞").size(FONT_SIZE_NORMAL),
                    )
                    .on_hover_text("Large icons (Ctrl+Shift+2)")
                    .clicked()
                {
                    actions.push(Action::SetView(
                        ViewMode::Icons,
                        Some(self.view.icon_size.max(96.0)),
                    ));
                }
                if ui
                    .selectable_label(
                        self.view.mode == ViewMode::Details,
                        RichText::new("☰").size(FONT_SIZE_NORMAL),
                    )
                    .on_hover_text("Details (Ctrl+Shift+6)")
                    .clicked()
                {
                    actions.push(Action::SetView(ViewMode::Details, None));
                }
                if self.view.mode == ViewMode::Icons {
                    let mut size = self.view.icon_size;
                    let slider = ui
                        .add(
                            egui::Slider::new(&mut size, MIN_ICON_SIZE..=MAX_ICON_SIZE)
                                .show_value(false)
                                .logarithmic(true),
                        )
                        .on_hover_text("Icon size (Ctrl + mouse wheel)");
                    if slider.changed() {
                        actions.push(Action::SetView(ViewMode::Icons, Some(size)));
                    }
                }
                if let Some(ms) = fs.get_load_time_ms() {
                    ui.label(
                        RichText::new(format!("{ms:.0} ms"))
                            .color(pal().text_secondary)
                            .size(FONT_SIZE_SMALL),
                    )
                    .on_hover_text("Time to read this folder");
                }
            });
        });
    }

    // ------------------------------------------------------------------
    // Dialogs
    // ------------------------------------------------------------------

    /// The bulk rename dialog: rules on top, a live preview below. Returns
    /// the new paths once the batch is renamed.
    fn bulk_rename_dialog(
        &mut self,
        ui: &mut Ui,
        d: &mut BulkRenameDialog,
        keep_open: &mut bool,
    ) -> Option<Vec<PathBuf>> {
        use crate::bulk_rename::{self as br, CaseChange, RenamePreset};
        ui.set_width(640.0);
        ui.heading(format!("Rename {} items", d.items.len()));
        ui.add_space(4.0);
        let before = d.rules.clone();
        ui.horizontal(|ui| {
            ui.label("Preset");
            egui::ComboBox::from_id_salt("rename_preset")
                .selected_text(if d.preset_name.is_empty() {
                    "None"
                } else {
                    d.preset_name.as_str()
                })
                .show_ui(ui, |ui| {
                    for preset in &self.settings.rename_presets {
                        if ui
                            .selectable_label(preset.name == d.preset_name, &preset.name)
                            .clicked()
                        {
                            d.rules = preset.rules.clone();
                            d.preset_name = preset.name.clone();
                        }
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut d.preset_name)
                    .hint_text("Preset name")
                    .desired_width(140.0),
            );
            let name = d.preset_name.trim().to_string();
            if ui
                .add_enabled(!name.is_empty(), Button::new("Save"))
                .clicked()
            {
                let preset = RenamePreset {
                    name: name.clone(),
                    rules: d.rules.clone(),
                };
                match self
                    .settings
                    .rename_presets
                    .iter_mut()
                    .find(|p| p.name == name)
                {
                    Some(existing) => *existing = preset,
                    None => self.settings.rename_presets.push(preset),
                }
            }
            let saved = self.settings.rename_presets.iter().any(|p| p.name == name);
            if ui.add_enabled(saved, Button::new("Delete")).clicked() {
                self.settings.rename_presets.retain(|p| p.name != name);
                d.preset_name.clear();
            }
        });
        ui.add_space(6.0);
        egui::Grid::new("bulk_rename_rules").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("New name");
            ui.add(egui::TextEdit::singleline(&mut d.rules.template).desired_width(f32::INFINITY));
            ui.end_row();
            ui.label("");
            ui.label(
                RichText::new("{name} old name · {n} number · {modified} · {taken} date taken · {parent} folder · {ext}  —  dates take a format: {taken:%Y-%m-%d %H.%M}")
                    .small()
                    .color(pal().text_secondary),
            );
            ui.end_row();
            ui.label("Find");
            ui.add(egui::TextEdit::singleline(&mut d.rules.find).desired_width(f32::INFINITY));
            ui.end_row();
            ui.label("Replace with");
            ui.add(egui::TextEdit::singleline(&mut d.rules.replace).desired_width(f32::INFINITY));
            ui.end_row();
            ui.label("");
            ui.horizontal(|ui| {
                ui.checkbox(&mut d.rules.regex, "Regular expression").on_hover_text("Use $1 or ${1} for groups");
                ui.checkbox(&mut d.rules.match_case, "Match case");
                ui.checkbox(&mut d.rules.include_extension, "Include extension");
            });
            ui.end_row();
            ui.label("Numbers");
            ui.horizontal(|ui| {
                ui.label("start");
                ui.add(egui::DragValue::new(&mut d.rules.start));
                ui.label("step");
                ui.add(egui::DragValue::new(&mut d.rules.step));
                ui.label("digits");
                ui.add(egui::DragValue::new(&mut d.rules.padding).range(0..=12));
            });
            ui.end_row();
            ui.label("Case");
            egui::ComboBox::from_id_salt("rename_case")
                .selected_text(d.rules.case.label())
                .show_ui(ui, |ui| {
                    for case in CaseChange::ALL {
                        ui.selectable_value(&mut d.rules.case, case, case.label());
                    }
                });
            ui.end_row();
        });
        if d.rules != before {
            d.error = None;
        }
        if d.preview
            .as_ref()
            .is_none_or(|(rules, _)| *rules != d.rules)
        {
            d.preview = Some((d.rules.clone(), br::preview(&d.items, &d.rules)));
        }
        let Some((_, preview)) = &d.preview else {
            return None;
        };
        ui.add_space(6.0);
        ui.separator();
        let mut ready = false;
        match preview {
            Err(e) => {
                ui.label(RichText::new(e.as_str()).color(pal().error));
            }
            Ok(rows) => {
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .auto_shrink([false, true])
                    .show_rows(
                        ui,
                        ui.text_style_height(&egui::TextStyle::Body) + 4.0,
                        rows.len(),
                        |ui, range| {
                            egui::Grid::new("bulk_rename_preview")
                                .num_columns(3)
                                .striped(true)
                                .show(ui, |ui| {
                                    for row in &rows[range] {
                                        ui.label(row.old_name());
                                        ui.label("→");
                                        let new = RichText::new(&row.new_name);
                                        match &row.problem {
                                            Some(problem) => {
                                                ui.label(new.color(pal().error))
                                                    .on_hover_text(problem);
                                            }
                                            None if !row.changes() => {
                                                ui.label(new.color(pal().text_secondary));
                                            }
                                            None => {
                                                ui.label(new);
                                            }
                                        }
                                        ui.end_row();
                                    }
                                });
                        },
                    );
                let problems = rows.iter().filter(|r| r.problem.is_some()).count();
                let changing = rows.iter().filter(|r| r.changes()).count();
                ui.add_space(4.0);
                if let Some(first) = rows.iter().find(|r| r.problem.is_some()) {
                    ui.label(
                        RichText::new(format!(
                            "{problems} item{} can't be renamed like this. {}: {}",
                            plural(problems),
                            first.old_name(),
                            first.problem.as_deref().unwrap_or_default()
                        ))
                        .color(pal().error),
                    );
                } else {
                    ui.label(
                        RichText::new(format!(
                            "{changing} of {} items will be renamed",
                            rows.len()
                        ))
                        .color(pal().text_secondary),
                    );
                    ready = changing > 0;
                }
            }
        }
        if let Some(e) = &d.error {
            ui.label(RichText::new(e.as_str()).color(pal().error));
        }
        ui.add_space(8.0);
        let (mut renamed, mut stale) = (None, false);
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, Button::new("Rename")).clicked() {
                if let Ok(rows) = preview {
                    match br::apply(rows) {
                        Ok(step) => {
                            *keep_open = false;
                            let paths: Vec<PathBuf> = rows
                                .iter()
                                .map(|r| r.from.with_file_name(&r.new_name))
                                .collect();
                            self.history.record(
                                count_label("Rename", rows.iter().filter(|r| r.changes()).count()),
                                vec![step],
                            );
                            renamed = Some(paths);
                        }
                        Err(e) => {
                            d.error = Some(e);
                            stale = true;
                        }
                    }
                }
            }
            if ui.button("Cancel").clicked() {
                *keep_open = false;
            }
        });
        if stale {
            // The folder may have changed under us: check the names again.
            d.preview = None;
        }
        renamed
    }

    fn show_dialog(&mut self, ctx: &egui::Context, fs: &mut FileSystem) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let mut keep_open = true;
        let mut pending_extract = None;
        let mut pending_connect: Option<(String, Option<Credentials>)> = None;
        #[cfg(all(unix, not(target_os = "macos")))]
        let mut pending_open_with: Option<(open_with::App, Vec<PathBuf>, Option<String>)> = None;
        let modal = egui::Modal::new(Id::unique("dialog")).show(ctx, |ui| {
            ui.set_width(420.0);
            match &mut dialog {
                Dialog::Disks { operation, target, read_only, recovery_password, error } => {
                    ui.heading("Disk images & BitLocker");
                    for choice in [DiskOperation::Mount, DiskOperation::Unmount, DiskOperation::Unlock, DiskOperation::Lock] {
                        if ui.radio_value(operation, choice, choice.label()).changed() {
                            *error = None;
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(if operation.is_image() {
                        "Full path to the VHD/VHDX file"
                    } else if cfg!(windows) {
                        "Volume (for example E:)"
                    } else {
                        "BitLocker partition (for example /dev/sdb1)"
                    });
                    ui.add(egui::TextEdit::singleline(target).desired_width(f32::INFINITY));
                    if *operation == DiskOperation::Mount || (*operation == DiskOperation::Unlock && cfg!(target_os = "linux")) {
                        ui.checkbox(read_only, "Read-only");
                        if !*read_only {
                            ui.label("Changes will be written to the disk. Close any VM using this image before mounting.");
                        }
                    }
                    if *operation == DiskOperation::Unlock && cfg!(windows) {
                        ui.checkbox(recovery_password, "Use a 48-digit recovery password");
                    }
                    ui.add_space(8.0);
                    ui.label("An OS terminal will ask for administrator permission and any unlock credentials. Check the result there; drives refresh automatically.");
                    if cfg!(target_os = "linux") {
                        ui.label("Requires sudo, Python 3, QEMU tools, cryptsetup and filesystem drivers. For an encrypted image, select its partition and enter the password at the terminal prompt. Unmount / Lock removes mounts created here.");
                    }
                    if let Some(message) = error {
                        ui.label(RichText::new(message.as_str()).color(pal().error));
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(self.disk_launch.is_none() && !target.trim().is_empty(), Button::new("Continue in terminal")).clicked() {
                            let (op, path, ro, recovery) = (*operation, target.clone(), *read_only, *recovery_password);
                            let (tx, rx) = std::sync::mpsc::channel();
                            let repaint = ctx.clone();
                            thread::spawn(move || {
                                let result = disk_images::launch(op, &path, ro, recovery);
                                let _ = tx.send(result);
                                repaint.request_repaint();
                            });
                            self.disk_launch = Some(rx);
                            keep_open = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                    });
                }
                Dialog::Rename { targets, text, error, first_frame } => {
                    ui.heading("Rename");
                    ui.add_space(6.0);
                    let edit = ui.add(egui::TextEdit::singleline(text).desired_width(f32::INFINITY));
                    if *first_frame {
                        *first_frame = false;
                        edit.request_focus();
                        // Select the name without its extension.
                        let stem_len = if !targets[0].is_dir() {
                            text.rfind('.').filter(|&i| i > 0).map(|i| text[..i].chars().count())
                        } else {
                            None
                        };
                        select_text_range(ui.ctx(), edit.id, 0, stem_len.unwrap_or(text.chars().count()));
                    }
                    if let Some(e) = error {
                        ui.label(RichText::new(e.as_str()).color(pal().error));
                    }
                    ui.add_space(8.0);
                    let enter = edit.lost_focus()
                        && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
                    ui.horizontal(|ui| {
                        let ok = ui.button("Rename").clicked() || enter;
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                        if ok {
                            match FileOperations::rename(&targets[0], text.trim()) {
                                Ok(renamed) => {
                                    keep_open = false;
                                    if renamed != targets[0] {
                                        self.history.record("Rename", vec![Step::Moved { from: targets[0].clone(), to: renamed.clone() }]);
                                    }
                                    fs.load_directory();
                                    self.refresh_list(fs);
                                    self.selection.select_only(&renamed);
                                    self.scroll_to_focus = true;
                                }
                                Err(e) => {
                                    *error = Some(e);
                                    edit.request_focus();
                                }
                            }
                        }
                    });
                }
                Dialog::BulkRename(dialog) => {
                    if let Some(renamed) = self.bulk_rename_dialog(ui, dialog, &mut keep_open) {
                        fs.load_directory();
                        self.refresh_list(fs);
                        if let Some(first) = renamed.first() {
                            self.selection.select_only(first);
                        }
                        self.selection.set(renamed);
                        self.scroll_to_focus = true;
                    }
                }
                Dialog::ConfirmLargeArchive { archive, bytes } => {
                    ui.heading("Extract a large archive?");
                    ui.label(format!("This archive declares {:.2} GiB of output, above the normal 4 GiB limit. Continue only if you trust it and have enough free disk space. Extraction will stop if it exceeds this declared size.", *bytes as f64 / (1024.0 * 1024.0 * 1024.0)));
                    ui.horizontal(|ui| {
                        if ui.button("Extract larger archive").clicked() {
                            pending_extract = Some((archive.clone(), *bytes));
                            keep_open = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                    });
                }
                Dialog::ConfirmPurge { ids } => {
                    ui.heading("Delete permanently?");
                    let what = match ids.len() {
                        1 => "this item".to_string(),
                        n if n == self.trash.items.len() => format!("all {n} items in the {}", TRASH_LABEL.trim_start_matches("🗑 ")),
                        n => format!("these {n} items"),
                    };
                    ui.label(format!("Are you sure you want to permanently delete {what}? This can't be undone."));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Delete").color(pal().error)).clicked() {
                            keep_open = false;
                            if let Err(e) = trash_bin::purge(ids) {
                                self.status(e);
                            }
                            self.load_trash();
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                    });
                }
                Dialog::ConfirmDelete { targets } => {
                    ui.heading("Delete permanently?");
                    let what = match targets.as_slice() {
                        [one] => format!("\"{}\"", display_name(one)),
                        many => format!("these {} items", many.len()),
                    };
                    ui.label(format!("Are you sure you want to permanently delete {what}? This can't be undone."));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Delete").color(pal().error)).clicked() {
                            keep_open = false;
                            let paths = std::mem::take(targets);
                            self.spawn_job(ctx, "Deleted", JobKind::NoHistory, paths, |p, _| {
                                FileOperations::delete_permanently(p)
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                    });
                }
                Dialog::Connect { address, user, domain, password, needs_credentials, asks_domain, error, busy, first_frame } => {
                    ui.heading("Connect to server");
                    ui.label(RichText::new("smb://server/share, sftp://user@host/folder, \\\\server\\share, ftp://, dav://…").color(pal().text_secondary).size(FONT_SIZE_SMALL));
                    ui.add_space(6.0);
                    let addr = ui.add_enabled(!*busy, egui::TextEdit::singleline(address).hint_text("smb://nas/photos").desired_width(f32::INFINITY));
                    if *first_frame {
                        *first_frame = false;
                        addr.request_focus();
                    }
                    let mut submit = addr.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if !self.settings.recent_servers.is_empty() && !*needs_credentials {
                        ui.add_space(4.0);
                        ui.label(RichText::new("Recent").color(pal().text_secondary).size(FONT_SIZE_SMALL));
                        for recent in self.settings.recent_servers.clone() {
                            if ui.add_enabled(!*busy, Button::new(&recent).frame(false)).clicked() {
                                *address = recent;
                                submit = true;
                            }
                        }
                    }
                    if *needs_credentials {
                        ui.add_space(8.0);
                        ui.label("This server needs you to sign in.");
                        egui::Grid::new("credentials").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label("User name");
                            ui.add_enabled(!*busy, egui::TextEdit::singleline(user).desired_width(240.0));
                            ui.end_row();
                            if *asks_domain {
                                ui.label("Domain");
                                ui.add_enabled(!*busy, egui::TextEdit::singleline(domain).hint_text("WORKGROUP").desired_width(240.0));
                                ui.end_row();
                            }
                            ui.label("Password");
                            let pw = ui.add_enabled(!*busy, egui::TextEdit::singleline(password).password(true).desired_width(240.0));
                            if pw.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                submit = true;
                            }
                            ui.end_row();
                        });
                        ui.label(RichText::new("Your password is only used for this connection and isn't saved.").color(pal().text_secondary).size(FONT_SIZE_SMALL));
                    }
                    if let Some(e) = error {
                        ui.add_space(4.0);
                        ui.label(RichText::new(e.as_str()).color(pal().error));
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if *busy {
                            ui.spinner();
                            ui.label("Connecting…");
                        } else if ui.button("Connect").clicked() {
                            submit = true;
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                            self.connecting = None; // a late result is ignored
                        }
                    });
                    if submit && !*busy {
                        match network::as_network_uri(address) {
                            Some(uri) => {
                                let creds = needs_credentials.then(|| Credentials {
                                    user: user.clone(),
                                    domain: domain.clone(),
                                    password: std::mem::take(password),
                                });
                                *busy = true;
                                *error = None;
                                pending_connect = Some((uri, creds));
                            }
                            None => *error = Some("Enter an address like smb://server/share or \\\\server\\share".into()),
                        }
                    }
                }
                Dialog::Conflict { paths, dest, mode, clashes } => {
                    ui.heading("Replace or skip files");
                    let n = clashes.len();
                    ui.label(format!(
                        "The destination already has {} with the same name{}:",
                        if n == 1 { "an item".to_string() } else { format!("{n} items") },
                        if n == 1 { "" } else { "s" }
                    ));
                    ui.add_space(4.0);
                    for c in clashes.iter().take(6) {
                        ui.label(RichText::new(format!("  • {}", display_name(c))).color(pal().text_secondary));
                    }
                    if n > 6 {
                        ui.label(RichText::new(format!("  … and {} more", n - 6)).color(pal().text_secondary));
                    }
                    if clashes.iter().any(|c| c.is_dir()) {
                        ui.label(RichText::new("Folders with the same name are merged.").color(pal().text_secondary).size(FONT_SIZE_SMALL));
                    }
                    ui.label(RichText::new("Replaced files go to the trash, and Ctrl+Z undoes this.").color(pal().text_secondary).size(FONT_SIZE_SMALL));
                    ui.add_space(10.0);
                    let mut choice = None;
                    ui.horizontal(|ui| {
                        if ui.button("Replace").clicked() {
                            choice = Some(Conflict::Replace);
                        }
                        if ui.button("Skip").clicked() {
                            choice = Some(Conflict::Skip);
                        }
                        if ui.button("Keep both").on_hover_text("Name the new items \"name (2)\"").clicked() {
                            choice = Some(Conflict::KeepBoth);
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                    });
                    if let Some(conflict) = choice {
                        keep_open = false;
                        let (paths, dest, mode) = (std::mem::take(paths), dest.clone(), *mode);
                        self.run_paste(ctx, paths, dest, mode, conflict);
                    }
                }
                #[cfg(all(unix, not(target_os = "macos")))]
                Dialog::OpenWith { paths, mime, filter, always } => {
                    let what = if paths.len() == 1 {
                        format!("Open {} with", display_name(&paths[0]))
                    } else {
                        format!("Open {} files with", paths.len())
                    };
                    ui.heading(what);
                    ui.add_space(6.0);
                    ui.add(
                        egui::TextEdit::singleline(filter)
                            .hint_text("Filter apps")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(6.0);
                    let catalog = self.apps.get_or_init(|| {
                        if cfg!(test) {
                            open_with::Catalog::default()
                        } else {
                            open_with::Catalog::load(&open_with::Dirs::from_env())
                        }
                    });
                    let (default, recommended) = mime
                        .as_deref()
                        .map(|m| catalog.apps_for(m))
                        .unwrap_or_default();
                    let needle = filter.to_lowercase();
                    let matches = |a: &&open_with::App| needle.is_empty() || a.name.to_lowercase().contains(&needle);
                    let mut chosen: Option<open_with::App> = None;
                    ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        let suggested: Vec<&open_with::App> =
                            default.into_iter().chain(recommended.iter().copied()).filter(matches).collect();
                        if !suggested.is_empty() {
                            ui.label(RichText::new("Suggested").color(pal().text_secondary).size(FONT_SIZE_SMALL));
                            for app in &suggested {
                                if ui.selectable_label(false, &app.name).clicked() {
                                    chosen = Some((*app).clone());
                                }
                            }
                            ui.add_space(6.0);
                        }
                        ui.label(RichText::new("All apps").color(pal().text_secondary).size(FONT_SIZE_SMALL));
                        for app in catalog.apps.iter().filter(matches) {
                            if !suggested.iter().any(|s| s.id == app.id)
                                && ui.selectable_label(false, &app.name).clicked()
                            {
                                chosen = Some(app.clone());
                            }
                        }
                    });
                    if let Some(m) = mime.as_deref() {
                        let ext = paths[0]
                            .extension()
                            .map(|e| format!(".{}", e.to_string_lossy()))
                            .unwrap_or_else(|| m.to_string());
                        ui.checkbox(always, format!("Always use this app for {ext} files"));
                    }
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        keep_open = false;
                    }
                    if let Some(app) = chosen {
                        let make_default = if *always { mime.clone() } else { None };
                        pending_open_with = Some((app, paths.clone(), make_default));
                        keep_open = false;
                    }
                }
                Dialog::Properties { paths, details, count } => {
                    let totals = count.totals();
                    let single = paths.len() == 1;
                    let is_dir = single && paths[0].is_dir();
                    let title = if single {
                        display_name(&paths[0])
                    } else {
                        format!("{} items", paths.len())
                    };
                    ui.heading(format!("{title} — Properties"));
                    ui.add_space(6.0);
                    let when = |t: &Option<chrono::DateTime<chrono::Local>>| {
                        t.map_or("—".to_string(), |t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                    };
                    let size_text = |bytes: u64| format!("{} ({} bytes)", format_bytes(bytes), group_digits(bytes));
                    egui::Grid::new("properties").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                        let mut row = |label: &str, value: String| {
                            ui.label(RichText::new(label).color(pal().text_secondary));
                            ui.add(egui::Label::new(value).wrap());
                            ui.end_row();
                        };
                        if single {
                            let p = &paths[0];
                            row(
                                "Type",
                                FileItem::new(p.clone()).map_or("—".into(), |i| i.get_type_name()),
                            );
                            row(
                                "Location",
                                p.parent().map_or("—".into(), |l| l.to_string_lossy().to_string()),
                            );
                        }
                        let counting = if totals.done { "" } else { " (counting…)" };
                        if is_dir || !single {
                            row("Size", format!("{}{counting}", size_text(totals.bytes)));
                            row(
                                "Contains",
                                format!(
                                    "{} files, {} folders{counting}",
                                    group_digits(totals.files),
                                    group_digits(totals.folders.saturating_sub(u64::from(is_dir)))
                                ),
                            );
                            if totals.unreadable > 0 {
                                row("Not counted", format!("{} folders couldn't be read", totals.unreadable));
                            }
                        }
                        if let Some(d) = details {
                            if !is_dir {
                                row("Size", size_text(d.size));
                                if let Some(on_disk) = d.on_disk {
                                    row("Size on disk", size_text(on_disk));
                                }
                            }
                            if let Some((w, h)) = d.dimensions {
                                row("Dimensions", format!("{w} × {h} pixels"));
                            }
                            if let Some(target) = &d.link_target {
                                row("Link to", target.to_string_lossy().to_string());
                            }
                            row("Created", when(&d.created));
                            row("Modified", when(&d.modified));
                            row("Accessed", when(&d.accessed));
                            if let Some(secs) = d.duration {
                                row("Length", properties::format_duration(secs));
                            }
                            if d.mode.is_none() {
                                if let Some(perm) = &d.permissions {
                                    row(if cfg!(windows) { "Attributes" } else { "Permissions" }, perm.clone());
                                }
                            }
                            if let Some(owner) = &d.owner {
                                row("Owner", owner.clone());
                            }
                        }
                    });
                    if let (true, Some(d)) = (single, details.as_mut()) {
                        if let Err(e) = edit_permissions(ui, &paths[0], d) {
                            ui.label(RichText::new(e).color(pal().error));
                        }
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if single && ui.button("🔗 Copy path").clicked() {
                            ui.ctx().copy_text(paths[0].to_string_lossy().to_string());
                        }
                        if ui.button("Close").clicked() {
                            keep_open = false;
                        }
                    });
                }
                Dialog::Shortcuts => {
                    ui.heading("Keyboard shortcuts");
                    ui.add_space(6.0);
                    ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        egui::Grid::new("shortcuts").num_columns(2).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                            for (keys, what) in SHORTCUTS {
                                ui.label(RichText::new(*keys).monospace().color(pal().text));
                                ui.label(*what);
                                ui.end_row();
                            }
                        });
                    });
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        keep_open = false;
                    }
                }
                Dialog::IndexSettings => {
                    // The tallest dialog: it scrolls instead of running off a
                    // small or highly scaled screen (top first, as a centred
                    // dialog grows both ways). Once it scrolls, it takes the
                    // whole height: egui's default there is a 64-point strip.
                    let max_height = (ctx.content_rect().height() - 80.0).max(200.0);
                    ScrollArea::vertical()
                        .max_height(max_height)
                        .min_scrolled_height(max_height)
                        .show(ui, |ui| {
                        keep_open = self.index.show_settings(ui, &mut self.settings.index, fs.get_current_path());
                    });
                }
                Dialog::About => {
                    ui.vertical_centered(|ui| {
                        ui.heading("Ninimma");
                        #[cfg(all(unix, not(target_os = "macos")))]
                        {
                            ui.label("Folder defaults apply only to your account.");
                            for (label, restore) in [("Make default file manager", false), ("Restore previous default", true)] {
                                if ui.button(label).clicked() {
                                    self.status(crate::desktop_default::change(restore).unwrap_or_else(|e| e));
                                }
                            }
                        }

                        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                        ui.add_space(6.0);
                        ui.label(env!("CARGO_PKG_DESCRIPTION"));
                        ui.add_space(6.0);
                        ui.hyperlink(env!("CARGO_PKG_REPOSITORY"));
                        ui.add_space(4.0);
                        ui.label("Copyright © 2026 tomblanchard312");
                        ui.label(
                            RichText::new("Licensed under the Apache License 2.0. Redistributions must keep the LICENSE and NOTICE files.")
                                .color(pal().text_secondary)
                                .size(FONT_SIZE_SMALL),
                        );
                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Photo place names:");
                            ui.hyperlink_to("GeoNames", "https://www.geonames.org");
                            ui.label("(unmodified data), licensed under");
                            ui.hyperlink_to("CC BY 4.0", "https://creativecommons.org/licenses/by/4.0/");
                        });
                        ui.collapsing("Licenses, model downloads and third-party notices", |ui| {
                            ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                                ui.label(include_str!("../NOTICE"));
                                ui.separator();
                                ui.label(include_str!("../LICENSE"));
                                ui.separator();
                                ui.label(include_str!("../packaging/icons/LICENSE-lucide"));
                            });
                        });
                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            keep_open = false;
                        }
                    });
                }
                Dialog::Errors { title, messages } => {
                    ui.heading("Something went wrong");
                    ui.label(title.as_str());
                    ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        for m in messages.iter() {
                            ui.label(RichText::new(m).color(pal().error).size(FONT_SIZE_SMALL));
                        }
                    });
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() {
                        keep_open = false;
                    }
                }
            }
        });
        if modal.should_close() {
            keep_open = false;
        }
        if keep_open {
            self.dialog = Some(dialog);
        }
        if let Some((archive, limit)) = pending_extract {
            self.apply(Action::ExtractApproved(archive, limit), ctx, fs);
        }
        if let Some((uri, creds)) = pending_connect {
            self.start_connect(ctx, uri, creds);
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some((app, paths, make_default)) = pending_open_with {
            self.apply(Action::OpenWithApp(app, paths, make_default), ctx, fs);
        }
    }
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

/// One menu entry: label, shortcut hint, enabled state, and what it does.
fn menu_item(
    ui: &mut Ui,
    label: &str,
    shortcut: &str,
    enabled: bool,
    action: Action,
    actions: &mut Vec<Action>,
) {
    if ui
        .add_enabled(enabled, Button::new(label).shortcut_text(shortcut))
        .clicked()
    {
        actions.push(action);
        ui.close();
    }
}

/// Open a terminal in `dir` using whatever terminal this system has.
fn open_terminal(dir: &Path) -> Result<(), String> {
    use std::process::Command;
    #[cfg(windows)]
    let attempts: Vec<Command> = {
        let mut wt = Command::new("wt");
        wt.arg("-d").arg(dir);
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "start", "cmd", "/K"]).current_dir(dir);
        vec![wt, cmd]
    };
    #[cfg(target_os = "macos")]
    let attempts: Vec<Command> = {
        let mut open = Command::new("open");
        open.args(["-a", "Terminal"]).arg(dir);
        vec![open]
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let attempts: Vec<Command> = {
        let wd = format!("--working-directory={}", dir.display());
        let mut list: Vec<(String, Vec<String>)> = Vec::new();
        if let Ok(t) = std::env::var("TERMINAL") {
            list.push((t, vec![]));
        }
        for (prog, args) in [
            ("x-terminal-emulator", vec![]),
            ("gnome-terminal", vec![wd.clone()]),
            ("kgx", vec![wd.clone()]),
            ("ptyxis", vec![wd.clone()]),
            (
                "konsole",
                vec!["--workdir".to_string(), dir.display().to_string()],
            ),
            ("xfce4-terminal", vec![wd.clone()]),
            ("mate-terminal", vec![wd.clone()]),
            ("tilix", vec![wd.clone()]),
            ("kitty", vec![]),
            ("alacritty", vec![]),
            (
                "wezterm",
                vec![
                    "start".to_string(),
                    "--cwd".to_string(),
                    dir.display().to_string(),
                ],
            ),
            ("foot", vec![]),
            ("xterm", vec![]),
        ] {
            list.push((prog.to_string(), args));
        }
        let mut attempts = Vec::new();
        for (prog, args) in list {
            if let Some(mut command) = crate::terminal::command(&prog, Some(dir))? {
                command.args(args);
                attempts.push(command);
            }
        }
        attempts
    };
    for mut cmd in attempts {
        if cmd.spawn().is_ok() {
            return Ok(());
        }
    }
    Err("Couldn't find a terminal app to open".to_string())
}

fn default_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
}

/// "Move 3 items", or just "Move" for one.
fn count_label(verb: &str, n: usize) -> String {
    if n == 1 {
        verb.to_string()
    } else {
        format!("{verb} {n} items")
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn sort_name(by: SortBy) -> &'static str {
    match by {
        SortBy::Name => "Name",
        SortBy::Modified => "Date modified",
        SortBy::Type => "Type",
        SortBy::Size => "Size",
    }
}

fn icon_preset_name(size: f32) -> String {
    ICON_PRESETS
        .iter()
        .find(|(_, s)| (size - s).abs() < 1.0)
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| format!("Icons ({size:.0}px)"))
}

fn details_columns(width: f32) -> (f32, f32, f32, f32) {
    let (date, typ, size) = (150.0, 150.0, 90.0);
    ((width - date - typ - size).max(160.0), date, typ, size)
}

/// Folders where most files are pictures open in Large icons by default.
fn looks_like_picture_folder(items: &[FileItem]) -> bool {
    let files: Vec<&FileItem> = items.iter().filter(|i| !i.is_dir).collect();
    let pictures = files
        .iter()
        .filter(|i| ThumbnailCache::can_generate_thumbnail(&i.path))
        .count();
    pictures >= 4 && pictures * 2 >= files.len()
}

/// Lay out text that is cut off with "…" if it doesn't fit, on a char
/// boundary (byte slicing here used to crash on accented file names).
fn elided(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: Color32,
    max_width: f32,
    max_rows: usize,
    halign: Align,
) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat::simple(FontId::proportional(size), color),
    );
    job.wrap = TextWrapping {
        max_width: max_width.max(10.0),
        max_rows,
        break_anywhere: max_rows == 1,
        overflow_character: Some('…'),
    };
    job.halign = halign;
    painter.layout_job(job)
}

fn info_row(ui: &mut Ui, label: &str, value: &str) {
    ui.label(
        RichText::new(label)
            .color(pal().text_secondary)
            .size(FONT_SIZE_SMALL),
    );
    ui.add(egui::Label::new(RichText::new(value).color(pal().text).size(FONT_SIZE_SMALL)).wrap());
    ui.add_space(6.0);
}

/// The tree's arrows. Glyphs the bundled fonts have ("▸"/"▾" aren't, and
/// showed as empty boxes).
const TREE_COLLAPSED: &str = "⏵";
const TREE_EXPANDED: &str = "⏷";

/// Width of one level of the folder tree, and of its expand arrow.
const TREE_INDENT: f32 = 14.0;

/// The expand/collapse arrow at `x` in a navigation pane row, clickable on
/// its own (added after the row, so it takes the click).
fn tree_arrow(ui: &mut Ui, row: Rect, x: f32, expanded: bool, name: &str) -> egui::Response {
    let rect = Rect::from_min_size(
        Pos2::new(x, row.top()),
        Vec2::new(TREE_INDENT + 4.0, row.height()),
    );
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("tree_arrow", name, row.top() as i32)),
        Sense::click(),
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        if expanded {
            TREE_EXPANDED
        } else {
            TREE_COLLAPSED
        },
        FontId::proportional(FONT_SIZE_SMALL),
        if resp.hovered() {
            pal().text
        } else {
            pal().text_secondary
        },
    );
    let verb = if expanded { "Collapse" } else { "Expand" };
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{verb} {name}"))
    });
    resp
}

/// A row of the folder tree: indented by `depth`, with room for the arrow.
fn tree_row(ui: &mut Ui, depth: usize, icon: &str, name: &str, current: bool) -> egui::Response {
    let (fg, _) = crate::theme::text_on(current);
    let lead = TREE_INDENT * (depth as f32 + 1.0);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::click());
    let bg = if current {
        pal().selected
    } else if response.hovered() {
        pal().hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter().text(
        rect.left_center() + Vec2::new(lead + 4.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(FONT_SIZE_NORMAL),
        fg,
    );
    let galley = elided(
        ui.painter(),
        name,
        FONT_SIZE_NORMAL,
        fg,
        rect.width() - lead - 34.0,
        1,
        Align::Min,
    );
    ui.painter().galley(
        Pos2::new(
            rect.left() + lead + 26.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        fg,
    );
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, current, name));
    response
}

fn nav_item(ui: &mut Ui, icon: &str, name: &str, current: bool) -> egui::Response {
    let (fg, _) = crate::theme::text_on(current);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
    let bg = if current {
        pal().selected
    } else if response.hovered() {
        pal().hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter().text(
        rect.left_center() + Vec2::new(10.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(FONT_SIZE_NORMAL),
        fg,
    );
    let galley = elided(
        ui.painter(),
        name,
        FONT_SIZE_NORMAL,
        fg,
        rect.width() - 44.0,
        1,
        Align::Min,
    );
    ui.painter().galley(
        Pos2::new(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, current, name));
    response
}

/// 1234567 as "1,234,567".
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn section_heading(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .color(pal().text)
            .size(FONT_SIZE_NORMAL)
            .strong(),
    );
    ui.add_space(6.0);
}

/// Tiles on the This PC page are at least this size (frame included);
/// rows stretch them to fill the width.
const PLACE_TILE_MIN: Vec2 = Vec2::new(240.0, 78.0);
/// ... and at most this wide, so a very wide window doesn't make them odd.
const PLACE_TILE_MAX_WIDTH: f32 = 420.0;
const PLACE_TILE_GAP: f32 = 10.0;

/// The tile size for a row `available` wide: as many tiles as fit at the
/// minimum width, stretched to fill the row (responsive, down to one
/// column).
fn place_tile_size(available: f32) -> Vec2 {
    let columns = ((available + PLACE_TILE_GAP) / (PLACE_TILE_MIN.x + PLACE_TILE_GAP))
        .floor()
        .max(1.0);
    let width = (available - PLACE_TILE_GAP * (columns - 1.0)) / columns;
    Vec2::new(
        width.clamp(PLACE_TILE_MIN.x.min(available), PLACE_TILE_MAX_WIDTH),
        PLACE_TILE_MIN.y,
    )
}

/// A drive or place on the This PC page: icon, name, path and (for
/// drives) how full it is. The whole tile is clickable.
fn place_tile(
    ui: &mut Ui,
    size: Vec2,
    icon: &str,
    label: &str,
    path: &Path,
    usage: Option<(f32, String)>,
) -> egui::Response {
    let frame = egui::Frame::NONE
        .fill(pal().sidebar)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10));
    // A fixed size, claimed before drawing, so a wrapping row puts every
    // tile at the same height and wraps one that wouldn't fit (laid out as
    // they were drawn, tiles stepped down and ran off the right edge).
    let inner = ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
        frame.show(ui, |ui| {
            ui.set_width(size.x - 20.0);
            ui.set_height(size.y - 20.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).size(FONT_SIZE_LARGE * 1.4));
                ui.vertical(|ui| {
                    ui.label(RichText::new(label).color(pal().text).strong());
                    if let Some((used, text)) = &usage {
                        let fill = if *used > 0.9 {
                            Color32::from_rgb(220, 60, 60)
                        } else if *used > 0.75 {
                            pal().warning
                        } else {
                            pal().accent
                        };
                        ui.add(
                            egui::ProgressBar::new(*used)
                                .desired_width((size.x - 80.0).max(60.0))
                                .desired_height(6.0)
                                .fill(fill),
                        );
                        ui.label(
                            RichText::new(text)
                                .color(pal().text_secondary)
                                .size(FONT_SIZE_SMALL),
                        );
                    } else {
                        ui.label(
                            RichText::new(path.to_string_lossy())
                                .color(pal().text_secondary)
                                .size(FONT_SIZE_SMALL),
                        );
                    }
                });
            });
        })
    });
    // Over the whole tile, after its contents, so it takes the clicks.
    let resp = ui
        .interact(
            inner.response.rect,
            ui.scope_id().with(("tile", path)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(path.to_string_lossy());
    if resp.hovered() {
        ui.painter().rect_stroke(
            inner.response.rect,
            CornerRadius::same(8),
            Stroke::new(1.0, pal().accent),
            egui::StrokeKind::Inside,
        );
    }
    resp
}

/// A drive in the navigation pane; `lead` leaves room for the tree arrow.
fn drive_item(ui: &mut Ui, drive: &DriveInfo, current: bool, lead: f32) -> egui::Response {
    let (fg, _) = crate::theme::text_on(current);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
    let bg = if current {
        pal().selected
    } else if response.hovered() {
        pal().hover
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(6), bg);
    let icon = match drive.drive_type {
        DriveType::Removable => "💾",
        DriveType::Network => "🌐",
        DriveType::CDRom => "💿",
        _ => "🖴",
    };
    painter.text(
        rect.left_center() + Vec2::new(10.0 + lead, 0.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(FONT_SIZE_LARGE),
        fg,
    );
    let galley = elided(
        painter,
        &drive.label,
        FONT_SIZE_SMALL,
        fg,
        rect.width() - 44.0 - lead,
        1,
        Align::Min,
    );
    painter.galley(
        Pos2::new(rect.left() + 34.0 + lead, rect.top() + 5.0),
        galley,
        fg,
    );

    let mut tooltip = drive.path.to_string_lossy().to_string();
    if let (Some(total), Some(free)) = (drive.total_space, drive.free_space) {
        let used = if total > 0 {
            (total - free) as f32 / total as f32
        } else {
            0.0
        };
        let bar = Rect::from_min_size(
            rect.left_bottom() + Vec2::new(34.0 + lead, -12.0),
            Vec2::new(rect.width() - 44.0 - lead, 5.0),
        );
        painter.rect_filled(bar, CornerRadius::same(2), pal().track);
        let fill = if used > 0.9 {
            Color32::from_rgb(220, 60, 60)
        } else if used > 0.75 {
            pal().warning
        } else {
            pal().accent
        };
        painter.rect_filled(
            Rect::from_min_size(bar.min, Vec2::new(bar.width() * used, bar.height())),
            CornerRadius::same(2),
            fill,
        );
        tooltip = format!(
            "{tooltip}\n{} free of {}",
            format_bytes(free),
            format_bytes(total)
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, current, &drive.label)
    });
    response.on_hover_text(tooltip)
}

fn select_all_text(ctx: &egui::Context, id: Id, text: &str) {
    select_text_range(ctx, id, 0, text.chars().count());
}

fn select_text_range(ctx: &egui::Context, id: Id, start: usize, end: usize) {
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(start),
                egui::text::CCursor::new(end),
            )));
        state.store(ctx, id);
    }
}

pub(crate) fn expand_home(typed: &str) -> PathBuf {
    match (typed.strip_prefix('~'), crate::file_system::home_dir()) {
        (Some(rest), Some(home))
            if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') =>
        {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(typed),
    }
}

/// Default drag-and-drop rule: dropping on the same drive moves, onto another drive
/// copies. Ctrl forces a copy, Shift forces a move.
fn drop_mode(src: &Path, dest: &Path, mods: Modifiers) -> PasteMode {
    if mods.command {
        PasteMode::Copy
    } else if mods.shift || same_volume(src, dest) {
        PasteMode::Move
    } else {
        PasteMode::Copy
    }
}

#[cfg(unix)]
fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_volume(a: &Path, b: &Path) -> bool {
    // Compare drive prefixes ("C:", "\\server\share").
    a.components().next() == b.components().next()
}

/// Check boxes for a file's permissions (Unix: read/write/run for owner,
/// group and others; Windows: Read-only), applied as they're clicked.
fn edit_permissions(ui: &mut Ui, path: &Path, d: &mut properties::Details) -> Result<(), String> {
    #[cfg(unix)]
    if let Some(mode) = d.mode {
        ui.add_space(8.0);
        ui.label(RichText::new("Permissions").color(pal().text_secondary));
        let mut changed = mode;
        egui::Grid::new("permission_bits")
            .num_columns(4)
            .spacing([14.0, 4.0])
            .show(ui, |ui| {
                ui.label("");
                for h in ["Read", "Write", if path.is_dir() { "Open" } else { "Run" }] {
                    ui.label(RichText::new(h).color(pal().text_secondary));
                }
                ui.end_row();
                for (who, shift) in [("Owner", 6), ("Group", 3), ("Others", 0)] {
                    ui.label(who);
                    for bit in [4, 2, 1] {
                        let mask = bit << shift;
                        let mut on = changed & mask != 0;
                        if ui.checkbox(&mut on, "").changed() {
                            changed ^= mask;
                        }
                    }
                    ui.end_row();
                }
            });
        if changed != mode {
            properties::set_mode(path, changed)?;
            d.mode = Some(changed);
            d.permissions = Some(format!(
                "{} ({changed:o})",
                properties::mode_string(changed)
            ));
        }
    }
    #[cfg(windows)]
    if let Some(read_only) = d.read_only {
        let mut on = read_only;
        ui.add_space(8.0);
        if ui.checkbox(&mut on, "Read-only").changed() {
            properties::set_read_only(path, on)?;
            d.read_only = Some(on);
        }
    }
    let _ = (path, d);
    Ok(())
}

/// " · 1.2 GB of 4.0 GB · 85 MB/s · 42 s left" for a running copy.
fn job_bytes_text(bytes: u64, total: Option<u64>, elapsed: Duration, paused: bool) -> String {
    let mut text = match total {
        Some(total) if total >= bytes => {
            format!(" · {} of {}", format_bytes(bytes), format_bytes(total))
        }
        _ => format!(" · {}", format_bytes(bytes)),
    };
    if paused {
        return text + " · paused";
    }
    let secs = elapsed.as_secs_f64();
    if secs >= 1.0 {
        let speed = bytes as f64 / secs;
        text += &format!(" · {}/s", format_bytes(speed as u64));
        if let Some(total) = total.filter(|t| *t > bytes && speed > 0.0) {
            let left = ((total - bytes) as f64 / speed).ceil() as u64;
            text += &match left {
                0..60 => format!(" · {left} s left"),
                60..3600 => format!(" · {} min left", left.div_ceil(60)),
                _ => format!(" · {:.1} h left", left as f64 / 3600.0),
            };
        }
    }
    text
}

fn clipboard_text(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Accept paths copied from other apps: plain paths (one per line), or the
/// `file://` URI lists that Nautilus, Nemo, Dolphin etc. put on the clipboard.
fn parse_clipboard_paths(text: &str) -> Option<(Vec<PathBuf>, PasteMode)> {
    let mut mode = PasteMode::Copy;
    let mut paths = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match line {
            "x-special/nautilus-clipboard" | "copy" => continue,
            "cut" => {
                mode = PasteMode::Move;
                continue;
            }
            _ => {}
        }
        let path = match line.strip_prefix("file://") {
            Some(uri) => PathBuf::from(percent_decode(uri)),
            None => PathBuf::from(line),
        };
        if !path.is_absolute() || path.symlink_metadata().is_err() {
            return None; // it's ordinary text, not a list of files
        }
        paths.push(path);
    }
    (!paths.is_empty()).then_some((paths, mode))
}

pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            // Work on bytes: slicing the &str here could split a UTF-8 character.
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Folder order: folders first, then by `by` with names as the tiebreak.
pub(crate) fn sort_items(items: &mut [FileItem], by: SortBy, ascending: bool) {
    items.sort_by(|a, b| {
        // Folders first, regardless of direction
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            let cmp = match by {
                SortBy::Name => CmpOrdering::Equal,
                SortBy::Size => a.size.cmp(&b.size),
                SortBy::Modified => a.modified.cmp(&b.modified),
                SortBy::Type => a.extension.cmp(&b.extension),
            }
            .then_with(|| natural_cmp(&a.name, &b.name));
            if ascending {
                cmp
            } else {
                cmp.reverse()
            }
        })
    });
}

/// "Logical" ordering: "file2" sorts before "file10".
pub fn natural_cmp(a: &str, b: &str) -> CmpOrdering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return CmpOrdering::Equal,
            (None, Some(_)) => return CmpOrdering::Less,
            (Some(_), None) => return CmpOrdering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take_num = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (take_num(&mut a), take_num(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta
                    .len()
                    .cmp(&tb.len())
                    .then_with(|| ta.cmp(tb))
                    .then_with(|| na.len().cmp(&nb.len()));
                if ord != CmpOrdering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != CmpOrdering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

pub fn get_file_icon(item: &FileItem) -> &'static str {
    if item.is_dir {
        return "📁";
    }

    match item.extension.as_deref() {
        // Microsoft Office
        Some("doc") | Some("docx") | Some("rtf") | Some("odt") => "📝",
        Some("xls") | Some("xlsx") | Some("xlsm") | Some("ods") | Some("csv") => "📊",
        Some("ppt") | Some("pptx") | Some("odp") => "📽",
        Some("pdf") => "📑",

        // Text/Documents
        Some("txt") | Some("log") | Some("nfo") => "📃",
        Some("md") | Some("markdown") | Some("rst") => "📋",

        // Images
        Some("jpg") | Some("jpeg") | Some("png") | Some("bmp") | Some("webp") | Some("ico")
        | Some("tiff") | Some("tif") => "🖼",
        Some("heic") | Some("heif") => "🖼",
        Some("gif") => "🎞",
        Some("svg") | Some("psd") => "🎨",
        Some("ai") | Some("eps") => "✒",
        Some("raw") | Some("cr2") | Some("cr3") | Some("nef") | Some("arw") | Some("dng") => "📷",

        // Video
        Some("mp4") | Some("m4v") | Some("webm") | Some("avi") | Some("wmv") | Some("mkv")
        | Some("mov") | Some("flv") | Some("mpg") | Some("mpeg") => "🎬",

        // Audio
        Some("mp3") | Some("wav") | Some("aiff") | Some("flac") | Some("alac") | Some("ape")
        | Some("aac") | Some("m4a") | Some("ogg") | Some("opus") | Some("wma") => "🎵",
        Some("mid") | Some("midi") => "🎹",

        // Archives
        Some("zip") | Some("rar") | Some("7z") | Some("tar") | Some("gz") | Some("bz2")
        | Some("xz") | Some("zst") => "📦",
        Some("iso") | Some("img") => "💿",

        // Executables & Scripts
        Some("exe") | Some("appimage") => "⚡",
        Some("msi") | Some("msix") | Some("deb") | Some("rpm") => "📥",
        Some("bat") | Some("cmd") | Some("ps1") | Some("psm1") | Some("sh") | Some("bash")
        | Some("zsh") => "⌨",

        // Programming Languages
        Some("rs") => "🦀",
        Some("py") | Some("pyw") => "🐍",
        Some("html") | Some("htm") => "🌐",
        Some("css") | Some("scss") | Some("sass") | Some("less") => "🎨",
        Some("java") => "☕",
        Some("rb") => "💎",
        Some("js") | Some("mjs") | Some("cjs") | Some("ts") | Some("tsx") | Some("jsx")
        | Some("vue") | Some("svelte") | Some("c") | Some("h") | Some("cpp") | Some("cc")
        | Some("cxx") | Some("hpp") | Some("cs") | Some("kt") | Some("go") | Some("php")
        | Some("swift") | Some("lua") | Some("dart") | Some("zig") => "📜",

        // Config & Data
        Some("json") | Some("yaml") | Some("yml") | Some("toml") | Some("xml") => "📋",
        Some("ini") | Some("cfg") | Some("conf") | Some("config") => "⚙",
        Some("env") | Some("pem") | Some("crt") | Some("cer") | Some("key") | Some("pfx") => "🔐",
        Some("sql") | Some("db") | Some("sqlite") | Some("sqlite3") => "🗄",

        // System & Binary
        Some("dll") | Some("so") | Some("sys") | Some("drv") => "🔧",
        Some("lnk") | Some("url") | Some("desktop") => "🔗",

        // Fonts
        Some("ttf") | Some("otf") | Some("woff") | Some("woff2") => "🔤",

        // Ebooks
        Some("epub") | Some("mobi") | Some("azw") | Some("azw3") => "📚",

        _ => "📄",
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod ui_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort_orders_numbers_numerically() {
        let mut names = vec![
            "IMG_10.jpg",
            "img_2.jpg",
            "IMG_1.jpg",
            "IMG_010.jpg",
            "a",
            "B",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "a",
                "B",
                "IMG_1.jpg",
                "img_2.jpg",
                "IMG_10.jpg",
                "IMG_010.jpg"
            ]
        );
    }

    #[test]
    fn copy_progress_shows_counts_and_bytes() {
        let gb = 1u64 << 30;
        let text = job_bytes_text(gb, Some(4 * gb), Duration::from_secs(10), false);
        assert!(text.contains(" of "), "{text}");
        assert!(text.contains("/s"), "{text}");
        assert!(text.ends_with("30 s left"), "{text}");
        assert!(job_bytes_text(gb, None, Duration::from_secs(10), true).ends_with("paused"));
    }

    #[test]
    fn clipboard_paths_from_other_file_managers() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("my photo.jpg");
        std::fs::write(&f, b"x").unwrap();
        let uri = format!(
            "x-special/nautilus-clipboard\ncut\nfile://{}",
            f.to_string_lossy().replace(' ', "%20")
        );
        assert_eq!(
            parse_clipboard_paths(&uri),
            Some((vec![f.clone()], PasteMode::Move))
        );
        assert_eq!(
            parse_clipboard_paths(&f.to_string_lossy()),
            Some((vec![f], PasteMode::Copy))
        );
        assert_eq!(parse_clipboard_paths("just some text"), None);
    }

    #[test]
    fn percent_decoding_never_panics_on_multibyte_text() {
        assert_eq!(percent_decode("/tmp/%€"), "/tmp/%€");
        assert_eq!(percent_decode("/a%20b/%C3%A9"), "/a b/é");
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(parse_clipboard_paths("file:///tmp/%€"), None);
    }

    #[test]
    fn picture_folder_detection() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("{i}.jpg")), b"x").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        let fs = FileSystem::new(dir.path().to_path_buf(), false);
        assert!(looks_like_picture_folder(fs.get_items()));
    }
}
