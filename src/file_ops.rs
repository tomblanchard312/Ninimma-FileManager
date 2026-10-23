// Ninimma modification: independent project branding and integration.
use crate::undo::{self, Step};
use std::cell::RefCell;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// What to do when a pasted or dropped item's name already exists in the
/// destination (Explorer's "Replace or Skip Files" prompt).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conflict {
    /// Overwrite. Replaced files go to the trash; folders are merged.
    Replace,
    Skip,
    /// Give the incoming item a new name: "photo (2).jpg".
    KeepBoth,
}

/// Names in `paths` that already exist in `dest_dir` and would need a
/// decision. Copies into their own folder don't count: they're always
/// "- Copy", as in Explorer.
pub fn conflicts(paths: &[PathBuf], dest_dir: &Path) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|p| p.parent() != Some(dest_dir))
        .filter_map(|p| {
            let target = dest_dir.join(p.file_name()?);
            target.symlink_metadata().is_ok().then_some(target)
        })
        .collect()
}

/// Copy or move `src` into `dest_dir`, resolving a name clash with
/// `conflict`. Each change is appended to `steps` as it happens, so even a
/// partly failed operation can be undone.
pub fn paste_into(
    src: &Path,
    dest_dir: &Path,
    mode: PasteMode,
    conflict: Conflict,
    steps: &mut Vec<Step>,
) -> Result<(), String> {
    let name = src.file_name().ok_or("Can't paste a drive root")?;
    if mode == PasteMode::Move && src.parent() == Some(dest_dir) {
        return Ok(()); // moving into the same folder is a no-op
    }
    if is_inside(dest_dir, src) {
        return Err(format!("Can't put {} inside itself", display_name(src)));
    }
    let target = dest_dir.join(name);
    if target.symlink_metadata().is_err() {
        match transfer(src, &target, mode, steps) {
            // Someone else created it just now: treat it as a clash below.
            Err(e) if e.contains(ALREADY_EXISTS) => {}
            other => return other,
        }
    }
    let name = name.to_string_lossy();
    if src.parent() == Some(dest_dir) {
        return transfer_to_free_name(src, mode, steps, || unique_copy_name(dest_dir, &name));
    }
    match conflict {
        Conflict::Skip => Ok(()),
        Conflict::KeepBoth => {
            let (stem, ext) = if src.is_dir() {
                (name.as_ref(), "")
            } else {
                split_name(&name)
            };
            transfer_to_free_name(src, mode, steps, || {
                unique_name(dest_dir, stem, ext, |n| format!(" ({n})"))
            })
        }
        Conflict::Replace if is_real_dir(src) && is_real_dir(&target) => {
            // Folder onto folder: merge, like Explorer.
            let entries = fs::read_dir(src)
                .map_err(|e| format!("Couldn't read {}: {e}", display_name(src)))?;
            for entry in entries {
                let entry =
                    entry.map_err(|e| format!("Couldn't read {}: {e}", display_name(src)))?;
                paste_into(&entry.path(), &target, mode, Conflict::Replace, steps)?;
            }
            // Only succeeds once everything moved out; recorded so undo/redo
            // reproduce the whole merge (and so an empty folder's move is undoable).
            if mode == PasteMode::Move && fs::remove_dir(src).is_ok() {
                steps.push(Step::Uncreated {
                    path: src.to_path_buf(),
                });
            }
            Ok(())
        }
        Conflict::Replace => {
            steps.push(undo::trash(&target)?);
            transfer(src, &target, mode, steps)
        }
    }
}

fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

/// Transfer to a fresh name, choosing again if another writer takes the
/// chosen name first (copies and moves never overwrite).
fn transfer_to_free_name(
    src: &Path,
    mode: PasteMode,
    steps: &mut Vec<Step>,
    mut pick: impl FnMut() -> PathBuf,
) -> Result<(), String> {
    for _ in 0..8 {
        match transfer(src, &pick(), mode, steps) {
            Err(e) if e.contains(ALREADY_EXISTS) => continue,
            other => return other,
        }
    }
    Err(format!(
        "Couldn't find a free name for {}",
        display_name(src)
    ))
}

