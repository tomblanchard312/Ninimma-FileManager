// Ninimma modification: independent project branding and integration.
//! Index settings (saved with the app's other preferences, so nothing
//! secret goes here), resource limits, and what's never indexed.

use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexMode {
    /// Names, folders, types, sizes and dates. No file contents.
    Basic,
    /// Basic, plus the text inside documents, and when and where photos
    /// were taken. The recommended default.
    #[default]
    FullText,
    /// Full text, plus meaning-based matches from a local model (a later
    /// update).
    Smart,
}

impl IndexMode {
    pub fn label(self) -> &'static str {
        match self {
            IndexMode::Basic => "Basic: names, types, sizes and dates",
            IndexMode::FullText => {
                "Full text: also words inside documents, and when and where photos were taken"
            }
            IndexMode::Smart => "Smart: also meaning-based matches",
        }
    }

    /// Modes this build can index.
    pub fn is_available(self) -> bool {
        self != IndexMode::Smart || crate::embedding::is_built_in()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexSettings {
    /// Off until the user turns it on.
    pub enabled: bool,
    pub mode: IndexMode,
    /// Folders to index. Never every mounted drive: only what's listed.
    pub roots: Vec<PathBuf>,
    /// Folders never indexed, on top of the built-in list.
    pub exclusions: Vec<PathBuf>,
    /// Network shares in `roots` that the user allowed (others are skipped).
    pub network_roots: Vec<PathBuf>,
    pub index_hidden: bool,
    pub pause_on_battery: bool,
    pub worker_threads: usize,
    pub max_database_mb: usize,
    pub max_extract_bytes: u64,
    /// Size limit for the smart-search vectors.
    pub max_semantic_mb: usize,
    /// The user has read what smart search does (shown before first use).
    pub smart_acknowledged: bool,
    /// Read the text in photos of documents and in scanned PDFs (OCR).
    pub ocr: bool,
    /// Search photos and videos by what they show (needs smart search
    /// and the picture model).
    pub pictures: bool,
    /// Experimental: a small local model re-ranks searches written as
    /// sentences.
    pub understand_sentences: bool,
}

pub const MAX_WORKERS: usize = 2;

impl Default for IndexSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: IndexMode::FullText,
            roots: Vec::new(),
            exclusions: Vec::new(),
            network_roots: Vec::new(),
            index_hidden: false,
            pause_on_battery: true,
            worker_threads: 1,
            max_database_mb: 512,
            max_extract_bytes: 2 * 1024 * 1024,
            max_semantic_mb: 512,
            smart_acknowledged: false,
            ocr: false,
            pictures: true,
            understand_sentences: false,
        }
    }
}

impl IndexSettings {
    /// Settings as the indexer uses them: limits clamped, and roots and
    /// exclusions that are relative or contain `..` dropped, so they can't
    /// be used to reach somewhere unexpected.
    pub fn sanitized(&self) -> Self {
        let clean = |paths: &[PathBuf]| -> Vec<PathBuf> {
            // Rebuilt from components, which also drops any `.` parts.
            let mut out: Vec<PathBuf> = paths
                .iter()
                .filter(|p| is_clean_absolute(p))
                .map(|p| p.components().collect())
                .collect();
            out.sort();
            out.dedup();
            out
        };
        Self {
            roots: clean(&self.roots),
            exclusions: clean(&self.exclusions),
            network_roots: clean(&self.network_roots),
            worker_threads: self.worker_threads.clamp(1, MAX_WORKERS),
            max_database_mb: self.max_database_mb.clamp(16, 64 * 1024),
            max_extract_bytes: self.max_extract_bytes.clamp(4096, 64 * 1024 * 1024),
            max_semantic_mb: self.max_semantic_mb.clamp(16, 64 * 1024),
            ..self.clone()
        }
    }

    /// The default places to offer when indexing is first turned on: the
    /// user's own document folders, not their whole home folder.
    pub fn suggested_roots() -> Vec<PathBuf> {
        // Tests must never index the real home folder.
        if cfg!(test) {
            return Vec::new();
        }
        let Some(home) = crate::file_system::home_dir() else {
            return Vec::new();
        };
        [
            "Documents",
            "Desktop",
            "Pictures",
            "Music",
            "Videos",
            "Downloads",
        ]
        .iter()
        .map(|d| home.join(d))
        .filter(|p| p.is_dir())
        .collect()
    }
}

fn is_clean_absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

/// Folder names skipped wherever they appear: version control, build
/// output, caches, credentials and virtual machines.
const EXCLUDED_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "bin",
    "obj",
    "__pycache__",
    ".venv",
    ".cache",
    ".ssh",
    ".gnupg",
    ".password-store",
    "keyrings",
    ".mozilla",
    ".thunderbird",
    "google-chrome",
    "chromium",
    "BraveSoftware",
    "Code Cache",
    "GPUCache",
    ".npm",
    ".cargo",
    ".rustup",
    ".gradle",
    ".m2",
    ".nuget",
    ".docker",
    "containers",
    "VirtualBox VMs",
    ".vagrant.d",
    "lost+found",
    "$RECYCLE.BIN",
    "System Volume Information",
    ".Trash",
    ".Trashes",
    ".Spotlight-V100",
    ".fseventsd",
];

