// Ninimma modification: independent project branding and integration.
use chrono::{DateTime, Local};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct DriveInfo {
    pub path: PathBuf,
    pub label: String,
    pub drive_type: DriveType,
    pub total_space: Option<u64>,
    pub free_space: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum DriveType {
    Fixed,
    Removable,
    Network,
    CDRom,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct FileItem {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<DateTime<Local>>,
    pub extension: Option<String>,
    pub is_hidden: bool,
    /// A cloud placeholder whose contents aren't on this computer (OneDrive
    /// Files On-Demand etc.); reading it would trigger a download.
    pub online_only: bool,
}

impl FileItem {
    pub fn new(path: PathBuf) -> Option<Self> {
        let link_meta = fs::symlink_metadata(&path).ok()?;
        let is_symlink = link_meta.file_type().is_symlink();
        // Follow symlinks for size/type, but still list broken links.
        let metadata = if is_symlink {
            fs::metadata(&path).unwrap_or(link_meta)
        } else {
            link_meta
        };

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        let is_dir = metadata.is_dir();
        let size = if is_dir { 0 } else { metadata.len() };
        let modified = metadata.modified().ok().map(DateTime::<Local>::from);
        let extension = if is_dir {
            None
        } else {
            path.extension().map(|e| e.to_string_lossy().to_lowercase())
        };
        let is_hidden = name.starts_with('.') || has_hidden_attribute(&metadata);
        let online_only = is_online_only(&metadata);

        Some(Self {
            path,
            name,
            is_dir,
            is_symlink,
            size,
            modified,
            extension,
            is_hidden,
            online_only,
        })
    }

    pub fn format_size(&self) -> String {
        if self.is_dir {
            String::new()
        } else {
            format_bytes(self.size)
        }
    }

    pub fn get_type_name(&self) -> String {
        if self.is_dir {
            return "File folder".to_string();
        }

        match self.extension.as_deref() {
            Some("txt") => "Text Document".to_string(),
            Some("md") => "Markdown Document".to_string(),
            Some("pdf") => "PDF Document".to_string(),
            Some("doc") | Some("docx") => "Word Document".to_string(),
            Some("xls") | Some("xlsx") => "Excel Spreadsheet".to_string(),
            Some("ppt") | Some("pptx") => "PowerPoint Presentation".to_string(),
            Some("jpg") | Some("jpeg") | Some("jpe") | Some("jfif") => "JPEG Image".to_string(),
            Some("png") => "PNG Image".to_string(),
            Some("gif") => "GIF Image".to_string(),
            Some("bmp") => "Bitmap Image".to_string(),
            Some("webp") => "WebP Image".to_string(),
            Some("heic") | Some("heif") => "HEIC Image".to_string(),
            Some("tif") | Some("tiff") => "TIFF Image".to_string(),
            Some("svg") => "SVG Image".to_string(),
            Some("mp4") | Some("mkv") | Some("avi") | Some("mov") | Some("wmv") | Some("webm") => {
                "Video File".to_string()
            }
            Some("mp3") | Some("wav") | Some("flac") | Some("aac") | Some("ogg") | Some("m4a")
            | Some("opus") => "Audio File".to_string(),
            Some("zip") => "ZIP Archive".to_string(),
            Some("rar") => "RAR Archive".to_string(),
            Some("7z") => "7-Zip Archive".to_string(),
            Some("exe") => "Application".to_string(),
            Some("msi") => "Windows Installer".to_string(),
            Some("dll") => "DLL File".to_string(),
            Some("rs") => "Rust Source File".to_string(),
            Some("py") => "Python File".to_string(),
            Some("js") => "JavaScript File".to_string(),
            Some("ts") => "TypeScript File".to_string(),
            Some("html") => "HTML Document".to_string(),
            Some("css") => "CSS Stylesheet".to_string(),
            Some("json") => "JSON File".to_string(),
            Some("xml") => "XML File".to_string(),
            Some("toml") => "TOML File".to_string(),
            Some("yaml") | Some("yml") => "YAML File".to_string(),
            Some(ext) => format!("{} File", ext.to_uppercase()),
            None => "File".to_string(),
        }
    }
}

#[cfg(windows)]
fn has_hidden_attribute(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(not(windows))]
fn has_hidden_attribute(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
pub(crate) fn is_online_only(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    metadata.file_attributes()
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

#[cfg(not(windows))]
pub(crate) fn is_online_only(_metadata: &fs::Metadata) -> bool {
    false
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}

pub struct FileSystem {
    current_path: PathBuf,
    items: Vec<FileItem>,
    history: Vec<PathBuf>,
    history_index: usize,
    load_time_ms: Option<f64>,
    load_error: Option<String>,
    show_hidden: bool,
    /// Incremented whenever `items` changes, so views can cache derived data.
    generation: u64,
    watcher: Option<RecommendedWatcher>,
    watched_path: Option<PathBuf>,
    changed_on_disk: Arc<AtomicBool>,
    repaint: Arc<Mutex<Option<egui::Context>>>,
    /// A folder read running in the background (network and cloud folders
    /// can take seconds; the window must never freeze while they load).
    loading: Option<PendingLoad>,
    load_seq: u64,
}

struct PendingLoad {
    seq: u64,
    rx: mpsc::Receiver<(u64, Result<Vec<FileItem>, String>, f64)>,
}

/// How long a load may run before the UI shows "Loading…" instead of
/// waiting; long enough that local folders appear in the same frame.
const SYNC_LOAD_WAIT: Duration = Duration::from_millis(120);

/// In tests, folders whose name contains this read slowly, to exercise the
/// background path without slowing other tests running in parallel.
#[cfg(test)]
pub const SLOW_TEST_FOLDER: &str = "__slow_folder__";

pub(crate) fn read_items(path: &Path, show_hidden: bool) -> Result<Vec<FileItem>, String> {
    #[cfg(test)]
    if path.to_string_lossy().contains(SLOW_TEST_FOLDER) {
        // Long enough that "didn't wait for it" is unambiguous even on slow CI machines.
        std::thread::sleep(Duration::from_millis(1500));
    }
    let entries = fs::read_dir(path).map_err(|e| format!("Can't open this folder: {e}"))?;
    Ok(entries
        .flatten()
        .filter_map(|entry| FileItem::new(entry.path()))
        .filter(|item| show_hidden || !item.is_hidden)
        .collect())
}

impl FileSystem {
    pub fn new(start: PathBuf, show_hidden: bool) -> Self {
        let mut fs = Self {
            current_path: start.clone(),
            items: Vec::new(),
            history: vec![start],
            history_index: 0,
            load_time_ms: None,
            load_error: None,
            show_hidden,
            generation: 0,
            watcher: None,
            watched_path: None,
            changed_on_disk: Arc::new(AtomicBool::new(false)),
            repaint: Arc::new(Mutex::new(None)),
            loading: None,
            load_seq: 0,
        };
        fs.start_load(true);
        fs
    }

    /// Lets the folder watcher wake the UI when files change.
    pub fn set_repaint_context(&self, ctx: &egui::Context) {
        let mut r = self.repaint.lock().unwrap();
        if r.is_none() {
            *r = Some(ctx.clone());
        }
    }

    pub fn get_current_path(&self) -> &Path {
        &self.current_path
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn set_path(&mut self, path: PathBuf) -> bool {
        if !path.is_dir() {
            return false;
        }
        if self.history_index + 1 < self.history.len() {
            self.history.truncate(self.history_index + 1);
        }
        if self.history.last() != Some(&path) {
            self.history.push(path.clone());
            self.history_index = self.history.len() - 1;
        }
        self.current_path = path;
        self.start_load(true);
        true
    }

    pub fn can_go_back(&self) -> bool {
        self.history_index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }

    pub fn go_back(&mut self) -> bool {
        if self.can_go_back() {
            self.history_index -= 1;
            self.current_path = self.history[self.history_index].clone();
            self.start_load(true);
            true
        } else {
            false
        }
    }

    pub fn go_forward(&mut self) -> bool {
        if self.can_go_forward() {
            self.history_index += 1;
            self.current_path = self.history[self.history_index].clone();
            self.start_load(true);
            true
        } else {
            false
        }
    }

    /// Re-read the current folder, keeping the current listing on screen
    /// until the new one is ready.
    pub fn load_directory(&mut self) {
        self.start_load(false);
    }

    /// Read the folder on a background thread. Waits briefly so that local
    /// folders still show up immediately; slower ones finish via `poll_load`.
    fn start_load(&mut self, navigated: bool) {
        self.load_seq += 1;
        let seq = self.load_seq;
        self.changed_on_disk.store(false, Ordering::Relaxed);
        if navigated {
            // Never show the previous folder's items under the new path.
            self.items.clear();
            self.load_error = None;
            self.generation += 1;
        }
        let (tx, rx) = mpsc::channel();
        let (path, show_hidden, repaint) = (
            self.current_path.clone(),
            self.show_hidden,
            self.repaint.clone(),
        );
        std::thread::spawn(move || {
            let start = Instant::now();
            let result = read_items(&path, show_hidden);
            let _ = tx.send((seq, result, start.elapsed().as_secs_f64() * 1000.0));
            if let Some(ctx) = repaint.lock().unwrap().as_ref() {
                ctx.request_repaint();
            }
        });
        self.loading = Some(PendingLoad { seq, rx });
        self.watch_current();
        if let Some(Ok(done)) = self
            .loading
            .as_ref()
            .map(|l| l.rx.recv_timeout(SYNC_LOAD_WAIT))
        {
            self.apply_load(done);
        }
    }

    /// Apply a finished background load, if any. Call once per frame;
    /// returns true if the listing changed.
    pub fn poll_load(&mut self) -> bool {
        let done = match &self.loading {
            Some(l) => match l.rx.try_recv() {
                Ok(done) => done,
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.loading = None;
                    return false;
                }
            },
            None => return false,
        };
        self.apply_load(done)
    }

    fn apply_load(&mut self, (seq, result, ms): (u64, Result<Vec<FileItem>, String>, f64)) -> bool {
        // A newer load (the user navigated on) supersedes this one.
        if self.loading.as_ref().map(|l| l.seq) != Some(seq) {
            return false;
        }
        self.loading = None;
        match result {
            Ok(items) => {
                self.items = items;
                self.load_error = None;
            }
            Err(e) => {
                self.items.clear();
                self.load_error = Some(e);
            }
        }
        self.load_time_ms = Some(ms);
        self.generation += 1;
        true
    }

    /// True while the current folder is still being read.
    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    fn watch_current(&mut self) {
        if self.watched_path.as_deref() == Some(self.current_path.as_path()) {
            return;
        }
        self.watcher = None;
        self.watched_path = None;
        let flag = self.changed_on_disk.clone();
        let repaint = self.repaint.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if event.kind.is_access() {
                return;
            }
            flag.store(true, Ordering::Relaxed);
            if let Some(ctx) = repaint.lock().unwrap().as_ref() {
                // Short delay coalesces bursts (e.g. a big copy) into one reload.
                ctx.request_repaint_after(std::time::Duration::from_millis(300));
            }
        });
        if let Ok(mut w) = watcher {
            if w.watch(&self.current_path, RecursiveMode::NonRecursive)
                .is_ok()
            {
                self.watcher = Some(w);
                self.watched_path = Some(self.current_path.clone());
            }
        }
    }

    /// True (once) if the watched folder changed on disk since the last load.
    pub fn take_changed_on_disk(&self) -> bool {
        self.changed_on_disk.swap(false, Ordering::Relaxed)
    }

    pub fn get_items(&self) -> &[FileItem] {
        &self.items
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn get_parent_path(&self) -> Option<PathBuf> {
        self.current_path.parent().map(|p| p.to_path_buf())
    }

    pub fn navigate_up(&mut self) -> bool {
        match self.get_parent_path() {
            Some(parent) => self.set_path(parent),
            None => false,
        }
    }

    pub fn get_load_time_ms(&self) -> Option<f64> {
        self.load_time_ms
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        if self.show_hidden != show {
            self.show_hidden = show;
            self.load_directory();
        }
    }

    pub fn is_showing_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Fairly slow on Windows (and can block on disconnected network drives),
    /// so call this from a background thread.
    pub fn get_drives() -> Vec<DriveInfo> {
        let mut drives = Vec::new();

        #[cfg(windows)]
        {
            #[link(name = "kernel32")]
            extern "system" {
                fn GetLogicalDrives() -> u32;
            }
            // Locked BitLocker volumes have a drive letter even though their
            // root cannot be read. Keep them visible so users can unlock them.
            let logical_drives = unsafe { GetLogicalDrives() };
            for letter in b'A'..=b'Z' {
                let drive_letter = letter as char;
                let path = PathBuf::from(format!("{}:\\", drive_letter));
                if logical_drives & (1 << (letter - b'A')) != 0 {
                    let (total_space, free_space) = Self::get_drive_space_windows(&path);
                    drives.push(DriveInfo {
                        label: Self::get_drive_label_windows(drive_letter),
                        drive_type: Self::get_drive_type_windows(drive_letter),
                        path,
                        total_space,
                        free_space,
                    });
                }
            }
        }

        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            let (total_space, free_space) = disk_space("/");
            drives.push(DriveInfo {
                path: PathBuf::from("/"),
                label: "File System (/)".to_string(),
                drive_type: DriveType::Fixed,
                total_space,
                free_space,
            });
            {
                let mounts = mount_table();
                for (_device, mount_point, fs_type) in parse_mounts(&mounts) {
                    let path = PathBuf::from(&mount_point);
                    let label = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| mount_point.clone());
                    let drive_type = if matches!(
                        fs_type.as_str(),
                        "nfs" | "nfs4" | "cifs" | "smb3" | "sshfs" | "fuse.sshfs"
                    ) {
                        DriveType::Network
                    } else if matches!(fs_type.as_str(), "iso9660" | "udf") {
                        DriveType::CDRom
                    } else if mount_point.starts_with("/media/")
                        || mount_point.starts_with("/run/media/")
                    {
                        DriveType::Removable
                    } else {
                        DriveType::Fixed
                    };
                    let (total_space, free_space) = disk_space(&mount_point);
                    drives.push(DriveInfo {
                        path,
                        label,
                        drive_type,
                        total_space,
                        free_space,
                    });
                }
            }
        }

        #[cfg(target_os = "macos")]
        {
            let (total_space, free_space) = disk_space("/");
            drives.push(DriveInfo {
                path: PathBuf::from("/"),
                label: "Macintosh HD".to_string(),
                drive_type: DriveType::Fixed,
                total_space,
                free_space,
            });
            if let Ok(entries) = fs::read_dir("/Volumes") {
                for entry in entries.flatten() {
                    let path = entry.path();
                    // The boot volume appears here as a symlink to "/".
                    if fs::canonicalize(&path)
                        .map(|p| p == Path::new("/"))
                        .unwrap_or(false)
                        || !path.is_dir()
                    {
                        continue;
                    }
                    let (total_space, free_space) = disk_space(&path.to_string_lossy());
                    drives.push(DriveInfo {
                        label: entry.file_name().to_string_lossy().to_string(),
                        path,
                        drive_type: DriveType::Removable,
                        total_space,
                        free_space,
                    });
                }
            }
        }

        drives
    }

    #[cfg(windows)]
    fn get_drive_type_windows(letter: char) -> DriveType {
        let path: Vec<u16> = format!("{}:\\", letter)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // GetDriveTypeW constants
        const DRIVE_REMOVABLE: u32 = 2;
        const DRIVE_FIXED: u32 = 3;
        const DRIVE_REMOTE: u32 = 4;
        const DRIVE_CDROM: u32 = 5;

        #[link(name = "kernel32")]
        extern "system" {
            fn GetDriveTypeW(lpRootPathName: *const u16) -> u32;
        }

        let drive_type = unsafe { GetDriveTypeW(path.as_ptr()) };

        match drive_type {
            DRIVE_REMOVABLE => DriveType::Removable,
            DRIVE_FIXED => DriveType::Fixed,
            DRIVE_REMOTE => DriveType::Network,
            DRIVE_CDROM => DriveType::CDRom,
            _ => DriveType::Unknown,
        }
    }

    #[cfg(windows)]
    fn get_drive_space_windows(path: &Path) -> (Option<u64>, Option<u64>) {
        let path_wide: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        #[link(name = "kernel32")]
        extern "system" {
            fn GetDiskFreeSpaceExW(
                lpDirectoryName: *const u16,
                lpFreeBytesAvailableToCaller: *mut u64,
                lpTotalNumberOfBytes: *mut u64,
                lpTotalNumberOfFreeBytes: *mut u64,
            ) -> i32;
        }

        let mut free_bytes: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free: u64 = 0;

        let result = unsafe {
            GetDiskFreeSpaceExW(
                path_wide.as_ptr(),
                &mut free_bytes,
                &mut total_bytes,
                &mut total_free,
            )
        };

        if result != 0 {
            (Some(total_bytes), Some(free_bytes))
        } else {
            (None, None)
        }
    }

    #[cfg(windows)]
    fn get_drive_label_windows(letter: char) -> String {
        let path: Vec<u16> = format!("{}:\\", letter)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut volume_name: [u16; 261] = [0; 261];
        let mut fs_name: [u16; 261] = [0; 261];
        let mut serial: u32 = 0;
        let mut max_component: u32 = 0;
        let mut flags: u32 = 0;

        #[link(name = "kernel32")]
        extern "system" {
            fn GetVolumeInformationW(
                lpRootPathName: *const u16,
                lpVolumeNameBuffer: *mut u16,
                nVolumeNameSize: u32,
                lpVolumeSerialNumber: *mut u32,
                lpMaximumComponentLength: *mut u32,
                lpFileSystemFlags: *mut u32,
                lpFileSystemNameBuffer: *mut u16,
                nFileSystemNameSize: u32,
            ) -> i32;
        }

        let result = unsafe {
            GetVolumeInformationW(
                path.as_ptr(),
                volume_name.as_mut_ptr(),
                261,
                &mut serial,
                &mut max_component,
                &mut flags,
                fs_name.as_mut_ptr(),
                261,
            )
        };

        if result != 0 {
            let label = String::from_utf16_lossy(&volume_name)
                .trim_end_matches('\0')
                .to_string();
            if label.is_empty() {
                format!("Local Disk ({}:)", letter)
            } else {
                format!("{} ({}:)", label, letter)
            }
        } else {
            format!("Drive ({}:)", letter)
        }
    }

    pub fn get_quick_access() -> Vec<(String, PathBuf)> {
        let mut items = Vec::new();
        let Some(home) = home_dir() else { return items };

        // On Linux/FreeBSD these folders may be renamed or localised; the XDG config says where they are.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        let xdg = fs::read_to_string(home.join(".config/user-dirs.dirs"))
            .map(|s| parse_user_dirs(&s, &home))
            .unwrap_or_default();

        for (name, folder, _xdg_key) in [
            ("Desktop", "Desktop", "XDG_DESKTOP_DIR"),
            ("Downloads", "Downloads", "XDG_DOWNLOAD_DIR"),
            ("Documents", "Documents", "XDG_DOCUMENTS_DIR"),
            ("Pictures", "Pictures", "XDG_PICTURES_DIR"),
            ("Music", "Music", "XDG_MUSIC_DIR"),
            ("Videos", "Videos", "XDG_VIDEOS_DIR"),
        ] {
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            let path = xdg
                .get(_xdg_key)
                .cloned()
                .unwrap_or_else(|| home.join(folder));
            #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
            let path = home.join(folder);
            // XDG maps unused folders to $HOME itself.
            if path != home && path.is_dir() {
                items.push((name.to_string(), path));
            }
        }
        items.push(("Home".to_string(), home));

        #[cfg(target_os = "macos")]
        {
            let applications = PathBuf::from("/Applications");
            if applications.exists() {
                items.push(("Applications".to_string(), applications));
            }
        }

        items
    }
}

/// Where "New Document" templates live: XDG's Templates folder on Linux,
/// Explorer's Templates folder on Windows, ~/Templates elsewhere.
pub fn templates_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        return std::env::var_os("APPDATA")
            .map(|a| {
                PathBuf::from(a)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Templates")
            })
            .filter(|p| p.is_dir());
    }
    #[allow(unreachable_code)]
    {
        let home = home_dir()?;
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        if let Ok(dirs) = fs::read_to_string(home.join(".config/user-dirs.dirs")) {
            if let Some(dir) = parse_user_dirs(&dirs, &home).remove("XDG_TEMPLATES_DIR") {
                // XDG points unused folders at $HOME itself.
                return (dir != home && dir.is_dir()).then_some(dir);
            }
        }
        Some(home.join("Templates")).filter(|p| p.is_dir())
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

#[cfg(unix)]
fn disk_space(path: &str) -> (Option<u64>, Option<u64>) {
    let Ok(c_path) = std::ffi::CString::new(path) else {
        return (None, None);
    };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 || stat.f_blocks == 0 {
        return (None, None);
    }
    let block = stat.f_frsize as u64;
    (
        Some(stat.f_blocks as u64 * block),
        Some(stat.f_bavail as u64 * block),
    )
}

/// The system's mounts in `/proc/self/mounts` format ("device mount-point
/// type options", spaces escaped as \040), on every platform that has
/// them, so the parsers below work the same everywhere.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn mount_table() -> String {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/self/mounts").unwrap_or_default()
    }
    #[cfg(target_os = "freebsd")]
    {
        freebsd_mount_table()
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        String::new()
    }
}