fn transfer(src: &Path, dest: &Path, mode: PasteMode, steps: &mut Vec<Step>) -> Result<(), String> {
    match mode {
        PasteMode::Copy => {
            copy_new(src, dest)?;
            steps.push(Step::Copied {
                src: src.to_path_buf(),
                dest: dest.to_path_buf(),
            });
        }
        PasteMode::Move => {
            if let Err(e) = move_path(src, dest) {
                // A cross-drive folder move may keep its complete copy even
                // though removing the original failed; make it undoable.
                if e.contains(COPY_KEPT) {
                    steps.push(Step::Copied {
                        src: src.to_path_buf(),
                        dest: dest.to_path_buf(),
                    });
                }
                return Err(e);
            }
            steps.push(Step::Moved {
                from: src.to_path_buf(),
                to: dest.to_path_buf(),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteMode {
    Copy,
    Move,
}

pub struct FileOperations;

impl FileOperations {
    /// Move to the Recycle Bin (Windows), Trash (macOS / freedesktop).
    pub fn move_to_trash(paths: &[PathBuf]) -> Result<(), String> {
        trash::delete_all(paths).map_err(|e| format!("Couldn't move to trash: {e}"))
    }

    /// Permanently delete. Callers must confirm with the user first.
    pub fn delete_permanently(path: &Path) -> Result<(), String> {
        let meta = fs::symlink_metadata(path).map_err(|e| format!("Couldn't delete: {e}"))?;
        if meta.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        }
        .map_err(|e| format!("Couldn't delete {}: {e}", display_name(path)))
    }

    /// Copy or move `src` into `dest_dir`, keeping both if the name is taken
    /// ("photo - Copy.jpg" in the same folder, "photo (2).jpg" elsewhere).
    /// Returns the final path.
    #[cfg(test)]
    pub fn paste(src: &Path, dest_dir: &Path, mode: PasteMode) -> Result<PathBuf, String> {
        let mut steps = Vec::new();
        paste_into(src, dest_dir, mode, Conflict::KeepBoth, &mut steps)?;
        Ok(match steps.last() {
            Some(Step::Copied { dest, .. }) | Some(Step::Moved { to: dest, .. }) => dest.clone(),
            _ => src.to_path_buf(),
        })
    }

    pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
        validate_file_name(new_name)?;
        let parent = path.parent().ok_or("Can't rename a drive root")?;
        let new_path = parent.join(new_name);
        // A case-only rename ("photo.JPG" -> "photo.jpg") on a case-insensitive
        // filesystem finds the file itself. On case-sensitive ones the other
        // name may be a different file, which must not be replaced.
        if new_path.symlink_metadata().is_ok() && !same_entry(path, &new_path) {
            return Err(format!("\"{new_name}\" already exists"));
        }
        if same_entry(path, &new_path) {
            fs::rename(path, &new_path).map_err(|e| format!("Couldn't rename: {e}"))?;
        } else {
            rename_noreplace(path, &new_path).map_err(|e| format!("Couldn't rename: {e}"))?;
        }
        Ok(new_path)
    }

    /// Create an empty file such as "New file" or "New Text Document.txt"
    /// (numbered if taken) and return its path.
    pub fn create_empty_file(parent: &Path, stem: &str, ext: &str) -> Result<PathBuf, String> {
        for _ in 0..8 {
            let path = unique_name(parent, stem, ext, |n| format!(" ({n})"));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Couldn't create file: {e}")),
            }
        }
        Err("Couldn't find a free name".to_string())
    }

    /// Create a new document from a template, named after it
    /// ("LibreOffice Writer.odt", "LibreOffice Writer (2).odt", ...).
    pub fn create_from_template(template: &Path, parent: &Path) -> Result<PathBuf, String> {
        let name = template
            .file_name()
            .ok_or("Not a template")?
            .to_string_lossy()
            .to_string();
        let (stem, ext) = split_name(&name);
        for _ in 0..8 {
            let path = unique_name(parent, stem, ext, |n| format!(" ({n})"));
            match copy_new(template, &path) {
                Err(e) if e.contains(ALREADY_EXISTS) => continue,
                other => return other.map(|_| path),
            }
        }
        Err("Couldn't find a free name".to_string())
    }

    /// Create "New folder", "New folder (2)", ... and return its path.
    pub fn create_new_folder(parent: &Path) -> Result<PathBuf, String> {
        let path = unique_name(parent, "New folder", "", |n| format!(" ({n})"));
        fs::create_dir(&path).map_err(|e| format!("Couldn't create folder: {e}"))?;
        Ok(path)
    }
}

/// A template file, or a folder of templates (shown as a submenu).
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    /// Shown without its extension, as in Nautilus and Nemo.
    pub label: String,
    pub path: PathBuf,
    pub children: Vec<Template>,
}

/// The templates in `dir`, folders first, both sorted by name. Hidden files
/// are skipped; nesting is limited so a symlink loop can't recurse forever.
pub fn list_templates(dir: &Path) -> Vec<Template> {
    fn walk(dir: &Path, depth: usize) -> Vec<Template> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<Template> = entries
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|e| {
                let path = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                if path.is_dir() {
                    let children = if depth < 4 {
                        walk(&path, depth + 1)
                    } else {
                        Vec::new()
                    };
                    (!children.is_empty()).then_some(Template {
                        label: name,
                        path,
                        children,
                    })
                } else {
                    let label = split_name(&name).0.to_string();
                    Some(Template {
                        label,
                        path,
                        children: Vec::new(),
                    })
                }
            })
            .collect();
        out.sort_by(|a, b| {
            a.children
                .is_empty()
                .cmp(&b.children.is_empty())
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
        });
        out
    }
    walk(dir, 0)
}

