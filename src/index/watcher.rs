// Ninimma modification: independent project branding and integration.
//! Keeping the index current as files change, without rescanning.
//!
//! The OS reports changes under each indexed folder. Changes are merged
//! into a bounded set of paths (repeats collapse into one; both sides of a
//! rename are kept). The worker handles them after a short pause, so a
//! burst of saves becomes one update. Changes inside excluded folders
//! (`.git`, `node_modules`, `target`, …) are dropped as they arrive, so
//! builds don't cause work. If the set fills up, or the OS says it dropped
//! events, the next pass rescans everything instead: a controlled
//! reconciliation, not a storm of rescans.
//!
//! This is separate from the folder-view watcher in `file_system.rs`, which
//! only watches the folder on screen.

use super::config::Exclusions;
use notify::{Event, RecursiveMode, Watcher as _};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Changed paths waiting to be handled.
pub const MAX_PENDING_CHANGES: usize = 10_000;

#[derive(Default)]
pub struct Changes {
    paths: Mutex<HashSet<PathBuf>>,
    /// Changes were lost (too many, or the OS dropped some): rescan.
    overflowed: AtomicBool,
}

impl Changes {
    fn add(&self, path: PathBuf) {
        let mut paths = self.paths.lock().unwrap();
        if paths.len() >= MAX_PENDING_CHANGES {
            self.overflowed.store(true, Ordering::SeqCst);
        } else {
            paths.insert(path);
        }
    }

    /// The paths changed since the last call, and whether a full rescan is
    /// needed instead.
    pub fn take(&self) -> (Vec<PathBuf>, bool) {
        let mut paths: Vec<PathBuf> = self.paths.lock().unwrap().drain().collect();
        // Parents first, so a new folder is scanned before its contents.
        paths.sort();
        (paths, self.overflowed.swap(false, Ordering::SeqCst))
    }

    pub fn is_empty(&self) -> bool {
        self.paths.lock().unwrap().is_empty() && !self.overflowed.load(Ordering::SeqCst)
    }
}

/// Whether any folder between its root and `path` is excluded (paths
/// outside every root count as excluded). Cheap enough for every event.
fn in_excluded_folder(path: &Path, roots: &[PathBuf], exclusions: &Exclusions) -> bool {
    let Some(root) = roots
        .iter()
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.as_os_str().len())
    else {
        return true;
    };
    path.ancestors()
        .skip(1)
        .take_while(|a| *a != root.as_path())
        .any(|a| {
            a.file_name()
                .is_some_and(|n| exclusions.excludes(a, n, true))
        })
}

/// How many folders may be watched live. On Linux each watched folder
/// takes one inotify watch, from a per-user limit the whole desktop shares,
/// and adding each one is a round trip to the filesystem: on a large NTFS
/// drive (ntfs-3g) tens of thousands of folders took minutes. On FreeBSD
/// kqueue needs an open file per watched item. Windows and macOS watch a
/// whole tree at once, so there's no budget there.
pub fn folder_budget() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let limit = std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(8192);
        // Half the limit, leaving the rest to other apps.
        (limit / 2).clamp(1000, 100_000)
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        u64::MAX
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        4096
    }
}

/// Which roots to watch now, and why the others aren't.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub watch: Vec<PathBuf>,
    /// Too many folders for the budget: kept current by rescans.
    pub too_big: Vec<PathBuf>,
    /// Not scanned yet, so their size isn't known: watched after the scan.
    pub after_scan: Vec<PathBuf>,
}

/// Plans watching `roots`, given how many folders each has (None: not
/// scanned yet). Smaller roots come first, so one huge drive doesn't stop
/// the rest from being watched.
pub fn plan(roots: &[(PathBuf, Option<u64>)], budget: u64) -> Plan {
    let mut out = Plan::default();
    let mut known: Vec<(&PathBuf, u64)> = Vec::new();
    for (root, folders) in roots {
        match folders {
            Some(n) => known.push((root, *n)),
            None => out.after_scan.push(root.clone()),
        }
    }
    known.sort_by_key(|(_, n)| *n);
    let mut used = 0u64;
    for (root, n) in known {
        if used + n <= budget {
            used += n;
            out.watch.push(root.clone());
        } else {
            out.too_big.push(root.clone());
        }
    }
    out
}

/// A watcher once set up, and what it couldn't watch.
type SetUp = (notify::RecommendedWatcher, Finished);

/// Roots that ended up not watched, and why.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Finished {
    /// The OS refused (e.g. its watch limit was hit).
    pub failed: Vec<PathBuf>,
    /// More folders than the budget left, counting the ones the index
    /// skips (`node_modules`, `.git`, ...) that watching still traverses.
    pub too_big: Vec<PathBuf>,
}

