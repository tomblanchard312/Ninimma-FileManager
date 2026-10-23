// Ninimma modification: independent project branding and integration.
//! Compress to ZIP and Extract all, like Explorer's "Send to → Compressed
//! (zipped) folder" and "Extract All". Nothing is ever overwritten: the new
//! archive or folder gets a free name ("Photos (2).zip").

use crate::file_ops::{display_name, unique_name};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use zip::write::SimpleFileOptions;

/// Where compressing `paths` puts the archive: named after the first item,
/// next to it.
pub fn zip_name_for(paths: &[PathBuf]) -> Option<PathBuf> {
    let first = paths.first()?;
    let dir = first.parent()?;
    let name = display_name(first);
    let stem = if first.is_dir() {
        name.as_str()
    } else {
        name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s)
    };
    Some(unique_name(dir, stem, ".zip", |n| format!(" ({n})")))
}

/// Writes `paths` (files and whole folders) into a new zip at `dest`. A
/// failed archive is removed again.
pub fn compress(paths: &[PathBuf], dest: &Path) -> Result<(), String> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .map_err(|e| format!("Couldn't create {}: {e}", display_name(dest)))?;
    let result = (|| {
        let mut zip = zip::ZipWriter::new(file);
        for path in paths {
            add(&mut zip, path, &display_name(path))?;
        }
        zip.finish()
            .map_err(|e| format!("Couldn't write the archive: {e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(dest);
    }
    result
}

fn add(zip: &mut zip::ZipWriter<fs::File>, path: &Path, name: &str) -> Result<(), String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|e| format!("Couldn't read {}: {e}", display_name(path)))?;
    let mut options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(meta.len() >= u32::MAX as u64);
    if let Some(time) = meta
        .modified()
        .ok()
        .map(chrono::DateTime::<chrono::Local>::from)
        .and_then(|t| {
            use chrono::{Datelike, Timelike};
            zip::DateTime::from_date_and_time(
                u16::try_from(t.year()).ok()?,
                t.month() as u8,
                t.day() as u8,
                t.hour() as u8,
                t.minute() as u8,
                t.second() as u8,
            )
            .ok()
        })
    {
        options = options.last_modified_time(time);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        options = options.unix_permissions(meta.permissions().mode());
    }
    let zip_err = |e: zip::result::ZipError| format!("Couldn't add {}: {e}", display_name(path));
    if meta.file_type().is_symlink() {
        // Links aren't followed (they may loop or point anywhere).
        return Ok(());
    }
    if meta.is_dir() {
        zip.add_directory(format!("{name}/"), options)
            .map_err(zip_err)?;
        let mut entries: Vec<_> = fs::read_dir(path)
            .map_err(|e| format!("Couldn't read {}: {e}", display_name(path)))?
            .flatten()
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let child = format!("{name}/{}", entry.file_name().to_string_lossy());
            add(zip, &entry.path(), &child)?;
        }
        return Ok(());
    }
    zip.start_file(name, options).map_err(zip_err)?;
    let mut from =
        fs::File::open(path).map_err(|e| format!("Couldn't read {}: {e}", display_name(path)))?;
    std::io::copy(&mut from, zip)
        .map_err(|e| format!("Couldn't add {}: {e}", display_name(path)))?;
    Ok(())
}

pub fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Where extracting `archive` goes: a new folder named after it, next to it.
pub fn folder_for(archive: &Path) -> Option<PathBuf> {
    let dir = archive.parent()?;
    let stem = archive.file_stem()?.to_string_lossy().to_string();
    Some(unique_name(dir, &stem, "", |n| format!(" ({n})")))
}