const ALREADY_EXISTS: &str = "already exists";
const COPY_KEPT: &str = "but couldn't remove the original";

/// Rename several items as one all-or-nothing operation. Goes through
/// temporary names first, so names may swap between the items (a -> b,
/// b -> a); on any failure everything is put back.
pub fn rename_batch(pairs: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    for (from, to) in pairs {
        let vacated = pairs.iter().any(|(f, _)| f == to);
        if to.symlink_metadata().is_ok() && !vacated && !same_entry(from, to) {
            return Err(format!("\"{}\" {ALREADY_EXISTS}", display_name(to)));
        }
    }
    let mut temps: Vec<PathBuf> = Vec::new();
    let rollback = |temps: &[PathBuf], finals: &[PathBuf]| {
        for (f, t) in finals.iter().zip(temps) {
            let _ = rename_noreplace(f, t);
        }
        for (t, (orig, _)) in temps.iter().zip(pairs) {
            let _ = rename_noreplace(t, orig);
        }
    };
    for (i, (from, _)) in pairs.iter().enumerate() {
        let tmp = from.with_file_name(format!(".ninimma-rename-{}-{i}", std::process::id()));
        if let Err(e) = rename_noreplace(from, &tmp) {
            rollback(&temps, &[]);
            return Err(format!("Couldn't rename {}: {e}", display_name(from)));
        }
        temps.push(tmp);
    }
    let mut finals = Vec::new();
    for (tmp, (_, to)) in temps.iter().zip(pairs) {
        if let Err(e) = rename_noreplace(tmp, to) {
            rollback(&temps, &finals);
            return Err(format!("Couldn't rename to {}: {e}", display_name(to)));
        }
        finals.push(to.clone());
    }
    Ok(())
}

/// True if `a` and `b` are the same directory entry (not just equal names).
#[cfg(unix)]
pub fn same_entry(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
pub fn same_entry(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Is `dir` the folder `src` or somewhere inside it? Resolves symlinks, so a
/// link pointing back into `src` can't cause an endless copy.
fn is_inside(dir: &Path, src: &Path) -> bool {
    let src_is_dir = fs::symlink_metadata(src)
        .map(|m| m.is_dir())
        .unwrap_or(false);
    if !src_is_dir {
        return false;
    }
    match (fs::canonicalize(dir), fs::canonicalize(src)) {
        (Ok(d), Ok(s)) => d.starts_with(s),
        _ => dir.starts_with(src),
    }
}

/// Rename that fails instead of replacing an existing destination.
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = |p: &Path| {
            std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(std::io::Error::other)
        };
        let (f, t) = (c(from)?, c(to)?);
        let r = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                f.as_ptr(),
                libc::AT_FDCWD,
                t.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if r == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // Some filesystems don't support the flag; fall back below.
        if !matches!(err.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS)) {
            return Err(err);
        }
    }
    if to.symlink_metadata().is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            ALREADY_EXISTS,
        ));
    }
    fs::rename(from, to)
}