/// Paths under the home folder that hold browser profiles, credentials,
/// caches or containers.
const EXCLUDED_HOME_PATHS: &[&str] = &[
    ".local/share/keyrings",
    ".local/share/containers",
    ".local/share/Trash",
    ".local/share/flatpak",
    ".local/share/Steam",
    ".config/google-chrome",
    ".config/chromium",
    ".config/BraveSoftware",
    ".config/microsoft-edge",
    ".var/app",
    "snap",
    "Library/Caches",
    "Library/Keychains",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Firefox",
    "Library/Mail",
    "AppData/Local/Temp",
    "AppData/Local/Microsoft",
    "AppData/Local/Google",
    "AppData/Local/Packages",
    "AppData/Roaming/Microsoft/Credentials",
    "AppData/Roaming/Microsoft/Protect",
    "AppData/Roaming/Mozilla",
];

/// System folders, never indexed even if a root contains them.
const EXCLUDED_SYSTEM_PATHS: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/tmp",
    "/var/tmp",
    "/var/cache",
    "/var/lib",
    "/boot",
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc",
    "/snap",
    "/System",
    "/Library",
    "/private",
    "/Applications",
    "C:\\Windows",
    "C:\\Program Files",
    "C:\\Program Files (x86)",
    "C:\\ProgramData",
];

/// Files skipped entirely: disk images and encrypted containers.
const EXCLUDED_EXTENSIONS: &[&str] = &[
    "iso",
    "img",
    "vhd",
    "vhdx",
    "vmdk",
    "vdi",
    "qcow2",
    "dmg",
    "sparseimage",
    "sparsebundle",
    "hc",
    "tc",
    "luks",
    "kdbx",
    "kdb",
    "keychain",
    "keychain-db",
];

/// Decides what the scanner skips. Paths are compared component by
/// component; the scanner never follows links and only builds paths by
/// joining real directory entries onto a root, so `..` and links can't be
/// used to slip past an exclusion.
pub struct Exclusions {
    dir_names: Vec<String>,
    prefixes: Vec<PathBuf>,
    index_hidden: bool,
}

impl Exclusions {
    /// The full rules: system folders and the temp folder, the home
    /// folder's private places, and the user's own exclusions.
    pub fn new(settings: &IndexSettings, home: Option<&Path>) -> Self {
        let mut system: Vec<PathBuf> = EXCLUDED_SYSTEM_PATHS.iter().map(PathBuf::from).collect();
        system.extend(std::env::temp_dir().canonicalize().ok());
        system.push(std::env::temp_dir());
        Self::with_system_paths(settings, home, system)
    }

    /// As `new`, with `system` in place of the system-folder list (tests
    /// scan inside the temp folder).
    pub fn with_system_paths(
        settings: &IndexSettings,
        home: Option<&Path>,
        system: Vec<PathBuf>,
    ) -> Self {
        let mut prefixes = system;
        if let Some(home) = home {
            prefixes.extend(EXCLUDED_HOME_PATHS.iter().map(|p| home.join(p)));
        }
        prefixes.extend(
            settings
                .exclusions
                .iter()
                .filter(|p| is_clean_absolute(p))
                .cloned(),
        );
        Self {
            dir_names: EXCLUDED_DIR_NAMES
                .iter()
                .map(|n| fold(OsStr::new(n)))
                .collect(),
            prefixes,
            index_hidden: settings.index_hidden,
        }
    }

    /// Whether to skip `path`, a folder (`is_dir`) or file named `name`.
    pub fn excludes(&self, path: &Path, name: &OsStr, is_dir: bool) -> bool {
        if !self.index_hidden && (is_hidden_name(name) || has_hidden_attribute(path)) {
            return true;
        }
        if is_dir {
            let folded = fold(name);
            if self.dir_names.contains(&folded) {
                return true;
            }
        } else if let Some(ext) = Path::new(name).extension() {
            let ext = fold(ext);
            if EXCLUDED_EXTENSIONS.iter().any(|e| *e == ext) {
                return true;
            }
        }
        self.prefixes.iter().any(|p| starts_with_folded(path, p))
    }

    /// Whether a root itself lies inside an excluded location.
    pub fn excludes_root(&self, root: &Path) -> bool {
        self.prefixes.iter().any(|p| starts_with_folded(root, p))
    }
}

/// Windows marks hidden and system files (desktop.ini, thumbs.db) with
/// attributes instead of a leading dot.
#[cfg(windows)]
fn has_hidden_attribute(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_attributes() & (HIDDEN | SYSTEM) != 0)
}