/// FreeBSD has no /proc/mounts; ask the kernel (getmntinfo) and translate
/// its filesystem names to the Linux ones the parsers know.
#[cfg(target_os = "freebsd")]
fn freebsd_mount_table() -> String {
    use std::ffi::CStr;
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // MNT_NOWAIT: don't block on unresponsive network mounts.
    let n = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if n <= 0 || buf.is_null() {
        return String::new();
    }
    // The buffer belongs to libc and stays valid until the next call.
    let mounts = unsafe { std::slice::from_raw_parts(buf, n as usize) };
    let field = |chars: &[libc::c_char]| {
        unsafe { CStr::from_ptr(chars.as_ptr()) }
            .to_string_lossy()
            .replace(' ', "\\040")
    };
    let mut out = String::new();
    for m in mounts {
        let fs_type = freebsd_fs_type(&field(&m.f_fstypename));
        out.push_str(&format!(
            "{} {} {} rw 0 0\n",
            field(&m.f_mntfromname),
            field(&m.f_mntonname),
            fs_type
        ));
    }
    out
}

/// FreeBSD filesystem names -> the Linux names used by the mount parsers.
#[cfg(any(target_os = "freebsd", test))]
fn freebsd_fs_type(name: &str) -> String {
    match name {
        "smbfs" => "cifs".to_string(),
        "nfs" => "nfs".to_string(),
        "cd9660" => "iso9660".to_string(),
        // FUSE mounts are reported as "fusefs.<subtype>", e.g. fusefs.sshfs.
        other => match other.strip_prefix("fusefs.") {
            Some(sub) => format!("fuse.{sub}"),
            None => other.to_string(),
        },
    }
}