/// Move a file or folder without ever replacing something at `to`. Only a
/// genuine cross-drive move falls back to copy + delete; other failures
/// (permissions, locked files) are reported, not turned into a copy.
pub fn move_path(from: &Path, to: &Path) -> Result<(), String> {
    match rename_noreplace(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(format!("\"{}\" {ALREADY_EXISTS}", display_name(to)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_new(from, to)?;
            let is_file = fs::symlink_metadata(from)
                .map(|m| !m.is_dir())
                .unwrap_or(false);
            if let Err(e) = FileOperations::delete_permanently(from) {
                if is_file {
                    // Nothing was removed from the source, so undo the copy.
                    let _ = FileOperations::delete_permanently(to);
                    return Err(format!("Couldn't move {}: {e}", display_name(from)));
                }
                // Part of a folder may already be gone; keep the complete copy.
                return Err(format!("Copied {}, {COPY_KEPT}: {e}", display_name(from)));
            }
            Ok(())
        }
        Err(e) => Err(format!("Couldn't move {}: {e}", display_name(from))),
    }
}

/// Progress of a copy or move, shared with the window: bytes copied, the
/// total once counted, and Pause/Cancel from the user.
#[derive(Default)]
pub struct Meter {
    pub bytes: AtomicU64,
    pub total: AtomicU64,
    pub total_known: AtomicBool,
    pub paused: AtomicBool,
    pub cancelled: AtomicBool,
}

/// The error of a copy the user cancelled.
pub const CANCELLED: &str = "Cancelled";

thread_local! {
    static METER: RefCell<Option<Arc<Meter>>> = const { RefCell::new(None) };
}

/// Runs `f` with copies on this thread reporting to `meter`.
pub fn metered<T>(meter: Arc<Meter>, f: impl FnOnce() -> T) -> T {
    METER.with(|m| *m.borrow_mut() = Some(meter));
    let result = f();
    METER.with(|m| *m.borrow_mut() = None);
    result
}

/// Waits while paused; an error once cancelled.
fn check_meter() -> Result<(), String> {
    METER.with(|m| {
        let Some(meter) = &*m.borrow() else {
            return Ok(());
        };
        while meter.paused.load(Ordering::Relaxed) && !meter.cancelled.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if meter.cancelled.load(Ordering::Relaxed) {
            Err(CANCELLED.to_string())
        } else {
            Ok(())
        }
    })
}

fn count_bytes(n: u64) {
    METER.with(|m| {
        if let Some(meter) = &*m.borrow() {
            meter.bytes.fetch_add(n, Ordering::Relaxed);
        }
    })
}

/// Bytes in files under `path` (not following links), for the total.
pub fn size_of(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        fs::read_dir(path)
            .map(|entries| entries.flatten().map(|e| size_of(&e.path())).sum())
            .unwrap_or(0)
    } else {
        meta.len()
    }
}

/// Copy to a destination that must not exist yet, removing anything
/// half-copied if it fails.
pub fn copy_new(src: &Path, dest: &Path) -> Result<(), String> {
    let result = copy_recursive(src, dest);
    if let Err(e) = &result {
        // copy_recursive only ever creates `dest` fresh, so it's ours to remove.
        if !e.contains(ALREADY_EXISTS) {
            let _ = FileOperations::delete_permanently(dest);
        }
    }
    result
}

