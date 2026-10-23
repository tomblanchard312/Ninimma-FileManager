// Ninimma modification: independent project branding and integration.
//! Walks the indexed folders and records what's in them.
//!
//! The walk never follows links (so it can't leave a root or loop), stays
//! on the root's own disk, skips excluded places, special files, cloud
//! placeholders and anything unreadable, and records names and metadata
//! only. It works through a bounded list of folders and writes in batches,
//! checking between batches whether to pause or stop.

use super::config::Exclusions;
use super::db::{self, FileRecord};
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const BATCH: usize = 256;
/// Folders waiting to be read. A tree wider than this is scanned in part
/// (and the status says so) rather than growing memory without bound.
pub const MAX_PENDING_DIRS: usize = 200_000;

#[cfg(test)]
thread_local! {
    /// Lets a test use a small cap without building 200,000 folders.
    static PENDING_CAP: std::cell::Cell<usize> = const { std::cell::Cell::new(MAX_PENDING_DIRS) };
}

fn pending_cap() -> usize {
    #[cfg(test)]
    return PENDING_CAP.with(|c| c.get());
    #[cfg(not(test))]
    MAX_PENDING_DIRS
}

/// Asked between batches.
pub trait ScanControl {
    /// Blocks while the scan should wait (paused, on battery, busy).
    /// Returns false when the scan should stop.
    fn keep_going(&self) -> bool;
    /// Reports progress (files seen so far in this scan).
    fn progress(&self, files: u64);
    /// Whether the database has reached its size limit.
    fn database_full(&self) -> bool;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanStats {
    pub files: u64,
    pub removed: usize,
    /// The scan stopped before finishing (cancelled, full, too wide), so
    /// nothing was forgotten.
    pub incomplete: bool,
    pub too_wide: bool,
}

/// Scans one root into the database.
pub fn scan_root(
    conn: &mut Connection,
    root: &Path,
    exclusions: &Exclusions,
    control: &dyn ScanControl,
    pace: Duration,
) -> Result<ScanStats, db::DbError> {
    let mut stats = ScanStats::default();
    let Ok(root_meta) = fs::symlink_metadata(root) else {
        // A root that's gone (e.g. an unplugged drive) is left as it was
        // until it comes back or the user removes it.
        stats.incomplete = true;
        return Ok(stats);
    };
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        stats.incomplete = true;
        return Ok(stats);
    }
    let root_volume = volume_id(root, &root_meta);
    let root_id = db::root_id(conn, root)?;
    let generation = db::begin_scan(conn, root_id)?;
    let target = Target {
        root_id,
        generation,
        root_volume: &root_volume,
    };
    walk(conn, &target, root, exclusions, control, pace, &mut stats)?;
    control.progress(stats.files);
    if !stats.incomplete {
        stats.removed = db::finish_scan(conn, root_id, generation)?;
    }
    Ok(stats)
}

/// Where scanned entries go: a root, its current scan, and its disk.
struct Target<'a> {
    root_id: i64,
    generation: i64,
    root_volume: &'a str,
}

/// Records everything below `start`. Stops early (marking `stats`
/// incomplete) when told to, or when the database is full.
fn walk(
    conn: &mut Connection,
    target: &Target,
    start: &Path,
    exclusions: &Exclusions,
    control: &dyn ScanControl,
    pace: Duration,
    stats: &mut ScanStats,
) -> Result<(), db::DbError> {
    let mut pending: Vec<PathBuf> = vec![start.to_path_buf()];
    let mut batch: Vec<FileRecord> = Vec::with_capacity(BATCH);
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue; // unreadable folder: skipped, so its old entries go too
        };
        for entry in entries.flatten() {
            let Some(record) = record_for(&entry.path(), exclusions, target.root_volume) else {
                continue;
            };
            if record.is_dir {
                if pending.len() >= pending_cap() {
                    stats.too_wide = true;
                    stats.incomplete = true;
                } else {
                    pending.push(record.path.clone());
                }
            }
            batch.push(record);
            if batch.len() >= BATCH {
                stats.files += batch.len() as u64;
                db::upsert_files(conn, target.root_id, target.generation, &batch)?;
                batch.clear();
                control.progress(stats.files);
                if control.database_full() || !control.keep_going() {
                    stats.incomplete = true;
                    return Ok(());
                }
                std::thread::sleep(pace);
            }
        }
    }
    stats.files += batch.len() as u64;
    db::upsert_files(conn, target.root_id, target.generation, &batch)?;
    Ok(())
}