/// Real, user-relevant mounts from /proc/self/mounts: block devices and
/// network shares, minus system partitions.
#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
fn parse_mounts(mounts: &str) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let (Some(device), Some(mount_point), Some(fs_type)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // /proc/mounts escapes spaces etc. as octal (\040).
        let mount_point = unescape_mount(mount_point);
        let is_network = matches!(fs_type, "nfs" | "nfs4" | "cifs" | "smb3" | "fuse.sshfs");
        let is_system = mount_point == "/"
            || [
                "/boot", "/snap/", "/var/", "/sys", "/proc", "/dev", "/run/", "/tmp", "/efi",
                "/usr", "/opt", "/srv",
            ]
            .iter()
            .any(|p| mount_point.starts_with(p) || mount_point == p.trim_end_matches('/'))
                && !mount_point.starts_with("/run/media/");
        let is_loop = device.starts_with("/dev/loop");
        // Network shares are listed under "Network & Cloud" instead.
        if !device.starts_with("/dev/")
            || is_network
            || is_system
            || is_loop
            || fs_type == "squashfs"
        {
            continue;
        }
        // Btrfs subvolumes etc. mount the same device several times; show it once.
        if out.iter().any(|(d, m, _)| d == device || *m == mount_point) {
            continue;
        }
        out.push((device.to_string(), mount_point, fs_type.to_string()));
    }
    out
}