#[cfg(not(windows))]
fn has_hidden_attribute(_path: &Path) -> bool {
    false
}

fn is_hidden_name(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// Names compare case-insensitively where the file system usually does.
fn fold(s: &OsStr) -> String {
    let s = s.to_string_lossy();
    if cfg!(any(windows, target_os = "macos")) {
        s.to_lowercase()
    } else {
        s.into_owned()
    }
}

fn starts_with_folded(path: &Path, prefix: &Path) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        let mut p = path.components();
        prefix.components().all(|c| {
            p.next()
                .is_some_and(|pc| fold(pc.as_os_str()) == fold(c.as_os_str()))
        })
    } else {
        path.starts_with(prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exclusions(extra: &[&str]) -> Exclusions {
        let settings = IndexSettings {
            exclusions: extra.iter().map(PathBuf::from).collect(),
            ..Default::default()
        };
        Exclusions::new(&settings, Some(Path::new("/home/a")))
    }

    fn skips(e: &Exclusions, path: &str, is_dir: bool) -> bool {
        let p = Path::new(path);
        e.excludes(p, p.file_name().unwrap(), is_dir)
    }

    #[test]
    fn sensitive_and_costly_places_are_skipped() {
        let e = exclusions(&[]);
        assert!(skips(&e, "/home/a/code/app/.git", true));
        assert!(skips(&e, "/home/a/code/app/node_modules", true));
        assert!(skips(&e, "/home/a/code/app/target", true));
        assert!(skips(&e, "/home/a/.ssh", true));
        assert!(skips(
            &e,
            "/home/a/.local/share/keyrings/login.keyring",
            false
        ));
        assert!(skips(&e, "/home/a/.config/google-chrome/Default", true));
        assert!(skips(&e, "/home/a/vm/disk.qcow2", false));
        assert!(skips(&e, "/home/a/secret.kdbx", false));
        assert!(skips(&e, "/proc/1", true));
        assert!(!skips(&e, "/home/a/Documents/report.txt", false));
        assert!(!skips(&e, "/home/a/Documents", true));
        // Only a folder named `target` is skipped, not a file.
        assert!(!skips(&e, "/home/a/Documents/target", false));
    }

    #[test]
    fn hidden_files_are_skipped_unless_asked_for() {
        assert!(skips(&exclusions(&[]), "/home/a/.bashrc", false));
        let settings = IndexSettings {
            index_hidden: true,
            ..Default::default()
        };
        let e = Exclusions::new(&settings, Some(Path::new("/home/a")));
        assert!(!skips(&e, "/home/a/.bashrc", false));
        // Hidden or not, credentials stay out.
        assert!(skips(&e, "/home/a/.ssh", true));
    }

    #[test]
    fn user_exclusions_cover_everything_below_them() {
        let e = exclusions(&["/home/a/Private"]);
        assert!(skips(&e, "/home/a/Private", true));
        assert!(skips(&e, "/home/a/Private/tax/2025.pdf", false));
        // A sibling that merely shares the prefix text isn't excluded.
        assert!(!skips(&e, "/home/a/PrivateNotes.txt", false));
    }

    #[test]
    fn exclusions_are_deterministic() {
        let a = exclusions(&["/home/a/x"]);
        let b = exclusions(&["/home/a/x"]);
        for path in ["/home/a/x/y", "/home/a/y", "/home/a/.git", "/home/a/z.iso"] {
            assert_eq!(skips(&a, path, false), skips(&b, path, false));
        }
    }

    #[test]
    fn sanitizing_drops_relative_and_dotdot_paths_and_clamps_limits() {
        let settings = IndexSettings {
            roots: vec![
                "/home/a/Documents".into(),
                "relative".into(),
                "/home/a/../b".into(),
            ],
            exclusions: vec!["/home/a/./x".into(), "/home/a/y".into()],
            worker_threads: 99,
            max_database_mb: 0,
            ..Default::default()
        };
        let s = settings.sanitized();
        assert_eq!(s.roots, vec![PathBuf::from("/home/a/Documents")]);
        assert_eq!(
            s.exclusions,
            vec![PathBuf::from("/home/a/x"), PathBuf::from("/home/a/y")]
        );
        assert_eq!(s.worker_threads, MAX_WORKERS);
        assert_eq!(s.max_database_mb, 16);
    }

    #[test]
    fn settings_from_an_older_version_get_safe_defaults() {
        // An older release saved no index settings at all.
        let s: IndexSettings = serde_json_like("{}");
        assert!(!s.enabled);
        assert!(s.pause_on_battery);
        assert_eq!(s.worker_threads, 1);
        assert_eq!(s.max_database_mb, 512);
    }

    /// eframe saves settings with RON; this parses the same way.
    fn serde_json_like(text: &str) -> IndexSettings {
        ron::from_str(text.replace('{', "(").replace('}', ")").as_str()).unwrap()
    }
}