/// What the index records for `path`, or `None` if it's skipped: a link,
/// a special file, excluded, a cloud placeholder, unreadable, or a folder
/// on another disk than the root.
fn record_for(path: &Path, exclusions: &Exclusions, root_volume: &str) -> Option<FileRecord> {
    let name = path.file_name()?;
    // symlink_metadata: a link is never followed.
    let meta = fs::symlink_metadata(path).ok()?;
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return None;
    }
    let is_dir = file_type.is_dir();
    if !is_dir && !file_type.is_file() {
        return None; // devices, sockets, FIFOs
    }
    if exclusions.excludes(path, name, is_dir) || crate::file_system::is_online_only(&meta) {
        return None;
    }
    let volume = volume_id(path, &meta);
    if is_dir && volume != root_volume {
        return None; // another disk mounted inside: not crossed
    }
    if !readable(path, is_dir) {
        return None;
    }
    Some(FileRecord {
        path: path.to_path_buf(),
        volume_id: volume,
        is_dir,
        size: if is_dir { 0 } else { meta.len() },
        modified_ns: meta.modified().ok().map_or(0, nanos_since_epoch),
        created_ns: meta.created().ok().map(nanos_since_epoch),
        identity: identity(&meta),
    })
}

/// Brings one changed path up to date (from file watching): records it
/// (and, for a new or moved-in folder, everything in it), or forgets it
/// and everything below it if it's gone or now skipped. Paths outside the
/// indexed roots are ignored.
pub fn update_path(
    conn: &mut Connection,
    path: &Path,
    exclusions: &Exclusions,
    control: &dyn ScanControl,
) -> Result<(), db::DbError> {
    let Some((root_id, generation, root)) = db::root_of(conn, path)? else {
        return Ok(());
    };
    if path == root {
        return Ok(()); // the root itself is handled by full scans
    }
    let root_volume = match fs::symlink_metadata(&root) {
        Ok(meta) => volume_id(&root, &meta),
        Err(_) => return Ok(()),
    };
    // Something above it may be excluded (or have become unreadable).
    let parent_ok = path
        .ancestors()
        .skip(1)
        .take_while(|a| *a != root)
        .all(|a| {
            a.file_name()
                .is_some_and(|n| !exclusions.excludes(a, n, true))
                && readable(a, true)
        });
    let record = if parent_ok {
        record_for(path, exclusions, &root_volume)
    } else {
        None
    };
    let Some(record) = record else {
        db::remove_tree(conn, path)?;
        return Ok(());
    };
    let is_dir = record.is_dir;
    db::upsert_files(conn, root_id, generation, &[record])?;
    if is_dir {
        let target = Target {
            root_id,
            generation,
            root_volume: &root_volume,
        };
        let mut stats = ScanStats::default();
        walk(
            conn,
            &target,
            path,
            exclusions,
            control,
            Duration::ZERO,
            &mut stats,
        )?;
    }
    Ok(())
}

fn nanos_since_epoch(t: std::time::SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_nanos().min(i64::MAX as u128) as i64),
    }
}

/// Which disk an entry is on, to stay on the root's own.
#[cfg(unix)]
fn volume_id(_path: &Path, meta: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    meta.dev().to_string()
}

#[cfg(windows)]
fn volume_id(path: &Path, _meta: &fs::Metadata) -> String {
    // The drive or share; mounts inside a folder are junctions, which are
    // links and never followed anyway.
    match path.components().next() {
        Some(std::path::Component::Prefix(p)) => p.as_os_str().to_string_lossy().to_uppercase(),
        _ => String::new(),
    }
}

#[cfg(not(any(unix, windows)))]
fn volume_id(_path: &Path, _meta: &fs::Metadata) -> String {
    String::new()
}

#[cfg(unix)]
fn identity(meta: &fs::Metadata) -> Option<Vec<u8>> {
    use std::os::unix::fs::MetadataExt;
    let mut id = meta.dev().to_le_bytes().to_vec();
    id.extend_from_slice(&meta.ino().to_le_bytes());
    Some(id)
}

#[cfg(not(unix))]
fn identity(_meta: &fs::Metadata) -> Option<Vec<u8>> {
    None
}

/// Only what this user can read goes into their index.
#[cfg(unix)]
pub fn readable(path: &Path, is_dir: bool) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mode = if is_dir {
        libc::R_OK | libc::X_OK
    } else {
        libc::R_OK
    };
    // SAFETY: c is NUL-terminated.
    unsafe { libc::access(c.as_ptr(), mode) == 0 }
}

