// Ninimma modification: independent project branding and integration.
//! Undo / redo for file operations, like Explorer's Ctrl+Z / Ctrl+Y.
//!
//! Every operation records a list of reversible [`Step`]s. Undoing a step
//! performs its inverse and returns the step that redoes it, so redo is
//! simply "undo the undo".

use crate::file_ops::{display_name, move_path, same_entry, FileOperations};
use crate::image_ops::{self, Rotation};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_HISTORY: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Item moved or renamed from `from` to `to`.
    Moved {
        from: PathBuf,
        to: PathBuf,
    },
    /// Several items renamed together (bulk rename); names may swap.
    Renamed {
        pairs: Vec<(PathBuf, PathBuf)>,
    },
    /// `dest` was created as a copy of `src`.
    Copied {
        src: PathBuf,
        dest: PathBuf,
    },
    /// A copy was removed again (redo copies it afresh).
    Uncopied {
        src: PathBuf,
        dest: PathBuf,
    },
    /// `path` went to the trash at unix time `at`; `id` is the exact trash
    /// entry when the platform reports it.
    Trashed {
        path: PathBuf,
        at: i64,
        id: Option<OsString>,
    },
    /// `path` came back out of the trash.
    Restored {
        path: PathBuf,
    },
    /// Empty folder created (new folder).
    Created {
        path: PathBuf,
    },
    /// Empty folder removed (e.g. the source of a merged move).
    Uncreated {
        path: PathBuf,
    },
    /// New empty file ("New Document").
    CreatedFile {
        path: PathBuf,
    },
    UncreatedFile {
        path: PathBuf,
    },
    Rotated {
        path: PathBuf,
        rotation: Rotation,
    },
}

/// Whether this platform can take items back out of the trash (the
/// `trash` crate can't list the macOS Trash).
const TRASH_RESTORES: bool = cfg!(any(windows, all(unix, not(target_os = "macos"))));

#[cfg(test)]
thread_local! {
    /// Tests decide whether the trash can be restored from, whatever the
    /// platform (None: as the platform says).
    static TRASH_RESTORES_IN_TEST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

fn trash_restores() -> bool {
    #[cfg(test)]
    if let Some(restores) = TRASH_RESTORES_IN_TEST.with(|c| c.get()) {
        return restores;
    }
    TRASH_RESTORES
}

impl Step {
    /// Why this step can't be undone on this platform at all (not "the
    /// name is taken again", which is only known when undoing).
    pub fn cannot_undo(&self) -> Option<String> {
        match self {
            Step::Trashed { path, .. } if !trash_restores() => Some(format!(
                "\"{}\" can't be brought back from the Trash on this computer; restore it from the Trash yourself",
                display_name(path)
            )),
            _ => None,
        }
    }

    /// Paths whose thumbnails/listing may change when this step is applied.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Step::Moved { from, to } => vec![from, to],
            Step::Renamed { pairs } => pairs
                .iter()
                .flat_map(|(a, b)| [a.as_path(), b.as_path()])
                .collect(),
            Step::Copied { dest, .. } | Step::Uncopied { dest, .. } => vec![dest],
            Step::Trashed { path, .. }
            | Step::Restored { path }
            | Step::Created { path }
            | Step::Uncreated { path }
            | Step::CreatedFile { path }
            | Step::UncreatedFile { path }
            | Step::Rotated { path, .. } => vec![path],
        }
    }

    /// Reverse this step, returning the step that would redo it.
    pub fn undo(&self) -> Result<Step, String> {
        match self {
            Step::Moved { from, to } => {
                // On a case-insensitive drive the old spelling of a case-only
                // rename still finds the file itself; that isn't a clash.
                if same_entry(from, to) {
                    fs::rename(to, from).map_err(|e| format!("Couldn't rename back: {e}"))?;
                    return Ok(Step::Moved {
                        from: to.clone(),
                        to: from.clone(),
                    });
                }
                if from.symlink_metadata().is_ok() {
                    return Err(format!(
                        "Can't move back: \"{}\" already exists",
                        display_name(from)
                    ));
                }
                // A folder merge may have removed the original parent.
                if let Some(parent) = from.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("Couldn't recreate folder: {e}"))?;
                }
                move_path(to, from)?;
                Ok(Step::Moved {
                    from: to.clone(),
                    to: from.clone(),
                })
            }
            Step::Renamed { pairs } => {
                let back: Vec<(PathBuf, PathBuf)> =
                    pairs.iter().map(|(a, b)| (b.clone(), a.clone())).collect();
                crate::file_ops::rename_batch(&back)?;
                Ok(Step::Renamed { pairs: back })
            }
            Step::Copied { src, dest } => {
                remove_copy(src, dest)?;
                Ok(Step::Uncopied {
                    src: src.clone(),
                    dest: dest.clone(),
                })
            }
            Step::Uncopied { src, dest } => {
                if dest.symlink_metadata().is_ok() {
                    return Err(format!(
                        "Can't copy again: \"{}\" already exists",
                        display_name(dest)
                    ));
                }
                crate::file_ops::copy_new(src, dest)?;
                Ok(Step::Copied {
                    src: src.clone(),
                    dest: dest.clone(),
                })
            }
            Step::Trashed { path, at, id } => {
                restore_from_trash(path, *at, id.as_deref())?;
                Ok(Step::Restored { path: path.clone() })
            }
            Step::Restored { path } => trash(path),
            Step::Created { path } => {
                fs::remove_dir(path)
                    .map_err(|_| format!("\"{}\" is no longer empty", display_name(path)))?;
                Ok(Step::Uncreated { path: path.clone() })
            }
            Step::Uncreated { path } => {
                fs::create_dir(path).map_err(|e| format!("Couldn't create folder: {e}"))?;
                Ok(Step::Created { path: path.clone() })
            }
            Step::CreatedFile { path } => {
                // Still empty: just remove it. Written to since: keep the work in the trash.
                let empty = fs::metadata(path)
                    .map(|m| m.is_file() && m.len() == 0)
                    .unwrap_or(false);
                if empty {
                    FileOperations::delete_permanently(path)?;
                } else {
                    FileOperations::move_to_trash(std::slice::from_ref(path))?;
                }
                Ok(Step::UncreatedFile { path: path.clone() })
            }
            Step::UncreatedFile { path } => {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|e| format!("Couldn't create file: {e}"))?;
                Ok(Step::CreatedFile { path: path.clone() })
            }
            Step::Rotated { path, rotation } => {
                let back = rotation.inverse();
                image_ops::rotate_file(path, back)?;
                Ok(Step::Rotated {
                    path: path.clone(),
                    rotation: back,
                })
            }
        }
    }
}

