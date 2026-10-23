// Ninimma modification: independent project branding and integration.
//! What Properties (Alt+Enter) shows: details of one item, and the total
//! size of folders or a selection, counted in the background.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Totals for a set of items, as counted so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
    /// Folders that couldn't be read (permissions); not counted.
    pub unreadable: u64,
    pub done: bool,
}

/// Counts sizes on a background thread; dropping it stops the count.
pub struct SizeCount {
    totals: Arc<Mutex<Totals>>,
    cancel: Arc<AtomicBool>,
}

impl SizeCount {
    /// Starts counting `paths` (folders recursively). Links aren't followed,
    /// so nothing is counted twice or outside what was chosen. `repaint` is
    /// called as the totals grow.
    pub fn start(paths: Vec<PathBuf>, repaint: impl Fn() + Send + 'static) -> Self {
        let totals = Arc::new(Mutex::new(Totals::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (t, c) = (totals.clone(), cancel.clone());
        let _ = std::thread::Builder::new()
            .name("properties-size".into())
            .spawn(move || {
                let mut local = Totals::default();
                let mut stack: Vec<PathBuf> = Vec::new();
                for p in paths {
                    match std::fs::symlink_metadata(&p) {
                        Ok(m) if m.is_dir() => stack.push(p),
                        Ok(m) => {
                            local.files += 1;
                            local.bytes += m.len();
                        }
                        Err(_) => {}
                    }
                }
                let mut seen = 0u64;
                while let Some(dir) = stack.pop() {
                    if c.load(Ordering::Relaxed) {
                        return;
                    }
                    local.folders += 1;
                    let Ok(entries) = std::fs::read_dir(&dir) else {
                        local.unreadable += 1;
                        continue;
                    };
                    for entry in entries.flatten() {
                        let Ok(m) = entry.metadata() else { continue };
                        if m.is_dir() {
                            stack.push(entry.path());
                        } else {
                            // Files and links (a link counts as itself).
                            local.files += 1;
                            local.bytes += m.len();
                        }
                        seen += 1;
                        if seen.is_multiple_of(2000) {
                            *t.lock().unwrap() = local.clone();
                            repaint();
                        }
                    }
                }
                local.done = true;
                *t.lock().unwrap() = local;
                repaint();
            });
        Self { totals, cancel }
    }

    pub fn totals(&self) -> Totals {
        self.totals.lock().unwrap().clone()
    }
}

impl Drop for SizeCount {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Details of one item.
#[derive(Clone, Debug, Default)]
pub struct Details {
    pub size: u64,
    /// Space it takes on disk (Unix; blocks used).
    pub on_disk: Option<u64>,
    pub created: Option<chrono::DateTime<chrono::Local>>,
    pub modified: Option<chrono::DateTime<chrono::Local>>,
    pub accessed: Option<chrono::DateTime<chrono::Local>>,
    /// e.g. "rw-r--r-- (644)" on Unix; "Read-only, Hidden" on Windows.
    pub permissions: Option<String>,
    /// Owner and group names (Unix).
    pub owner: Option<String>,
    /// Width and height, for images (read from the header only).
    pub dimensions: Option<(u32, u32)>,
    /// Where a link points.
    pub link_target: Option<PathBuf>,
    /// Length of a video, in seconds (needs ffprobe).
    pub duration: Option<f64>,
    /// Permission bits (Unix), editable in the dialog.
    pub mode: Option<u32>,
    /// The read-only attribute (Windows), editable in the dialog.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub read_only: Option<bool>,
}

pub fn details(path: &Path) -> Details {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Details::default();
    };
    let local = |t: std::io::Result<std::time::SystemTime>| t.ok().map(chrono::DateTime::from);
    let mut d = Details {
        size: meta.len(),
        created: local(meta.created()),
        modified: local(meta.modified()),
        accessed: local(meta.accessed()),
        link_target: meta
            .file_type()
            .is_symlink()
            .then(|| std::fs::read_link(path).ok())
            .flatten(),
        ..Details::default()
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = meta.mode() & 0o777;
        d.mode = (!meta.file_type().is_symlink()).then_some(mode);
        d.permissions = Some(format!("{} ({mode:o})", mode_string(mode)));
        d.on_disk = Some(meta.blocks() * 512);
        d.owner = Some(format!(
            "{} : {}",
            name_for_id("/etc/passwd", meta.uid()).unwrap_or_else(|| meta.uid().to_string()),
            name_for_id("/etc/group", meta.gid()).unwrap_or_else(|| meta.gid().to_string())
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let attrs = meta.file_attributes();
        d.read_only = Some(attrs & 0x1 != 0);
        let mut flags = Vec::new();
        if attrs & 0x1 != 0 {
            flags.push("Read-only");
        }
        if attrs & 0x2 != 0 {
            flags.push("Hidden");
        }
        if attrs & 0x4 != 0 {
            flags.push("System");
        }
        d.permissions = Some(if flags.is_empty() {
            "None".into()
        } else {
            flags.join(", ")
        });
    }
    if meta.is_file() {
        d.dimensions = image::ImageReader::open(path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.into_dimensions().ok());
        if crate::thumbnail::ThumbnailCache::is_video_file(path) {
            d.duration = video_duration(path);
        }
    }
    d
}

/// A video's length from its container, via ffprobe (if installed).
pub(crate) fn video_duration(path: &Path) -> Option<f64> {
    let ffprobe = crate::thumbnail::find_ffprobe()?;
    let out = std::process::Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let secs: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    (secs.is_finite() && secs > 0.0).then_some(secs)
}

/// "1:02:03" or "2:05", as players show it.
pub fn format_duration(secs: f64) -> String {
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Sets the permission bits (Unix).
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))
        .map_err(|e| format!("Couldn't change permissions: {e}"))
}

/// Sets or clears the read-only attribute (Windows).
#[cfg(windows)]
pub fn set_read_only(path: &Path, read_only: bool) -> Result<(), String> {
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("Couldn't read it: {e}"))?
        .permissions();
    perms.set_readonly(read_only);
    std::fs::set_permissions(path, perms).map_err(|e| format!("Couldn't change it: {e}"))
}

/// "rwxr-x---" for the permission bits.
#[cfg_attr(windows, allow(dead_code))] // Unix permissions
pub fn mode_string(mode: u32) -> String {
    (0..9)
        .map(|i| {
            let bit = 1 << (8 - i);
            if mode & bit == 0 {
                '-'
            } else {
                ['r', 'w', 'x'][i % 3]
            }
        })
        .collect()
}

/// A user or group name from a passwd/group-style file (Unix).
#[cfg(unix)]
fn name_for_id(file: &str, id: u32) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines().find_map(|line| {
        let mut parts = line.split(':');
        let name = parts.next()?;
        let _password = parts.next()?;
        (parts.next()?.parse::<u32>().ok()? == id).then(|| name.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_a_player() {
        assert_eq!(format_duration(125.4), "2:05");
        assert_eq!(format_duration(3723.0), "1:02:03");
    }

    #[cfg(unix)]
    #[test]
    fn permissions_can_be_changed_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("run.sh");
        std::fs::write(&file, "echo").unwrap();
        set_mode(&file, 0o750).unwrap();
        let d = details(&file);
        assert_eq!(d.mode, Some(0o750));
        assert!(d.permissions.unwrap().starts_with("rwxr-x---"));
    }
    use std::time::{Duration, Instant};

    fn finished(count: &SizeCount) -> Totals {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let t = count.totals();
            if t.done {
                return t;
            }
            assert!(Instant::now() < deadline, "count never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn folder_sizes_are_counted_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Trips");
        std::fs::create_dir_all(root.join("2025/December")).unwrap();
        std::fs::write(root.join("plan.txt"), vec![0u8; 1000]).unwrap();
        std::fs::write(root.join("2025/December/receipt.pdf"), vec![0u8; 2500]).unwrap();
        // A link to a big folder elsewhere is counted as a link, not followed.
        let big = dir.path().join("big");
        std::fs::create_dir(&big).unwrap();
        std::fs::write(big.join("huge.bin"), vec![0u8; 50_000]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&big, root.join("shortcut")).unwrap();

        let t = finished(&SizeCount::start(vec![root.clone()], || {}));
        assert_eq!(t.folders, 3, "Trips, 2025, December");
        assert!(t.bytes >= 3500 && t.bytes < 50_000, "{t:?}");
        assert_eq!(t.files, if cfg!(unix) { 3 } else { 2 });

        // A selection of a file and a folder adds up.
        let t = finished(&SizeCount::start(
            vec![root.join("plan.txt"), root.join("2025")],
            || {},
        ));
        assert_eq!((t.files, t.folders, t.bytes), (2, 2, 3500));
    }

    #[test]
    fn details_of_a_file_and_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.png");
        image::RgbImage::new(64, 48).save(&photo).unwrap();
        let d = details(&photo);
        assert_eq!(d.dimensions, Some((64, 48)));
        assert!(d.modified.is_some());
        assert!(d.permissions.is_some());
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "hello").unwrap();
        let d = details(&text);
        assert_eq!((d.size, d.dimensions), (5, None));
        assert_eq!(mode_string(0o754), "rwxr-xr--");
    }
}