/// Unpacks `archive` into the new folder `dest`. Entries that would land
/// outside it ("../", absolute paths) are refused. A failed extraction
/// removes the folder again.
pub fn extract(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = fs::File::open(archive)
        .map_err(|e| format!("Couldn't open {}: {e}", display_name(archive)))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| format!("{} isn't a readable zip file: {e}", display_name(archive)))?;
    fs::create_dir(dest).map_err(|e| format!("Couldn't create {}: {e}", display_name(dest)))?;
    let result = (|| {
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| format!("Couldn't read the archive: {e}"))?;
            let Some(relative) = entry
                .enclosed_name()
                .filter(|p| p.components().all(|c| matches!(c, Component::Normal(_))))
            else {
                return Err(format!(
                    "\"{}\" would land outside the folder",
                    entry
                        .name()
                        .unwrap_or_else(|_| "invalid ZIP filename".into())
                ));
            };
            let target = dest.join(relative);
            if entry.is_dir() {
                fs::create_dir_all(&target)
                    .map_err(|e| format!("Couldn't create {}: {e}", display_name(&target)))?;
                continue;
            }
            if entry.is_symlink() {
                continue; // never create links from an archive
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Couldn't create {}: {e}", display_name(parent)))?;
            }
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| format!("Couldn't create {}: {e}", display_name(&target)))?;
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = entry
                    .read(&mut buf)
                    .map_err(|e| format!("Couldn't extract {}: {e}", display_name(&target)))?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])
                    .map_err(|e| format!("Couldn't extract {}: {e}", display_name(&target)))?;
            }
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                // Never setuid/setgid from an archive.
                let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777));
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_then_extract_round_trips_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("Photos");
        fs::create_dir_all(photos.join("2025")).unwrap();
        fs::write(photos.join("2025/beach.jpg"), b"jpeg").unwrap();
        fs::write(dir.path().join("notes.txt"), "hello").unwrap();
        let items = vec![photos.clone(), dir.path().join("notes.txt")];

        let zip = zip_name_for(&items).unwrap();
        assert_eq!(zip, dir.path().join("Photos.zip"));
        compress(&items, &zip).unwrap();
        assert_eq!(
            zip_name_for(&items).unwrap(),
            dir.path().join("Photos (2).zip")
        );

        // "Photos" exists, so the folder gets a free name.
        let out = folder_for(&zip).unwrap();
        assert_eq!(out, dir.path().join("Photos (2)"));
        extract(&zip, &out).unwrap();
        assert_eq!(
            fs::read(out.join("Photos/2025/beach.jpg")).unwrap(),
            b"jpeg"
        );
        assert_eq!(fs::read_to_string(out.join("notes.txt")).unwrap(), "hello");
    }

    #[test]
    fn entries_outside_the_folder_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("evil.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        zip.start_file("../escaped.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();

        let out = dir.path().join("evil");
        assert!(extract(&zip_path, &out).is_err());
        assert!(!dir.path().join("escaped.txt").exists());
        assert!(!out.exists(), "the half-made folder is removed");
    }

    #[test]
    fn extracted_links_cannot_redirect_writes_outside_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"keep").unwrap();
        let path = dir.path().join("links.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.add_symlink(
            "link",
            outside.to_string_lossy(),
            SimpleFileOptions::default(),
        )
        .unwrap();
        zip.start_file("link/new.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"inside only").unwrap();
        zip.finish().unwrap();
        let out = dir.path().join("extracted");
        extract(&path, &out).unwrap();
        assert!(!out.join("link").is_symlink());
        assert_eq!(fs::read(out.join("link/new.txt")).unwrap(), b"inside only");
        assert!(!outside.join("new.txt").exists());
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn extracted_files_never_receive_setuid_or_setgid_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("modes.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("tool", SimpleFileOptions::default().unix_permissions(0o755))
            .unwrap();
        zip.write_all(b"payload").unwrap();
        zip.finish().unwrap();
        // Inject bits that well-behaved ZIP writers deliberately strip.
        let mut bytes = fs::read(&path).unwrap();
        let central = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        bytes[central + 38..central + 42].copy_from_slice(&(0o106755_u32 << 16).to_le_bytes());
        fs::write(&path, bytes).unwrap();
        let out = dir.path().join("extracted");
        extract(&path, &out).unwrap();
        assert_eq!(
            fs::metadata(out.join("tool")).unwrap().permissions().mode() & 0o7777,
            0o755
        );
    }

    #[cfg(feature = "smart-search")]
    #[test]
    fn patched_candle_reads_zip9_tensor_archives() {
        use candle_core::{npy::NpzTensors, Device, Tensor};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tensor.npz");
        let tensor = Tensor::from_slice(&[1.0_f32, 2.0], 2, &Device::Cpu).unwrap();
        Tensor::write_npz(&[("sample", &tensor)], &path).unwrap();
        let loaded = Tensor::read_npz(&path).unwrap();
        assert_eq!(loaded[0].0, "sample");
        assert_eq!(loaded[0].1.to_vec1::<f32>().unwrap(), vec![1.0, 2.0]);
        let lazy = NpzTensors::new(&path).unwrap();
        assert_eq!(
            lazy.get("sample")
                .unwrap()
                .unwrap()
                .to_vec1::<f32>()
                .unwrap(),
            vec![1.0, 2.0]
        );
        // The pickle reader scans filenames too; this archive contains no pickle tensors.
        assert!(
            candle_core::pickle::read_pth_tensor_info(&path, false, None)
                .unwrap()
                .is_empty()
        );
    }
}