/// Exclusive file copy: fails if `dest` exists instead of overwriting it.
fn copy_file_new(src: &Path, dest: &Path) -> Result<(), String> {
    let mut from =
        fs::File::open(src).map_err(|e| format!("Couldn't copy {}: {e}", display_name(src)))?;
    let mut to = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("\"{}\" {ALREADY_EXISTS}", display_name(dest))
            } else {
                format!("Couldn't copy {}: {e}", display_name(src))
            }
        })?;
    // In chunks, to count progress and stop on Pause or Cancel.
    let mut buf = vec![0u8; 1 << 20];
    loop {
        check_meter()?;
        let n = from
            .read(&mut buf)
            .map_err(|e| format!("Couldn't copy {}: {e}", display_name(src)))?;
        if n == 0 {
            break;
        }
        to.write_all(&buf[..n])
            .map_err(|e| format!("Couldn't copy {}: {e}", display_name(src)))?;
        count_bytes(n as u64);
    }
    if let Ok(meta) = from.metadata() {
        let _ = to.set_permissions(meta.permissions());
        if let Ok(modified) = meta.modified() {
            let _ = to.set_modified(modified);
        }
    }
    Ok(())
}

pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Split "archive.tar.gz" style names into ("archive", ".tar.gz") for numbering.
/// Folders and dotfiles keep the whole name as the stem.
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => name.split_at(i),
    }
}

fn unique_copy_name(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = if dir.join(name).is_dir() {
        (name, "")
    } else {
        split_name(name)
    };
    unique_name(dir, &format!("{stem} - Copy"), ext, |n| format!(" ({n})"))
}

pub(crate) fn unique_name(
    dir: &Path,
    stem: &str,
    ext: &str,
    suffix: impl Fn(u32) -> String,
) -> PathBuf {
    let first = dir.join(format!("{stem}{ext}"));
    if first.symlink_metadata().is_err() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}{}{ext}", suffix(n))))
        .find(|p| p.symlink_metadata().is_err())
        .expect("some name is always free")
}

pub fn validate_file_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("A file name can't be empty".to_string());
    }
    if trimmed == "." || trimmed == ".." {
        return Err("That name is reserved".to_string());
    }
    let forbidden: &[char] = if cfg!(windows) {
        &['\\', '/', ':', '*', '?', '"', '<', '>', '|']
    } else {
        &['/', '\0']
    };
    if let Some(c) = name.chars().find(|c| forbidden.contains(c)) {
        return Err(format!("A file name can't contain \"{c}\""));
    }
    Ok(())
}

/// Copy files and folders without following symlinks (avoids loops and
/// duplicating whatever a link points at).
fn copy_recursive(src: &Path, dest: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(src)
        .map_err(|e| format!("Couldn't read {}: {e}", display_name(src)))?;
    if meta.file_type().is_symlink() {
        return copy_symlink(src, dest);
    }
    if meta.is_dir() {
        fs::create_dir(dest).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("\"{}\" {ALREADY_EXISTS}", display_name(dest))
            } else {
                format!("Couldn't create {}: {e}", display_name(dest))
            }
        })?;
        let entries =
            fs::read_dir(src).map_err(|e| format!("Couldn't read {}: {e}", display_name(src)))?;
        for entry in entries {
            check_meter()?;
            let entry = entry.map_err(|e| format!("Couldn't read {}: {e}", display_name(src)))?;
            copy_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        copy_file_new(src, dest)
    }
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<(), String> {
    let target = fs::read_link(src).map_err(|e| format!("Couldn't read link: {e}"))?;
    std::os::unix::fs::symlink(target, dest).map_err(|e| format!("Couldn't copy link: {e}"))
}