/// Undoing a copy: a copy whose bytes still match its source is just a
/// duplicate, so it's deleted outright; anything else (folders, edited
/// copies) goes to the trash so no work can be lost.
fn remove_copy(src: &Path, dest: &Path) -> Result<(), String> {
    if files_identical(src, dest) {
        FileOperations::delete_permanently(dest)
    } else {
        FileOperations::move_to_trash(&[dest.to_path_buf()])
    }
}

fn files_identical(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (Ok(ma), Ok(mb)) = (fs::symlink_metadata(a), fs::symlink_metadata(b)) else {
        return false;
    };
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = match fa.read(&mut ba) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if n == 0 {
            return true;
        }
        if fb.read_exact(&mut bb[..n]).is_err() || ba[..n] != bb[..n] {
            return false;
        }
    }
}

/// Move `path` to the trash, remembering exactly which trash entry it became
/// so undo restores this deletion even if the same path was deleted before.
pub fn trash(path: &Path) -> Result<Step, String> {
    let before = trash_ids(path);
    let at = now();
    FileOperations::move_to_trash(&[path.to_path_buf()])?;
    let id = trash_ids(path).into_iter().find(|id| !before.contains(id));
    Ok(Step::Trashed {
        path: path.to_path_buf(),
        at,
        id,
    })
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn trash_ids(path: &Path) -> Vec<OsString> {
    trash::os_limited::list()
        .map(|items| {
            items
                .into_iter()
                .filter(|i| i.original_path() == path)
                .map(|i| i.id)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn trash_ids(_path: &Path) -> Vec<OsString> {
    Vec::new()
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn restore_from_trash(path: &Path, at: i64, id: Option<&OsStr>) -> Result<(), String> {
    if path.symlink_metadata().is_ok() {
        return Err(format!(
            "Can't restore: \"{}\" already exists",
            display_name(path)
        ));
    }
    let items = trash::os_limited::list().map_err(|e| format!("Couldn't read the trash: {e}"))?;
    // Several trashed items can share a path: use the exact entry if known,
    // otherwise the one trashed closest to when this step happened.
    let item = items
        .into_iter()
        .filter(|i| i.original_path() == path)
        .min_by_key(|i| (Some(i.id.as_os_str()) != id, (i.time_deleted - at).abs()))
        .ok_or_else(|| format!("\"{}\" is no longer in the trash", display_name(path)))?;
    trash::os_limited::restore_all([item]).map_err(|e| format!("Couldn't restore: {e}"))
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
fn restore_from_trash(path: &Path, _at: i64, _id: Option<&OsStr>) -> Result<(), String> {
    Err(format!(
        "Restoring \"{}\" from the Trash isn't supported on this platform yet",
        display_name(path)
    ))
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// What the user did, e.g. "Rename" or "Move 3 items".
    pub label: String,
    /// Steps in the order they were performed.
    pub steps: Vec<Step>,
    /// When the user started the action; history stays in this order even
    /// if background operations finish out of order.
    pub order: u64,
}

impl Entry {
    /// Why this entry can't be undone on this platform, if any step can't.
    pub fn cannot_undo(&self) -> Option<String> {
        self.steps.iter().find_map(Step::cannot_undo)
    }
}

/// Undo and redo stacks.
#[derive(Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    next_order: u64,
}

impl History {
    /// Claim a place in history when an action starts.
    pub fn reserve(&mut self) -> u64 {
        self.next_order += 1;
        self.next_order
    }

    /// Record an action that just completed synchronously.
    pub fn record(&mut self, label: impl Into<String>, steps: Vec<Step>) {
        let order = self.reserve();
        self.record_at(label, steps, order);
    }

    /// Record a background action at the place reserved when it started.
    /// Clears redo, as in every editor.
    pub fn record_at(&mut self, label: impl Into<String>, steps: Vec<Step>, order: u64) {
        if steps.is_empty() {
            return;
        }
        self.redo.clear();
        self.push_undo(Entry {
            label: label.into(),
            steps,
            order,
        });
    }

    fn push_undo(&mut self, entry: Entry) {
        let at = self
            .undo
            .iter()
            .rposition(|e| e.order <= entry.order)
            .map_or(0, |i| i + 1);
        self.undo.insert(at, entry);
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    /// Why the next undo can't be done on this platform, if it can't (so
    /// it isn't offered).
    pub fn undo_blocked(&self) -> Option<String> {
        self.undo.last().and_then(Entry::cannot_undo)
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn take_undo(&mut self) -> Option<Entry> {
        self.undo.pop()
    }

    pub fn take_redo(&mut self) -> Option<Entry> {
        self.redo.pop()
    }

    /// After undoing (or redoing) an entry: what was reversed becomes
    /// redoable (or undoable again), and steps that failed stay where they
    /// were so the user can retry once the obstruction is gone.
    pub fn finished(&mut self, reversed: Entry, failed: Entry, was_undo: bool) {
        if was_undo {
            if !reversed.steps.is_empty() {
                self.redo.push(reversed);
            }
            if !failed.steps.is_empty() {
                self.push_undo(failed);
            }
        } else {
            if !reversed.steps.is_empty() {
                self.push_undo(reversed);
            }
            if !failed.steps.is_empty() {
                self.redo.push(failed);
            }
        }
    }
}

/// Reverse an entry's steps (last first). Returns the entry that reverses
/// what succeeded, the entry of steps that failed (to retry later) and the
/// error messages. A panicking step counts as a failure.
pub fn reverse(entry: &Entry, mut on_step: impl FnMut(&Step)) -> (Entry, Entry, Vec<String>) {
    // All or nothing for what the platform can't do: undoing the other
    // steps first (removing a Replace's new copy, say) and then failing
    // to restore the original would leave neither.
    if let Some(why) = entry.cannot_undo() {
        let none = Entry {
            label: entry.label.clone(),
            steps: Vec::new(),
            order: entry.order,
        };
        return (none, entry.clone(), vec![why]);
    }
    let mut done = Vec::new();
    let mut failed = Vec::new();
    let mut errors = Vec::new();
    for step in entry.steps.iter().rev() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| step.undo()))
            .unwrap_or_else(|_| Err("unexpected internal error".to_string()));
        match result {
            Ok(inverse) => {
                on_step(&inverse);
                done.push(inverse);
            }
            Err(e) => {
                errors.push(e);
                failed.push(step.clone());
            }
        }
    }
    failed.reverse(); // back to the order they were performed
    let with = |steps| Entry {
        label: entry.label.clone(),
        steps,
        order: entry.order,
    };
    (with(done), with(failed), errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_ops::{Conflict, PasteMode};

    fn entry(steps: Vec<Step>) -> Entry {
        Entry {
            label: "x".into(),
            steps,
            order: 0,
        }
    }

    #[test]
    fn where_the_trash_cant_be_restored_replace_is_undone_all_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(src.join("a.txt"), "new").unwrap();
        // What Replace records: the old file trashed, then the new one copied in.
        let steps = vec![
            Step::Trashed {
                path: dest.join("a.txt"),
                at: now(),
                id: None,
            },
            Step::Copied {
                src: src.join("a.txt"),
                dest: dest.join("a.txt"),
            },
        ];
        fs::write(dest.join("a.txt"), "new").unwrap();
        TRASH_RESTORES_IN_TEST.with(|c| c.set(Some(false)));
        let mut history = History::default();
        history.record("Replace", steps.clone());
        // Not offered...
        assert!(history.undo_blocked().is_some_and(|w| w.contains("a.txt")));
        // ...and if run anyway, nothing is touched: the new copy stays.
        let (done, failed, errors) = reverse(&entry(steps.clone()), |_| {});

        assert!(done.steps.is_empty());
        assert_eq!(failed.steps, steps, "kept whole, to undo where it can be");
        assert_eq!(errors.len(), 1);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "new");
        // Where the trash can be restored, the same entry is offered.
        TRASH_RESTORES_IN_TEST.with(|c| c.set(Some(true)));
        assert!(history.undo_blocked().is_none());
        TRASH_RESTORES_IN_TEST.with(|c| c.set(None));
    }

    /// Undo then redo; returns (undone, redone). Fails on any error.
    fn undo_redo(steps: Vec<Step>) -> (Entry, Entry) {
        let (undone, _, errors) = reverse(&entry(steps), |_| {});
        assert!(errors.is_empty(), "{errors:?}");
        let (redone, _, errors) = reverse(&undone, |_| {});
        assert!(errors.is_empty(), "{errors:?}");
        (undone, redone)
    }

    #[test]
    fn rename_and_move_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, "1").unwrap();
        let b = FileOperations::rename(&a, "b.txt").unwrap();
        let (undone, _, _) = reverse(
            &entry(vec![Step::Moved {
                from: a.clone(),
                to: b.clone(),
            }]),
            |_| {},
        );
        assert!(a.exists() && !b.exists());
        reverse(&undone, |_| {});
        assert!(!a.exists() && b.exists());
    }

    #[test]
    fn merged_move_undo_recreates_source_folder() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("Photos");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("1.jpg"), "a").unwrap();
        let dest_dir = dir.path().join("Backup");
        fs::create_dir_all(dest_dir.join("Photos")).unwrap();
        fs::write(dest_dir.join("Photos/2.jpg"), "b").unwrap();

        let mut steps = Vec::new();
        crate::file_ops::paste_into(
            &src,
            &dest_dir,
            PasteMode::Move,
            Conflict::Replace,
            &mut steps,
        )
        .unwrap();
        assert!(
            !src.exists(),
            "emptied source folder is removed after a merge"
        );
        assert!(dest_dir.join("Photos/1.jpg").exists() && dest_dir.join("Photos/2.jpg").exists());

        // Undo + redo reproduces the merge exactly (no leftover empty folder)...
        undo_redo(steps.clone());
        assert!(!src.exists());
        // ...and undoing again restores the original layout.
        let (_, _, errors) = reverse(&entry(steps), |_| {});
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(fs::read_to_string(src.join("1.jpg")).unwrap(), "a");
        assert!(dest_dir.join("Photos/2.jpg").exists());
    }

    #[test]
    fn copy_undo_removes_copy_and_redo_recreates_it() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, "hello").unwrap();
        let mut steps = Vec::new();
        crate::file_ops::paste_into(
            &src,
            dir.path(),
            PasteMode::Copy,
            Conflict::KeepBoth,
            &mut steps,
        )
        .unwrap();
        let copy = dir.path().join("a - Copy.txt");
        assert!(copy.exists());
        let (undone, _, _) = reverse(&entry(steps), |_| {});
        assert!(!copy.exists() && src.exists());
        reverse(&undone, |_| {});
        assert_eq!(fs::read_to_string(&copy).unwrap(), "hello");
    }

    #[test]
    fn edited_copy_of_same_size_is_not_treated_as_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dest) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&src, "hello").unwrap();
        fs::write(&dest, "hello").unwrap();
        assert!(files_identical(&src, &dest));
        fs::write(&dest, "world").unwrap(); // same length, different bytes
        assert!(!files_identical(&src, &dest));
    }

    #[test]
    fn new_folder_and_rotation_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let folder = FileOperations::create_new_folder(dir.path()).unwrap();
        undo_redo(vec![Step::Created {
            path: folder.clone(),
        }]);
        assert!(folder.is_dir());

        let png = dir.path().join("p.png");
        image::DynamicImage::new_rgb8(4, 2).save(&png).unwrap();
        image_ops::rotate_file(&png, Rotation::Clockwise).unwrap();
        reverse(
            &entry(vec![Step::Rotated {
                path: png.clone(),
                rotation: Rotation::Clockwise,
            }]),
            |_| {},
        );
        assert_eq!(image::image_dimensions(&png).unwrap(), (4, 2));
    }

    #[test]
    fn bulk_rename_with_swapped_names_undoes_and_redoes() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "A").unwrap();
        fs::write(&b, "B").unwrap();
        let pairs = vec![(a.clone(), b.clone()), (b.clone(), a.clone())];
        crate::file_ops::rename_batch(&pairs).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "B");
        let (undone, _, errors) = reverse(&entry(vec![Step::Renamed { pairs }]), |_| {});
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(fs::read_to_string(&a).unwrap(), "A");
        reverse(&undone, |_| {});
        assert_eq!(fs::read_to_string(&a).unwrap(), "B");
    }

    #[test]
    fn history_keeps_start_order_and_retries_failures() {
        let mut h = History::default();
        let step = |p: &str| Step::Created {
            path: PathBuf::from(p),
        };
        let first = h.reserve();
        let second = h.reserve();
        // The later action finishes first, the earlier one second.
        h.record_at("Second", vec![step("/nonexistent/b")], second);
        h.record_at("First", vec![step("/nonexistent/a")], first);
        assert_eq!(
            h.undo_label(),
            Some("Second"),
            "most recently started is undone first"
        );

        // An undo whose step fails stays undoable for a retry.
        let e = h.take_undo().unwrap();
        let (reversed, failed, errors) = reverse(&e, |_| {});
        assert_eq!(errors.len(), 1);
        h.finished(reversed, failed, true);
        assert_eq!(h.undo_label(), Some("Second"));
        assert_eq!(h.redo_label(), None);
    }

    #[test]
    fn history_redo_is_cleared_by_new_actions() {
        let mut h = History::default();
        let step = || Step::Created {
            path: PathBuf::from("/x"),
        };
        h.record("A", vec![step()]);
        let e = h.take_undo().unwrap();
        let none = Entry {
            steps: vec![],
            ..e.clone()
        };
        h.finished(e, none, true);
        assert_eq!(h.redo_label(), Some("A"));
        h.record("B", vec![step()]);
        assert_eq!(h.redo_label(), None);
        assert_eq!(h.undo_label(), Some("B"));
    }

    #[test]
    fn undo_refuses_to_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::write(&a, "new occupant").unwrap();
        fs::write(&b, "moved").unwrap();
        assert!(Step::Moved {
            from: a.clone(),
            to: b.clone()
        }
        .undo()
        .is_err());
        assert_eq!(fs::read_to_string(&a).unwrap(), "new occupant");
    }
}