#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
fn unescape_mount(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 4 <= bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            out.push(u8::from_str_radix(&s[i + 1..i + 4], 8).unwrap_or(b'?'));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
pub fn parse_user_dirs(contents: &str, home: &Path) -> std::collections::HashMap<String, PathBuf> {
    contents
        .lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once('=')?;
            if key.starts_with('#') {
                return None;
            }
            let value = value.trim().trim_matches('"');
            let path = match value.strip_prefix("$HOME") {
                Some(rest) => home.join(rest.trim_start_matches('/')),
                None => PathBuf::from(value),
            };
            Some((key.to_string(), path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounts_keep_user_drives_only() {
        let mounts = "\
/dev/nvme0n1p2 / ext4 rw 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
/dev/loop3 /snap/core/1 squashfs ro 0 0
tmpfs /run/user/1000 tmpfs rw 0 0
/dev/sdb1 /media/tom/USB\\040STICK vfat rw 0 0
/dev/sdc1 /run/media/tom/Photos exfat rw 0 0
/dev/sda1 /mnt/data ext4 rw 0 0
/dev/sda1 /mnt/data2 ext4 rw 0 0
server:/export /mnt/nas nfs4 rw 0 0
";
        let got: Vec<String> = parse_mounts(mounts)
            .into_iter()
            .map(|(_, m, _)| m)
            .collect();
        assert_eq!(
            got,
            ["/media/tom/USB STICK", "/run/media/tom/Photos", "/mnt/data"]
        );
    }

    #[test]
    fn freebsd_mounts_parse_like_linux_ones() {
        // What freebsd_mount_table() produces from getmntinfo().
        let mounts = format!(
            "zroot/ROOT/default / zfs rw 0 0\n\
             devfs /dev devfs rw 0 0\n\
             /dev/da0s1 /media/USB\\040DRIVE msdosfs rw 0 0\n\
             /dev/ada1p1 /mnt/data ufs rw 0 0\n\
             //tom@nas/photos /mnt/photos {} rw 0 0\n",
            freebsd_fs_type("smbfs")
        );
        let got: Vec<String> = parse_mounts(&mounts)
            .into_iter()
            .map(|(_, m, _)| m)
            .collect();
        assert_eq!(got, ["/media/USB DRIVE", "/mnt/data"]);
        assert_eq!(freebsd_fs_type("fusefs.sshfs"), "fuse.sshfs");
        assert_eq!(freebsd_fs_type("cd9660"), "iso9660");
    }

    #[test]
    fn user_dirs_resolve_home() {
        let home = Path::new("/home/a");
        let dirs = parse_user_dirs(
            "# c\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\nXDG_MUSIC_DIR=\"/data/music\"\n",
            home,
        );
        assert_eq!(dirs["XDG_PICTURES_DIR"], PathBuf::from("/home/a/Bilder"));
        assert_eq!(dirs["XDG_MUSIC_DIR"], PathBuf::from("/data/music"));
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlinks_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("missing"), dir.path().join("dangling"))
            .unwrap();
        let fs = FileSystem::new(dir.path().to_path_buf(), false);
        assert_eq!(fs.get_items().len(), 1);
        assert!(fs.get_items()[0].is_symlink);
    }

    #[test]
    fn slow_folders_load_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), "x").unwrap();
        let sub = dir.path().join(SLOW_TEST_FOLDER);
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("b"), "y").unwrap();
        let mut fsys = FileSystem::new(dir.path().to_path_buf(), false);
        assert_eq!(fsys.get_items().len(), 2, "fast folders load immediately");

        let start = Instant::now();
        fsys.set_path(sub.clone());
        assert!(
            start.elapsed() < Duration::from_millis(1000),
            "navigation doesn't block"
        );
        assert!(fsys.is_loading());
        assert!(
            fsys.get_items().is_empty(),
            "old folder's items aren't shown under the new path"
        );

        let deadline = Instant::now() + Duration::from_secs(5);
        while !fsys.poll_load() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!fsys.is_loading());
        assert_eq!(fsys.get_items()[0].name, "b");
    }

    #[test]
    fn history_starts_at_start_path() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileSystem::new(dir.path().to_path_buf(), false);
        assert!(!fs.can_go_back());
        assert!(!fs.can_go_forward());
    }
}