#[cfg(not(unix))]
fn copy_symlink(src: &Path, dest: &Path) -> Result<(), String> {
    // Creating symlinks on Windows needs special privileges; copy the target instead.
    if src.is_dir() {
        Err(format!("Skipped folder link {}", display_name(src)))
    } else {
        copy_file_new(src, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn paste_into_same_folder_makes_explorer_style_copies() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("photo.jpg");
        fs::write(&f, b"x").unwrap();
        FileOperations::paste(&f, dir.path(), PasteMode::Copy).unwrap();
        FileOperations::paste(&f, dir.path(), PasteMode::Copy).unwrap();
        assert_eq!(
            names(dir.path()),
            ["photo - Copy (2).jpg", "photo - Copy.jpg", "photo.jpg"]
        );
    }

    #[test]
    fn paste_copies_folders_recursively_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join("inner")).unwrap();
        fs::write(src.join("inner/a.txt"), b"new").unwrap();
        let dest = dir.path().join("dest");
        fs::create_dir_all(dest.join("src")).unwrap();
        fs::write(dest.join("src/keep.txt"), b"old").unwrap();

        let out = FileOperations::paste(&src, &dest, PasteMode::Copy).unwrap();
        assert_eq!(out, dest.join("src (2)"));
        assert_eq!(fs::read(out.join("inner/a.txt")).unwrap(), b"new");
        assert_eq!(fs::read(dest.join("src/keep.txt")).unwrap(), b"old");
    }

    #[test]
    fn cannot_paste_folder_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a");
        fs::create_dir_all(src.join("b")).unwrap();
        assert!(FileOperations::paste(&src, &src.join("b"), PasteMode::Copy).is_err());
        assert!(FileOperations::paste(&src, &src, PasteMode::Move).is_err());
    }

    #[test]
    fn conflict_policies() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&dest).unwrap();
        let src = src_dir.join("a.txt");
        fs::write(&src, "new").unwrap();
        fs::write(dest.join("a.txt"), "old").unwrap();
        assert_eq!(
            conflicts(std::slice::from_ref(&src), &dest),
            [dest.join("a.txt")]
        );

        let mut steps = Vec::new();
        paste_into(&src, &dest, PasteMode::Copy, Conflict::Skip, &mut steps).unwrap();
        assert!(steps.is_empty());
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "old");

        paste_into(&src, &dest, PasteMode::Copy, Conflict::KeepBoth, &mut steps).unwrap();
        assert_eq!(fs::read_to_string(dest.join("a (2).txt")).unwrap(), "new");
    }

    #[test]
    fn moving_an_empty_folder_onto_a_same_named_one_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a/X");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(dir.path().join("b/X")).unwrap();
        let mut steps = Vec::new();
        paste_into(
            &src,
            &dir.path().join("b"),
            PasteMode::Move,
            Conflict::Replace,
            &mut steps,
        )
        .unwrap();
        assert!(!src.exists());
        assert_eq!(steps, [Step::Uncreated { path: src }]);
    }

    #[test]
    fn rename_batch_swaps_and_refuses_outside_clashes() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        fs::write(&a, "A").unwrap();
        fs::write(&b, "B").unwrap();
        fs::write(&c, "C").unwrap();
        rename_batch(&[(a.clone(), b.clone()), (b.clone(), a.clone())]).unwrap();
        assert_eq!(
            (
                fs::read_to_string(&a).unwrap(),
                fs::read_to_string(&b).unwrap()
            ),
            ("B".into(), "A".into())
        );
        // "c" isn't part of the batch, so renaming onto it must fail and change nothing.
        assert!(rename_batch(&[(a.clone(), c.clone())]).is_err());
        assert_eq!(fs::read_to_string(&a).unwrap(), "B");
    }

    #[test]
    fn new_documents_from_templates_and_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let templates = dir.path().join("Templates");
        fs::create_dir_all(templates.join("Office")).unwrap();
        fs::write(templates.join("Letter.odt"), "letter").unwrap();
        fs::write(templates.join("Office/Budget.ods"), "budget").unwrap();
        fs::write(templates.join(".hidden"), "x").unwrap();
        let list = list_templates(&templates);
        assert_eq!(
            list.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(),
            ["Office", "Letter"]
        );
        assert_eq!(list[0].children[0].label, "Budget");

        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let a = FileOperations::create_from_template(&templates.join("Letter.odt"), &out).unwrap();
        let b = FileOperations::create_from_template(&templates.join("Letter.odt"), &out).unwrap();
        assert_eq!(a, out.join("Letter.odt"));
        assert_eq!(b, out.join("Letter (2).odt"));
        assert_eq!(fs::read_to_string(&b).unwrap(), "letter");

        let e1 = FileOperations::create_empty_file(&out, "New Text Document", ".txt").unwrap();
        let e2 = FileOperations::create_empty_file(&out, "New Text Document", ".txt").unwrap();
        assert_eq!(e2, out.join("New Text Document (2).txt"));
        assert_eq!(fs::metadata(&e1).unwrap().len(), 0);
    }

    #[test]
    fn same_folder_copies_never_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, "x").unwrap();
        assert!(conflicts(&[f], dir.path()).is_empty());
    }

    #[test]
    fn move_removes_source() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"x").unwrap();
        let dest = dir.path().join("d");
        fs::create_dir(&dest).unwrap();
        FileOperations::paste(&f, &dest, PasteMode::Move).unwrap();
        assert!(!f.exists());
        assert!(dest.join("a.txt").exists());
    }

    #[test]
    fn new_folder_names_increment() {
        let dir = tempfile::tempdir().unwrap();
        FileOperations::create_new_folder(dir.path()).unwrap();
        FileOperations::create_new_folder(dir.path()).unwrap();
        assert_eq!(names(dir.path()), ["New folder", "New folder (2)"]);
    }

    #[test]
    fn rename_refuses_to_clobber() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), b"1").unwrap();
        fs::write(dir.path().join("b"), b"2").unwrap();
        assert!(FileOperations::rename(&dir.path().join("a"), "b").is_err());
        assert!(FileOperations::rename(&dir.path().join("a"), "").is_err());
        assert!(FileOperations::rename(&dir.path().join("a"), "x/y").is_err());
        FileOperations::rename(&dir.path().join("a"), "c").unwrap();
        assert_eq!(names(dir.path()), ["b", "c"]);
    }

    #[cfg(unix)]
    #[test]
    fn case_only_rename_never_replaces_a_different_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.JPG"), "upper").unwrap();
        fs::write(dir.path().join("a.jpg"), "lower").unwrap();
        // Same names case-insensitively, but different files on Linux.
        if !same_entry(&dir.path().join("a.JPG"), &dir.path().join("a.jpg")) {
            assert!(FileOperations::rename(&dir.path().join("a.JPG"), "a.jpg").is_err());
            assert_eq!(
                fs::read_to_string(dir.path().join("a.jpg")).unwrap(),
                "lower"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_alias_cannot_paste_folder_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        fs::create_dir_all(a.join("sub")).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&a, &alias).unwrap();
        assert!(FileOperations::paste(&a, &alias.join("sub"), PasteMode::Copy).is_err());
        assert!(!a.join("sub/a").exists());
    }

    #[test]
    fn copies_never_overwrite_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dest) = (dir.path().join("s"), dir.path().join("d"));
        fs::write(&src, "new").unwrap();
        fs::write(&dest, "keep").unwrap();
        assert!(copy_new(&src, &dest).is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "keep");
        assert!(move_path(&src, &dest).is_err());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "keep");
        assert!(src.exists());
    }

    #[cfg(unix)]
    #[test]
    fn copies_count_their_bytes_and_cancelling_leaves_nothing_half_copied() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("folder");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.bin"), vec![1u8; 3 << 20]).unwrap();
        fs::write(src.join("b.bin"), vec![2u8; 1000]).unwrap();
        assert_eq!(size_of(&src), (3 << 20) + 1000);

        let meter = Arc::new(Meter::default());
        metered(meter.clone(), || copy_new(&src, &dir.path().join("copy"))).unwrap();
        assert_eq!(meter.bytes.load(Ordering::Relaxed), (3 << 20) + 1000);

        let meter = Arc::new(Meter::default());
        meter.cancelled.store(true, Ordering::Relaxed);
        let dest = dir.path().join("cancelled");
        let err = metered(meter, || copy_new(&src, &dest)).unwrap_err();
        assert_eq!(err, CANCELLED);
        assert!(!dest.exists(), "the partial copy is removed");
    }

    #[test]
    fn copy_does_not_follow_symlink_loops() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("loop");
        fs::create_dir(&src).unwrap();
        std::os::unix::fs::symlink(&src, src.join("self")).unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        let out = FileOperations::paste(&src, &dest, PasteMode::Copy).unwrap();
        assert!(fs::symlink_metadata(out.join("self"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