/// Counts the folders under `root` (itself included) that recursive
/// watching would traverse: every one, excluded or not, without following
/// links (the watcher doesn't either). Stops once past `limit` or when
/// cancelled, so a huge tree costs at most `limit` folder reads.
pub fn count_folders(root: &Path, limit: u64, cancel: &AtomicBool) -> u64 {
    let mut count = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        count += 1;
        if count > limit || cancel.load(Ordering::Relaxed) {
            return count;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    count
}

/// Watching set up on its own thread, so a slow filesystem never holds up
/// the index. Dropping it stops the setup between roots and ends watching.
pub struct IndexWatcher {
    cancel: Arc<AtomicBool>,
    /// The watcher once set up, and the roots that couldn't be watched.
    done: Arc<Mutex<Option<SetUp>>>,
    finished: bool,
}

impl IndexWatcher {
    /// Once setup has finished (only reported once): the roots that
    /// aren't watched after all.
    pub fn take_finished(&mut self) -> Option<Finished> {
        if self.finished {
            return None;
        }
        let done = self.done.lock().unwrap();
        let finished = done.as_ref()?.1.clone();
        self.finished = true;
        Some(finished)
    }
}

impl Drop for IndexWatcher {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Starts watching `roots` on a background thread, at most `budget`
/// folders in all (`u64::MAX`: no limit, nothing is counted). `wake` is
/// called when changes arrive (from the OS's thread) and once setup has
/// finished; it must be cheap and non-blocking.
pub fn watch(
    roots: &[PathBuf],
    budget: u64,
    exclusions: Arc<Exclusions>,
    changes: Arc<Changes>,
    wake: impl Fn() + Send + Sync + 'static,
) -> IndexWatcher {
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(Mutex::new(None));
    let (thread_cancel, thread_done) = (cancel.clone(), done.clone());
    let roots = roots.to_vec();
    let wake = Arc::new(wake);
    let _ = std::thread::Builder::new()
        .name("index-watch".into())
        .spawn(move || {
            super::system::lower_thread_priority();
            let handler_roots = roots.clone();
            let handler_wake = wake.clone();
            let handler = move |result: notify::Result<Event>| {
                match result {
                    Ok(event) => {
                        if event.need_rescan() {
                            changes.overflowed.store(true, Ordering::SeqCst);
                        }
                        if event.kind.is_access() {
                            return; // reads and opens change nothing
                        }
                        for path in event.paths {
                            if !in_excluded_folder(&path, &handler_roots, &exclusions) {
                                changes.add(path);
                            }
                        }
                    }
                    Err(_) => changes.overflowed.store(true, Ordering::SeqCst),
                }
                handler_wake();
            };
            // Links aren't followed, as when scanning (and as counted).
            let config = notify::Config::default().with_follow_symlinks(false);
            let Ok(mut watcher) = notify::RecommendedWatcher::new(handler, config) else {
                *thread_done.lock().unwrap() = None;
                return;
            };
            let mut finished = Finished::default();
            let mut left = budget;
            for root in &roots {
                if thread_cancel.load(Ordering::SeqCst) {
                    return; // stopped: drop what was set up
                }
                if budget != u64::MAX {
                    let folders = count_folders(root, left, &thread_cancel);
                    if folders > left {
                        finished.too_big.push(root.clone());
                        continue;
                    }
                    left -= folders;
                }
                if watcher.watch(root, RecursiveMode::Recursive).is_err() {
                    finished.failed.push(root.clone());
                }
            }
            if !thread_cancel.load(Ordering::SeqCst) {
                *thread_done.lock().unwrap() = Some((watcher, finished));
                wake();
            }
        });
    IndexWatcher {
        cancel,
        done,
        finished: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::config::IndexSettings;
    use std::time::{Duration, Instant};

    fn exclusions() -> Arc<Exclusions> {
        Arc::new(Exclusions::with_system_paths(
            &IndexSettings::default(),
            None,
            Vec::new(),
        ))
    }

    fn wait_for(changes: &Changes, want: impl Fn(&[PathBuf]) -> bool) -> Vec<PathBuf> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            seen.extend(changes.take().0);
            if want(&seen) {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("changes not reported: {seen:?}");
    }

    /// Watches `root` and waits until the watch is set up.
    fn watching(root: &Path, changes: &Arc<Changes>) -> IndexWatcher {
        let mut w = watch(
            &[root.to_path_buf()],
            u64::MAX,
            exclusions(),
            changes.clone(),
            || {},
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(finished) = w.take_finished() {
                assert_eq!(finished, Finished::default());
                return w;
            }
            assert!(Instant::now() < deadline, "watching wasn't set up");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn setup_runs_in_the_background_and_stops_when_dropped() {
        // A tree of folders: watching it takes a while, but `watch`
        // returns at once and dropping it stops the setup.
        let root = tempfile::tempdir().unwrap();
        for i in 0..300 {
            std::fs::create_dir_all(root.path().join(format!("d{i}/sub"))).unwrap();
        }
        let roots: Vec<PathBuf> = (0..5).map(|_| root.path().to_path_buf()).collect();
        let started = Instant::now();
        let w = watch(
            &roots,
            u64::MAX,
            exclusions(),
            Arc::new(Changes::default()),
            || {},
        );
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "watch() doesn't wait for setup"
        );
        drop(w);
        // The thread notices the cancel between roots and exits; nothing to
        // assert beyond not hanging or panicking.
        std::thread::sleep(Duration::from_millis(50));
    }

    #[test]
    fn excluded_folders_count_against_the_budget() {
        // node_modules isn't indexed, but recursive watching traverses it.
        let root = tempfile::tempdir().unwrap();
        for i in 0..30 {
            std::fs::create_dir_all(root.path().join(format!("node_modules/pkg{i}/lib"))).unwrap();
        }
        std::fs::create_dir(root.path().join("notes")).unwrap();
        let never = AtomicBool::new(false);
        assert_eq!(count_folders(root.path(), u64::MAX, &never), 63);
        assert_eq!(
            count_folders(root.path(), 10, &never),
            11,
            "stops just past the limit"
        );
        // Over budget: not watched, and said so.
        let mut w = watch(
            &[root.path().to_path_buf()],
            20,
            exclusions(),
            Arc::new(Changes::default()),
            || {},
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let finished = loop {
            if let Some(f) = w.take_finished() {
                break f;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(finished.too_big, [root.path().to_path_buf()]);
    }

    #[test]
    fn plans_smallest_roots_first_within_the_budget() {
        let r = |s: &str| PathBuf::from(s);
        let p = plan(
            &[
                (r("/data/windows-old"), Some(90_000)),
                (r("/home/t/Documents"), Some(400)),
                (r("/home/t/Pictures"), Some(1_200)),
                (r("/home/t/New"), None),
            ],
            10_000,
        );
        assert_eq!(p.watch, [r("/home/t/Documents"), r("/home/t/Pictures")]);
        assert_eq!(p.too_big, [r("/data/windows-old")]);
        assert_eq!(p.after_scan, [r("/home/t/New")]);
        assert!(folder_budget() >= 1000);
    }

    #[test]
    fn creations_renames_and_deletions_are_reported() {
        let root = tempfile::tempdir().unwrap();
        let changes = Arc::new(Changes::default());
        let _w = watching(root.path(), &changes);
        let a = root.path().join("a.txt");
        std::fs::write(&a, "x").unwrap();
        wait_for(&changes, |seen| seen.contains(&a));
        let b = root.path().join("b.txt");
        std::fs::rename(&a, &b).unwrap();
        let seen = wait_for(&changes, |seen| seen.contains(&b));
        assert!(
            seen.contains(&a) || cfg!(not(target_os = "linux")),
            "both sides of a rename"
        );
        std::fs::remove_file(&b).unwrap();
        wait_for(&changes, |seen| seen.contains(&b));
    }

    #[test]
    fn changes_in_excluded_folders_are_dropped() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("node_modules/pkg")).unwrap();
        let changes = Arc::new(Changes::default());
        let _w = watching(root.path(), &changes);
        for i in 0..50 {
            std::fs::write(root.path().join(format!("node_modules/pkg/f{i}.js")), "").unwrap();
        }
        let marker = root.path().join("real.txt");
        std::fs::write(&marker, "").unwrap();
        let seen = wait_for(&changes, |seen| seen.contains(&marker));
        assert!(
            seen.iter().all(|p| !p.to_string_lossy().contains("pkg/")),
            "{seen:?}"
        );
    }

    #[test]
    fn only_folders_below_the_root_are_checked() {
        let e = exclusions();
        let roots = [PathBuf::from("/home/a/.notes")];
        // The root itself is hidden; that's the user's choice.
        assert!(!in_excluded_folder(
            Path::new("/home/a/.notes/todo.md"),
            &roots,
            &e
        ));
        assert!(in_excluded_folder(
            Path::new("/home/a/.notes/.git/HEAD"),
            &roots,
            &e
        ));
        assert!(in_excluded_folder(Path::new("/elsewhere/x"), &roots, &e));
    }

    #[test]
    fn too_many_changes_ask_for_a_rescan() {
        let changes = Changes::default();
        for i in 0..(MAX_PENDING_CHANGES + 5) {
            changes.add(PathBuf::from(format!("/r/f{i}")));
        }
        let (paths, rescan) = changes.take();
        assert_eq!(paths.len(), MAX_PENDING_CHANGES);
        assert!(rescan);
        assert!(changes.is_empty());
    }
}
