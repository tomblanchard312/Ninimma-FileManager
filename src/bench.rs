// Ninimma modification: independent project branding and integration.
//! Benchmarks with baseline numbers in the opt-in benchmark suite. Opt-in and
//! single-threaded, on files made from scratch in a temporary folder:
//! `cargo test --release bench_ -- --ignored --nocapture --test-threads=1`
//! (or `tools/bench.sh`, which adds startup and idle). Set
//! NINIMMA_BENCH_DIR to put the fixtures on a particular disk; the
//! default is the system temp folder. NINIMMA_BENCH_LARGE=1 adds the
//! 100,000-file folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::file_system::{read_items, FileItem};
use crate::thumbnail::{load_or_generate, ThumbSize};
use crate::ui::{sort_items, SortBy};

/// A fresh folder for fixtures, removed when dropped.
pub(crate) fn scratch() -> tempfile::TempDir {
    match std::env::var_os("NINIMMA_BENCH_DIR") {
        Some(dir) => tempfile::tempdir_in(dir).unwrap(),
        None => tempfile::tempdir().unwrap(),
    }
}

/// Runs `f` `runs` times and prints the fastest and the median.
pub(crate) fn measure(label: &str, runs: usize, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed()
        })
        .collect();
    times.sort();
    let median = times[times.len() / 2];
    eprintln!(
        "BENCH {label:<44} median {:>10.2} ms  min {:>10.2} ms  ({runs} runs)",
        ms(median),
        ms(times[0])
    );
    median
}

pub(crate) fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn sizes() -> Vec<usize> {
    let mut n = vec![10_000];
    if std::env::var_os("NINIMMA_BENCH_LARGE").is_some() {
        n.push(100_000);
    }
    n
}

/// `n` small files with mixed names, sizes, extensions and dates, plus a
/// few folders, as in a busy Downloads folder.
fn make_folder(dir: &Path, n: usize) {
    const EXT: &[&str] = &["jpg", "pdf", "txt", "docx", "png", "zip", "mp4", "rs"];
    for i in 0..n {
        let path = if i % 100 == 0 {
            let p = dir.join(format!("Folder {i}"));
            fs::create_dir(&p).unwrap();
            p
        } else {
            let p = dir.join(format!("file {} v{i}.{}", i % 37, EXT[i % EXT.len()]));
            fs::write(&p, vec![b'x'; (i * 7919) % 4096]).unwrap();
            p
        };
        let when = SystemTime::UNIX_EPOCH
            + Duration::from_secs(1_600_000_000 + (i as u64 * 104_729) % 100_000_000);
        fs::File::options()
            .write(true)
            .open(&path)
            .or_else(|_| fs::File::open(&path))
            .and_then(|f| f.set_modified(when))
            .ok();
    }
}

#[test]
#[ignore]
fn bench_large_folders() {
    for n in sizes() {
        let dir = scratch();
        make_folder(dir.path(), n);
        let mut items: Vec<FileItem> = Vec::new();
        // Warm: the folder was just written, so its entries are cached, as
        // when reopening a folder.
        measure(&format!("list {n} files"), 5, || {
            items = read_items(dir.path(), false).unwrap();
        });
        assert_eq!(items.len(), n);
        for (by, name) in [
            (SortBy::Name, "name"),
            (SortBy::Size, "size"),
            (SortBy::Modified, "date"),
            (SortBy::Type, "type"),
        ] {
            measure(&format!("sort {n} files by {name}"), 5, || {
                let mut copy = items.clone();
                sort_items(&mut copy, by, true);
            });
        }
    }
}

/// Camera-sized photos (4000x3000 JPEG), thumbnailed at 256 px.
#[test]
#[ignore]
fn bench_thumbnails() {
    const PHOTOS: usize = 20;
    let dir = scratch();
    let img = image::RgbImage::from_fn(4000, 3000, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8])
    });
    let photos: Vec<PathBuf> = (0..PHOTOS)
        .map(|i| {
            let p = dir.path().join(format!("IMG_{i:04}.jpg"));
            img.save(&p).unwrap();
            p
        })
        .collect();
    let cache = dir.path().join("cache");
    fs::create_dir(&cache).unwrap();
    let cold = measure(&format!("thumbnail {PHOTOS} photos, no cache"), 1, || {
        for p in &photos {
            load_or_generate(p, ThumbSize::Icon, &cache).unwrap();
        }
    });
    let warm = measure(&format!("thumbnail {PHOTOS} photos, cached"), 3, || {
        for p in &photos {
            load_or_generate(p, ThumbSize::Icon, &cache).unwrap();
        }
    });
    eprintln!(
        "BENCH per photo: {:.1} ms uncached, {:.2} ms cached (one thread)",
        ms(cold) / PHOTOS as f64,
        ms(warm) / PHOTOS as f64
    );
}

#[test]
#[ignore]
fn bench_transfers() {
    const BIG: usize = 1 << 30;
    const SMALL: usize = 10_000;
    let dir = scratch();
    let big = dir.path().join("big.bin");
    // Not all zeros, so nothing can shortcut it.
    let block: Vec<u8> = (0..1 << 20).map(|i| (i * 31 % 251) as u8).collect();
    {
        use std::io::Write;
        let mut f = fs::File::create(&big).unwrap();
        for _ in 0..BIG / block.len() {
            f.write_all(&block).unwrap();
        }
        f.sync_all().unwrap();
    }
    let mut n = 0;
    let t = measure("copy one 1 GiB file", 3, || {
        n += 1;
        let dest = dir.path().join(format!("big copy {n}.bin"));
        crate::file_ops::copy_new(&big, &dest).unwrap();
        fs::remove_file(dest).unwrap();
    });
    eprintln!(
        "BENCH   {:.0} MiB/s (page cache warm; not synced)",
        1024.0 / t.as_secs_f64()
    );

    let small = dir.path().join("small");
    fs::create_dir(&small).unwrap();
    for i in 0..SMALL {
        fs::write(small.join(format!("{i}.txt")), &block[..4096]).unwrap();
    }
    let t = measure(&format!("copy a folder of {SMALL} 4 KiB files"), 3, || {
        n += 1;
        let dest = dir.path().join(format!("small copy {n}"));
        crate::file_ops::copy_new(&small, &dest).unwrap();
        fs::remove_dir_all(dest).unwrap();
    });
    eprintln!("BENCH   {:.0} files/s", SMALL as f64 / t.as_secs_f64());
}