/// Windows decides access with ACLs, which only opening reveals; the scan
/// relies on listing and metadata succeeding, and results are checked
/// again before they're shown.
#[cfg(not(unix))]
pub fn readable(_path: &Path, _is_dir: bool) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::config::IndexSettings;
    use crate::index::db::tests::open_test_db;
    use crate::security::secret::{SecretBytes, KEY_LEN};
    use std::cell::Cell;

    struct Go {
        stop_after: Option<u64>,
        seen: Cell<u64>,
    }

    impl ScanControl for Go {
        fn keep_going(&self) -> bool {
            self.stop_after.is_none_or(|n| self.seen.get() < n)
        }
        fn progress(&self, files: u64) {
            self.seen.set(files);
        }
        fn database_full(&self) -> bool {
            false
        }
    }

    fn go() -> Go {
        Go {
            stop_after: None,
            seen: Cell::new(0),
        }
    }

    fn names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT filename FROM files ORDER BY filename")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn exclusions() -> Exclusions {
        Exclusions::with_system_paths(&IndexSettings::default(), None, Vec::new())
    }

    #[test]
    fn scans_files_and_folders_and_skips_exclusions() {
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let r = root.path();
        fs::create_dir_all(r.join("Projects/app/.git/objects")).unwrap();
        fs::create_dir_all(r.join("Projects/app/node_modules/lib")).unwrap();
        fs::write(r.join("Projects/app/main.rs"), "fn main() {}").unwrap();
        fs::write(r.join("Projects/app/node_modules/lib/index.js"), "").unwrap();
        fs::write(r.join("report.pdf"), "x").unwrap();
        fs::write(r.join(".hidden"), "x").unwrap();
        fs::write(r.join("vm.qcow2"), "x").unwrap();

        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        let stats = scan_root(&mut conn, r, &exclusions(), &go(), Duration::ZERO).unwrap();
        assert!(!stats.incomplete);
        assert_eq!(names(&conn), ["Projects", "app", "main.rs", "report.pdf"]);
    }

    #[test]
    fn a_rescan_picks_up_changes_and_forgets_deletions() {
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a.txt"), "").unwrap();
        fs::write(root.path().join("b.txt"), "").unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        scan_root(&mut conn, root.path(), &exclusions(), &go(), Duration::ZERO).unwrap();
        fs::remove_file(root.path().join("a.txt")).unwrap();
        fs::write(root.path().join("c.txt"), "").unwrap();
        let stats =
            scan_root(&mut conn, root.path(), &exclusions(), &go(), Duration::ZERO).unwrap();
        assert_eq!(stats.removed, 1);
        assert_eq!(names(&conn), ["b.txt", "c.txt"]);
    }

    #[test]
    fn a_stopped_scan_forgets_nothing() {
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        for i in 0..(BATCH * 3) {
            fs::write(root.path().join(format!("f{i}")), "").unwrap();
        }
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        scan_root(&mut conn, root.path(), &exclusions(), &go(), Duration::ZERO).unwrap();
        let before = db::file_count(&conn).unwrap();
        let stopping = Go {
            stop_after: Some(BATCH as u64),
            seen: Cell::new(0),
        };
        let stats = scan_root(
            &mut conn,
            root.path(),
            &exclusions(),
            &stopping,
            Duration::ZERO,
        )
        .unwrap();
        assert!(stats.incomplete);
        assert_eq!(db::file_count(&conn).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed_so_loops_and_escapes_are_impossible() {
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("outside-secret.txt"), "").unwrap();
        fs::create_dir(root.path().join("sub")).unwrap();
        // A loop back to the root, and a link out of it.
        std::os::unix::fs::symlink(root.path(), root.path().join("sub/loop")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        let stats =
            scan_root(&mut conn, root.path(), &exclusions(), &go(), Duration::ZERO).unwrap();
        assert!(!stats.incomplete);
        assert_eq!(names(&conn), ["sub"]);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_entries_are_skipped() {
        use std::os::unix::fs::PermissionsExt;
        // root can read anything, so this only means something as a user.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("open.txt"), "").unwrap();
        fs::write(root.path().join("locked.txt"), "").unwrap();
        fs::set_permissions(
            root.path().join("locked.txt"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        fs::create_dir(root.path().join("closed")).unwrap();
        fs::write(root.path().join("closed/inner.txt"), "").unwrap();
        fs::set_permissions(
            root.path().join("closed"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        scan_root(&mut conn, root.path(), &exclusions(), &go(), Duration::ZERO).unwrap();
        fs::set_permissions(
            root.path().join("closed"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert_eq!(names(&conn), ["open.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn special_files_are_skipped() {
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let fifo = std::ffi::CString::new(root.path().join("pipe").to_str().unwrap()).unwrap();
        // SAFETY: fifo is a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        fs::write(root.path().join("normal.txt"), "").unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        scan_root(&mut conn, root.path(), &exclusions(), &go(), Duration::ZERO).unwrap();
        assert_eq!(names(&conn), ["normal.txt"]);
    }

    #[test]
    fn the_folder_queue_is_bounded_and_a_partial_scan_forgets_nothing() {
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        PENDING_CAP.with(|c| c.set(40));
        for d in 0..50 {
            fs::create_dir(root.path().join(format!("d{d}"))).unwrap();
        }
        fs::write(root.path().join("d0/old.txt"), "").unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        let stats =
            scan_root(&mut conn, root.path(), &exclusions(), &go(), Duration::ZERO).unwrap();
        assert!(stats.too_wide && stats.incomplete);
        assert_eq!(stats.removed, 0);
    }

    #[test]
    fn a_full_database_stops_the_scan() {
        struct Full;
        impl ScanControl for Full {
            fn keep_going(&self) -> bool {
                true
            }
            fn progress(&self, _files: u64) {}
            fn database_full(&self) -> bool {
                true
            }
        }
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        for i in 0..(BATCH * 2) {
            fs::write(root.path().join(format!("f{i}")), "").unwrap();
        }
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        let stats =
            scan_root(&mut conn, root.path(), &exclusions(), &Full, Duration::ZERO).unwrap();
        assert!(stats.incomplete);
        assert_eq!(
            db::file_count(&conn).unwrap(),
            BATCH as u64,
            "stopped after the first batch"
        );
    }

    #[test]
    fn many_entries_scan_with_bounded_batches() {
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        for d in 0..30 {
            let sub = root.path().join(format!("d{d}"));
            fs::create_dir(&sub).unwrap();
            for f in 0..150 {
                fs::write(sub.join(format!("f{f}.txt")), "").unwrap();
            }
        }
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        let stats =
            scan_root(&mut conn, root.path(), &exclusions(), &go(), Duration::ZERO).unwrap();
        assert_eq!(stats.files, 30 * 151);
        assert_eq!(db::file_count(&conn).unwrap(), 30 * 151);
    }

    /// Measures a real scan: `NINIMMA_BENCH_ROOT=/usr cargo test
    /// --release measure_scan -- --ignored --nocapture`. Read-only on the
    /// root; the index goes to a temp folder.
    #[test]
    #[ignore]
    fn measure_scan() {
        let Some(root) = std::env::var_os("NINIMMA_BENCH_ROOT") else {
            return;
        };
        let db_dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        let exclusions = Exclusions::with_system_paths(&IndexSettings::default(), None, Vec::new());
        let start = std::time::Instant::now();
        let stats = scan_root(
            &mut conn,
            Path::new(&root),
            &exclusions,
            &go(),
            Duration::ZERO,
        )
        .unwrap();
        let took = start.elapsed();
        db::checkpoint(&conn).unwrap();
        let size = db::size_on_disk(&db_dir.path().join(db::METADATA_DB));
        let rss = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmHWM"))
                    .map(str::to_string)
            })
            .unwrap_or_default();
        println!(
            "indexed {} items in {:.1}s ({:.0}/s), database {:.1} MB ({} bytes/item), peak {rss}",
            stats.files,
            took.as_secs_f64(),
            stats.files as f64 / took.as_secs_f64(),
            size as f64 / 1e6,
            size / stats.files.max(1),
        );
        for text in ["conf", "type:image", "ext:h size:>10kb", "zzzz-no-match"] {
            let request = crate::index::types::SearchRequest {
                generation: 1,
                text: text.into(),
                scope: crate::index::types::SearchScope::Everywhere,
                folder: root.clone().into(),
                offset: 0,
                limit: crate::index::search::PAGE_SIZE,
                full_text: true,
                smart: false,
                understand: false,
            };
            let query = crate::index::search::parse(text).unwrap();
            let start = std::time::Instant::now();
            let (results, _) =
                crate::index::search::run(&conn, &request, &query, &[], &[], &|| false).unwrap();
            println!(
                "search {text:?}: {} shown in {:.1} ms",
                results.hits.len(),
                start.elapsed().as_secs_f64() * 1e3
            );
        }
    }
}