/// Goes through the real trash, so it's opt-in. Point XDG_DATA_HOME at a
/// scratch folder on the same filesystem as the temp dir to keep it isolated.
#[cfg(all(test, unix, not(target_os = "macos")))]
#[test]
#[ignore]
fn replace_then_undo_restores_the_original_from_trash() {
    use crate::file_ops::{paste_into, Conflict, PasteMode};
    let base = std::env::var_os("TRASH_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = tempfile::tempdir_in(base).unwrap();
    let (src, dest) = (dir.path().join("src"), dir.path().join("dest"));
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dest).unwrap();
    fs::write(src.join("a.txt"), "new").unwrap();
    fs::write(dest.join("a.txt"), "old").unwrap();

    // An older "a.txt" was trashed from the same place, in the same second:
    // undo must restore the version this operation replaced, not that one.
    fs::write(dest.join("a.txt"), "older").unwrap();
    trash(&dest.join("a.txt")).unwrap();
    fs::write(dest.join("a.txt"), "old").unwrap();

    let mut steps = Vec::new();
    paste_into(
        &src.join("a.txt"),
        &dest,
        PasteMode::Copy,
        Conflict::Replace,
        &mut steps,
    )
    .unwrap();
    assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "new");
    assert!(
        matches!(&steps[0], Step::Trashed { id: Some(_), .. }),
        "exact trash entry recorded"
    );

    let e = Entry {
        label: "Copy".into(),
        steps,
        order: 0,
    };
    let (undone, _, errors) = reverse(&e, |_| {});
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "old");

    let (_, _, errors) = reverse(&undone, |_| {});
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "new");
}
